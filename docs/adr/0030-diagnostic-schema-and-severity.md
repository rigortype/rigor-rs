# Diagnostic schema, severity resolution, suppression order, rule families (extends ADR-0014)

Status: accepted

rigor-rs carries the reference's full `Diagnostic` field set, severity resolution precedence, suppression pipeline order, and rule-id taxonomy as a parity contract: a diagnostic's `rule` + location is the key that [ADR-0002](0002-diagnostic-set-parity.md) defines parity over, so the schema and taxonomy are load-bearing and held stable. Source authority: [diagnostic-policy.md](../../../../ruby/rigor/docs/type-specification/diagnostic-policy.md), [internal-spec/diagnostic-shape.md](../../../../ruby/rigor/docs/internal-spec/diagnostic-shape.md), [manual/04-diagnostics.md](../../../../ruby/rigor/docs/manual/04-diagnostics.md), [manual/06-baseline.md](../../../../ruby/rigor/docs/manual/06-baseline.md), [manual/11-ci.md](../../../../ruby/rigor/docs/manual/11-ci.md), reference ADRs 8, 34, 35, 47, 51, 61, 64, 65, 66.

## Structured `Diagnostic` fields

Every diagnostic carries:

| Field | Type | Notes |
|---|---|---|
| `path` | `String` | Analysed file path. |
| `line` | `u32` | 1-based. Prism columns are 0-based; the constructor adds 1. |
| `column` | `u32` | 1-based. |
| `message` | `String` | Human-readable. **Presentation, not contract** — may be reworded in a minor release; consumers MUST NOT parse it. |
| `severity` | `Severity` | Authored severity before profile re-stamping (`:error` / `:warning` / `:info`). |
| `rule` | `Option<String>` | Stable `family.rule-name` id. `None` for parse / internal errors — a `None`-rule diagnostic is **unsuppressible**. |
| `source_family` | `SourceFamily` | `:builtin` (default), `"plugin.<id>"`, `:rbs_extended`, `"generated.<provider>"`. |
| `receiver_type` | `Option<String>` | Rendered receiver type for call/def rules with a dispatch subject; `None` otherwise. |
| `method_name` | `Option<String>` | Called / defined method name for call/def rules; `None` otherwise. |
| `project_definition_site` | `Option<String>` | `"path:line"` set by `call.undefined-method` when the project defines the called method elsewhere — the monkey-patch / `pre_eval:` triage signal ([ADR-17](../../../../ruby/rigor/docs/adr/17-monkey-patch-pre-evaluation.md)). |

Two further fields are enriched onto the JSON output stream but are **not** carried on the `Diagnostic` object itself — they are per-rule properties of the rule catalogue ([reference ADR-65](../../../../ruby/rigor/docs/adr/65-diagnostic-evidence-tier-and-doc-url.md)):

| Field | Type | Notes |
|---|---|---|
| `evidence_tier` | `Option<"high"\|"medium"\|"low">` | Omitted for informational / plugin rules. **Orthogonal to severity; never a severity gate.** |
| `documentation_url` | `String` | Stable per-rule URL. Enriched for `:builtin`, non-`None`-rule diagnostics only. |

`qualified_rule` derivation: `None` when `rule` is `None`; the bare `rule` when `source_family` is `:builtin`; `"<source_family>.<rule>"` otherwise. This is the key suppression, baseline, and JSON output uses ([ADR-0014](0014-diagnostic-output-formats.md)).

## Severity resolution precedence

`Configuration::SeverityProfile::resolve` applies in this order (highest first):

