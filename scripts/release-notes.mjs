#!/usr/bin/env node
// Prints the section of a version in CHANGELOG.md, the notes of its GitHub release.
//
// Usage: node scripts/release-notes.mjs <version>   fails if CHANGELOG.md has no section for it
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");

const version = process.argv[2];
if (!version) {
  console.error("Usage: node scripts/release-notes.mjs <version>");
  process.exit(1);
}

// Line endings are CRLF in a Windows checkout
const lines = fs.readFileSync(path.join(root, "CHANGELOG.md"), "utf-8").split(/\r?\n/);
const start = lines.findIndex((line) => line.startsWith(`## [${version}]`));
if (start === -1) {
  console.error(`CHANGELOG.md has no section for ${version}. Add one starting with "## [${version}]".`);
  process.exit(1);
}

// The section ends at the next version heading, or at the link references at the end of the file
let end = start + 1;
while (end < lines.length && !lines[end].startsWith("## ") && !/^\[[^\]]+\]: /.test(lines[end])) {
  end++;
}

const notes = lines.slice(start + 1, end).join("\n").trim();
if (!notes) {
  console.error(`The section for ${version} in CHANGELOG.md is empty.`);
  process.exit(1);
}
console.log(notes);
