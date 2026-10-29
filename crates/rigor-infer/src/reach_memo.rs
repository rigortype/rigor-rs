//! The memo of [`Typer::local_reach`] (rigor-rs#422).
//!
//! `local_reach` re-enters each reaching write's value through four paths
//! (`expr_scalar`, `expr_reach`, `latest_definite_assignment`'s folds, the
//! `Op`/`Multi` arms) with the same `seen` stack, so its call count grows as
//! about 4^3 × C(W, 4) for W writes of a local — 961,696 calls for 165
//! distinct reads on `cond_24`. The memo answers a repeated call from the
//! first one, and it must answer EXACTLY what recomputing would answer now.
//! A `local_reach` computation reads its context only through:
//!
//! - `seen.len()` (the caps in `expr_scalar`, `root_reach` and `def_exits`):
//!   part of the key, together with the AST, the root, both ends of the use
//!   span and `skip_class_guards`;
//! - `seen` MEMBERSHIP (the same three sites, which ask through
//!   [`Typer::seen_contains`]): every key a computation tests is recorded
//!   with its answer against the stack the computation started on, and an
//!   entry is reused only where the current stack gives the same answers;
//! - the dead set (`dead_positions`): during the scan the partial set,
//!   replaced at every publish, then the settled one. An entry is valid only
//!   for the dead-set OBJECT it was computed against — the memo holds that
//!   `Rc` and compares identities on every lookup, so a publish, the scan's
//!   end or an AST switch invalidates it without a clear call anyone could
//!   forget — and a computation whose dead set changed while it ran (the call
//!   that triggered the scan) is never stored;
//! - `expr_truthiness`, whose memo only grows while the dead set stands: a
//!   predicate answered once answers the same again. The exception is a
//!   RE-ENTRY: a predicate whose fold is still running answers `None`, and
//!   answers its folded value once the fold lands. A computation that saw a
//!   re-entry therefore records the predicate, and its entry is reused only
//!   while every such predicate still answers `None` — still folding, or
//!   landed on `None` — when recomputing would see the same `None`; it is
//!   dropped once one lands on a value. (Inside a scan every read under a
//!   loop re-enters the predicate being folded, so the stricter "never
//!   store it" left `loopmix_48` at 27 s.) A fold that runs
//!   to completion inside a computation is a barrier: its answer is
//!   memoized on the spot and a recompute reads that answer instead of
//!   re-running the fold, so neither the `seen` tests nor the re-entries
//!   inside the fold are dependencies of the computations around it.
//!
//! `RIGOR_REACH_MEMO_AUDIT=1` recomputes on every hit and panics on a
//! mismatch.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;
use std::sync::OnceLock;

use rigor_parse::{LoweredAst, Span};
use rigor_types::Scalar;

use crate::dead::DeadPositions;
use crate::reach::Reach;
use crate::Typer;

/// What `local_reach` answers.
type ReachOut = (Reach, Option<Scalar>);

/// A multiply-rotate hasher (the `FxHasher` scheme) for the memo's own maps:
/// every `seen` test interns a short key and every lookup hashes the stack,
/// which SipHash made a visible share of the profile. The keys are the
/// analysis's own spans and names, not attacker-chosen input.
#[derive(Default)]
struct FastHasher(u64);

impl Hasher for FastHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, n: u8) {
        self.add(u64::from(n));
    }

    fn write_u32(&mut self, n: u32) {
        self.add(u64::from(n));
    }

    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl FastHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// The most entries kept per key. One key collects an entry per `seen`
/// variant its answer depended on; a generated lexer's state-machine loop
/// (hundreds of writes of one local, every read reaching every other) mints
/// variants without bound. Dropping the least recently used one only costs a
/// recompute — a computation's side effects all happened the first time it
/// ran.
const MAX_ENTRIES_PER_KEY: usize = 128;

/// An `expr_truthiness` memo key: `(ast address, predicate node)`.
type TruthyKey = (usize, u32);

/// One stored answer.
struct MemoEntry {
    /// The root it is for (the key carries only the use span).
    root: String,
    /// Every `seen` key the computation tested (interned, see
    /// [`ReachMemo::keys`]), sorted.
    tested: Rc<[u32]>,
    /// `tested` folded into 64 bits ([`id_bit`]): a clear bit proves an id
    /// untested without the search.
    tested_bits: u64,
    /// The tested keys its starting stack held, sorted: a stack of the same
    /// length answers every test the same way iff it holds exactly these
    /// among `tested`.
    present: Box<[u32]>,
    /// The folds it saw re-entered: it holds only while each still answers
    /// `None` (running, or landed on `None`).
    folding: Rc<[TruthyKey]>,
    out: ReachOut,
}

