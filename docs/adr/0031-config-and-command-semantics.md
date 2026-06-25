# Config semantics and command surface (extends ADR-0009/ADR-0015)

Status: accepted

rigor-rs reads the reference's `.rigor.yml` format unchanged ([ADR-0009](0009-config-baseline-compatibility.md)) and presents the reference's full CLI surface ([ADR-0015](0015-cli-surface-compatibility.md)). This ADR pins the decisions that are not obvious from those cross-references: config-file discovery semantics, `includes:` layering, path resolution, always-excluded directories, config validation warnings, `dependencies.budget_overrun_strategy`, `bleeding_edge:` grammar, `pre_eval:` contract, and the fuller command semantics for `coverage --protection`, `doctor`, and `sig-gen`. Source authority: [manual/03-configuration.md](../../../../ruby/rigor/docs/manual/03-configuration.md), [manual/02-cli-reference.md](../../../../ruby/rigor/docs/manual/02-cli-reference.md), [manual/12-caching.md](../../../../ruby/rigor/docs/manual/12-caching.md), reference ADRs 17, 40, 50, 63, 70, 72, 77, handbook/11-sig-gen.

## Config discovery and layering

**Winner-takes-all discovery.** `.rigor.yml` beats `.rigor.dist.yml`; the two are **never merged**. `--config=PATH` bypasses discovery. Unimplemented keys are warned-and-ignored, never errored ([ADR-0009](0009-config-baseline-compatibility.md)).

