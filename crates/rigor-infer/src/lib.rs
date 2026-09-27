//! The inference engine (ADR-0004/0005): flow-sensitive inference, narrowing,
//! RBS method-type translation, typed dispatch. Pure query functions take the
//! db explicitly (ADR-0006 — Salsa-ready, not Salsa-bound). Constant folding
//! splits between a conservative Rust core and the cached Ruby sidecar
//! (ADR-0008); foldability is decided here from an embedded catalogue.
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
mod flow_writes;
mod class_narrowing;
mod collection_shape;
mod nilable;
mod block_call;
mod reach;
mod call_dispatch;
mod expr_type;
mod flow_eval;

use std::collections::HashMap;
use std::sync::OnceLock;

use rigor_index::CoreIndex;
use rigor_parse::{LoweredAst, NodeId};
use rigor_types::{Interner, TypeId};

pub use folding::RubyFolder;
pub use source_index::{
    lexical_scopes, method_body_spans, ConstLit, DefKind, Harvest, ParamBoundReturn, SourceIndex,
    SOURCE_CLASS_BASE,
};
pub use flow_writes::collect_flow_writes;
pub(crate) use flow_writes::*;
pub use class_narrowing::ClassNarrowing;
pub(crate) use expr_type::*;

/// A process-wide empty [`SourceIndex`], used as the default `source` for a
/// [`Typer`] built via [`Typer::new`] (callers that predate in-source typing).
/// Sharing one empty index keeps `Typer::new` allocation-free and infallible.
fn empty_source() -> &'static SourceIndex {
    static EMPTY: OnceLock<SourceIndex> = OnceLock::new();
    EMPTY.get_or_init(SourceIndex::default)
}

/// A flat name -> type binding environment, populated by `LocalVariableWrite`
/// as the statement sequence is walked in order. Intentionally not
/// flow-sensitive in this slice.
pub type TypeEnv = HashMap<String, TypeId>;

/// The expression typer (ADR-0023: the reference's `ExpressionTyper` /
/// `MethodDispatcher` split). Holds a borrow of the [`CoreIndex`] so it can
/// resolve a receiver's class and a method's return type — the data a CHAINED
/// call needs to type correctly (`s.downcase : String`, so the next `.lenght`
/// can be flagged).
///
/// The index is a *field*, not a per-call parameter, so the existing free
/// [`type_of`] / [`build_toplevel_env`] signatures stay source-compatible: they
/// are thin wrappers over a [`Typer`] built with an empty index. Callers that
/// want chained-call result typing construct a [`Typer`] with the real index.
pub struct Typer<'i> {
    index: &'i CoreIndex,
    /// The per-run in-source class index (ADR-0023 tier-4). Empty for a
    /// [`Typer::new`] caller; real for [`Typer::with_source`]. Lets `X.new` type
    /// to an instance of a project-defined class and a typo on it be witnessed.
    source: &'i SourceIndex,
    /// The optional real-Ruby folder (ADR-0008 sidecar). `None` keeps folding to
    /// the conservative Rust core (the sound subset); `Some` lets the dispatcher
    /// route a [`folding::sidecar_foldable`] call the Rust core declined to real
    /// Ruby. Must be `Sync` so one folder is shared across the file-parallel walk.
    folder: Option<&'i (dyn folding::RubyFolder + Sync)>,
    /// C1 (constant-shadow gate): the CURRENT FILE's lexical class/module scopes,
    /// `(span, qualified segments)`, so the `ConstantRead` arm can recover a
    /// use-site lexical prefix by span containment and consult
    /// [`SourceIndex::constant_shadowed`] precisely. Empty (`&[]`) for callers
    /// that do not set it (unit tests / pre-C1 entry points) — with no scopes
    /// every use site reads as toplevel, so only TOPLEVEL project definitions
    /// suppress, matching the conservative default.
    lexical_scopes: &'i [(rigor_parse::Span, Vec<String>)],
    /// The analyzed file's [`rigor_parse::FileKey`] — the per-file
    /// def-attribution overlay index (`SourceIndex::project_declares_method`
    /// / `is_toplevel_def` consult `file_defs` through it). `None` for callers
    /// that do not set it ⇒ the union-over-all-files answer.
    file_key: Option<&'i rigor_parse::FileKey>,
}

/// A shared empty lexical-scope slice — the default `lexical_scopes` for a
/// [`Typer`] built without the C1 per-file scopes.
const EMPTY_LEXICAL_SCOPES: &[(rigor_parse::Span, Vec<String>)] = &[];

impl<'i> Typer<'i> {
    /// Build a typer over a borrowed core index, with an EMPTY source index
    /// (no in-source typing). Kept for callers that predate tier-4.
    pub fn new(index: &'i CoreIndex) -> Self {
        Typer { index, source: empty_source(), folder: None, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None }
    }

    /// Build a typer over a borrowed core index AND a per-run [`SourceIndex`],
    /// enabling `X.new` instance typing and in-source method resolution.
    pub fn with_source(index: &'i CoreIndex, source: &'i SourceIndex) -> Self {
        Typer { index, source, folder: None, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None }
    }

    /// As [`Typer::with_source`], plus the ADR-0008 real-Ruby folder for
    /// sidecar-routed constant folds. `None` is byte-identical to
    /// [`Typer::with_source`] (the sound subset).
    pub fn with_source_and_folder(
        index: &'i CoreIndex,
        source: &'i SourceIndex,
        folder: Option<&'i (dyn folding::RubyFolder + Sync)>,
    ) -> Self {
        Typer { index, source, folder, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None }
    }

    /// C1: attach the CURRENT FILE's lexical class/module scopes (from
    /// [`source_index::lexical_scopes`]) so the `ConstantRead` arm resolves a
    /// use-site lexical prefix. A consuming builder — the analyze pass computes
    /// the scopes once per file and threads them here.
    pub fn with_lexical_scopes(
        mut self,
        scopes: &'i [(rigor_parse::Span, Vec<String>)],
    ) -> Self {
        self.lexical_scopes = scopes;
        self
    }

    /// Attach the analyzed file's [`rigor_parse::FileKey`] so the
    /// source-index def queries resolve its per-file overlay.
    pub fn with_file_key(mut self, key: &'i rigor_parse::FileKey) -> Self {
        self.file_key = Some(key);
        self
    }

    /// The analyzed file's [`rigor_parse::FileKey`], `None` when unset.
    pub fn file_key(&self) -> Option<&rigor_parse::FileKey> {
        self.file_key
    }

    /// C1: the use-site lexical prefix (enclosing class/module qualified segments)
    /// for a node at `span` — the INNERMOST enclosing scope by span containment,
    /// or an empty slice at toplevel / when no scopes are attached.
    pub fn enclosing_prefix(&self, span: rigor_parse::Span) -> &[String] {
        let mut best: Option<&(rigor_parse::Span, Vec<String>)> = None;
        for sc in self.lexical_scopes {
            if sc.0 .0 <= span.0 && span.1 <= sc.0 .1 {
                // Contained: keep the innermost (narrowest span).
                match best {
                    None => best = Some(sc),
                    Some(b) if (sc.0 .1 - sc.0 .0) < (b.0 .1 - b.0 .0) => best = Some(sc),
                    _ => {}
                }
            }
        }
        best.map(|b| b.1.as_slice()).unwrap_or(&[])
    }

    /// The borrowed source index (for the rules layer's method-resolution gate).
    pub fn source(&self) -> &SourceIndex {
        self.source
    }

    /// The borrowed core index.
    pub fn core(&self) -> &CoreIndex {
        self.index
    }
}

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
