import {execFileSync} from "node:child_process";
import {existsSync, mkdirSync, readFileSync, rmSync} from "node:fs";
import {join, resolve} from "node:path";
import {fileURLToPath} from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const [target, requestedBundles] = process.argv.slice(2);
const macTargets = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
const mac = target === "universal-apple-darwin" || macTargets.includes(target);
const windows = target === "x86_64-pc-windows-msvc";
if ((!mac || process.platform !== "darwin") && (!windows || process.platform !== "win32")) {
    throw new Error("请在对应系统传入 macOS target 或 x86_64-pc-windows-msvc");
}

const tauriDir = join(root, "src-tauri");
const tauriCli = join(root, "ui/node_modules/@tauri-apps/cli/tauri.js");
if (!existsSync(tauriCli)) {
    throw new Error("请先执行 pnpm --dir ui install --frozen-lockfile");
}

const targetDir = resolve(root, process.env.CARGO_TARGET_DIR || "target");
const env = {...process.env, CARGO_TARGET_DIR: targetDir};
if (mac) {
    for (const name of ["APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY"]) {
        if (env[name] === "") {
            delete env[name];
        }
    }
    if (!env.APPLE_SIGNING_IDENTITY && !env.APPLE_CERTIFICATE) {
        const config = JSON.parse(readFileSync(join(tauriDir, "tauri.conf.json"), "utf8"));
        if (!config.bundle?.macOS?.signingIdentity) {
            env.APPLE_SIGNING_IDENTITY = "-";
        }
    }
}
const targets = target === "universal-apple-darwin" ? macTargets : [target];
const shimCrate = mac ? "localproxy-shim-macos" : "localproxy-shim-win";
const core = mac ? "localproxy-core" : "localproxy-core.exe";
const shim = mac ? "liblocalproxy_shim_macos.dylib" : "localproxy_shim_win.dll";
for (const arch of targets) {
    execFileSync("cargo", ["build", "--locked", "--release", "--target", arch, "-p", "localproxy-core", "-p", shimCrate], {
        cwd: root, env, stdio: "inherit",
    });
}

const bundle = {active: true, resources: {}};
const resourceDir = join(targetDir, target, "localproxy-resources");
mkdirSync(resourceDir, {recursive: true});
for (const name of [core, shim]) {
    const inputs = targets.map(arch => join(targetDir, arch, "release", name));
    let source = inputs[0];
    if (targets.length > 1) {
        source = join(resourceDir, name);
        rmSync(source, {force: true});
        execFileSync("lipo", ["-create", ...inputs, "-output", source], {stdio: "inherit"});
    }
    if (mac) {
        execFileSync("codesign", ["--force", "--sign", "-", source], {stdio: "inherit"});
    }
    bundle.resources[source] = name;
}

const bundles = requestedBundles ?? (mac ? "app,dmg" : "msi,nsis");
execFileSync(process.execPath, [tauriCli, "build", "--target", target, "--bundles", bundles,
    "--config", JSON.stringify({bundle}), "--", "--locked"], {
    cwd: tauriDir, env, stdio: "inherit",
});
