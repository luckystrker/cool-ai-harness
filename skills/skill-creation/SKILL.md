---
name: skill-creation
description: Create new skills based on user requests - generates properly formatted SKILL.md files with metadata and instructions
metadata:
  cool.version: "1.0"
  cool.tags: "skill create generate new author build"
allowed-tools: "create_skill list_skills read_file write_file"
---

# Skill Creation

You are now operating in **Skill Creation** mode. Your goal is to help the user create a new, well-structured skill that can be reused across conversations.

## Process

1. **Understand**: Clarify what the user wants the skill to do. Ask about:
   - What task or workflow should the skill guide?
   - What tools does it need access to?
   - Should it be global (shared) or user-specific?
   - Any specific output format or constraints?

2. **Design**: Structure the skill with:
   - A clear, descriptive name (lowercase-hyphenated)
   - A concise description for discovery/matching
   - Relevant tags for automatic relevance detection
   - A list of recommended tools
   - Well-organized instruction body

3. **Create**: Use the `create_skill` tool with the designed parameters.

4. **Verify**: Confirm the skill was created and show the user what was generated.

## Skill Body Guidelines

The instruction body should:
- Start with a `# Title` heading
- Clearly state the operating mode
- Define a step-by-step **Process** section
- Include an **Output Format** section (if applicable)
- List **Guidelines** or principles to follow
- Be specific and actionable (avoid vague instructions)
- Reference tool names when the skill expects tool usage

## Naming Conventions

- Use lowercase with hyphens: `code-review`, `api-design`, `test-generation`
- Keep names short (2-3 words max)
- Make names descriptive of the skill's purpose
- Avoid generic names like `helper` or `tool`

## Example Tags

Choose 3-6 tags that help match the skill to relevant user queries:
- Action tags: `research`, `analyze`, `generate`, `review`, `create`
- Domain tags: `code`, `documentation`, `testing`, `security`, `design`
- Format tags: `report`, `summary`, `plan`, `checklist`

## Scope Decision

- **global**: Skills useful across all projects (e.g., `deep-research`, `translate`)
- **user**: Personal/project-specific skills (e.g., `deploy-my-app`, `company-style-guide`)

Default to `user` scope unless the user explicitly wants a shared skill.
