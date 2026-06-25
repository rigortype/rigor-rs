# The finalized plugin contract (extends ADR-0013)

Status: accepted

The rigor-rs plugin trait mirrors the reference's frozen narrow interface exactly, so sidecar-hosted Ruby plugins and native Rust ports share one surface. Every hook is declaratively gated — compiled once per run into a frozen table — so the engine never calls plugin code for nodes, receivers, or paths it cannot match at the consultation site.

## Context

[ADR-0013](0013-plugin-architecture.md) established the two-kind model (sidecar-hosted Ruby / native Rust) and the strangler-fig migration order. It left the *shape* of the plugin trait open, noting only that plugins return facts, synthetic RBS, and diagnostics. That shape is now fixed by the reference's own contract freeze ([internal-spec/plugin](../../../../ruby/rigor/docs/internal-spec/plugin.md); [design/plugin-mechanism-pre-1.0-review](../../../../ruby/rigor/docs/design/20260601-plugin-mechanism-pre-1.0-review.md); reference ADR-37, ADR-52, ADR-9, ADR-38, ADR-26, ADR-28, ADR-25). This ADR records the decisions that translate that freeze into the rigor-rs trait.

## Decisions

### Hook surface

The plugin trait exposes exactly the narrow, engine-gated hooks the reference ships; no additional hooks are added.

**`node_rule(node_type) { |node, scope, path, node_file_context, NodeContext| }`** — the engine owns the AST walk and dispatches each node to every rule whose declared `node_type` matches (`is_a?`). The block receives five arguments: the matched Prism node, the file scope, the file path, the per-file context value from `node_file_context` (see below), and a `NodeContext` carrying the node's lexical ancestor chain (`enclosing_def`, `enclosing_module`, `enclosing_block`, and the full `ancestors` slice). A plugin that declares no rules pays zero cost.

`node_file_context { |root, scope| }` supports two-pass (collect-then-validate) plugins: it runs once per file before any node rule fires, and its return value is threaded to every rule as the fourth block argument. A same-file collect pass belongs here; a cross-file collect belongs in `#prepare` + `services.fact_store`.

**`dynamic_return(receivers:, methods:, file_methods:) { |call_node, scope| Type? }`** — per-call-site return type. At least one of `receivers:`, `methods:`, or `file_methods:` is REQUIRED; a rule gated on none would fire on every dispatch and is rejected at load. `receivers:` accepts a non-empty array of class names or a proc resolved once per run after `#prepare` (ADR-52 slice 3). `methods:` accepts an array of method-name symbols/strings or a run-time callable (ADR-52 slice 4). `file_methods:` is a callable receiving the path, memoised per `(rule, path)` — the per-file name-set specialisation for rspec-style let-bindings (ADR-52 slice 5a); it is mutually exclusive with `methods:`. First non-nil block result wins. Binary operators are ordinary calls: a `receivers: ["Money"]` rule can branch on `call_node.name ∈ {:+, :-, …}`.

**`type_specifier(methods:) { |call_node, scope| facts? }`** — post-return narrowing facts, gated on `call_node.name` being in the declared `methods:` set.

**`flow_contribution_for` is DELETED.** A plugin that still defines this hook raises `ArgumentError` at load ([internal-spec/plugin](../../../../ruby/rigor/docs/internal-spec/plugin.md); reference ADR-52 WD3). All five reference production consumers migrated to `dynamic_return` / `type_specifier` before deletion.

**`diagnostics_for_file(path:, scope:, root:)`** survives as the whole-file `FileRule` escape valve (dispatched after node rules), non-preferred. It is reserved for genuinely file-scoped diagnostics — a single load-error row, or a check that requires the whole parsed file at once. The engine re-stamps every returned diagnostic with `source_family: "plugin.<manifest.id>"`.

### FactStore and pre-pass ordering

