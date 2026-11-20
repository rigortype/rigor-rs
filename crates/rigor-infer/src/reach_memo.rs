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

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};

use rigor_parse::{LoweredAst, NodeId, Span};
use rigor_types::{Scalar, Type};

use crate::dead::DeadPositions;
use crate::reach::Reach;
use crate::Typer;

/// What `local_reach` answers.
type ReachOut = (Reach, Option<Scalar>);

/// A multiply-rotate hasher (the `FxHasher` scheme) for the memo's own maps:
/// every `seen` test interns a short key and every lookup hashes the stack,
/// which SipHash made a visible share of the profile. The keys are the
/// analysis's own spans and names, not attacker-chosen input. `pub(crate)`
/// for the other name-hashed maps whose keys are likewise our own
/// (rigor-rs#657's declared-name census).
#[derive(Default)]
pub(crate) struct FastHasher(u64);

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

/// An `expr_truthiness` memo key: `([`LoweredAst::uid`], predicate node)`.
pub(crate) type TruthyKey = (usize, u32);

/// The most `kills` cells [`ReachMemo::kill_scans`] may hold. Rows are
/// unbounded in the statement list's length and in their count; without a
/// bound a file could trade the quadratic scan the rows exist to remove
/// (rigor-rs#424) for quadratic memory. Past it the map clears and new rows
/// stop being built until the next `(ast, dead)` window — the scans then
/// cost what they did before the rows existed.
pub(crate) const MAX_KILL_CELLS: usize = 1 << 21;

/// One [`Typer::kill_prefix`] row's key: the statement list's identity (its
/// arena slice address and length — stable for an `LoweredAst`'s lifetime,
/// and the rows die with the memo's `(ast, dead)` window anyway), the
/// target's name (an ivar's `@`/`@@`/`$` sigil cannot collide with a local
/// spelling), and `seen.len()` — the fold fuel the caps in `expr_scalar`,
/// `root_reach` and `def_exits` read. The stack's CONTENTS are not part of
/// the key (two reads of one local carry different `name@offset` keys);
/// each [`KillSeg`] carries the [`MemoEntry`] dependency record of the
/// tests its evaluation made instead.
pub(crate) type KillKey = (usize, usize, Box<str>, usize);

/// One extension segment of a [`KillScan`] row: the cells before `end` were
/// folded under the dependency record the segment carries — every `seen`
/// key the evaluations tested (and which of them the building stack held),
/// plus the folds they saw re-entered. A prefix reaching into the segment
/// is reusable only by a stack answering those tests the same way
/// ([`deps_match`]) AND while every re-entered fold still answers `None` —
/// the [`MemoEntry`] rules, verbatim.
pub(crate) struct KillSeg {
    /// `kills` is covered through this index (exclusive).
    pub(crate) end: usize,
    /// The folds the evaluations saw re-entered.
    pub(crate) folding: Rc<[TruthyKey]>,
    /// Every `seen` key the evaluations tested (interned), sorted.
    pub(crate) tested: Rc<[u32]>,
    /// `tested` folded into 64 bits ([`id_bit`]).
    pub(crate) tested_bits: u64,
    /// The tested keys the building stack held, sorted.
    pub(crate) present: Rc<[u32]>,
}

/// The running prefix fold of `definitely_assigns` over one statement list
/// (rigor-rs#424). `kills[i]` is the span of the last statement in
/// `body[..=i]` that definitely assigns — the answer the `body[..pos]` scan
/// in [`Typer::latest_definite_assignment`] computes for `pos = i + 1`.
/// Without it N reads in one flat statement list (`x_i = 1 if y` × N)
/// rescan the prefix per read — O(N²) `definitely_assigns` calls.
pub(crate) struct KillScan {
    /// `kills[i]`: the last definitely-assigning span in `body[..=i]`.
    pub(crate) kills: Vec<Option<Span>>,
    /// One [`KillSeg`] per extension, in order.
    pub(crate) segs: Vec<KillSeg>,
}