/// The bit an interned id sets in [`MemoEntry::tested_bits`].
fn id_bit(id: u32) -> u64 {
    1 << (id % 64)
}

impl MemoEntry {
    /// Whether a stack holding the (distinct) interned keys `stack` answers
    /// every test this computation made the way its starting stack did.
    fn matches(&self, stack: &[u32]) -> bool {
        let mut held = 0;
        for id in stack {
            if self.tested_bits & id_bit(*id) != 0 && self.tested.binary_search(id).is_ok() {
                if !self.present.contains(id) {
                    return false;
                }
                held += 1;
            }
        }
        held == self.present.len()
    }
}

/// The memo: entries per `(use span, skip_class_guards, seen.len())`, valid
/// for one `(AST identity, dead-set object)` pair.
#[derive(Default)]
pub(crate) struct ReachMemo {
    /// The AST identity (address, arena length) the entries belong to.
    ast: (usize, usize),
    /// The dead set the entries were computed against — held, so its address
    /// cannot be reused by a later set while the memo compares against it.
    dead: Option<Rc<DeadPositions>>,
    /// Most recently used first; at most [`MAX_ENTRIES_PER_KEY`] per key.
    entries: FastMap<(Span, bool, usize), Vec<MemoEntry>>,
    /// Every `seen` key tested on this AST, interned, so dependencies are
    /// lists of small ids. Reset with the AST only while no computation is
    /// in progress — the frames hold ids too.
    keys: FastMap<String, u32>,
}

impl ReachMemo {
    /// Whether the entries belong to `ast` and `dead`.
    fn valid_for(&self, ast: (usize, usize), dead: &Option<Rc<DeadPositions>>) -> bool {
        self.ast == ast && same_dead(&self.dead, dead)
    }

    /// Start over for `ast` and `dead` unless the entries already belong to
    /// them.
    fn reset_unless_valid(&mut self, ast: (usize, usize), dead: &Option<Rc<DeadPositions>>) {
        if !self.valid_for(ast, dead) {
            self.ast = ast;
            self.dead = dead.clone();
            self.entries.clear();
        }
    }

    fn intern(&mut self, key: &str) -> u32 {
        if let Some(&id) = self.keys.get(key) {
            return id;
        }
        let id = self.keys.len() as u32;
        self.keys.insert(key.to_string(), id);
        id
    }

    /// The interned ids of the keys on `stack` (a key never interned was
    /// never tested, so it cannot matter to any entry).
    fn stack_ids(&self, stack: &[String]) -> Vec<u32> {
        stack.iter().filter_map(|k| self.keys.get(k.as_str()).copied()).collect()
    }
}

/// One frame of the memo's dependency tracking.
enum MemoFrame {
    /// A `local_reach` computation in progress: the `seen` stack it runs on
    /// (by address), the stamp that marks the keys it already recorded (see
    /// [`MemoFrames::marks`]), and what it has depended on so far — the
    /// interned `seen` keys it tested and the folds it saw re-entered.
    Reach { seen: usize, stamp: u32, tested: Vec<u32>, folding: Vec<TruthyKey> },
    /// An `expr_truthiness` fold running to completion: what happens above
    /// it reaches the frames below only through the fold's memoized answer.
    Barrier,
}

/// The frames of the computations in progress, innermost last. A dependency
/// is recorded in the innermost frame only, and handed to the frame below
/// when a computation ends — never across a barrier.
#[derive(Default)]
pub(crate) struct MemoFrames {
    frames: Vec<MemoFrame>,
    /// Per frame depth, the stamp of the frame that last recorded each
    /// interned key — a frame records a key once without a set.
    marks: Vec<Vec<u32>>,
    next_stamp: u32,
}

impl MemoFrames {
    fn push_reach(&mut self, seen: usize) {
        if self.next_stamp == u32::MAX {
            // Stamps wrapped: forget every mark so none can alias.
            self.marks.iter_mut().for_each(|m| m.iter_mut().for_each(|s| *s = 0));
            self.next_stamp = 0;
        }
        self.next_stamp += 1;
        let stamp = self.next_stamp;
        self.frames.push(MemoFrame::Reach { seen, stamp, tested: Vec::new(), folding: Vec::new() });
    }

