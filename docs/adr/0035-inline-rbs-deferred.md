# Defer inline RBS — the reference delegates to the rbs-inline gem, no Ruby-free path yet

Status: accepted

[ADR-0007](0007-rbs-stdlib-shipping.md) names the analysis-time type environment
as embedded stdlib RBS ⊕ project `sig/` ⊕ gem RBS ⊕ **inline RBS**.
[ADR-0033](0033-project-sig-ingestion.md) landed project `sig/`;
[ADR-0034](0034-rbs-collection-ingestion.md) landed the gem-RBS `rbs_collection`
half. This ADR decides the fourth leg — **inline RBS** (rbs-inline-shaped comments
like `#: () -> T`, `# @rbs name: T`, `# @rbs return: T`, attribute `#:`) — and the
decision is to **defer** it, recording why so the deferral is a decision and not
an omission.

## The decision — defer, staged

Inline RBS is **not implemented** now. Three independent reasons, and a staged
plan for when demand arrives.

### Why defer

1. **It is opt-in in the reference, not the default environment.** Inline RBS
   ships as the `rigor-rbs-inline` *plugin* (reference ADR-32,
   `plugins/rigor-rbs-inline/`), activated only via `plugins:` or
   `--treat-all-as-inline-rbs` — the reference `Environment` notes the plugin "is
   responsible for its own" rbs-inline parse, and skips the cost otherwise. So a
   default run ingests no inline RBS. **Deferring is parity-safe**: the corpus
   differential never enables the plugin, and a project that *does* enable it hits
   only a *coverage gap* (a missed diagnostic), never a false positive — coverage
   gaps are expected, not gate failures ([ADR-0002](0002-diagnostic-set-parity.md)).

2. **No Ruby-free parse path exists.** The reference plugin (185 lines) does not
   implement the rbs-inline grammar — it delegates to the **`rbs-inline` gem's
   `RBS::Inline::Parser`**. rigor-rs has no equivalent: the vendored `ruby-rbs`
   crate parses `.rbs` *files*, not the rbs-inline *comment* sub-language layered
   over Ruby source (`#:` method types, `# @rbs` declarations, ivars, generics,
   override, attributes). A faithful Ruby-free port means **reimplementing that
   gem's parser in Rust** — a large, standalone effort against an upstream grammar
   that still evolves. Shelling to the gem would break the single-binary,
   Ruby-free contract ([ADR-0001](0001-rust-reimplementation-strategy.md)).

3. **rigor-rs has no mechanism for a source-parsing plugin.** Its plugin model
   (ADR-25 / [ADR-0027](0027-plugin-contract.md)) contributes **bundled RBS
   payloads** only. Inline RBS is a per-file, source-derived contribution — a
   contribution surface rigor-rs does not yet have.

The effort (a new grammar parser + a new contribution mechanism) is
disproportionate to an **opt-in** feature's value while the higher-leverage
default-environment legs are the priority — consistent with the reference itself
isolating inline RBS in its own plugin and ADR.

### Staged plan (when demand or a parser arrives)

- **WD1** — a source-derived RBS contribution surface in rigor-rs (the mechanism
  gap), plugin-gated so the default path stays inert (parity).
- **WD2** — the minimal, highest-value slice: the trailing `#: (params) -> Return`
  method-signature comment immediately preceding a `def`. Its payload is an RBS
  *method type*, which the native `ruby-rbs` parser can likely parse as a fragment
  — attach it to that method. This alone covers the common case without the full
  grammar.
- **WD3** — the `# @rbs` declaration long tail (name / return / ivars / generics /
  override / attribute `#:`). Largest surface, lowest marginal value; demand-gated.
- **Gate** — a plugin-enabled harness fixture (the ADR-0033/0034 fixture-env
  pattern extended to activate `rigor-rbs-inline`), differential vs the reference
  with the plugin active.

## Scope note — the gem-RBS bundler leg

The other outstanding ADR-0007 item, the **bundler-installed-gem `sig/`** leg
(loading a gem's own bundled RBS from the bundler install root), remains deferred
per [ADR-0034](0034-rbs-collection-ingestion.md) — it needs bundler/environment-
specific gem-path discovery (Ruby-free tension) and, like inline RBS, deferring it
is parity-safe (coverage-gap only). With this ADR, every ADR-0007 leg is now
either implemented (embedded stdlib, project `sig/`, `rbs_collection`) or a
recorded, rationalized deferral (bundler-gem-`sig/`, inline RBS).

## Considered options

- **Shell to the `rbs-inline` gem** — rejected: breaks the Ruby-free / single-
  binary contract (ADR-0001), the whole reason the port exists.
- **Reimplement the full rbs-inline grammar in Rust now** — rejected: a large
  standalone parser project for an opt-in feature, disproportionate while default-
  environment coverage is the priority.
- **Ship the minimal `#:` slice now** — considered and deferred (WD2): still needs
  the WD1 contribution mechanism and a plugin gate; queued behind demand rather
  than built speculatively.

## Revisiting

Re-open when a project demand for `rigor-rbs-inline` parity is demonstrated, or
when a maintained Rust rbs-inline parser becomes available (removing reason 2).
Start at WD1 + WD2.
