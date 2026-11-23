---
name: repo-governance
description: "Use for repository-wide governance audits, directory ownership and naming reviews, technical-debt analysis, and phased cleanup planning. Do not use for ordinary feature edits; per-edit safety is handled by repo guard hooks."
---

# Repo Governance

Keep repository structure and code hygiene from drifting as changes accumulate. This skill is for whole-repository review and cleanup planning, not for silently changing unrelated code.

## Operating modes

Choose one mode before acting:

### Audit

Use when the user asks for a repository review, a "复盘", a health check, a naming/ownership audit, or a cleanup plan.

1. Read the repository's `AGENTS.md` first and preserve its active-work boundaries.
2. Check `git status`, recent commits, and the last governance report if one exists.
3. Run the deterministic scanner:

   ```powershell
   pwsh -NoProfile -ExecutionPolicy Bypass -File .agents/skills/repo-governance/scripts/repo_scan.ps1
   ```

4. Review domain cohesion with [references/audit-checklist.md](references/audit-checklist.md). The scanner cannot decide whether a directory still represents a meaningful boundary.
5. Read only the files needed to verify scanner and cohesion findings. Do not equate a large file with a bad design.
6. Produce a concise report with severity, `file:line` evidence, whether the issue is new or pre-existing, fix cost, risk, and verification.
7. Write the report to `docs/repo-governance-report.md` only when the user asked for a persisted report or the audit workflow calls for one.

Audit is read-only by default. Do not rename, move, delete, or bulk-edit during audit.

### Guard

The project hooks in `.codex/hooks.json` run deterministic checks for `apply_patch` and at turn stop. They cover path protection, new-file naming, formatting, and backend compile checks. They cannot judge semantic ownership or whether a module should be split, and they do not see files written through arbitrary shell commands.

If a hook blocks an edit, fix the stated issue or explain why the operation needs explicit user approval. Do not bypass hook trust permanently.

### Fix

Use only after the user approves the audit findings or explicitly asks for cleanup.

1. Split work into small, reversible phases: naming/path-only moves, then ownership/structure moves, then behavior-preserving extractions, then deletion.
2. Use `git mv` for moves so history is preserved.
3. Update imports, docs, tests, and configuration in the same phase.
4. Do not combine a broad rename with behavior changes.
5. Run the repository verification baseline after each phase and report any pre-existing failures separately.
6. Never auto-commit. Commit only when the user explicitly asks.

## Audit checklist

Read [references/audit-checklist.md](references/audit-checklist.md) for the full review order, severity rubric, report shape, and cleanup rules. Keep the report focused on actionable evidence; do not dump the entire repository.

## Trigger policy

Prefer event-based and staleness-based audits over a calendar task:

- after a major feature or architecture change;
- after dependency or toolchain upgrades;
- after roughly 20 changed files or 500 changed lines since the last audit;
- before a release or a large cleanup;
- when the last report is older than about 30 days.

The per-edit guard is intentionally separate from the audit. A full scan on every edit is too expensive and too noisy.

## Repository boundaries

- The active development line is Telegram. Do not modify `backend/src/api/**`, `backend/src/channels/ilink/**`, or `frontend/**` unless the user explicitly asks for those areas.
- Shared code may be maintained, but verify Telegram behavior; do not add compatibility branches for Web/iLink.
- Keep `README.md` for introduction only. Do not update `docs/architecture.md` unless explicitly requested.
- `docs/archive/` directory names must be English words. Archived documents are point-in-time records and do not need to track current code.
- New environment variables must be added to `backend/.env.example`.
- Do not use the real `smart_assistant.db` for validation. Use `Database::in_memory()` or a temporary `SMART_ASSISTANT_DB`; clean up test messages, memories, sessions, and vectors.
- Use `cargo fmt --all` rather than hand-formatting Rust.

## Token discipline

- Deterministic hooks are cheap and silent on success.
- Audit the whole repository only on an explicit request or a real trigger.
- Read the scanner summary first, then inspect selected files.
- Put long evidence in the report; return only the highest-signal findings and next actions to the conversation.

