//! The analysis depth bound (rigor-rs#412, ADR-0045).
//!
//! Prism caps most nesting at 10,000 depth units, but it parses left-nested
//! chains (`x.a.a…`, `1 + 1 + …`, `a && a && …`, `1 if a if a…`, a `rescue`
//! clause list) iteratively, with no cap. Every recursive walker after the
//! parse — lowering, `Visit` scans, inference, even Prism's own
//! `pm_node_destroy` — would then recurse once per link. [`parse_bounded`]
//! measures the tree's nesting before any of them runs and refuses a tree
//! deeper than [`MAX_NESTING_DEPTH`]. Every analysis thread's stack is sized
//! for a tree at that bound (`ANALYSIS_STACK_BYTES` in rigor-cli).

use std::ffi::c_void;
use std::mem::ManuallyDrop;

use ruby_prism::ParseResult;

/// The deepest tree rigor analyses, counted in Prism child edges (every
/// node field, as `pm_visit_child_nodes` walks them; the program node is
/// depth 1).
///
/// Measured (ADR-0045): every construct Prism caps stays at or below 19,999
/// at Prism's own cap (`((…))` and `[*[*…]]`), and the deepest of 19,085
/// real files is 61. An uncapped chain costs one level per link, or two for
/// an `if`/`unless`/`while` modifier.
pub const MAX_NESTING_DEPTH: usize = 25_000;

/// A tree deeper than [`MAX_NESTING_DEPTH`]: the file is not analysed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooDeep {
    /// The tree's full depth, measured on the thread that released it;
    /// `None` when that thread could not be created (the tree was leaked).
    pub depth: Option<usize>,
    /// The bound it exceeded.
    pub limit: usize,
}

impl TooDeep {
    /// The message the skipped file reports.
    #[must_use]
    pub fn message(&self) -> String {
        match self.depth {
            Some(depth) => format!(
                "source nesting depth {depth} exceeds the analysis limit of {} (rigor-rs#412); \
                 file not analysed",
                self.limit
            ),
            None => format!(
                "source nesting depth exceeds the analysis limit of {} (rigor-rs#412); \
                 file not analysed",
                self.limit
            ),
        }
    }
}

/// Parse `source`, then bound its nesting depth.
///
/// # Errors
///
/// [`TooDeep`] when the tree nests deeper than [`MAX_NESTING_DEPTH`]. The
/// parse result is then released on a thread sized for its depth, never on
/// the caller's stack: `pm_node_destroy` recurses once per level (48 B per
/// call link, 144 B per `if`-modifier level in release), so an unbounded
/// tree cannot be dropped in place.
pub fn parse_bounded(source: &[u8]) -> Result<ParseResult<'_>, TooDeep> {
    let result = ruby_prism::parse(source);
    if measure(&result, MAX_NESTING_DEPTH).over {
        return Err(release(result, source.len()));
    }
    Ok(result)
}

unsafe extern "C" {
    /// Prism's generic child walk (`include/prism/node.h`,
    /// `PRISM_EXPORTED_FUNCTION`). ruby-prism-sys does not generate a binding
    /// for it, but the symbol is in the libprism archive it links.
    fn pm_visit_child_nodes(
        node: *const c_void,
        visitor: extern "C" fn(*const c_void, *mut c_void) -> bool,
        data: *mut c_void,
    );
}

/// The depth walk's state.
struct Walk {
    depth: usize,
    max: usize,
    limit: usize,
    over: bool,
}

/// One node of the walk: descend unless the bound is already passed, and
/// return `false` so Prism never descends a second time on its own.
///
/// This walks in C rather than through ruby-prism's `Visit`. The C walk
/// reaches every child field, including `RescueNode.subsequent`, the one
/// recursion `Visit` takes without passing through `Visit::visit`. It stops
/// without unwinding, and a level costs ~93 B in release and ~1.5 KiB in
/// debug, against ~160 B and ~12 KiB through `Visit`.
extern "C" fn walk_node(node: *const c_void, data: *mut c_void) -> bool {
    // SAFETY: `data` is the `Walk` that `measure` owns for the whole walk,
    // and nothing else aliases it while the walk runs.
    let walk = unsafe { &mut *data.cast::<Walk>() };
    if walk.over {
        return false;
    }
    walk.depth += 1;
    walk.max = walk.max.max(walk.depth);
    if walk.depth > walk.limit {
        walk.over = true;
    } else {
        // SAFETY: `node` is a live node of the parse result `measure` borrows.
        unsafe { pm_visit_child_nodes(node, walk_node, data) };
    }
    walk.depth -= 1;
    false
}

