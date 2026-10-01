//! The [`Typer`] struct: its fields, constructors and accessors. The typing
//! passes are further `impl<'i> Typer<'i>` blocks in sibling modules; the
//! crate root's docs map them.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use rigor_index::CoreIndex;

use crate::{folding, SourceIndex};

/// A process-wide empty [`SourceIndex`], used as the default `source` for a
/// [`Typer`] built via [`Typer::new`] (callers that predate in-source typing).
/// Sharing one empty index keeps `Typer::new` allocation-free and infallible.
fn empty_source() -> &'static SourceIndex {
    static EMPTY: OnceLock<SourceIndex> = OnceLock::new();
    EMPTY.get_or_init(SourceIndex::default)
}

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
///
/// [`type_of`]: crate::type_of
/// [`build_toplevel_env`]: crate::build_toplevel_env
pub struct Typer<'i> {
    pub(crate) index: &'i CoreIndex,
    /// The per-run in-source class index (ADR-0023 tier-4). Empty for a
    /// [`Typer::new`] caller; real for [`Typer::with_source`]. Lets `X.new` type
    /// to an instance of a project-defined class and a typo on it be witnessed.
    pub(crate) source: &'i SourceIndex,
    /// The optional real-Ruby folder (ADR-0008 sidecar). `None` keeps folding to
    /// the conservative Rust core (the sound subset); `Some` lets the dispatcher
    /// route a [`folding::sidecar_foldable`] call the Rust core declined to real
    /// Ruby. Must be `Sync` so one folder is shared across the file-parallel walk.
    pub(crate) folder: Option<&'i (dyn folding::RubyFolder + Sync)>,
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
    pub(crate) file_key: Option<&'i rigor_parse::FileKey>,
    /// rigor-rs#368: the file's provably-dead positions, memoized by `ast`
    /// identity — folding a predicate inside `dead_positions` consults
    /// `local_reach`, which itself calls `dead_positions`, so without the
    /// cache every reach query recomputes (and the mutual recursion never
    /// converges). A `RefCell` is sound here: one `Typer` serves a single
    /// file's analysis on one thread.
    pub(crate) dead_cache: RefCell<Option<(usize, crate::dead::DeadPositions)>>,
    /// `true` while the memoized `dead_positions` walk runs. A re-entrant
    /// call sees it and answers "nothing is dead" — the conservative half of
    /// the result — which is what breaks the `dead_positions` →
    /// `expr_truthiness` → `local_reach` → `dead_positions` cycle.
    pub(crate) dead_in_flight: Cell<bool>,
}

/// A shared empty lexical-scope slice — the default `lexical_scopes` for a
/// [`Typer`] built without the C1 per-file scopes.
const EMPTY_LEXICAL_SCOPES: &[(rigor_parse::Span, Vec<String>)] = &[];

impl<'i> Typer<'i> {
    /// Build a typer over a borrowed core index, with an EMPTY source index
    /// (no in-source typing). Kept for callers that predate tier-4.
    pub fn new(index: &'i CoreIndex) -> Self {
        Typer { index, source: empty_source(), folder: None, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None, dead_cache: RefCell::new(None), dead_in_flight: Cell::new(false) }
    }

    /// Build a typer over a borrowed core index AND a per-run [`SourceIndex`],
    /// enabling `X.new` instance typing and in-source method resolution.
    pub fn with_source(index: &'i CoreIndex, source: &'i SourceIndex) -> Self {
        Typer { index, source, folder: None, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None, dead_cache: RefCell::new(None), dead_in_flight: Cell::new(false) }
    }

    /// As [`Typer::with_source`], plus the ADR-0008 real-Ruby folder for
    /// sidecar-routed constant folds. `None` is byte-identical to
    /// [`Typer::with_source`] (the sound subset).
    pub fn with_source_and_folder(
        index: &'i CoreIndex,
        source: &'i SourceIndex,
        folder: Option<&'i (dyn folding::RubyFolder + Sync)>,
    ) -> Self {
        Typer { index, source, folder, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None, dead_cache: RefCell::new(None), dead_in_flight: Cell::new(false) }
    }

    /// C1: attach the CURRENT FILE's lexical class/module scopes (from
    /// [`source_index::lexical_scopes`]) so the `ConstantRead` arm resolves a
    /// use-site lexical prefix. A consuming builder — the analyze pass computes
    /// the scopes once per file and threads them here.
    ///
    /// [`source_index::lexical_scopes`]: crate::source_index::lexical_scopes
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
