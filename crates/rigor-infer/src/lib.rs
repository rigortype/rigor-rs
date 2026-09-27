//! The inference engine (ADR-0004/0005): flow-sensitive inference, narrowing,
//! RBS method-type translation, typed dispatch. Pure query functions take the
//! db explicitly (ADR-0006 — Salsa-ready, not Salsa-bound). Constant folding
//! splits between a conservative Rust core and the cached Ruby sidecar
//! (ADR-0008); foldability is decided here from an embedded catalogue.
//!
//! ## Module map
//!
//! [`Typer`] is one struct whose `impl` is split by pass: every module below
//! except `flow_writes` (free functions) adds its own `impl<'i> Typer<'i>`
//! block, so a change to one pass stays in one file. A method called from
//! another module is at least `pub(crate)`; `lib.rs` itself holds only
//! declarations, re-exports, [`TypeEnv`] and the free shims.
//!
//! | module | holds |
//! |---|---|
//! | `typer` | the struct, its constructors and accessors |
//! | `expr_type` | [`Typer::type_of`], the dispatch by node variant, projection folds |
//! | `call_dispatch` | block-less call typing (`type_call`, the RBS return lookup) |
//! | `block_call` | calls carrying a literal block (exactly-once, never-completes) |
//! | `reach` | argument reach — the untyped-argument declines |
//! | `flow_writes` | span-keyed rebind/mutation tables (free functions) |
//! | `flow_eval` | top-level env builders, [`Typer::always_truthy_snapshots`] |
//! | `nilable` | [`Typer::nilable_receiver_snapshots`] |
//! | `class_narrowing` | [`Typer::class_narrowing_pass`] |
//! | `collection_shape` | [`Typer::collection_shape_snapshots`] |
//!
//! Unit tests live in the `*tests.rs` modules beside them (`use super::*`).
//!
//! ## Tracer-bullet expression typer
//!
//! This slice ships the smallest [`type_of`] able to type the *receiver* of a
//! call: string/integer literals fold to value-pinned `Constant` carriers, a
//! local read is resolved from a flat [`TypeEnv`] populated as statements are
//! walked in order, and everything else degrades to `Dynamic[top]` (ADR-0023
//! tier-5 fallback). The pure-function-dispatched-by-node-variant shape mirrors
//! the reference's `ExpressionTyper` (ADR-0023).
//!
// TODO(spec): flow sensitivity, narrowing, the full dispatch tier cascade
// (folding -> shape -> RBS -> in-source -> Dynamic) and budgets (ADR-0023/0024).
#![allow(dead_code)]

pub mod folding;
pub mod kernel_fold;
pub mod multi_target_binder;
pub mod source_index;

mod block_call;
mod call_dispatch;
mod class_narrowing;
mod collection_shape;
mod expr_type;
mod flow_eval;
mod flow_writes;
mod nilable;
mod reach;
mod typer;

use std::collections::HashMap;

use rigor_index::CoreIndex;
use rigor_parse::{LoweredAst, NodeId};
use rigor_types::{Interner, TypeId};

pub use class_narrowing::ClassNarrowing;
pub use flow_writes::collect_flow_writes;
pub use folding::RubyFolder;
pub use source_index::{
    lexical_scopes, method_body_spans, ConstLit, DefKind, Harvest, ParamBoundReturn, SourceIndex,
    SOURCE_CLASS_BASE,
};
pub use typer::Typer;

// Crate-internal names the sibling modules and `source_index.rs` reach as
// `crate::NAME` (e.g. `crate::MUTATOR_METHODS`, `crate::shape_key_to_scalar`),
// and the test modules through `use super::*`.
pub(crate) use expr_type::*;
pub(crate) use flow_writes::*;

/// A flat name -> type binding environment, populated by `LocalVariableWrite`
/// as the statement sequence is walked in order. Intentionally not
/// flow-sensitive in this slice.
pub type TypeEnv = HashMap<String, TypeId>;

/// Type an owned-AST node against the current `env`. Free-function wrapper kept
/// source-compatible for callers (e.g. rigor-rules) that predate [`Typer`]; it
/// runs over an *empty* index, so a `Call` receiver types via folding only and
/// otherwise degrades to `Dynamic[top]`. Migrate to [`Typer::type_of`] (with the
/// real index) to get chained-call result typing.
///
/// - `StringLit` -> `Constant["..."]`
/// - `IntegerLit` -> `Constant[n]`
/// - `LocalVariableRead` -> the env binding, else `Dynamic[top]`
/// - anything else -> `Dynamic[top]` (`Interner::untyped`)
pub fn type_of(ast: &LoweredAst, id: NodeId, env: &TypeEnv, interner: &mut Interner) -> TypeId {
    let empty = CoreIndex::new();
    Typer::new(&empty).type_of(ast, id, env, interner)
}

/// Walk the top-level statement sequence binding each local write. Free-function
/// wrapper over an empty-index [`Typer`], kept source-compatible (see
/// [`type_of`]).
// TODO(spec): real flow-sensitive scoping + narrowing across branches (ADR-0022).
pub fn build_toplevel_env(ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
    let empty = CoreIndex::new();
    Typer::new(&empty).build_toplevel_env(ast, interner)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod m2_go_slice_tests;

#[cfg(test)]
mod meta_new_lift_tests;

#[cfg(test)]
mod rbs_tuple_return_tests;

// ---------------------------------------------------------------------------
// `is_a?` / `case-when` class narrowing (census mechanism 1) — the oracle
// probe matrix a1–a6 + the load-bearing declines
// ---------------------------------------------------------------------------

#[cfg(test)]
mod class_narrowing_tests;

// ---------------------------------------------------------------------------
// Collection-shape receiver survival (stage 1) — the oracle probe matrix
// m01-m20 of docs/notes/20260807-collection-shape-slice-spec.md. The SILENT
// rows are the FP-safety envelope, not coverage bookkeeping: each one is a
// shape the reference itself declines on (m04/m05/m08/m13/m15/m18/m20) or a
// deliberate coverage give-up (m16 op-writes, `while`, ivars, safe-nav).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod collection_shape_tests;

/// Collection-shape **stage 2** — the chain ROOTS. Each micro-slice gets a
/// fire + decline pair, mirroring the oracle probes recorded in
/// `docs/notes/20260807-collection-shape-slice-spec.md` §1.
#[cfg(test)]
mod collection_shape_stage2_tests;