/// The dependencies a `kill_prefix` extension collects while it runs —
/// every `seen` test ([`Typer::seen_contains`]) and every re-entered fold
/// ([`Typer::note_truthy_reentry`]) lands on the top recording, and a
/// nested extension's pop merges it into the enclosing one (an outer
/// segment's cells were folded through the inner evaluations too).
#[derive(Default)]
pub(crate) struct KillRec {
    pub(crate) tested: Vec<u32>,
    pub(crate) folding: Vec<TruthyKey>,
}

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
pub(crate) fn id_bit(id: u32) -> u64 {
    1 << (id % 64)
}

/// Whether a stack holding the (distinct) interned keys `stack` answers
/// every test the recorded computation made the way its starting stack did.
pub(crate) fn deps_match(tested_bits: u64, tested: &[u32], present: &[u32], stack: &[u32]) -> bool {
    let mut held = 0;
    for id in stack {
        if tested_bits & id_bit(*id) != 0 && tested.binary_search(id).is_ok() {
            if !present.contains(id) {
                return false;
            }
            held += 1;
        }
    }
    held == present.len()
}

impl MemoEntry {
    /// Whether a stack holding the (distinct) interned keys `stack` answers
    /// every test this computation made the way its starting stack did.
    fn matches(&self, stack: &[u32]) -> bool {
        deps_match(self.tested_bits, &self.tested, &self.present, stack)
    }
}

/// One stored `expr_scalar` / `expr_reach` answer — the [`MemoEntry`]
/// dependency record minus the local root these per-expression walks do not
/// have (the node id in the key already says which expression it is for).
struct ExprEntry<T> {
    /// Every `seen` key the computation tested (interned), sorted.
    tested: Rc<[u32]>,
    /// `tested` folded into 64 bits.
    tested_bits: u64,
    /// The tested keys its starting stack held, sorted.
    present: Box<[u32]>,
    /// The folds it saw re-entered, same rule as [`MemoEntry::folding`].
    folding: Rc<[TruthyKey]>,
    out: T,
}

impl<T> ExprEntry<T> {
    fn matches(&self, stack: &[u32]) -> bool {
        deps_match(self.tested_bits, &self.tested, &self.present, stack)
    }
}

/// What [`expr_entry_hit`] returns: the stored answer plus the dependency
/// record handed to the enclosing computation.
type ExprHit<T> = (T, Rc<[u32]>, Rc<[TruthyKey]>);

/// `type_of` memo payload: the carrier plus whether it embeds no interner
/// ids. The FLAT memo (`type_of_memo`) stores only when the flag is `true`
/// — its entries re-intern under whichever interner the hit arrives with.
/// The env memo (`type_of_env_memo`) is uid-scoped, so its callers pass
/// `true` and every carrier stores.
type TypeOfOut = (Type, bool);

/// One stored `type_of` answer for an env-SENSITIVE `Node::Call` — the
/// [`ExprEntry`] dependency record plus the env context the answer was
/// computed under (rigor-rs#658). An env-free call's answer stands under
/// every env; an env-sensitive one's stands only under the env that
/// produced it, so the env rides the entry — fingerprinted for the cheap
/// reject, snapshotted for the airtight check.
struct EnvExprEntry {
    /// Every `seen` key the computation tested (interned), sorted.
    tested: Rc<[u32]>,
    /// `tested` folded into 64 bits.
    tested_bits: u64,
    /// The tested keys its starting stack held, sorted.
    present: Box<[u32]>,
    /// The folds it saw re-entered, same rule as [`MemoEntry::folding`].
    folding: Rc<[TruthyKey]>,
    /// Order-independent fingerprint of the env at compute time — a fast
    /// reject before [`Self::env`] is walked.
    env_fp: u64,
    /// The env bindings verbatim; a fingerprint match is confirmed
    /// against this — the `TypeId`s are only meaningful under
    /// [`Self::interner`], which is why that is compared first.
    env: Rc<Vec<(Rc<str>, rigor_types::TypeId)>>,
    /// `Typer::def_scope` at compute time — `type_of` reads it
    /// (`LocalVariableOpWrite`, `decline_fold_gap`), so it is part of the
    /// context an entry is valid for.
    def_scope: Option<rigor_parse::Span>,
    /// The interner `env`'s ids belong to ([`rigor_types::Interner::uid`]).
    /// A `TypeId` is an index into one interner; under another the
    /// snapshot compares nonsense, so the interner must match before it
    /// is consulted.
    interner: u64,
    out: TypeOfOut,
}

