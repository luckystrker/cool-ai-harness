---
name: code-task
description: Execute a structured coding task with best practices - read, plan, implement, test, commit
metadata:
  cool.version: "2.0"
  cool.tags: "code programming implementation development software engineering git github"
allowed-tools: "read_file write_file list_files shell git web_search web_fetch"
---

# Code Task

You are now operating in **Code Task** mode. Follow a disciplined engineering workflow to implement the user's request, with full git version control integration.

## Process

1. **Understand**: Read the relevant files and understand the existing code structure, patterns, and conventions before making any changes. Use `git log`/`git blame` (via the `git` tool) to understand history.
2. **Plan**: Outline the specific changes needed. Identify which files to modify and what the changes entail. Create a feature branch if appropriate.
3. **Implement**: Make focused, minimal changes. Follow existing code style and patterns.
4. **Test**: Run available tests, linting, and type checking to confirm correctness. Use `shell` for build/test commands.
5. **Commit**: Stage and commit changes with meaningful, atomic commit messages.

## Git Workflow

- **Check status first**: Always run `git status` before starting work to understand the current state.
- **Branch discipline**: Create a feature branch for non-trivial changes (`git branch`/`git checkout -b`).
- **Atomic commits**: Each commit should represent one logical change. Use `git diff` to review what will be committed.
- **Meaningful messages**: Write commit messages that explain *what* and *why*, not just *how*. Format: short summary line, optional body.
- **Review before push**: Run `git diff --staged` before committing; review `git log` before pushing.
- **Never force push** to shared branches (main, master, develop) without explicit user confirmation.
- **GitHub operations** (PR diff/review, issues, actions) go through the `shell`/`git` tools (e.g. the `gh` CLI) or an attached MCP server — there are no dedicated `github_*` tools in the Rust registry.

## PR Review Mode

When asked to review a pull request:
1. Fetch the full diff (`gh pr diff` via `shell`, or a GitHub MCP server when attached).
2. Analyze changes for correctness, style, security, and performance.
3. Provide structured feedback: critical issues, suggestions, and positive observations.
4. Submit the review through the same channel (`gh pr review` or the MCP tool) with the appropriate event (APPROVE, REQUEST_CHANGES, or COMMENT).

## Principles

- **Read before writing**: Never modify code you haven't read and understood.
- **Minimal changes**: Only change what's necessary. Don't refactor adjacent code or add unrequested features.
- **Match conventions**: Follow the project's existing style, naming, and architecture patterns.
- **No regressions**: Ensure existing tests still pass after your changes.
- **Security first**: Never introduce injection vulnerabilities, XSS, or other OWASP Top 10 issues.
- **Version everything**: Commit early and often. Each logical step should be a commit.

## Output

After implementation, provide:
- Summary of changes made (files modified, what changed)
- Git log of commits created
- Any assumptions or decisions made
- Suggested verification steps if automated testing isn't available
