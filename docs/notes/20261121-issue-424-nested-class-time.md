# Nested `class`/`module` time (rigor-rs#424, final rows)

The issue's "4,999 nested `class`/`module`: 60 s and about 1 GB RSS, not
profiled" row, profiled and fixed. All numbers release builds on this
machine, `rigor check` on `class C0 … class C3998; end × n` (the
`nestclass` shape now in `harness/time_nesting.py`).

| nested classes | master `2104267e` | this PR |
|---|---|---|
| 1,000 | 2.0 s | 0.78 s |
| 2,000 | 12 s | 1.0 s |
| 4,000 | 100 s | 3.7 s |
| 4,999 | (not run; projected > 2 min) | 5.9 s |

Diagnostics byte-identical before and after on every measured size (the
output's own `Wall time`/`Memory peak` footer aside). The reference cannot
analyse the shape at any of these sizes: its recursive walk dies at
`SystemStackError` and reports `internal analyzer error: this file is
nested too deeply to analyse`, exit 1 — so at ≥ 1,000 nesting the port now
completes strictly more of the input than the oracle it mirrors. Past
5,000 nesting the port's own lowering refuses the file (`nesting too deep`,
exit 1) — the existing deterministic depth bound, not a wall-clock guard.

## The four hot spots (each a per-owner or per-name rescan)

1. **`walk_fold_defs`/`qualified_bodies` written chains** — the WRITTEN-name
   chain was a `Vec<String>` cloned whole (`to_vec`) at every `class`/
   `module` level: O(D²) `String` clones and ~1.5 GB of transient
   allocation at 4,000. Now a persistent `WrittenChain` (cons list,
   O(1) push/clone, tail-match by walking innermost-first). The qualified
   prefix also no longer re-splits per level (`qualify_str` joins the
   already-joined parent name).
2. **`decline_superseded_singletons`** — three full `ast.iter()` scans per
   owner (nested-scope list, eval census, main loop) plus an O(scopes)
   innermost-scope search per node: cubic on a nest, since every level is
   an owner. Now one per-file `SupersedeIndex`: a merged containment sweep
   (`containment_links`) gives each node its innermost scope and eval
   block once, `class << self` interiors fan out to every enclosing owner,
   and `singleton_events` walks only the nodes attributed to the owner,
   in arena order.
3. **`synthesize_namespace_prefixes` (merge)** — replayed every discovered
   name by re-joining every `::` prefix. In a nest each discovered name is
   itself D segments long, so the replay copies and hashes O(D³) bytes
   (measured 53 GB-equivalent of `memcpy`+SipHash at 4,000 — 75 of the
   remaining 78 s in the first profile). The merge now names each
   DISTINCT prefix once in a (parent, segment) trie and materializes each
   prefix string exactly once.
4. **`superseded_defs` (method returns)** — one full-node scan per
   class/module/eval-block region with an O(scopes) `nested.any` per node.
   Same treatment: the shared containment sweep, per-region attribution
   for class bodies, and an innermost-other-scope test for eval-block
   regions (a scope between the statement and the block files under that
   scope's owner, exactly the old `nested` list's meaning).

## The measured residual

Per-name passes over the qualified names themselves: a D-deep nest's
innermost name is D segments, so any per-class table keyed by name holds
O(D²) bytes — `source_classes`, `override_classes`, `discovered_decls`,
`discovered_classes`, ancestry — and the per-name work over them is
quadratic bytes (2,000 → 4,000 measures ×3.7, ≈ the 4× of quadratic;
4,999 RSS ≈ 2.3 GB, master's own 1 GB row scaled the same way). That is
the reference's own data shape — its `qualified_prefix` is the same
per-level design; it just never gets past the `SystemStackError` to show
it. Filing the per-name passes as the residual rather than redesigning
the name tables matches the brief's algorithmic-fix-first bar: the row
went 100 s → 3.7 s at 4,000 and the shape is refused outright past 5,000.

`harness/time_nesting.py --all` now times the row (`nestclass`, 1,000 and
2,000) alongside the landed rows.
