// M10 contract coverage: every frontend API operation must either map to a
// generated App Protocol command (with a typed SDK method and a real dispatch
// arm) or be an explicitly documented static/blob/SSE/M11-runtime exception.
//
// The gate deliberately reads source text instead of trusting the inventory:
// operations are discovered from src/api/*.ts, and the sdk half is checked
// against the generated Command union, the typed CoolSdk class members, and
// the cool-app-server dispatch sources.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

type Status = "sdk" | "static/blob" | "sse/stream" | "deferred";

interface InventoryOperation {
  file: string;
  member: string;
  source?: string;
  http?: string;
  status: Status;
  command?: string;
  rationale?: string;
}

interface Inventory {
  version: number;
  operations: InventoryOperation[];
}

interface DetectedOperation {
  file: string;
  member: string;
  source: string;
}

const STATUSES: readonly Status[] = ["sdk", "static/blob", "sse/stream", "deferred"];
const EXCLUDED_FILES = new Set(["client.ts", "types.ts", "canonicalReducer.ts"]);
// A 2-space line inside an object body that starts with a keyword is body
// code, not an exported operation (the parser is intentionally source-shape
// based: these files keep members at two spaces and bodies deeper).
const CONTROL_KEYWORDS = new Set([
  "if",
  "for",
  "while",
  "switch",
  "return",
  "const",
  "let",
  "await",
  "throw",
  "catch",
  "else",
]);
const DISPATCH_FILES = [
  "legacy/mod.rs",
  "legacy/memory.rs",
  "legacy/admin.rs",
  "lib.rs",
].map((name) => resolve(process.cwd(), "..", "crates", "cool-app-server", "src", name));

const scopedKey = (file: string, source: string | undefined, member: string) =>
  `${file}\u0000${source ?? ""}\u0000${member}`;
const plainKey = (file: string, member: string) => `${file}\u0000${member}`;

function discoverOperations(directory: string): DetectedOperation[] {
  const discovered: DetectedOperation[] = [];
  const files = readdirSync(directory)
    .filter((name) => name.endsWith(".ts") && !EXCLUDED_FILES.has(name))
    .sort();
  for (const file of files) {
    const text = readFileSync(resolve(directory, file), "utf8");
    let object: string | null = null;
    for (const line of text.split(/\r?\n/)) {
      const objectStart = /^export const (\w+)(?::[^=]+)? = \{$/.exec(line);
      if (objectStart) {
        object = objectStart[1];
        continue;
      }
      if (object !== null && /^\};?\s*$/.test(line)) {
        object = null;
        continue;
      }
      if (object !== null) {
        const member =
          /^ {2}(?:async )?(\w+)(?:\(|:)/.exec(line) ?? /^ {2}(?:async )?(\w+)\(.*\)\s*\{$/.exec(line);
        if (member && !CONTROL_KEYWORDS.has(member[1])) {
          discovered.push({ file, member: member[1], source: object });
        }
      }
      const fn = /^export (?:async )?function\*? (\w+)/.exec(line);
      if (fn) {
        discovered.push({ file, member: fn[1], source: "" });
        continue;
      }
      const arrow = /^export const (\w+)(?::[^=]+)? = (?:async )?\(/.exec(line);
      if (arrow && !objectStart) {
        discovered.push({ file, member: arrow[1], source: "" });
      }
    }
  }
  return discovered;
}

function generatedCommandMethods(typescript: string): Set<string> {
  const methods = new Set<string>();
  const pattern = /"method": "([^"]+)", "params":/g;
  for (const match of typescript.matchAll(pattern)) {
    methods.add(match[1]);
  }
  return methods;
}

/** Parse `CoolSdk` members that carry a command method literal on the same line. */
function sdkCommandMembers(source: string): Map<string, string> {
  const members = new Map<string, string>();
  let inClass = false;
  for (const line of source.split(/\r?\n/)) {
    if (/^export class CoolSdk \{/.test(line)) {
      inClass = true;
      continue;
    }
    if (inClass && /^\}/.test(line)) {
      inClass = false;
      continue;
    }
    if (!inClass) continue;
    const member = /^ {2}(\w+)\(/.exec(line);
    const command = /method: "([^"]+)"/.exec(line);
    if (member && command) {
      assert.ok(
        !members.has(command[1]),
        `duplicate SDK method for command ${command[1]}`,
      );
      members.set(command[1], member[1]);
    }
  }
  return members;
}

