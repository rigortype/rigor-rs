# Cache descriptor and incremental dependency graph (extends ADR-0006/0017)

Status: accepted

The rigor-rs cache descriptor has six typed slots whose composition and invalidation rules are ported faithfully from the reference. The incremental dependency graph records per-file cross-file reads and negative lookups, re-analyzes only the changed closure on each run, and is kept sound by a mandatory `--verify-incremental` byte-identity gate in CI. Worker thread ordering matches the reference's fork-pool contract: pre-pass discovery tables are built single-threaded and frozen before any rayon worker starts.

## Context

[ADR-0017](0017-analysis-cache.md) committed to a content-addressed persistent cache and a `fetch_or_validate` record-and-validate path. [ADR-0006](0006-incremental-computation.md) deferred Salsa and chose file-level caching with pure query functions. The reference finalized its cache descriptor schema at `SCHEMA_VERSION 4` (adding the `globs` slot for plugin-producer record-and-validate) and its incremental graph at reference ADR-46, both now stable. This ADR records the rigor-rs decisions that concretize those two designs.

## Decisions

### Cache descriptor: six typed slots

The `CacheDescriptor` value type has exactly six slots ([internal-spec/cache](../../../../ruby/rigor/docs/internal-spec/cache.md); [design/cache-slice-taxonomy](../../../../ruby/rigor/docs/design/20260505-cache-slice-taxonomy.md)):

| Slot | Entry shape | Notes |
| --- | --- | --- |
| `files` | `{ path, comparator: Digest\|Mtime\|Exists, value }` | Stricter comparator wins on conflict (Digest > Mtime > Exists); values must agree under the winner or `Conflict` is raised |
| `gems` | `{ name, requirement, locked? }` | Grouped by name; `(requirement, locked)` must agree |
| `plugins` | `{ id, version, config_hash? }` | Grouped by id; `(version, config_hash)` must agree |
| `configs` | `{ key, value_hash }` | Grouped by key; `value_hash` must agree |
| `dependencies` | `{ gem_name, gem_version, mode: Disabled\|WhenMissing\|Full }` | Per-gem dependency-source inference mode (reference ADR-10); grouped by `gem_name` |
| `globs` | `{ root, pattern, value }` | Directory-glob coverage for plugin producer `watch:` (reference ADR-60 WD3); grouped by `(root, pattern)` |

`CacheDescriptor::compose` merges any number of descriptors union-by-key-per-slot. Non-files conflicts (gems, plugins, configs, dependencies, globs) with mismatched values raise `Conflict` — never silently shadow. A `Conflict` is treated as a cache miss, not an error that aborts the run.

### Two store entry points

**`fetch_or_compute`** — all inputs known up-front. The cache key is a SHA-256 over `(schema_version, producer_id, params, descriptor)`. The block runs only on miss; its return value is stored with no dependency recording.

**`fetch_or_validate`** — record-and-validate path for producers that discover their inputs during the run (e.g. a plugin that reads project files mid-analysis, or a glob-based producer). The block returns `(value, dependency_descriptor)`. On the next run the stored dependency descriptor is re-validated by re-reading every `FileEntry` / `GlobEntry`; a stale entry forces a recompute. A descriptor carrying non-file slots (`gems`, `plugins`, `configs`, `dependencies`) is never considered fresh — those belong in the cache *key*, not the validated set.

The key for `fetch_or_validate` entries covers only stable identity inputs (`key_descriptor` + `params`); the dependency descriptor is stored alongside the value and re-validated separately.

### Schema-version marker

`<root>/schema_version.txt` holds `"<SCHEMA_VERSION>.<FORMAT_VERSION>"`. On first access:

- Missing → write and proceed.
- Matches → proceed.
- Disagrees → wipe all entries under `<root>`, rewrite the marker, proceed as empty.

A version bump therefore drops every entry on the next writable run with no migration step. The format-version half ensures old entries (which would read as misses anyway) are also reclaimed from disk ([internal-spec/cache](../../../../ruby/rigor/docs/internal-spec/cache.md)).

### Multi-contributor descriptor composition

Multiple producers that contribute to the same cached value compose their descriptors via `CacheDescriptor::compose`. The union-by-key rules above apply. A single contributor that adds a duplicate equal entry to its own descriptor is harmless; a duplicate with a conflicting value is a producer-side bug surfaced as `Conflict`.