impl EnvExprEntry {
    /// The env context plus the stack-dependency answer — see
    /// [`ExprEntry::matches`].
    fn matches(
        &self,
        stack: &[u32],
        env_fp: u64,
        env: &crate::TypeEnv,
        def_scope: Option<rigor_parse::Span>,
        interner: u64,
    ) -> bool {
        self.env_fp == env_fp
            && self.def_scope == def_scope
            && self.interner == interner
            && self.env.len() == env.len()
            && self
                .env
                .iter()
                .all(|(n, t)| env.get(n.as_ref()) == Some(t))
            && deps_match(self.tested_bits, &self.tested, &self.present, stack)
    }
}

/// The env-sensitive counterpart of [`expr_entry_hit`]: the dep-match loop
/// with [`EnvExprEntry::matches`]'s env test folded in.
// too_many_arguments: the env context is one logical key — a bundle struct
/// would obscure what a hit must match.
#[allow(clippy::too_many_arguments)]
fn env_expr_entry_hit(
    entries: &mut Vec<EnvExprEntry>,
    stack: &[u32],
    running: &std::collections::HashSet<TruthyKey>,
    landed: &std::collections::HashMap<TruthyKey, Option<bool>>,
    env_fp: u64,
    env: &crate::TypeEnv,
    def_scope: Option<rigor_parse::Span>,
    interner: u64,
) -> Option<ExprHit<TypeOfOut>> {
    let mut found = None;
    let mut i = 0;
    while i < entries.len() {
        let e = &entries[i];
        if !e.matches(stack, env_fp, env, def_scope, interner) {
            i += 1;
            continue;
        }
        if e.folding
            .iter()
            .all(|p| running.contains(p) || landed.get(p) == Some(&None))
        {
            found = Some(i);
            break;
        }
        entries.remove(i);
    }
    found.map(|i| {
        entries[..=i].rotate_right(1);
        let e = &entries[0];
        (e.out.clone(), e.tested.clone(), e.folding.clone())
    })
}

/// An order-independent fingerprint of an env's `(name, TypeId)` pairs,
/// under the interner that minted the ids. Order-independence is required —
/// `HashMap` iteration order is not part of the context a computation
/// read, only the bindings are.
fn env_fingerprint(env: &crate::TypeEnv) -> u64 {
    let mut acc = 0u64;
    for (name, ty) in env {
        let mut h = FastHasher::default();
        h.write(name.as_bytes());
        h.write_u64(name.len() as u64);
        h.write_u64(ty.index() as u64);
        acc = acc.wrapping_add(h.finish());
    }
    acc
}

/// Per-node table keyed by `(node id, seen.len())`.
type ExprTable<T> = FastMap<(u32, usize), Vec<ExprEntry<T>>>;

/// The newest usable `ExprEntry` in `entries` for `stack` — the matching
/// half of `local_reach`'s hit loop (the `root` test does not apply: the key
/// names the node). MRU first; an entry whose fold landed on a value is
/// dropped rather than matched, exactly as [`Typer::local_reach`] does.
fn expr_entry_hit<T: Clone>(
    entries: &mut Vec<ExprEntry<T>>,
    stack: &[u32],
    running: &std::collections::HashSet<TruthyKey>,
    landed: &std::collections::HashMap<TruthyKey, Option<bool>>,
) -> Option<ExprHit<T>> {
    let mut found = None;
    let mut i = 0;
    while i < entries.len() {
        let e = &entries[i];
        if !e.matches(stack) {
            i += 1;
            continue;
        }
        if e.folding
            .iter()
            .all(|p| running.contains(p) || landed.get(p) == Some(&None))
        {
            found = Some(i);
            break;
        }
        entries.remove(i);
    }
    found.map(|i| {
        entries[..=i].rotate_right(1);
        let e = &entries[0];
        (e.out.clone(), e.tested.clone(), e.folding.clone())
    })
}

