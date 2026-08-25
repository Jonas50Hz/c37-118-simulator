## Agent skills

### Issue tracker

GitHub Issues is this repository's issue tracker; use `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the five canonical triage labels. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: use the root `CONTEXT.md` and `docs/adr/`. See `docs/agents/domain.md`.

### Parallel work

Use the `C37.118 Coordinator` custom agent for multi-agent tasks; it dispatches disjoint simulator work through the repository's worker agents.