/** `memory.list` -> `MemoryList`; `workspace.git_info` -> `WorkspaceGitInfo`. */
function rustVariantName(command: string): string {
  return command
    .split(".")
    .flatMap((part) => part.split("_"))
    .map((part) => `${part[0].toUpperCase()}${part.slice(1)}`)
    .join("");
}

/** `memory.list` -> `memoryList`; `tasks.runs_read` -> `tasksRunsRead`. */
function sdkMemberName(command: string): string {
  const parts = command.split(".").flatMap((part) => part.split("_"));
  return parts
    .map((part, index) => (index === 0 ? part : `${part[0].toUpperCase()}${part.slice(1)}`))
    .join("");
}

export function runCoverage(): void {
  const root = process.cwd();
  const inventory = JSON.parse(
    readFileSync(resolve(root, "protocol-tests", "inventory.json"), "utf8"),
  ) as Inventory;
  assert.equal(inventory.version, 1, "unsupported inventory version");

  const detected = discoverOperations(resolve(root, "src", "api"));
  const byScoped = new Map<string, InventoryOperation>();
  const byPlain = new Map<string, InventoryOperation>();
  const unconsumed = new Set<InventoryOperation>();
  for (const operation of inventory.operations) {
    assert.ok(STATUSES.includes(operation.status), `bad status for ${operation.member}`);
    if (operation.source === "backend-only") continue;
    if (operation.source) {
      const operationKey = scopedKey(operation.file, operation.source, operation.member);
      assert.ok(!byScoped.has(operationKey), `duplicate inventory entry ${operationKey}`);
      byScoped.set(operationKey, operation);
    } else {
      const operationKey = plainKey(operation.file, operation.member);
      assert.ok(!byPlain.has(operationKey), `duplicate inventory entry ${operationKey}`);
      byPlain.set(operationKey, operation);
    }
    unconsumed.add(operation);
  }

  for (const operation of detected) {
    const match =
      byScoped.get(scopedKey(operation.file, operation.source, operation.member)) ??
      byScoped.get(scopedKey(operation.file, "", operation.member)) ??
      byPlain.get(plainKey(operation.file, operation.member));
    assert.ok(
      match,
      `undeclared frontend API operation: ${operation.file}#${operation.member} (${operation.source || "module"})`,
    );
    unconsumed.delete(match);
  }
  assert.equal(
    unconsumed.size,
    0,
    `inventory entries with no matching source: ${[...unconsumed]
      .map((operation) => `${operation.file}#${operation.member}`)
      .join(", ")}`,
  );

  const generated = generatedCommandMethods(
    readFileSync(resolve(root, "src", "api", "generated", "cool_protocol.ts"), "utf8"),
  );
  const sdk = sdkCommandMembers(
    readFileSync(resolve(root, "..", "sdk", "typescript", "src", "client.ts"), "utf8"),
  );
  const dispatch = DISPATCH_FILES.map((path) => readFileSync(path, "utf8")).join("\n");

  let sdkCount = 0;
  const exceptionCounts: Record<string, number> = {};
  for (const operation of inventory.operations) {
    if (operation.status === "sdk") {
      assert.ok(operation.command, `sdk operation ${operation.file}#${operation.member} has no command`);
      assert.ok(
        generated.has(operation.command),
        `command ${operation.command} is missing from the generated Command union`,
      );
      const member = sdk.get(operation.command);
      assert.ok(
        member,
        `command ${operation.command} has no paired typed SDK method in sdk/typescript/src/client.ts`,
      );
      assert.equal(
        member,
        sdkMemberName(operation.command),
        `SDK method for ${operation.command} is named ${member}, expected ${sdkMemberName(operation.command)}`,
      );
      assert.ok(
        dispatch.includes(`Command::${rustVariantName(operation.command)}(`),
        `command ${operation.command} has no dispatch arm (Command::${rustVariantName(operation.command)}(...) in cool-app-server`,
      );
      sdkCount += 1;
    } else {
      assert.ok(
        operation.rationale && operation.rationale.length > 0,
        `non-sdk operation ${operation.file}#${operation.member} needs a rationale`,
      );
      exceptionCounts[operation.status] = (exceptionCounts[operation.status] ?? 0) + 1;
    }
  }

  for (const method of sdk.keys()) {
    assert.ok(
      generated.has(method),
      `SDK method ${method} does not exist in the generated Command union`,
    );
  }

  console.log(
    `protocol coverage passed: ${sdkCount} sdk operations across ${generated.size} commands, ` +
      `exceptions ${JSON.stringify(exceptionCounts)}`,
  );
}
