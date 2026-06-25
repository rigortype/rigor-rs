# CLI surface: present the reference's full command surface; unimplemented commands report clearly

Status: accepted

rigor-rs presents the reference's full CLI surface — the same subcommand names, flags, and exit-code semantics (`check`, `annotate`, `type-of`, `trace`, `type-scan`, `explain`, `diff`, `sig-gen`, `baseline`, `triage`, `coverage`, `plugins`, `plugin`, `lsp`, `mcp`, `skill`, `docs`, `init`). Implemented commands behave identically to the reference; commands or flags **not yet implemented in the current phase report a clear "not yet implemented in rigor-rs" message with a distinct exit code**, never a cryptic "unknown command" — the CLI analogue of [ADR-0009](0009-config-baseline-compatibility.md)'s warn-don't-error stance.

The surface is filled in by phase: `check` / `annotate` / `type-of` first, then `baseline` / `diff` / `explain` / `init` / `coverage` / `triage` / `plugins` / `docs`, then `lsp` / `mcp` / `sig-gen` / `skill` / `trace` / `type-scan`. This keeps existing scripts, CI invocations, and editor integrations working across the migration: a script calling `rigor baseline` before that command lands gets an actionable message, not a broken pipeline.

## Considered options

- **Expose only implemented commands (unknown command errors otherwise)** — rejected: scripts invoking a not-yet-ported command get a cryptic failure instead of a clear status.
- **A bespoke CLI with a compatibility subset** — rejected: breaks muscle memory, scripts, and editor integrations built on the reference's CLI.