**`includes:` layering.** Layers configs recursively beneath the current one (the including file's keys take precedence). Relative paths in an `includes:` entry resolve against the **including file's own directory**, not the working directory.

**Always-excluded directories.** `vendor/bundle`, `.bundle`, and `node_modules` are excluded regardless of `exclude:` config — never reported as missing or misconfigured.

**Relative path resolution.** All relative paths in a config file (including `signature_paths:`, `pre_eval:`, `baseline:`, `cache.path`, `bundler.lockfile`) resolve against that config file's own directory.

## Config validation warnings

`rigor check` emits named `kind` warnings to stderr (and populates `config_warnings[]` in `--format json`) on:

- A `signature_paths:` entry whose directory does not exist.
- A `signature_paths:` entry that exists but matches 0 `.rbs` files.
- A `libraries:` entry that names an unavailable RBS library.
- A `disable:` token that is not a recognized rule id.
- A `severity_overrides:` key that is not a recognized rule id.
- A `bundler.lockfile:` path that does not exist.

**Exemptions:** tokens under a plugin family (`rspec.…`, `rbs_extended.…`, `plugin.<id>.…`) are left alone — plugin rule ids cannot be enumerated statically and may resolve at run time. Auto-detected defaults (auto-discovered `<root>/sig`, auto-detected bundle) are never warned about. These are warnings, not errors; partial / optional bundles are valid setups (reference [manual/03-configuration.md](../../../../ruby/rigor/docs/manual/03-configuration.md) § "Config validation warnings").

## Dependency budget

`dependencies.budget_per_gem` counts **method definitions** (not time), default 5 000, range 1 250–20 000. The `dependencies.budget_overrun_strategy` key controls what happens when a gem exceeds the cap:

- `walker_cap` (default): methods past the cap fall through to the engine's normal user-class resolution.
- `dependency_silence`: any call on a class from a budget-exceeded gem resolves to `Dynamic[top]`, silencing `call.undefined-method` on that gem's unrecorded surface at the cost of weaker static checking there.

## `bleeding_edge:` grammar

Four legal values:

| Value | Meaning |
|---|---|
| `false` (default) | Adopt none of the overlay. |
| `true` | Adopt the whole overlay. |
| `[id, …]` | Adopt only the named feature ids. |
| `{all: true, except: [id, …]}` | Adopt all but the named. |

The overlay may be empty in a given release; implement the mechanism regardless. Override per-run with `rigor check --bleeding-edge[=ids]` / `--no-bleeding-edge`. Inspect with `rigor show-bleedingedge` (reference [ADR-50](../../../../ruby/rigor/docs/adr/50-release-engineering-and-stability-strategy.md) WD2).

## `pre_eval:` contract

`pre_eval:` triggers a parse + scope-scan pass with **no type inference** that builds a project-wide monkey-patch method registry (`ProjectPatchedMethods`) consumed by dispatch. It is the canonical escape hatch for `call.unresolved-toplevel` (reference [ADR-17](../../../../ruby/rigor/docs/adr/17-monkey-patch-pre-evaluation.md) / [ADR-34](../../../../ruby/rigor/docs/adr/34-toplevel-unresolved-self-call-default.md)). A malformed `pre_eval:` file does not prevent analysis from continuing — the dispatcher's miss simply fires for the methods that file would have registered (fail-soft, per ADR-17 WD3).

## `rigor coverage --protection`

Two tiers (reference [ADR-63](../../../../ruby/rigor/docs/adr/63-type-protection-coverage.md) / [ADR-70](../../../../ruby/rigor/docs/adr/70-fused-protection-coverage.md)):

- **Tier 1 (dispatch-site receiver-concreteness ratio)**: `--protection` alone reports "if I introduce a bug, would Rigor catch it" — the ratio of dispatch sites whose receiver resolves to a concrete class. `--threshold RATIO` exits `1` below the ratio.
- **Tier 2 (mutation kill-rate)**: `--protection --mutation` introduces type-visible breakages at each dispatch site, re-analyses against a clean baseline, and reports the kill rate (caught breakages). Defaults to git-changed `.rb` files.

`--test-command CMD` is split to argv and executed **without a shell** (shell constructs are not interpreted; no inline env-var prefix). Rigor's own `BUNDLE_*` env is stripped before the suite runs. The suite must pass on clean code first, or the run aborts.

## `rigor doctor`

Five checks (reference [ADR-77](../../../../ruby/rigor/docs/adr/77-doctor-and-upgrade-commands.md)):

1. **Config audit** — unresolved `signature_paths:`, unknown `libraries:`, inert `disable:` / `severity_overrides:` tokens.
2. **RBS environment health** — whether the RBS class universe built successfully (`0` classes means a broken setup).
3. **Plugin load errors** — whether every configured plugin loaded.
4. **Baseline drift** — whether current diagnostics have drifted from the saved baseline.
5. **Rails plugin gap** — whether `Gemfile.lock` contains Rails gems but no Rails plugin is enabled.

JSON output is a **stable contract** from day one:

```json
{
  "status": "issues_found",
  "checks": [
    { "id": "config_audit", "status": "fail", "message": "...", "hint": "..." }
  ]
}
```

`status` values: `"ok"` / `"issues_found"`. Per-check `status`: `"pass"` / `"fail"` / `"warn"`. Exit `1` when any check fails, `0` when all pass. Message wording is presentation; the `id` + `status` + `hint` fields are contract.

## `rigor sig-gen` tighter-return policy

A `sig-gen` pass that would produce a **tighter return** by dropping declared union members (e.g. `T | nil` → `T`) is a **contradiction signal**, not a precision win — the narrower type contradicts the existing authored declaration. Such a proposed change MUST be flagged for `--diff` review and MUST NOT silently delete the `nil` / `false` arm. `--overwrite` is required for tighter-return updates to replace user-authored RBS, and even then the change is surfaced explicitly (reference handbook/11-sig-gen).

## `rigor baseline` and `rigor triage`

`rigor triage` always exits `0`. `:info` diagnostics are excluded from the volume views (distribution, selectors, hotspots) by default; pass `--include-info` to override. The `selectors` axis aggregates `receiver_type` / `method_name` structured fields — never by parsing `message` text ([ADR-0030](0030-diagnostic-schema-and-severity.md) / reference [ADR-61](../../../../ruby/rigor/docs/adr/61-agent-friendly-diagnostic-statistics.md)).

Baseline activation is explicit (`baseline:` config key or `--baseline=PATH`); presence of the file alone does nothing. The baseline filter runs last in the suppression pipeline and never resurrects a diagnostic another layer has already suppressed ([ADR-0030](0030-diagnostic-schema-and-severity.md)).

## Gemfile.lock-gated RBS overlays

`dependencies.source_inference` and the RBS overlay mechanism (reference [ADR-72](../../../../ruby/rigor/docs/adr/72-gemfile-lock-gated-rbs-overlays.md)) gate per-gem RBS overlays on the gem being present in `Gemfile.lock`. Implement the gating mechanism; the overlay set is demand-gated.

## Considered options

- **Merge `.rigor.yml` and `.rigor.dist.yml`** — rejected: the reference's winner-takes-all semantics are the contract a drop-in replacement must reproduce; merging would change suppression, severity, and path behaviour for projects that rely on the override convention.
- **Error on unknown config keys** — rejected per [ADR-0009](0009-config-baseline-compatibility.md): unimplemented-feature keys are warn-and-ignored so a project's existing config keeps working across migration phases.
- **Shell-execute `--test-command`** — rejected: shell execution makes the command sensitive to the invoking shell and allows environment variable injection. argv-split + direct exec (with Rigor's own `BUNDLE_*` stripped) is deterministic and isolation-safe.
- **Implicit baseline activation** — rejected: the reference requires explicit activation; implicit activation would suppress diagnostics on projects that happen to have a baseline file present without intending to use it.