/// The memo: entries per `(use span, skip_class_guards, seen.len())`, valid
/// for one `(AST identity, dead-set object)` pair.
#[derive(Default)]
pub(crate) struct ReachMemo {
    /// The AST identity ([`LoweredAst::uid`], arena length) the entries
    /// belong to.
    ast: (usize, usize),
    /// The dead set the entries were computed against — held, so its address
    /// cannot be reused by a later set while the memo compares against it.
    dead: Option<Rc<DeadPositions>>,
    /// Most recently used first; at most [`MAX_ENTRIES_PER_KEY`] per key.
    entries: FastMap<(Span, bool, usize), Vec<MemoEntry>>,
    /// `expr_scalar` answers per `(node, base stack depth)` — the same
    /// `(ast, dead)` validity window as `entries`, and the same dependency
    /// record per entry. A deep right-nested chain (`2 ** 2 ** …`,
    /// rigor-rs#424) is what re-walked its whole remaining suffix per level
    /// without this.
    expr_scalars: ExprTable<Option<Scalar>>,
    /// `expr_reach` answers under the same keying.
    expr_reaches: ExprTable<Reach>,
    /// `type_of` answers for env-free `Node::Call` subtrees — [`TypeOfOut`]:
    /// only a `Type` that embeds no interner ids is stored (see
    /// [`Typer::type_of_memo`]), so a hit re-interns it under whatever
    /// interner asks (rigor-rs#424).
    type_ofs: ExprTable<TypeOfOut>,
    /// `type_of` answers for env-SENSITIVE `Node::Call` subtrees
    /// (rigor-rs#658): same table shape as `type_ofs`, each entry
    /// additionally carrying the env context it is valid under. Without it
    /// every `check_env_at` replay of a mutator chain's call re-types the
    /// whole remaining suffix — `v.push(1).push(1).…` at N links cost
    /// O(N²) `type_call` levels, on top of the replay's own applies.
    type_env_ofs: FastMap<(u32, usize), Vec<EnvExprEntry>>,
    /// `expr_truthiness` answers for folds run under a NON-EMPTY `seen`
    /// (rigor-rs#437): same key and dependency shape as `expr_scalars`. The
    /// dead scan's own `if` visits and post-scan callers fold with an empty
    /// `seen` — those land in the canonical [`Typer::truthy_memo`]. A
    /// nested fold's answer depends on the stack it ran under (the fuel
    /// caps AND `seen` membership), so it lives here where a hit must
    /// reproduce a recompute in THIS context; a fuel-starved or
    /// differently-stacked first visit can never answer a shallower or
    /// differently-stacked later one.
    truthys: ExprTable<Option<bool>>,
    /// Every `seen` key tested on this AST, interned, so dependencies are
    /// lists of small ids. Reset with the AST only while no computation is
    /// in progress — the frames hold ids too.
    keys: FastMap<String, u32>,
    /// `latest_definite_assignment`'s prefix folds per [`KillKey`]
    /// (rigor-rs#424) — the same `(ast, dead)` validity window as
    /// `entries`: a `definitely_assigns` answer reads the dead set
    /// (`dead.covers`), the `seen` stack (in the key) and the truthiness
    /// memo (the per-segment `folds` records), nothing else mutable.
    pub(crate) kill_scans: FastMap<KillKey, KillScan>,
    /// Total `kills` cells over `kill_scans`, for [`MAX_KILL_CELLS`].
    pub(crate) kill_cells: usize,
    /// `true` once `kill_cells` overflowed in this window — new rows stop
    /// being built until the next reset.
    pub(crate) kill_overflowed: bool,
    /// The [`KillRec`] stack of `kill_prefix` extensions in progress —
    /// transient, one push per extension, not part of the validity window.
    pub(crate) kill_rec: RefCell<Vec<KillRec>>,
}

impl ReachMemo {
    /// Whether the entries belong to `ast` and `dead`.
    pub(crate) fn valid_for(&self, ast: (usize, usize), dead: &Option<Rc<DeadPositions>>) -> bool {
        self.ast == ast && same_dead(&self.dead, dead)
    }

    /// Entries stored for [`Typer::type_of_env_memo`] at `node` — the
    /// rigor-rs#427 pin that a block chain's interner-bound `Tuple`
    /// answers STORE (a `0` would mean every driver visit re-types the
    /// whole receiver suffix).
    #[cfg(test)]
    pub(crate) fn env_type_entry_count(&self, node: NodeId) -> usize {
        self.type_env_ofs.get(&(node.0, 0)).map_or(0, Vec::len)
    }

