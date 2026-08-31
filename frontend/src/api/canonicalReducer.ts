import type { ClientState, EventEnvelope } from "./generated/cool_protocol.js";

export function initialCanonicalState(): ClientState {
  return {
    runStatus: null,
    content: "",
    reasoning: "",
    tools: {},
    approvals: {},
    activePlanId: null,
    planStatus: null,
    planSteps: {},
    planCompletedSteps: 0,
    planTotalSteps: 0,
    artifacts: [],
    subagents: {},
    workers: {},
    budgetStatus: null,
    researchStatus: null,
    lastSeq: null,
  };
}

export function replayCanonicalEvents(events: EventEnvelope[]): ClientState {
  const state = initialCanonicalState();
  const seenEvents = new Map<string, string>();
  const ordered = [...events].sort((left, right) => left.seq - right.seq);
  const runId = ordered[0]?.runId;

  for (const event of ordered) {
    if (event.runId !== runId) throw new Error("a client state cannot mix run ids");
    const fingerprint = JSON.stringify(event);
    const previous = seenEvents.get(event.eventId);
    if (previous !== undefined) {
      if (previous !== fingerprint) {
        throw new Error(`event id ${event.eventId} was reused with different content`);
      }
      continue;
    }
    const planId =
      event.event.kind === "plan.created" ||
      event.event.kind === "plan.step_started" ||
      event.event.kind === "plan.step_completed" ||
      event.event.kind === "plan.progress"
        ? event.event.payload.planId
        : null;
    if (state.activePlanId !== null && planId !== null && state.activePlanId !== planId) {
      throw new Error(`plan id mismatch: active ${state.activePlanId}, got ${planId}`);
    }
    if (state.lastSeq === null && event.seq !== 1) {
      throw new Error(`sequence must start at 1, got ${event.seq}`);
    }
    if (state.lastSeq !== null && event.seq !== state.lastSeq + 1) {
      throw new Error(`sequence gap or conflict after ${state.lastSeq}: got ${event.seq}`);
    }
    seenEvents.set(event.eventId, fingerprint);
    state.lastSeq = event.seq;
    const canonical = event.event;

    switch (canonical.kind) {
      case "run.started":
        state.runStatus = "running";
        break;
      case "run.completed":
        state.runStatus = "completed";
        break;
      case "run.failed":
        state.runStatus = "failed";
        break;
      case "run.cancelled":
        state.runStatus = "cancelled";
        break;
      case "content.delta":
        state.content += canonical.payload.text;
        break;
      case "reasoning.delta":
        state.reasoning += canonical.payload.text;
        break;
      case "tool.requested":
        state.tools[canonical.payload.callId] = "requested";
        break;
      case "tool.approval_required":
        state.tools[canonical.payload.callId] = "awaiting_approval";
        state.approvals[canonical.payload.approvalId ?? canonical.payload.callId] = "pending";
        break;
      case "tool.approval_resolved":
        state.approvals[canonical.payload.approvalId] = canonical.payload.decision;
        break;
      case "tool.started":
        state.tools[canonical.payload.callId] = "running";
        break;
      case "tool.completed":
        state.tools[canonical.payload.callId] = "completed";
        break;
      case "tool.failed":
        state.tools[canonical.payload.callId] = "failed";
        break;
      case "plan.created":
        state.activePlanId = canonical.payload.planId;
        state.planStatus = "planned";
        state.planSteps = {};
        state.planCompletedSteps = 0;
        state.planTotalSteps = canonical.payload.totalSteps;
        break;
      case "plan.step_started":
        state.activePlanId ??= canonical.payload.planId;
        state.planStatus = "running";
        state.planSteps[String(canonical.payload.position)] = "running";
        break;
      case "plan.step_completed":
        state.activePlanId ??= canonical.payload.planId;
        state.planSteps[String(canonical.payload.position)] = canonical.payload.status;
        break;
      case "plan.progress":
        state.activePlanId ??= canonical.payload.planId;
        state.planCompletedSteps = canonical.payload.completedSteps;
        state.planTotalSteps = canonical.payload.totalSteps;
        state.planStatus =
          canonical.payload.status === "executing" ? "running" : canonical.payload.status;
        break;
      case "artifact.created":
        if (!state.artifacts.includes(canonical.payload.artifactId)) {
          state.artifacts.push(canonical.payload.artifactId);
        }
        break;
      case "budget.warning":
        state.budgetStatus = "warning";
        break;
      case "budget.exceeded":
        state.budgetStatus = "exceeded";
        break;
      case "subagent.started":
      case "subagent.progress":
        state.subagents[canonical.payload.subagentRunId] = "running";
        break;
      case "subagent.completed":
        state.subagents[canonical.payload.subagentRunId] = "completed";
        break;
      case "subagent.failed":
        state.subagents[canonical.payload.subagentRunId] = "failed";
        break;
      case "worker.started":
      case "worker.restarted":
        state.workers[canonical.payload.workerId] = "running";
        break;
      case "worker.failed":
        state.workers[canonical.payload.workerId] = "failed";
        break;
      case "research.started":
      case "research.stage":
        state.researchStatus = "running";
        break;
      case "research.completed":
        state.researchStatus = "completed";
        break;
      case "research.failed":
        state.researchStatus = "failed";
        break;
      case "research.cancelled":
        state.researchStatus = "cancelled";
        break;
      default:
        break;
    }
  }

  return state;
}