Plugins declare `produces:` and `consumes:` in their manifest. The loader runs a Kahn topological sort over the `consumes` graph; a producer's `#prepare` is guaranteed to run before any consumer's `#prepare`. Cycles and missing producers are `LoadError`s before any analysis begins. Facts published in `#prepare` are visible in `dynamic_return` / `type_specifier` blocks and node rules via `services.fact_store` ([internal-spec/plugin](../../../../ruby/rigor/docs/internal-spec/plugin.md); reference ADR-9).

### Manifest extension fields

The manifest fields below are ported faithfully. Omitting a field leaves the corresponding engine gate inert.

| Field | Engine site |
| --- | --- |
| `additional_initializers:` [{`receiver_constraint:`, `methods:`}] | Extends `ScopeIndexer`'s ivar-seeding gate from `initialize` to declared lifecycle methods (`setup`, `after_initialize`, DI setters), suppressing false-nil widening (reference ADR-38) |
| `open_receivers:` | Classes exempt from `call.undefined-method` (unbounded method surface, e.g. `ActiveRecord::Relation`) (reference ADR-26) |
| `protocol_contracts:` | Path-glob → param-type injection + return-type verification at two engine sites (reference ADR-28) |
| `signature_paths:` | Plugin-contributed RBS directories, resolved relative to the plugin gem root and merged into the environment (reference ADR-25) |

### Compiled contribution dispatch

Every hook must be gated by a key available at the consultation site — method-name symbol, receiver class name, file path, or Prism node class — compiled once per run into a frozen table (reference ADR-52 WD1). A plugin capability that cannot state such a key is a DSL vocabulary gap to fix, not a license for an ungated walk. The compiled table is `Send`-safe and is passed to worker threads without re-derivation.

### Plugin cache producers

Plugins use `IoBoundary`-equivalent auto-capture: every file a plugin reads inside its producer block is tracked for cache invalidation via the `fetch_or_validate` record-and-validate path (ADR-0017; reference ADR-60 WD3). `watch:` glob declarations cover directory additions and removals. `TrustPolicy` fields (`trusted_gems`, `allowed_read_roots`, `network_policy: :disabled` default) are enforced on sidecar-hosted Ruby plugins; a Rust-native plugin's I/O is audited at the call site ([internal-spec/plugin-cache-producers](../../../../ruby/rigor/docs/internal-spec/plugin-cache-producers.md); [internal-spec/plugin-trust](../../../../ruby/rigor/docs/internal-spec/plugin-trust.md)).

For sidecar-hosted Ruby plugins specifically, the sidecar ([ADR-0008](0008-real-ruby-sidecar.md)) must report the set of files the plugin read during its producer block — the IoBoundary-equivalent — back to rigor-rs so those files are recorded in the dependency descriptor for cache invalidation.

### Machine-readable capability catalogue

`rigor plugins --capabilities` enumerates per-plugin: `node_rule_types`, `dynamic_return_receivers`, `type_specifier_methods`, `produces`, `consumes`. These are exactly the declarative gates, greppable without loading plugin code ([internal-spec/plugin](../../../../ruby/rigor/docs/internal-spec/plugin.md)).

## Relationship to other ADRs

- Extends [ADR-0013](0013-plugin-architecture.md): fixes the trait shape the two-kind model requires.
- Uses [ADR-0008](0008-real-ruby-sidecar.md): sidecar I/O reporting for Ruby plugin cache tracking.
- Uses [ADR-0017](0017-analysis-cache.md): `fetch_or_validate` is the plugin producer entry point.

## Considered options

- **Design a Rust-idiomatic trait diverging from the reference** — rejected: the governing principle is parity and drop-in replacement; third-party Ruby plugins must run unmodified in the sidecar; a divergent trait would require translation layers and risk silent diagnostic drift.
- **Expose `flow_contribution_for` as a compatibility shim** — rejected: the reference deleted it at its own pre-1.0 freeze (ADR-52 WD3); shipping it in rigor-rs would guarantee third-party code targets the deleted hook rather than the narrow DSL forms.
- **Skip `node_file_context` (two-pass support)** — rejected: without it a same-file collect-then-validate plugin (e.g. statesman-style state machine rules) must either re-walk the file or fall back to `diagnostics_for_file`, eliminating the boilerplate win node_rule exists to provide.
