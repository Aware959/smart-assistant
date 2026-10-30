# Repository Governance Audit Checklist

Use this checklist after the deterministic scanner has produced its summary. Read only the sections that apply to the repository and the user's request.

## 1. Scope and safety

- Read `AGENTS.md`, `git status`, recent commits, and the current governance report.
- Identify user-owned dirty files and never overwrite or revert them.
- Identify the active development line and protected paths before proposing changes.
- Confirm whether the audit is read-only or approved for cleanup.

## 2. Structure and ownership

- Map top-level directories to responsibilities and entry points.
- Check whether files live near the code that owns their behavior.
- Check module boundaries: transport, application, capability, storage, configuration.
- For each application-core module or directory, verify its reason to exist: the domain it owns, its primary type or entry point, invariants or state that belong only there, and its real consumers. A directory that cannot answer these is a weak boundary, not automatically a cleanup target.
- Build the module dependency view with targeted searches such as `rg -n '^use crate::' <module>`. Flag cycles between domain modules, thin wrappers that only forward to another module, and fields or functions that no production path uses.
- Compare domain names with current behavior. Flag conceptual drift such as a module named for a removed decision process when it now only schedules and dispatches a related behavior.
- Flag files that mix unrelated responsibilities, not merely files that are long.
- Look for duplicate implementations, abandoned modules, and re-export-only layers.
- Check whether shared code changes have a clear Telegram-first validation path.

## 3. Naming and paths

- Rust source files use lowercase snake_case; binary targets may use hyphenated names where Cargo requires them.
- New unit tests belong in the source file's `#[cfg(test)] mod tests`, not a new sibling `_tests.rs` file.
- `docs/archive/` directory names use English words; archived documents do not need to follow current implementation.
- Flag spaces, mixed case, temporary suffixes, and misleading names in newly added source paths.
- Do not rename existing paths without checking imports, build targets, docs, and Git history.

## 4. Technical debt

- Review `TODO`, `FIXME`, `HACK`, panic sites, silent error swallowing, and dead code.
- Distinguish intentional invariants from accidental panic paths.
- Check oversized files for multiple concerns, hidden state, and difficult test seams.
- Check dependency usage, duplicate crates, unused features, and stale toolchain assumptions.
- Check generated or local artifacts that should be ignored rather than tracked.

## 5. Tests and verification

- Check that changed behavior has a test at the closest practical layer.
- Unit tests follow the source file; integration tests stay under the top-level `tests/`.
- Identify tests that only assert implementation details or generated wording.
- Run the repository baseline when authorized: `cargo test`, `cargo check --all-targets`, and `cargo fmt --all -- --check`.
- If the real database is involved, use a temporary database and clean up messages, memories, sessions, and vector rows.

## 6. Documentation and configuration

- `README.md`: introduction, components, quick start only.
- `docs/architecture.md`: update only when explicitly requested.
- `docs/api-doc.md`: update only when explicitly requested.
- `docs/archive/`: point-in-time records; do not update them to match current code.
- `backend/.env.example`: every new environment variable must be represented.

## 7. Severity and report

Use `P0` for active data loss, security exposure, or user-visible breakage; `P1` for a capability or trust problem without immediate breakage; `P2` for known debt with no current symptom.

For each finding include:

- severity and short title;
- evidence with `file:line`;
- new or pre-existing;
- impact and affected boundary;
- recommended phase, cost, risk, and verification.

Classify weak domain cohesion as `P2` unless it causes a real boundary violation, an import cycle that blocks a feature, or a public contract that misleads callers.

End with:

- the three highest-value actions;
- what should not be changed yet;
- the next audit trigger.

## 8. Cleanup rules

- Separate audit from mutation.
- Get explicit approval before moving, renaming, deleting, or bulk-editing.
- Use `git mv` for path moves.
- Keep each phase reviewable and behavior-preserving where possible.
- Do not mix formatting-only churn with logic changes.
- Do not auto-commit or rewrite history.

