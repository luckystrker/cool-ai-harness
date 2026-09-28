---
name: long-running-task
description: Drive a long-running task through workspace progress files (.cool/task/progress.md + features.json) that survive context compaction and new runs
metadata:
  cool.version: "2.0"
  cool.tags: "long-running task progress checkpoint resume compaction durable"
allowed-tools: "read_file write_file edit_file list_files shell git"
---

# Long-Running Task

You are now operating in **Long-Running Task** mode. A task too big for one
context window is tracked on disk so progress survives compaction, crashes
and fresh runs.

## Progress files

Maintain two files in the workspace:

- `.cool/task/progress.md` — human-readable task journal with three
  sections:
  - **Done** — completed work, one bullet per verified step.
  - **Next** — the immediate next actions, ordered.
  - **Acceptance criteria** — how the finished task is verified.
- `.cool/task/features.json` — machine-readable checklist:
  `{"features": [{"id": "f1", "title": "...", "status": "pending|in_progress|done", "notes": "..."}]}`.

Keep both files consistent with each other.

## Process

1. **Start**: if `.cool/task/progress.md` exists, read it and
   `features.json`, then resume from **Next**. Otherwise create both files:
   break the request into a feature checklist and record acceptance
   criteria.
2. **Work**: do the next pending item only. Prefer `edit_file` for targeted
   changes so progress-file updates stay small.
3. **Update before answering**: before every reply to the user, update
   `progress.md` (Done/Next) and `features.json` statuses. The file is the
   source of truth for the next run — a new session must be able to resume
   from it alone.
4. **Finish**: when every feature is `done` and the acceptance criteria are
   met, say so explicitly and leave the final state in the files.

## Principles

- **File over memory**: never keep task state only in the conversation —
  compaction may drop it. Write progress down as it happens.
- **Small verifiable steps**: each feature should be independently
  checkable (a test, a command, a file to inspect).
- **State the state**: every reply briefly notes what is done and what is
  next, matching the files.
