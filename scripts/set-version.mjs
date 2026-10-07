#!/usr/bin/env node
// The crate and all npm packages share one version.
//
// Usage: node scripts/set-version.mjs <version>   sets the version everywhere
//        node scripts/set-version.mjs --check     fails if the versions differ
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");

function packageJsonFiles() {
  const files = ["package.json", "examples/vite/package.json"];
  for (const dir of ["packages", "npm"]) {
    for (const entry of fs.readdirSync(path.join(root, dir)).sort()) {
      const file = path.join(dir, entry, "package.json");
      if (fs.existsSync(path.join(root, file))) {
        files.push(file);
      }
    }
  }
  return files;
}

// The first version line of Cargo.toml is the one of [package]
const CARGO_TOML_VERSION = /^version = "(.+)"$/m;
const CARGO_LOCK_VERSION = /(\[\[package\]\]\nname = "rootsqz"\nversion = ")(.+)(")/;

function read(file) {
  return fs.readFileSync(path.join(root, file), "utf-8");
}

const arg = process.argv[2];
if (!arg) {
  console.error("Usage: node scripts/set-version.mjs <version> | --check");
  process.exit(1);
}

if (arg === "--check") {
  const versions = new Map();
  versions.set("Cargo.toml", CARGO_TOML_VERSION.exec(read("Cargo.toml"))?.[1]);
  versions.set("Cargo.lock", CARGO_LOCK_VERSION.exec(read("Cargo.lock"))?.[2]);
  for (const file of packageJsonFiles()) {
    versions.set(file, JSON.parse(read(file)).version);
  }

  const expected = versions.get("Cargo.toml");
  const different = [...versions].filter(([, version]) => version !== expected);
  if (different.length > 0) {
    console.error(`Versions differ from ${expected} in Cargo.toml:`);
    for (const [file, version] of different) {
      console.error(`  ${file}: ${version}`);
    }
    process.exit(1);
  }
  console.log(`All versions are ${expected}`);
} else {
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(arg)) {
    console.error(`'${arg}' is not a version`);
    process.exit(1);
  }

  fs.writeFileSync(path.join(root, "Cargo.toml"), read("Cargo.toml").replace(CARGO_TOML_VERSION, `version = "${arg}"`));
  fs.writeFileSync(path.join(root, "Cargo.lock"), read("Cargo.lock").replace(CARGO_LOCK_VERSION, `$1${arg}$3`));
  for (const file of packageJsonFiles()) {
    fs.writeFileSync(path.join(root, file), read(file).replace(/^(  "version": ")(.+)(")/m, `$1${arg}$3`));
  }
  console.log(`Set all versions to ${arg}`);
}