/// Walk `result`'s tree, stopping once a level passes `limit`.
fn measure(result: &ParseResult<'_>, limit: usize) -> Walk {
    let mut walk = Walk { depth: 0, max: 0, limit, over: false };
    let root: *const c_void = match result.node() {
        ruby_prism::Node::ProgramNode { pointer, .. } => pointer.cast(),
        // `pm_parse` always returns a program node.
        _ => return walk,
    };
    let data: *mut Walk = &mut walk;
    walk_node(root, data.cast());
    walk
}

/// Stack for the thread that measures and drops a refused tree: a fixed base
/// plus a per-source-byte allowance. A chain level takes at least two source
/// bytes, and the walk plus the destroy cost at most ~100 B per level in
/// release and ~1.5 KiB in debug, so these sizes keep a margin above one
/// level per byte. The stack is reserved, not committed.
const RELEASE_STACK_BASE: usize = 16 << 20;
const RELEASE_STACK_PER_BYTE: usize = if cfg!(debug_assertions) { 4096 } else { 256 };

/// Measure a refused tree's full depth and drop it on a dedicated thread.
/// If the thread cannot be created, the tree is leaked rather than dropped
/// on a stack too small for it.
fn release(result: ParseResult<'_>, source_len: usize) -> TooDeep {
    /// The parse result, movable to the release thread.
    struct Detached<'pr>(ManuallyDrop<ParseResult<'pr>>);
    // SAFETY: `ParseResult` is `!Send` because it holds raw pointers into
    // Prism's parser. Only the scoped thread below touches them, while the
    // spawning thread waits in `join`, so there is no concurrent access.
    unsafe impl Send for Detached<'_> {}

    let stack = RELEASE_STACK_BASE.saturating_add(source_len.saturating_mul(RELEASE_STACK_PER_BYTE));
    let detached = Detached(ManuallyDrop::new(result));
    let depth = std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new().stack_size(stack).spawn_scoped(scope, move || {
            let detached = detached;
            let result = ManuallyDrop::into_inner(detached.0);
            let depth = measure(&result, usize::MAX).max;
            drop(result);
            depth
        });
        // A failed spawn drops the closure, and with it the `ManuallyDrop`,
        // which leaks the tree instead of destroying it here.
        spawned.ok().and_then(|handle| handle.join().ok())
    });
    TooDeep { depth, limit: MAX_NESTING_DEPTH }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shallow_source_parses() {
        let result = parse_bounded(b"x = [1, [2, [3]]]\n").expect("within the bound");
        assert_eq!(result.errors().count(), 0);
        assert_eq!(measure(&result, usize::MAX).max, 7);
    }

    #[test]
    fn a_rescue_clause_list_counts_one_level_per_clause() {
        // `RescueNode.subsequent` is the one recursion ruby-prism's `Visit`
        // takes outside `Visit::visit`; the C walk must still count it.
        let src = format!("begin\n  1\n{}end\n", "rescue A\n".repeat(50));
        let result = parse_bounded(src.as_bytes()).expect("within the bound");
        assert!(measure(&result, usize::MAX).max > 50);
    }

    #[test]
    fn the_walk_stops_at_the_limit() {
        let src = format!("x{}", ".a".repeat(500));
        let result = ruby_prism::parse(src.as_bytes());
        let walk = measure(&result, 100);
        assert!(walk.over);
        assert_eq!(walk.max, 101);
        assert_eq!(walk.depth, 0);
    }

    #[test]
    fn a_tree_past_the_bound_is_refused_with_its_depth() {
        // One level per link; a 2 MiB test thread carries the bounded walk
        // (~93 B/level release, ~1.5 KiB debug) only part of the way, so run
        // on a thread sized like an analysis thread.
        let err = std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(|| {
                let src = format!("x{}", ".a".repeat(MAX_NESTING_DEPTH + 10));
                parse_bounded(src.as_bytes()).map(|_| ()).unwrap_err()
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(err.limit, MAX_NESTING_DEPTH);
        assert_eq!(err.depth, Some(MAX_NESTING_DEPTH + 13));
        assert!(err.message().contains("25013"), "{}", err.message());
    }
}