    /// Counts of stored `local_reach` entries by whether they saw a
    /// re-entered fold — `#[cfg(test)]` diagnostics for the rigor-rs#439
    /// audit regressions.
    #[cfg(test)]
    pub(crate) fn folding_entry_counts(&self) -> (usize, usize) {
        let mut with = 0;
        let mut without = 0;
        for es in self.entries.values() {
            for e in es {
                if e.folding.is_empty() {
                    without += 1;
                } else {
                    with += 1;
                }
            }
        }
        (with, without)
    }

    /// Entries stored for one `local_reach` key — `#[cfg(test)]` for the
    /// rigor-rs#439 pin that an audited hit's recompute does not push a
    /// duplicate onto the per-key LRU.
    #[cfg(test)]
    pub(crate) fn reach_entry_count(
        &self,
        use_span: Span,
        skip_class_guards: bool,
        seen_len: usize,
    ) -> usize {
        self.entries
            .get(&(use_span, skip_class_guards, seen_len))
            .map_or(0, Vec::len)
    }

    /// Start over for `ast` and `dead` unless the entries already belong to
    /// them.
    pub(crate) fn reset_unless_valid(&mut self, ast: (usize, usize), dead: &Option<Rc<DeadPositions>>) {
        if !self.valid_for(ast, dead) {
            self.ast = ast;
            self.dead = dead.clone();
            self.entries.clear();
            self.expr_scalars.clear();
            self.expr_reaches.clear();
            self.type_ofs.clear();
            self.type_env_ofs.clear();
            self.truthys.clear();
            self.kill_scans.clear();
            self.kill_cells = 0;
            self.kill_overflowed = false;
        }
    }

    pub(crate) fn intern(&mut self, key: &str) -> u32 {
        if let Some(&id) = self.keys.get(key) {
            return id;
        }
        let id = self.keys.len() as u32;
        self.keys.insert(key.to_string(), id);
        id
    }

