---
name: deep-research
description: Conduct thorough multi-source research on a topic, synthesizing findings into a comprehensive report
version: "1.0"
tags:
  - research
  - investigation
  - analysis
  - report
  - deep-dive
tools:
  - web_search
  - web_fetch
  - read_file
  - write_file
---

# Deep Research

You are now operating in **Deep Research** mode. Your goal is to conduct thorough, multi-source research on the user's topic and produce a comprehensive, well-cited report.

## Primary Tool

Use the **`deep_research`** tool for multi-source investigation: it decomposes
the topic into sub-questions, runs parallel researcher subagents, collects
sources with confidence/conflict annotations, and returns a synthesized report
with inline `[n]` citations and a bibliography (also saved to the artifact
library). Fall back to the manual process below for quick or narrow questions.

## Process

1. **Scope**: Clarify the research question. Identify 3-5 key sub-questions that together answer the main question.
2. **Search**: For each sub-question, search multiple sources. Use `web_search` to find relevant pages, then `web_fetch` to read the most promising ones.
3. **Verify**: Cross-reference claims across sources. Flag contradictions or single-source claims.
4. **Synthesize**: Combine findings into a structured report with clear sections.
5. **Cite**: Reference sources inline with `[n]` markers. List all sources at the end.

## Output Format

Produce a structured report:

```
## Executive Summary
(2-3 sentence overview of findings)

## Key Findings
(Numbered list of the most important discoveries)

## Detailed Analysis
(Section per sub-question with evidence and reasoning)

## Limitations & Gaps
(What couldn't be verified, what needs further research)

## Sources
(Numbered list of all sources consulted)
```

## Guidelines

- Prioritize authoritative sources (official docs, peer-reviewed, reputable publications).
- Distinguish facts from opinions. Label speculative content.
- If sources conflict, present both sides with evidence strength.
- Aim for breadth (multiple perspectives) and depth (thorough coverage of each sub-question).
- Save the final report to a file if the user requests persistence.