### Incremental cross-file dependency graph

On each analyzed file A, a lightweight **dependency recorder** captures every cross-file symbol resolved through the scope accessor choke points (`user_def_for`, `superclass_of`, `includes_of`, `top_level_def_for`, and the discovered-class lookups). The recorder notes the **source file** of each resolved entry as a positive edge and the queried name as a **negative edge** for unresolved lookups ([internal-spec/cache](../../../../ruby/rigor/docs/internal-spec/cache.md); reference ADR-46).

The persisted graph is inverted into `dependents[X] = { A : X ∈ deps[A] }`. On a run, the changed-file set ΔF is diffed from the last snapshot. Re-analysis covers exactly `ΔF ∪ dependents[ΔF]`; every other file is served from its per-file cache entry.

**Structural edits** (class/method/constant name added, removed, renamed, or moved — detected via a declaration-shape fingerprint that hashes names but not bodies) widen the closure: negative edges are consulted to find files that queried a name that now exists (or no longer exists). A file that looked up a symbol and found nothing must be re-checked if that symbol is now defined. The root-keyed snapshot fingerprint keys on the analysis *roots*, not the file list — adding or removing a file under a root does not drop the whole snapshot.

### `--verify-incremental` soundness gate

`rigor check --verify-incremental` runs a full re-analysis (bypassing incremental cache) and asserts byte-for-byte diagnostic identity against the incremental result. This gate is **mandatory in CI** to catch soundness regressions. A mismatch is a fatal error; the full-run result is authoritative.

### Parallel worker ordering contract

rigor-rs uses rayon as its worker pool (the reference uses forked persistent workers; both satisfy the same contract).

**Pre-pass discovery tables** — the defined-method index, class-hierarchy seed, and all tables holding Prism `DefNode` references — MUST be built single-threaded and fully frozen before any rayon worker starts. Workers that attempt cross-file symbol resolution before the pre-pass completes emit spurious `call.undefined-method` diagnostics because the index is incomplete ([internal-spec/worker-session](../../../../ruby/rigor/docs/internal-spec/worker-session.md)).

**Per-worker state** — the diagnostics reporter, plugin accumulators, and per-run mutable state are owned per worker and merged post-pool. Severity-profile re-stamping is a post-pool aggregate concern; individual workers do not re-stamp.

**Worker count precedence** (descending): `--workers` CLI flag > `RIGOR_RACTOR_WORKERS` env var > `.rigor.yml` `parallel.workers:` > `0` (sequential default). This matches the reference's three-tier opt-in ([design/ractor-migration](../../../../ruby/rigor/docs/design/20260514-ractor-migration.md) Phase 4c).

**Equivalence invariant**: the diagnostic multiset across workers MUST equal the sequential path (same set, possibly different order before location-sort). This is the property that lets the runner shard files across workers without changing what `rigor check` reports.

## Relationship to other ADRs

- Extends [ADR-0006](0006-incremental-computation.md): concretizes the file-level cache with the six-slot descriptor, the two entry points, and the dependency graph.
- Extends [ADR-0017](0017-analysis-cache.md): adds the incremental graph, the multi-contributor composition rules, and the parallel ordering contract.
- Uses [ADR-0013](0013-plugin-architecture.md) / [ADR-0027](0027-plugin-contract.md): plugin producers use `fetch_or_validate`; `IoBoundary`-equivalent file reads feed the `globs` and `files` slots of the dependency descriptor.

## Considered options

- **Single `fetch_or_compute` entry point, no record-and-validate** — rejected: unsound for producers that discover their inputs mid-run (the reference's `pundit_plugin_spec` cross-process regression proved this; reference ADR-45). A pre-analysis fingerprint misses files the analysis itself discovers.
- **Always full re-analysis, no incremental graph** — rejected: the reference measured ~9.6× warm no-change speedup and ~6.3× single-leaf-edit speedup on 262 files (reference ADR-46); the incremental graph is load-bearing for local development workflows.
- **Salsa for the incremental layer** — deferred per [ADR-0006](0006-incremental-computation.md): adopted only if profiling shows cross-file invalidation dominates editor latency; the pure query functions and explicit dependency recording keep the option open.
- **Drop the `--verify-incremental` gate** — rejected: the gate is the only machine-checkable guarantee of soundness; omitting it means incremental regressions are found by users, not CI.