    /// The interned ids of the keys on `stack` (a key never interned was
    /// never tested, so it cannot matter to any entry).
    pub(crate) fn stack_ids(&self, stack: &[String]) -> Vec<u32> {
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
    pub(crate) fn record(&mut self, stack: usize, tested: &[u32], folding: &[TruthyKey]) {
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
pub(crate) fn same_dead(a: &Option<Rc<DeadPositions>>, b: &Option<Rc<DeadPositions>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}

/// `RIGOR_REACH_MEMO_AUDIT` is set (to anything but empty or `0`). The env
/// read is cached in a tri-state (`0` unread, `1` off, `2` on) so a test can
/// force it via [`set_audit_mode_for_test`] — an env var set from inside the
/// test binary would race the first read.
pub(crate) fn audit_mode() -> bool {
    match AUDIT.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var("RIGOR_REACH_MEMO_AUDIT")
                .is_ok_and(|v| !v.is_empty() && v != "0");
            AUDIT.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        v => v == 2,
    }
}

/// `audit_mode`'s tri-state — see its docs.
static AUDIT: AtomicU8 = AtomicU8::new(0);

/// Pin the audit flag for the rest of the test binary's run. The flag is
/// process-global: other tests running in parallel get audited too — sound
/// (the audit only asserts an invariant), just slower.
#[cfg(test)]
pub(crate) fn set_audit_mode_for_test(on: bool) {
    AUDIT.store(if on { 2 } else { 1 }, Ordering::Relaxed);
}

pub(crate) fn ast_identity(ast: &LoweredAst) -> (usize, usize) {
    (ast.uid() as usize, ast.len())
}

/// The address that identifies a `seen` stack. The `Vec` itself, not its
/// buffer: a push may move the buffer, never the borrowed `Vec`.
#[allow(clippy::ptr_arg)]
pub(crate) fn stack_id(seen: &Vec<String>) -> usize {
    seen as *const Vec<String> as usize
}

impl<'i> Typer<'i> {
    /// `seen.contains(key)`, recorded as a dependency of the `local_reach`
    /// computation in progress on this stack. Every `seen` membership test
    /// on the reach path goes through here.
    #[allow(clippy::ptr_arg)] // the `Vec`'s address identifies the stack
    pub(crate) fn seen_contains(&self, seen: &Vec<String>, key: &str) -> bool {
        let stack = stack_id(seen);
        // A `kill_prefix` extension in progress takes the test as a
        // dependency of the segment it is folding.
        let rec_active = self.reach_memo.borrow().kill_rec.borrow().last().is_some();
        if rec_active {
            let id = self.reach_memo.borrow_mut().intern(key);
            if let Some(rec) = self
                .reach_memo
                .borrow()
                .kill_rec
                .borrow_mut()
                .last_mut()
            {
                rec.tested.push(id);
            }
        }
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
        {
            let memo = self.reach_memo.borrow();
            let mut recs = memo.kill_rec.borrow_mut();
            if let Some(rec) = recs.last_mut() {
                if !rec.folding.contains(&pred) {
                    rec.folding.push(pred);
                }
            }
        }
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
    pub(crate) fn current_dead(&self, ast: &LoweredAst) -> Option<Rc<DeadPositions>> {
        let key = ast.uid() as usize;
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
                    let landed = self.truthy_landed.borrow();
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
                // The recompute must NOT store: a duplicate entry at the
                // front of the per-key LRU would halve its effective cap
                // (rigor-rs#439) and shadow the very entry being audited.
                let again = self.local_reach_framed(
                    ast,
                    root,
                    use_span,
                    seen,
                    skip_class_guards,
                    dead,
                    false,
                );
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
        self.local_reach_framed(ast, root, use_span, seen, skip_class_guards, dead, true)
    }

    /// Compute under a fresh dependency frame, store the answer unless the
    /// dead set changed while it ran (or `store` is `false` — the audit's
    /// recompute, which must leave the LRU untouched), and hand its
    /// dependencies to the enclosing computation.
    fn local_reach_framed(
        &self,
        ast: &LoweredAst,
        root: &str,
        use_span: Span,
        seen: &mut Vec<String>,
        skip_class_guards: bool,
        dead: Option<Rc<DeadPositions>>,
        store: bool,
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
        if store && same_dead(&dead, &self.current_dead(ast)) {
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

    /// The memoizing front of the per-expression walks [`Typer::expr_scalar`]
    /// and [`Typer::expr_reach`] (rigor-rs#424). Those are called once per
    /// call-site argument (`arg_reach`), and each level of a deep chain
    /// re-walks its whole remaining suffix — `2 ** 2 ** …` cost cubic time
    /// before the per-node memo. The entries live in the SAME `(ast, dead)`
    /// window as the `local_reach` ones and carry the same dependency
    /// record: a computation that tested `seen` is reused only on a stack
    /// answering those tests identically, one that saw a re-entered fold
    /// only while that fold still answers `None`, and a computation whose
    /// dead set moved under it is never stored.
    ///
    /// `map` selects which of the two node-keyed tables the entry lives in;
    /// `compute` is the uncached body.
    fn expr_memo<T>(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        seen: &mut Vec<String>,
        map: impl Fn(&mut ReachMemo) -> &mut ExprTable<T>,
        compute: impl FnOnce(&Self, &LoweredAst, NodeId, &mut Vec<String>) -> T,
        store_if: impl Fn(&T) -> bool,
    ) -> T
    where
        T: Clone + PartialEq + std::fmt::Debug,
    {
        let ast_id = ast_identity(ast);
        let dead = self.current_dead(ast);
        let key = (id.0, seen.len());
        let hit = {
            let mut memo = self.reach_memo.borrow_mut();
            if memo.ast != ast_id {
                // Same interner housekeeping as `local_reach`'s front.
                let mut frames = self.reach_frames.borrow_mut();
                if frames.frames.is_empty() {
                    memo.keys.clear();
                    frames.marks.clear();
                }
            }
            memo.reset_unless_valid(ast_id, &dead);
            let stack = memo.stack_ids(seen);
            match map(&mut memo).get_mut(&key) {
                Some(entries) => expr_entry_hit(
                    entries,
                    &stack,
                    &self.truthy_in_flight.borrow(),
                    &self.truthy_landed.borrow(),
                ),
                None => None,
            }
        };
        if let Some((out, tested, folding)) = hit {
            // The hit's dependencies are the enclosing computation's too.
            self.reach_frames.borrow_mut().record(stack_id(seen), &tested, &folding);
            if audit_mode() {
                let again = compute(self, ast, id, seen);
                assert!(
                    again == out,
                    "rigor-rs#422 reach memo audit: expr_memo({id:?}, seen={seen:?}) memo \
                     {out:?} != recomputed {again:?} in {:?}",
                    ast.file_key(),
                );
            }
            return out;
        }
        let base = seen.len();
        let stack = stack_id(seen);
        self.reach_frames.borrow_mut().push_reach(stack);
        let out = compute(self, ast, id, seen);
        let (mut tested, folding) = {
            let mut frames = self.reach_frames.borrow_mut();
            let Some(MemoFrame::Reach { tested, folding, .. }) = frames.frames.pop() else {
                unreachable!("reach memo frames out of balance");
            };
            frames.record(stack, &tested, &folding);
            (tested, folding)
        };
        if store_if(&out)
            && same_dead(&dead, &self.current_dead(ast))
        {
            let mut memo = self.reach_memo.borrow_mut();
            if memo.valid_for(ast_id, &dead) {
                tested.sort_unstable();
                let mut present: Vec<u32> = memo
                    .stack_ids(&seen[..base])
                    .into_iter()
                    .filter(|id| tested.binary_search(id).is_ok())
                    .collect();
                present.sort_unstable();
                let entries = map(&mut memo).entry(key).or_default();
                entries.truncate(MAX_ENTRIES_PER_KEY - 1);
                entries.insert(
                    0,
                    ExprEntry {
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

    /// [`Typer::expr_memo`] for `expr_truthiness`'s NON-EMPTY-`seen` folds
    /// (rigor-rs#437) — same validity and dependency discipline as
    /// `expr_scalar`. The empty-`seen` canonical path is memoized flat on
    /// `Typer::truthy_memo` instead (it is context-free).
    pub(crate) fn expr_truthy_memo(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        seen: &mut Vec<String>,
    ) -> Option<bool> {
        self.expr_memo(
            ast,
            pred,
            seen,
            |m| &mut m.truthys,
            |s, a, p, seen| s.expr_truthiness_fold(a, p, seen),
            |_| true,
        )
    }

    /// [`Typer::expr_memo`] for `expr_scalar` — see its docs for the
    /// validity and dependency discipline (rigor-rs#424).
    pub(crate) fn expr_scalar_memo(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        seen: &mut Vec<String>,
        compute: impl FnOnce(&Self, &LoweredAst, NodeId, &mut Vec<String>) -> Option<Scalar>,
    ) -> Option<Scalar> {
        self.expr_memo(ast, id, seen, |m| &mut m.expr_scalars, compute, |_| true)
    }

    /// [`Typer::expr_memo`] for `expr_reach` — see its docs for the
    /// validity and dependency discipline (rigor-rs#424).
    pub(crate) fn expr_reach_memo(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        seen: &mut Vec<String>,
        compute: impl FnOnce(&Self, &LoweredAst, NodeId, &mut Vec<String>) -> Reach,
    ) -> Reach {
        self.expr_memo(ast, id, seen, |m| &mut m.expr_reaches, compute, |_| true)
    }

    /// [`Typer::expr_memo`] for `type_of` on an env-free `Node::Call`
    /// subtree (rigor-rs#424). `compute` returns the `Type` the call
    /// interned and whether it embeds NO interner ids (resolved through the
    /// interner that produced it — `Dynamic[top]` is free, `Dynamic` over a
    /// later id is not). Only free answers are stored, so a hit can
    /// re-intern the `Type` under the caller's interner.
    pub(crate) fn type_of_memo(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        compute: impl FnOnce(&Self) -> (Type, bool),
    ) -> Type {
        self.expr_memo(
            ast,
            id,
            &mut Vec::new(),
            |m| &mut m.type_ofs,
            |s, _a, _i, _seen| compute(s),
            |(_, free)| *free,
        )
        .0
    }

    /// [`Typer::expr_memo`] for `type_of` on an env-SENSITIVE `Node::Call`
    /// subtree (rigor-rs#658): the same `(ast, dead)` window, key shape and
    /// dependency record as [`Typer::type_of_memo`], but each entry also
    /// carries the env it was computed under — fingerprint + verbatim
    /// snapshot — plus `def_scope` and the interner identity, which
    /// [`EnvExprEntry::matches`] checks exactly before the stack
    /// dependency answer is trusted.
    ///
    /// `type_of` routes a call here only when its subtree reads env and
    /// `env` is small (the snapshot+compare is O(env), so a bound keeps a
    /// large method env from paying it per call site); every other env-
    /// sensitive call types uncached exactly as before.
    /// `interner_id` is [`rigor_types::Interner::uid`] — the caller passes
    /// it rather than a borrow so the `compute` closure keeps its `&mut`
    /// (the env snapshot's `TypeId`s are only meaningful under it).
    ///
    /// Known key gap (rigor-rs#699's family): `Typer::loop_pass` — the
    /// fixpoint replay that changes how `if` decisions re-type — is NOT
    /// part of the entry key, like it is not part of the flat memo's. No
    /// probe has reached a divergence through it (the loop-pass shapes all
    /// decline before `type_of` matters; the 9k-file memo audit is clean),
    /// but storing interner-bound answers widens the set an entry may
    /// answer, so a future loop-pass reachability change must either fold
    /// the flag into this key or invalidate the window when it flips.
    ///
    /// Unlike the flat memo, ANY carrier may be stored, including one that
    /// embeds `TypeId`s: a hit requires the very interner that produced the
    /// ids (uid match), so they are re-interned — a dedup hit — under the
    /// same arena rather than translated across interners (rigor-rs#427:
    /// without this a block chain whose every answer is a deep `Tuple`
    /// (`[1].map { |y| [[…[y]…]] }.map { … }`) never stores — and with a
    /// block call typing its receiver twice, every driver visit then
    /// DOUBLED the remaining suffix's work: exponential in the chain,
    /// 2^k − 1 `type_block_call` events for the k-th link).
    pub(crate) fn type_of_env_memo(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &crate::TypeEnv,
        interner_id: u64,
        compute: impl FnOnce(&Self) -> TypeOfOut,
    ) -> Type {
        let ast_id = ast_identity(ast);
        let dead = self.current_dead(ast);
        let env_fp = env_fingerprint(env);
        let def_scope = self.def_scope.get();
        let key = (id.0, 0usize);
        let hit = {
            let mut memo = self.reach_memo.borrow_mut();
            if memo.ast != ast_id {
                let mut frames = self.reach_frames.borrow_mut();
                if frames.frames.is_empty() {
                    memo.keys.clear();
                    frames.marks.clear();
                }
            }
            memo.reset_unless_valid(ast_id, &dead);
            let stack = memo.stack_ids(&[]);
            match memo.type_env_ofs.get_mut(&key) {
                Some(entries) => env_expr_entry_hit(
                    entries,
                    &stack,
                    &self.truthy_in_flight.borrow(),
                    &self.truthy_landed.borrow(),
                    env_fp,
                    env,
                    def_scope,
                    interner_id,
                ),
                None => None,
            }
        };
        let seen: Vec<String> = Vec::new();
        if let Some((out, tested, folding)) = hit {
            self.reach_frames.borrow_mut().record(stack_id(&seen), &tested, &folding);
            if audit_mode() {
                let again = compute(self);
                assert!(
                    again == out,
                    "rigor-rs#658 reach memo audit: type_of_env_memo({id:?}) memo {out:?} != \
                     recomputed {again:?} in {:?}",
                    ast.file_key(),
                );
            }
            return out.0;
        }
        let stack = stack_id(&seen);
        self.reach_frames.borrow_mut().push_reach(stack);
        let out = compute(self);
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
            if memo.valid_for(ast_id, &dead) {
                tested.sort_unstable();
                let mut present: Vec<u32> = memo
                    .stack_ids(&seen[..])
                    .into_iter()
                    .filter(|id| tested.binary_search(id).is_ok())
                    .collect();
                present.sort_unstable();
                let entries = memo.type_env_ofs.entry(key).or_default();
                entries.truncate(MAX_ENTRIES_PER_KEY - 1);
                entries.insert(
                    0,
                    EnvExprEntry {
                        tested_bits: tested.iter().fold(0, |bits, &id| bits | id_bit(id)),
                        tested: tested.into(),
                        present: present.into(),
                        folding: folding.into(),
                        env_fp,
                        env: Rc::new(
                            env.iter()
                                .map(|(n, t)| (n.clone(), *t))
                                .collect(),
                        ),
                        def_scope,
                        interner: interner_id,
                        out: out.clone(),
                    },
                );
            }
        }
        out.0
    }
}
