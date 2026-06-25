# Lower Prism into an owned, indexed AST mirroring its shape, with synthetic-node variants

Status: accepted

`rigor-parse` lowers the borrowed `ruby-prism` AST ([ADR-0003](0003-prism-rust-bindings.md)) once into an **owned, `NodeId`-indexed** rigor-rs AST whose node shape mirrors Prism's, extended with **synthetic-node variants** for definitions that have no source (plugin/macro-generated methods — the reference's `lib/rigor/ast.rb` synthetic nodes and its ADR-16 macro expansion). The inference engine and rules walk this owned AST, never the borrowed Prism tree.

Two forces drive this:

- **Lifetime ergonomics.** `ruby-prism`'s AST borrows the parse buffer (a lifetime on every node). The inference engine recirculates type information across many passes; threading a borrowed-AST lifetime through all of it reproduces the pervasive-`'a` pain that [ADR-0005](0005-rust-architecture.md) and [ADR-0006](0006-incremental-computation.md) deliberately avoid. Owned, `NodeId`-keyed nodes free inference from that lifetime, consistent with ADR-0005's interned-handle stance.
- **Synthetic nodes.** Plugins and macro expansion generate method/definition nodes with no source text. Mirroring the reference, these must coexist with real nodes during inference; in an owned AST they are simply additional node variants.

Mirroring Prism's node *shape* (rather than normalizing to a semantically different HIR) keeps node-level behaviour aligned with the reference — which also overlays synthetic nodes on Prism — easing [diagnostic-set parity](0002-diagnostic-set-parity.md). The lowering pass is mechanical because the shapes correspond 1:1.

## Considered options

- **Walk the borrowed Prism AST directly + a synthetic overlay** (selene/pzoom-style on their external parser) — rejected: zero lowering cost, but the parse-buffer lifetime propagates through the whole inference engine, the `'a` pain ADR-0005/0006 avoid.
- **Lower into a semantically normalized custom HIR** — rejected: maximal freedom to insert/normalize, but drifts from the reference's node-level behaviour (harder parity) and the lowering is heavy.
