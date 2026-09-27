#!/usr/bin/env node
// Bump the workspace version in Cargo.toml and the cool-* entries in Cargo.lock.
// Usage: node scripts/bump-version.mjs 0.2.0
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2];
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version ?? "")) {
  console.error("usage: node scripts/bump-version.mjs <semver>");
  process.exit(2);
}

const tomlPath = join(root, "Cargo.toml");
const toml = readFileSync(tomlPath, "utf8");
const updated = toml.replace(
  /(\[workspace\.package\][^[]*?^version = )"[^"]*"/ms,
  `$1"${version}"`,
);
if (updated === toml) {
  console.error("workspace.package version not found in Cargo.toml");
  process.exit(1);
}
writeFileSync(tomlPath, updated);

const lockPath = join(root, "Cargo.lock");
const lines = readFileSync(lockPath, "utf8").split("\n");
let name = null;
let changed = 0;
const out = lines.map((line) => {
  const m = line.match(/^name = "([^"]+)"$/);
  if (m) {
    name = m[1];
  } else if (line.startsWith("[")) {
    name = null;
  } else if (name?.startsWith("cool-") && line.startsWith("version = ")) {
    changed += 1;
    name = null;
    return `version = "${version}"`;
  }
  return line;
});
if (changed === 0) {
  console.error("no cool-* packages found in Cargo.lock");
  process.exit(1);
}
writeFileSync(lockPath, out.join("\n"));

console.log(`version set to ${version} (${changed} lockfile entries)`);
