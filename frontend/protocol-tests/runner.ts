import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

import { replayCanonicalEvents } from "../src/api/canonicalReducer.js";
import type { EventEnvelope, GoldenTrace } from "../src/api/generated/cool_protocol.js";

// @ts-expect-error App Protocol v1 rejects every other schema version at compile time.
const unsupportedVersion: EventEnvelope["schemaVersion"] = 2;
void unsupportedVersion;

const goldenDirectory = resolve(process.cwd(), "..", "crates", "cool-protocol", "tests", "golden");
const files = readdirSync(goldenDirectory).filter((name) => name.endsWith(".json")).sort();
assert.equal(files.length, 12, "M1 requires all critical golden scenarios");

for (const file of files) {
  const trace = JSON.parse(readFileSync(resolve(goldenDirectory, file), "utf8")) as GoldenTrace;
  assert.deepEqual(replayCanonicalEvents(trace.events), trace.expectedState, `TS reducer drift: ${file}`);
}

const reconnect = JSON.parse(
  readFileSync(resolve(goldenDirectory, "cancel-reconnect.json"), "utf8"),
) as GoldenTrace;
assert.deepEqual(
  replayCanonicalEvents([...reconnect.events].reverse()),
  reconnect.expectedState,
  "permuted transport order must reduce by canonical seq",
);

const gap = structuredClone(reconnect.events.filter((event) => event.seq !== 2));
assert.throws(() => replayCanonicalEvents(gap), /sequence gap/);

const collision = structuredClone(reconnect.events);
collision[2] = {
  ...collision[2],
  event: { kind: "content.delta", payload: { text: "different", channel: "final" } },
};
assert.throws(() => replayCanonicalEvents(collision), /reused with different content/);

const plan = JSON.parse(
  readFileSync(resolve(goldenDirectory, "plan.json"), "utf8"),
) as GoldenTrace;
const mismatchedPlan = structuredClone(plan.events);
const progressIndex = mismatchedPlan.findIndex((event) => event.event.kind === "plan.progress");
assert.notEqual(progressIndex, -1);
const progress = mismatchedPlan[progressIndex];
if (progress.event.kind !== "plan.progress") throw new Error("expected plan.progress fixture");
mismatchedPlan[progressIndex] = {
  ...progress,
  event: { ...progress.event, payload: { ...progress.event.payload, planId: "other-plan" } },
};
assert.throws(() => replayCanonicalEvents(mismatchedPlan), /plan id mismatch/);

console.log(`canonical protocol replay passed for ${files.length} golden traces`);
