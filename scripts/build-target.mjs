#!/usr/bin/env node
// Builds the rootsqz executable for one target and puts it in its npm platform package,
// as npm/<platform>/bin/rootsqz[.exe].
//
// Usage: node scripts/build-target.mjs [<rust target triple> | <npm platform>]
// Without an argument it builds for the platform it runs on.
//
// Targets other than the host need the rust target (rustup target add <triple>).
// The Linux targets are built with cargo-zigbuild, which needs zig.
import child_process from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const TARGETS = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "linux-x64": "x86_64-unknown-linux-musl",
  "win32-x64": "x86_64-pc-windows-msvc",
};

const root = path.resolve(import.meta.dirname, "..");

function fail(message) {
  console.error(message);
  process.exit(1);
}

function hostTriple() {
  const out = child_process.execFileSync("rustc", ["-vV"], { encoding: "utf-8" });
  return /^host: (.+)$/m.exec(out)[1].trim();
}

const arg = process.argv[2] ?? `${process.platform}-${process.arch}`;
const platform = arg in TARGETS ? arg : Object.keys(TARGETS).find((key) => TARGETS[key] === arg);
if (!platform) {
  fail(`Unknown target '${arg}'. Known targets:\n${Object.entries(TARGETS).map(([key, triple]) => `  ${key} (${triple})`).join("\n")}`);
}
const triple = TARGETS[platform];
const host = hostTriple();

const env = { ...process.env };
let subcommand = "build";
if (triple.includes("-linux-")) {
  subcommand = "zigbuild";
} else if (triple.includes("-windows-")) {
  if (!host.includes("-windows-")) {
    fail(`${triple} can only be built on Windows.`);
  }
  // Link the C runtime statically, so the executable runs without the Visual C++ redistributable
  env.RUSTFLAGS = [env.RUSTFLAGS, "-C target-feature=+crt-static"].filter(Boolean).join(" ");
} else if (!host.includes("-apple-")) {
  fail(`${triple} can only be built on macOS.`);
}

const cargoArgs = [subcommand, "--release", "--target", triple];
console.log(`cargo ${cargoArgs.join(" ")}`);
const result = child_process.spawnSync("cargo", cargoArgs, { cwd: root, env, stdio: "inherit" });
if (result.error) {
  fail(`Failed to run cargo: ${result.error.message}`);
}
if (result.status !== 0) {
  process.exit(result.status ?? 1);
}

const executable = triple.includes("-windows-") ? "rootsqz.exe" : "rootsqz";
const packageDir = path.join(root, "npm", platform);
const destination = path.join(packageDir, "bin", executable);
fs.mkdirSync(path.dirname(destination), { recursive: true });
fs.copyFileSync(path.join(root, "target", triple, "release", executable), destination);
fs.chmodSync(destination, 0o755);
fs.copyFileSync(path.join(root, "LICENSE"), path.join(packageDir, "LICENSE"));

console.log(`Built ${path.relative(root, destination)}`);
