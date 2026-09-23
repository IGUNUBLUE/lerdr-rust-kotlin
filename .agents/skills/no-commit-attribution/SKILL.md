---
name: no-commit-attribution
description: "Trigger: commits, commit messages, git commit, push, PRs. Do not add agent/tool attribution, credits, or Co-Authored-By trailers to commits or PRs."
metadata:
  version: "1.0"
---

## Activation Contract

Use this skill before creating any git commit, amending a commit message, or opening a pull request.

## Hard Rules

- NEVER add attribution trailers or footers to commit messages or PR bodies. Forbidden patterns include:
  - `Generated with [Devin](...)`, `Generated with Claude`, or any tool/vendor badge line
  - `Co-Authored-By: Devin <...>`, `Co-Authored-By: Claude <...>`, or any agent co-author trailer
  - Emoji or signature blocks crediting the AI/agent
- Write the commit message as if the user authored it: concise subject, "why" over "what", matching the repository's existing commit style.
- If a tool or agent template auto-appends attribution, strip it before committing.
- If existing commits already contain attribution and the user asks to clean them, rewrite the messages (for example `git filter-branch --msg-filter` or an interactive-free rebase) and force-push only with explicit user request.
