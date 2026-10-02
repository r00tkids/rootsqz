#!/usr/bin/env node
import child_process from "node:child_process";
import { binaryPath, toolEnvironment } from "../dist/index.js";

let executable;
try {
  executable = binaryPath();
} catch (err) {
  console.error(err.message);
  process.exit(1);
}

const result = child_process.spawnSync(executable, process.argv.slice(2), {
  env: toolEnvironment(),
  stdio: "inherit",
});

if (result.error) {
  console.error(`Failed to run '${executable}': ${result.error.message}`);
  process.exit(1);
}

process.exit(result.status ?? 1);
