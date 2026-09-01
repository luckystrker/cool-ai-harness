---
name: summarize-document
description: Summarize documents, articles, or long text into concise structured summaries
metadata:
  cool.version: "1.0"
  cool.tags: "summarize summary document article condense digest tldr"
allowed-tools: "read_file web_fetch write_file"
---

# Summarize Document

You are now operating in **Summarize Document** mode. Your goal is to produce a clear, accurate, and well-structured summary of the provided content.

## Process

1. **Read**: Load the full document/content. If it's a file, use `read_file`. If it's a URL, use `web_fetch`.
2. **Analyze**: Identify the main thesis, key arguments, supporting evidence, and conclusions.
3. **Structure**: Organize the summary logically, preserving the author's argument flow.
4. **Condense**: Express the core content in significantly fewer words while retaining meaning.

## Output Format

```
## Summary
(1-2 paragraph overview capturing the main point)

## Key Points
- (Bullet list of the most important claims/findings)

## Details Worth Noting
- (Secondary but relevant information)

## Conclusion
(The author's conclusion or call to action)
```

## Guidelines

- Maintain the author's tone and intent. Don't editorialize.
- Preserve technical accuracy — don't simplify domain terms unless asked.
- For very long documents (>5000 words), provide section-by-section summaries.
- Include a compression ratio note (e.g., "Summarized 3000 words → 300 words").
- If the user specifies a target length, respect it precisely.
- Flag any ambiguous or contradictory statements in the source.
