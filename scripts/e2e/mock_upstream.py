#!/usr/bin/env python3
"""LocalProxy e2e mock SOCKS5 上游。

行为：
1. 完成 SOCKS5 方法协商（无认证）；
2. 记录每个 CONNECT 目标到 --seen 文件（供断言）；
3. 隧道建立后直接返回一个固定 HTTP 200 响应（MOCK-UPSTREAM-OK），
   并把客户端发送的首个请求体回显（验证双向数据通路）。

用法：
    python3 mock_upstream.py --port 19080 --seen /tmp/lp-e2e-seen.txt
"""

import argparse
import socket
import struct
import sys
import threading
import time

HTTP_RESPONSE = (
    b"HTTP/1.1 200 OK\r\n"
    b"Content-Type: text/plain\r\n"
    b"Content-Length: 18\r\n"
    b"Connection: close\r\n"
    b"\r\n"
    b"MOCK-UPSTREAM-OK\n"
)


def recv_exact(conn: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("eof")
        buf += chunk
    return buf


def read_socks5_request(conn: socket.socket) -> str:
    head = recv_exact(conn, 4)
    ver, cmd, rsv, atyp = head
    if atyp == 0x01:
        addr = socket.inet_ntoa(recv_exact(conn, 4))
    elif atyp == 0x04:
        addr = socket.inet_ntop(socket.AF_INET6, recv_exact(conn, 16))
    elif atyp == 0x03:
        length = recv_exact(conn, 1)[0]
        addr = recv_exact(conn, length).decode("utf-8", "replace")
    else:
        raise ValueError(f"bad atyp {atyp}")
    port = struct.unpack(">H", recv_exact(conn, 2))[0]
    return f"{addr}:{port}"


def handle(conn: socket.socket, seen_path: str) -> None:
    try:
        # 方法协商
        head = recv_exact(conn, 2)
        nmethods = head[1]
        recv_exact(conn, nmethods)
        conn.sendall(b"\x05\x00")
        # CONNECT 请求
        target = read_socks5_request(conn)
        with open(seen_path, "a") as f:
            f.write(target + "\n")
        # 成功响应（BND 0.0.0.0:0）
        conn.sendall(b"\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00")
        # 发送固定 HTTP 响应
        conn.sendall(HTTP_RESPONSE)
        # 读取客户端请求（验证上行通路）后保持到对端关闭
        conn.settimeout(3)
        try:
            while True:
                data = conn.recv(4096)
                if not data:
                    break
        except socket.timeout:
            pass
    except Exception as e:  # noqa: BLE001
        print(f"[mock] connection error: {e}", file=sys.stderr)
    finally:
        conn.close()


def udp_echo_loop(sock: socket.socket, seen_path: str) -> None:
    """UDP echo：记录每个收到的包（供断言放行/阻断）。"""
    while True:
        try:
            data, addr = sock.recvfrom(4096)
        except OSError:
            return
        with open(seen_path, "a") as f:
            f.write(f"{addr[0]}:{addr[1]} {data.decode('utf-8', 'replace')}\n")
        try:
            sock.sendto(data, addr)
        except OSError:
            return


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=19080)
    parser.add_argument("--seen", default="/tmp/lp-e2e-seen.txt")
    parser.add_argument("--ready-file", default=None)
    parser.add_argument("--udp-port", type=int, default=0, help="UDP echo 端口（0=不监听）")
    parser.add_argument("--udp-seen", default="/tmp/lp-e2e-udp-seen.txt")
    args = parser.parse_args()

    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", args.port))
    server.listen(16)
    print(f"[mock] listening on 127.0.0.1:{args.port}", flush=True)
    if args.ready_file:
        with open(args.ready_file, "w") as f:
            f.write("ready")

    # UDP echo（验证「局域网 UDP 放行、公网 UDP 阻断」）
    if args.udp_port > 0:
        udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        udp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        udp.bind(("127.0.0.1", args.udp_port))
        print(f"[mock] udp echo on 127.0.0.1:{args.udp_port}", flush=True)
        threading.Thread(target=udp_echo_loop, args=(udp, args.udp_seen), daemon=True).start()

    try:
        while True:
            conn, _ = server.accept()
            threading.Thread(target=handle, args=(conn, args.seen), daemon=True).start()
    except KeyboardInterrupt:
        pass
    finally:
        server.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
