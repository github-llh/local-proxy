"""使用独立回环监听验证 macOS shim、核心和 SOCKS5 上游。"""

import contextlib
import http.client
import json
import os
import secrets
import selectors
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path


def read_ready(process):
    with selectors.DefaultSelector() as ready:
        ready.register(process.stdout, selectors.EVENT_READ)
        if not ready.select(10):
            raise TimeoutError(f"进程未就绪: {process.args[0]}")
    return process.stdout.readline()


@contextlib.contextmanager
def running(command, *, env=None, desktop=False, log_path):
    with log_path.open("w") as log, subprocess.Popen(
        command,
        stdin=subprocess.PIPE if desktop else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=log,
        env=env,
        text=True,
    ) as process:
        try:
            yield json.loads(read_ready(process))
        except Exception:
            print(log_path.read_text(), file=sys.stderr)
            raise
        finally:
            if process.poll() is None:
                if desktop:
                    process.stdin.close()
                else:
                    process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


def run_probe(client, env, args, expected_output, expected_code=0):
    result = subprocess.run(
        [client, *args], env=env, capture_output=True, text=True, timeout=8
    )
    output = result.stdout + result.stderr
    if result.returncode != expected_code or any(text not in output for text in expected_output):
        raise AssertionError(f"{args}: 退出码 {result.returncode}\n{output}")


def verify_injection(work, mock):
    name = "node.localproxy.invalid"
    config_path = work / "config.json"
    token = secrets.token_hex(16)
    listener_names = ("socks5_port", "http_port", "control_port")
    seen = work / "tcp.txt"
    udp_seen = work / "udp.txt"
    fixture = Path(__file__).parent
    target = Path(os.environ.get("CARGO_TARGET_DIR", "target")).resolve() / "debug"
    client = work / "test-client"
    subprocess.run(["clang", "-O1", "-o", client, fixture / "test_client.c"], check=True)
    shim = work / "shim.dylib"
    shutil.copyfile(target / "liblocalproxy_shim_macos.dylib", shim)
    subprocess.run(["codesign", "--force", "--sign", "-", shim], check=True)

    with contextlib.ExitStack() as sockets:
        occupied = {}
        for key in listener_names:
            sock = sockets.enter_context(socket.socket())
            sock.bind(("127.0.0.1", 0))
            sock.listen()
            occupied[key] = sock.getsockname()[1]

        config = {
            **occupied,
            "control_token": token,
            "upstream": {
                "active": "mock",
                "servers": [{"id": "mock", "type": "socks5", "host": "127.0.0.1", "port": mock["tcp"]}],
            },
            "proxy_rules": {"udp_mode": "block", "allowed_ports": [80, 3478]},
        }
        config_path.write_text(json.dumps(config))
        child_env = {key: value for key, value in os.environ.items() if not key.startswith(("LP_", "DYLD_"))}
        child_env["TMPDIR"] = str(work)
        with running(
            [target / "localproxy-core", "--desktop", "--config", config_path],
            env=child_env, desktop=True, log_path=work / "core.log",
        ) as core_info:
            assert all(core_info[key] != port for key, port in occupied.items())
            injected = {
                **child_env,
                "DYLD_INSERT_LIBRARIES": str(shim),
                "LP_MODE": "proxy",
                "LP_CORE_HOST": core_info["listen_host"],
                "LP_CORE_PORT": str(core_info["socks5_port"]),
                "LP_POLICY_PATH": core_info["policy_path"],
            }
            run_probe(client, injected, [name, "80"], ["MOCK-UPSTREAM-OK", "198.18."])
            run_probe(client, injected, ["--nonblock", name, "80"], ["MOCK-UPSTREAM-OK"])
            assert seen.read_text().splitlines() == [f"{name}:80", f"{name}:80"]
            run_probe(client, child_env, ["--dns", "localhost"], ["addr=127.0.0.1"])
            run_probe(client, injected, ["--dns", "localhost"], ["addr=127.0.0.1"])
            run_probe(client, injected, ["--dns", name], ["addr=198.18."])
            run_probe(
                client, injected, ["--udp", "203.0.113.36", "3478"], ["UDP_SENDTO_FAILED errno=51"], 2
            )
            run_probe(
                client, injected, ["--udp-proxy", "127.0.0.1", str(mock["udp"])],
                ["UDP_PROXY_RECV data=LP-UDP-PROXY-PROBE"],
            )
            assert "UDP ASSOCIATE" not in udp_seen.read_text()
            print("通过 TCP 代理、FakeIP、非阻塞连接、UDP 阻断及回环放行验证")

            def reload_udp(mode):
                config["proxy_rules"]["udp_mode"] = mode
                config_path.write_text(json.dumps(config))
                with contextlib.closing(
                    http.client.HTTPConnection("127.0.0.1", core_info["control_port"], timeout=5)
                ) as connection:
                    connection.request("POST", "/v1/config/reload", headers={"Authorization": f"Bearer {token}"})
                    response = connection.getresponse()
                    body = response.read()
                    assert response.status == 200, body

            reload_udp("direct")
            with subprocess.Popen(
                [client, "--udp-live"], env=injected, stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            ) as live:
                assert read_ready(live).strip() == "UDP_CONNECTED"
                reload_udp("block")
                _, errors = live.communicate(input="1", timeout=8)
                assert live.returncode == 0, errors
            print("通过已连接 UDP socket 的直连到阻断热切换验证")

            reload_udp("proxy")
            run_probe(
                client, injected, ["--udp-proxy", name, "3478"],
                ["UDP_PROXY_RECV data=LP-UDP-PROXY-PROBE"],
            )
            records = udp_seen.read_text().splitlines()
            assert f"UDP ASSOCIATE {name}:3478" in records, records
            print("通过运行期热切换及公网域名 UDP 中继往返验证")

        assert not Path(core_info["policy_path"]).exists()
        for key, port in occupied.items():
            with socket.create_connection(("127.0.0.1", port), timeout=2):
                assert core_info[key] != port


def main():
    with tempfile.TemporaryDirectory(prefix="localproxy-e2e-") as directory:
        work = Path(directory)
        command = [
            sys.executable, Path(__file__).with_name("mock_upstream.py"),
            "--port", "0", "--udp-port", "0",
            "--seen", work / "tcp.txt", "--udp-seen", work / "udp.txt",
        ]
        with running(command, log_path=work / "mock.log") as mock:
            verify_injection(work, mock)


if __name__ == "__main__":
    main()