1. `rule` is `None` → keep authored severity (nothing to look up).
2. Exact `severity_overrides:` entry for the rule id.
3. Family-wildcard `severity_overrides:` entry (the rule id's first dotted segment).
4. Active `severity_profile:` table entry for the rule id.
5. Authored severity.

`:off` drops the diagnostic entirely. An unknown `severity_profile:` value falls back to `balanced`.

## Suppression pipeline order

Applied in this fixed order:

1. Inline `# rigor:disable` / `# rigor:disable-file` comment markers.
2. File-level disable markers.
3. Project `disable:` config key.
4. Severity profile (`:off` → drop).
5. **Baseline last** — never resurrects a diagnostic another layer has already suppressed ([ADR-0009](0009-config-baseline-compatibility.md) / reference ADR-22).

## Token expansion

A rule token in a `# rigor:disable` marker or `disable:` list is **expanded to a canonical-id set at parse time** (`resolve_rule_token`). Four shapes:

- `all` — kept as the sentinel; suppresses every rule in scope.
- Legacy unprefixed alias (`undefined-method`) → single canonical id (`call.undefined-method`).
- Family wildcard (`call` / `flow` / `assert` / `dump` / `def`) → every canonical id under `<family>.`.
- Exact canonical id (`call.undefined-method`) → kept as-is.

The per-line / per-file match is then **exact set membership** of the diagnostic's canonical `rule` against the expanded set — never prefix matching. A diagnostic whose `rule` is `None` is never suppressed by a token.

## Rule-id taxonomy (parity contract)

The full identifier taxonomy from [diagnostic-policy.md](../../../../ruby/rigor/docs/type-specification/diagnostic-policy.md) is the parity contract. Non-obvious per-rule canonical severities:

| Rule | `lenient` | `balanced` | `strict` | Notes |
|---|---|---|---|---|
| `call.self-undefined-method` | `:off` | `:off` | `:off` | Ships `:off`; NOT promotable until a subclass-aware FP gate exists (reference ADR-24 WD4). |
| `call.unresolved-toplevel` | suppressed | `:warning` | `:error` | Toplevel implicit-self miss; escape hatch is `pre_eval:` (reference [ADR-34](../../../../ruby/rigor/docs/adr/34-toplevel-unresolved-self-call-default.md)). |
| `flow.unreachable-clause` | `:info` | `:info` | `:warning` | `case`/`when` + bare-class `in` clause (reference [ADR-47](../../../../ruby/rigor/docs/adr/47-narrowing-driven-clause-reachability.md)). |
| `def.override-visibility-reduced` / `def.override-return-widened` / `def.override-param-narrowed` | `:off` | `:warning` | `:error` | Only when both the override and the shadowed ancestor carry an author-supplied signature (reference [ADR-35](../../../../ruby/rigor/docs/adr/35-override-signature-compatibility.md)). |
| `call.argument-type-mismatch` | `:warning` | `:error` | `:error` | Generalised to any concrete class with a fixed `COERCE_DISPATCH_METHODS` exclusion (reference [ADR-64](../../../../ruby/rigor/docs/adr/64-non-nil-argument-type-mismatch.md)). |
| `def.return-type-mismatch` | `:off` | `:warning` | `:error` | |
| `call.unresolved-toplevel` discriminated-union narrowing | additive-only | additive-only | additive-only | Member narrowing rules are additive (reference [ADR-66](../../../../ruby/rigor/docs/adr/66-discriminated-union-member-typing.md)). |

New rule families to register in this port: `call.unresolved-toplevel`, `def.override-*`, `flow.unreachable-clause` (on `case`/`when` + bare-class `in`), `call.argument-type-mismatch` generalized, discriminated-union member narrowing.

## CI output formats and auto-detection

Six CI formats (extending [ADR-0014](0014-diagnostic-output-formats.md), source: reference [ADR-51](../../../../ruby/rigor/docs/adr/51-ci-diagnostic-output-formats.md)):

`sarif` / `github` / `gitlab` / `checkstyle` / `junit` / `teamcity`

**CI auto-detection** fires on the default `text` output only (never on an explicit `--format`): GitHub Actions and TeamCity emit platform-native annotations **on top of** the human text (stdout-native); GitLab CI emits a one-line hint to use `--format gitlab` (artifact-based); other CIs emit a hint toward reviewdog / `--format junit`. Suppressed with `--no-ci-detect` or `RIGOR_CI_DETECT=0`.

Severity → format mappings (contract):

| Rigor | SARIF | GitHub | GitLab | Checkstyle | JUnit | TeamCity |
|---|---|---|---|---|---|---|
| error | `error` | `::error` | `major` | `error` | `error` | `ERROR` |
| warning | `warning` | `::warning` | `minor` | `warning` | `warning` | `WARNING` |
| info | `note` | `::notice` | `info` | `info` | `info` | `INFO` |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | No error-severity diagnostics. |
| `1` | Diagnostics found, or per-command failure. |
| `64` | Usage error. |

`rigor triage` always exits `0` and excludes `:info` diagnostics from its volume views by default (pass `--include-info` to override). The `selectors` axis aggregates `receiver_type` / `method_name` from the structured fields — never by parsing `message` text (reference [ADR-61](../../../../ruby/rigor/docs/adr/61-agent-friendly-diagnostic-statistics.md)).

## Considered options

- **Parse `message` for triage / selector aggregation** — rejected: `message` is presentation, not contract (ADR-50 declares it rewordable); any parser built on it breaks on a minor-release rewording. Structured fields are the contract surface.
- **Single severity column, no evidence_tier** — rejected: `evidence_tier` is orthogonal to severity and enables consumers to route attention (`high`-tier firings to a strict gate; `low` to a human review queue) without re-deriving confidence. It never gates a diagnostic.
- **Prefix-match token expansion** — rejected: prefix matching causes a rule added under an existing prefix to be silently suppressed by an existing marker. Exact-set-membership after parse-time expansion is deterministic and prefix-safe.