    /// Record dependencies in the innermost frame when it is a computation:
    /// the tested keys only when it runs on stack `stack`.
    fn record(&mut self, stack: usize, tested: &[u32], folding: &[TruthyKey]) {
        let depth = self.frames.len();
        let Some(MemoFrame::Reach { seen, stamp, tested: t, folding: f, .. }) = self.frames.last_mut()
        else {
            return;
        };
        if *seen == stack && !tested.is_empty() {
            if self.marks.len() < depth {
                self.marks.resize_with(depth, Vec::new);
            }
            let marks = &mut self.marks[depth - 1];
            for &id in tested {
                let i = id as usize;
                if marks.len() <= i {
                    marks.resize(i + 1, 0);
                }
                if marks[i] != *stamp {
                    marks[i] = *stamp;
                    t.push(id);
                }
            }
        }
        for p in folding {
            if !f.contains(p) {
                f.push(*p);
            }
        }
    }
}

/// Same dead-set object (or both absent).
fn same_dead(a: &Option<Rc<DeadPositions>>, b: &Option<Rc<DeadPositions>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}

/// `RIGOR_REACH_MEMO_AUDIT` is set (to anything but empty or `0`).
fn audit_mode() -> bool {
    static AUDIT: OnceLock<bool> = OnceLock::new();
    *AUDIT.get_or_init(|| {
        std::env::var("RIGOR_REACH_MEMO_AUDIT").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

fn ast_identity(ast: &LoweredAst) -> (usize, usize) {
    (ast as *const LoweredAst as usize, ast.len())
}

/// The address that identifies a `seen` stack. The `Vec` itself, not its
/// buffer: a push may move the buffer, never the borrowed `Vec`.
#[allow(clippy::ptr_arg)]
fn stack_id(seen: &Vec<String>) -> usize {
    seen as *const Vec<String> as usize
}

impl<'i> Typer<'i> {
    /// `seen.contains(key)`, recorded as a dependency of the `local_reach`
    /// computation in progress on this stack. Every `seen` membership test
    /// on the reach path goes through here.
    #[allow(clippy::ptr_arg)] // the `Vec`'s address identifies the stack
    pub(crate) fn seen_contains(&self, seen: &Vec<String>, key: &str) -> bool {
        let stack = stack_id(seen);
        let mut frames = self.reach_frames.borrow_mut();
        if matches!(frames.frames.last(), Some(MemoFrame::Reach { seen: s, .. }) if *s == stack) {
            let id = self.reach_memo.borrow_mut().intern(key);
            frames.record(stack, &[id], &[]);
        }
        seen.iter().any(|s| s == key)
    }

    /// The fold of `pred` re-entered and answered `None`: the `local_reach`
    /// computations in progress hold only while that fold is still running.
    pub(crate) fn note_truthy_reentry(&self, pred: TruthyKey) {
        self.reach_frames.borrow_mut().record(0, &[], &[pred]);
    }

    /// Run a truthiness fold behind a barrier (see [`MemoFrame::Barrier`]).
    pub(crate) fn behind_truthy_barrier<T>(&self, fold: impl FnOnce() -> T) -> T {
        self.reach_frames.borrow_mut().frames.push(MemoFrame::Barrier);
        let out = fold();
        let popped = self.reach_frames.borrow_mut().frames.pop();
        debug_assert!(matches!(popped, Some(MemoFrame::Barrier)));
        out
    }

    /// The dead set `dead_positions(ast, …)` would answer right now WITHOUT
    /// running a scan — the same resolution order: the settled set when it
    /// is cached for `ast`, else the partial set while a scan is in flight,
    /// else none (a call would start the scan).
    fn current_dead(&self, ast: &LoweredAst) -> Option<Rc<DeadPositions>> {
        let key = ast as *const LoweredAst as usize;
        if let Some((cached_key, cached)) = &*self.dead_cache.borrow() {
            if *cached_key == key {
                return Some(cached.clone());
            }
        }
        if self.dead_in_flight.get() {
            return Some(self.dead_partial.borrow().clone());
        }
        None
    }

    /// The memoizing front of [`Typer::local_reach_uncached`] — see the
    /// module docs for what a hit must (and does) reproduce.
    pub(crate) fn local_reach(
        &self,
        ast: &LoweredAst,
        root: &str,
        use_span: Span,
        seen: &mut Vec<String>,
        skip_class_guards: bool,
    ) -> ReachOut {
        let ast_id = ast_identity(ast);
        let dead = self.current_dead(ast);
        let key = (use_span, skip_class_guards, seen.len());
        let hit = {
            let mut memo = self.reach_memo.borrow_mut();
            if memo.ast != ast_id {
                // A long-lived `Typer` (`infer_method_returns`) walks many
                // ASTs: start the interner over rather than let it grow
                // across all of them, once nothing holds an id.
                let mut frames = self.reach_frames.borrow_mut();
                if frames.frames.is_empty() {
                    memo.keys.clear();
                    frames.marks.clear();
                }
            }
            memo.reset_unless_valid(ast_id, &dead);
            let stack = memo.stack_ids(seen);
            match memo.entries.get_mut(&key) {
                Some(entries) => {
                    // An entry that saw a re-entry of a fold that has since
                    // landed on a value can never match again: drop it when
                    // it would have matched. The truthiness memo only grows
                    // while the dead set stands, so a fold that landed on
                    // `None` keeps answering `None`.
                    let running = self.truthy_in_flight.borrow();
                    let landed = self.truthy_memo.borrow();
                    let mut found = None;
                    let mut i = 0;
                    while i < entries.len() {
                        let e = &entries[i];
                        if e.root != root || !e.matches(&stack) {
                            i += 1;
                            continue;
                        }
                        if e.folding.iter().all(|p| running.contains(p) || landed.get(p) == Some(&None)) {
                            found = Some(i);
                            break;
                        }
                        entries.remove(i);
                    }
                    found.map(|i| {
                        // Most recently used first: a repeated lookup finds
                        // its entry at once, and eviction drops the coldest.
                        entries[..=i].rotate_right(1);
                        let e = &entries[0];
                        (e.out.clone(), e.tested.clone(), e.folding.clone())
                    })
                }
                None => None,
            }
        };
        if let Some((out, tested, folding)) = hit {
            // The hit's dependencies are the enclosing computation's too.
            self.reach_frames.borrow_mut().record(stack_id(seen), &tested, &folding);
            if audit_mode() {
                let again =
                    self.local_reach_framed(ast, root, use_span, seen, skip_class_guards, dead);
                assert!(
                    again == out,
                    "rigor-rs#422 reach memo audit: local_reach({root}, {use_span:?}, \
                     skip_class_guards={skip_class_guards}, seen={seen:?}) memo {out:?} != \
                     recomputed {again:?} in {:?}",
                    ast.file_key(),
                );
            }
            return out;
        }
        self.local_reach_framed(ast, root, use_span, seen, skip_class_guards, dead)
    }

    /// Compute under a fresh dependency frame, store the answer unless the
    /// dead set changed while it ran, and hand its dependencies to the
    /// enclosing computation.
    fn local_reach_framed(
        &self,
        ast: &LoweredAst,
        root: &str,
        use_span: Span,
        seen: &mut Vec<String>,
        skip_class_guards: bool,
        dead: Option<Rc<DeadPositions>>,
    ) -> ReachOut {
        let base = seen.len();
        let stack = stack_id(seen);
        self.reach_frames.borrow_mut().push_reach(stack);
        let out = self.local_reach_uncached(ast, root, use_span, seen, skip_class_guards);
        let (mut tested, folding) = {
            let mut frames = self.reach_frames.borrow_mut();
            let Some(MemoFrame::Reach { tested, folding, .. }) = frames.frames.pop() else {
                unreachable!("reach memo frames out of balance");
            };
            frames.record(stack, &tested, &folding);
            (tested, folding)
        };
        if same_dead(&dead, &self.current_dead(ast)) {
            let mut memo = self.reach_memo.borrow_mut();
            if memo.valid_for(ast_identity(ast), &dead) {
                tested.sort_unstable();
                // The stack below `base` is the one the computation started
                // on: nested calls only push above it.
                let mut present: Vec<u32> = memo
                    .stack_ids(&seen[..base])
                    .into_iter()
                    .filter(|id| tested.binary_search(id).is_ok())
                    .collect();
                present.sort_unstable();
                let entries = memo.entries.entry((use_span, skip_class_guards, base)).or_default();
                entries.truncate(MAX_ENTRIES_PER_KEY - 1);
                entries.insert(
                    0,
                    MemoEntry {
                        root: root.to_string(),
                        tested_bits: tested.iter().fold(0, |bits, &id| bits | id_bit(id)),
                        tested: tested.into(),
                        present: present.into(),
                        folding: folding.into(),
                        out: out.clone(),
                    },
                );
            }
        }
        out
    }
}
