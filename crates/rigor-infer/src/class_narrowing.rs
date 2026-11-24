//! Class narrowing: the `is_a?` / `case`-`when` guard pass
//! ([`Typer::class_narrowing_pass`]) that produces [`ClassNarrowing`] — the
//! per-call-node narrowed-class and disjoint-guard (`dead`) snapshots the rules
//! layer consumes — with its fact/guard types, the guard-predicate analysis,
//! and the branch-join and kill helpers the pass threads its env through.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rigor_index::ClassOrdering;
use rigor_parse::{JumpKind, LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Scalar, Type, TypeId};

use crate::{
    collect_flow_writes, indexed_flow_writes, multi_target_binder,
    widen_flow_writes, TypeEnv, Typer,
};
use crate::flow_writes::{is_shape_mutator, writes_within};

/// One local's class fact inside the narrowing flow pass
/// ([`Typer::class_narrowing_pass`]). `Narrowed` requires a `Dynamic`/`Top`
/// carrier; `Bot` and `Widened` a precise one.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ClassFact {
    /// The local was narrowed FROM `Dynamic`/`Top` TO `Nominal[C]` — the
    /// reference's `narrow_class_other` (`narrowing.rb:2425`). A call on the
    /// local witnesses `call.undefined-method` against `C`.
    Narrowed(String),
    /// The guard class is DISJOINT from (or, under `instance_of?`, unequal to)
    /// the local's carrier class, so the reference's narrowing yields `Bot`
    /// (`narrow_nominal_to_class` / `narrow_shape_to_class` /
    /// `narrow_constant_to_class`). Dispatch through `Bot` witnesses nothing:
    /// every call on the local is SUPPRESSED, for every rule.
    Bot,
    /// The guard proved membership in a class the environment cannot ORDER
    /// against a NOMINAL carrier, so the reference's `narrow_nominal_to_class`
    /// answers `untyped` — upstream #533 item 4 (`70ca7e74`), ported at the
    /// `v0.3.4 → v0.3.8` re-pin. "The guard destroyed the old knowledge": a
    /// `Concurrent::Maybe` guard on an `Array`-bound local licensed
    /// `undefined-method` on the branch the guard had just PROVEN.
    ///
    /// It is deliberately NOT `Bot`, though both suppress every receiver-typed
    /// witness on the edge. `Bot` is the JOIN IDENTITY — `Bot ∪ Array` is
    /// `Array`, so a call AFTER the `if` still fires (measured: row b15b, a
    /// Tuple carrier, where upstream still answers `Bot` and both engines fire).
    /// `untyped` ABSORBS — `Dynamic ∪ Array` is `Dynamic`, so the post-guard
    /// call goes silent too (row b2b). `Bot` also feeds the Bot-on-entry
    /// reasoning in [`join_cenv`]; `Widened` feeds no `Bot`-derived verdict.
    ///
    /// Its rules, each measured against the pin (`ffb456b0`):
    /// * it SUPPRESSES every rule at a call on the local, like `Bot`
    ///   (`ClassNarrowing::dead`) — rows b1/b3/b4/b8/b9/b12;
    /// * it SURVIVES a branch join, and PROPAGATES out of one
    ///   ([`propagate_widened`]) — rows b2b/b26c/b27b;
    /// * a REBIND clears it ([`Facts::kill_local`]) — row b16b, where both
    ///   engines fire again after `h = Array.new`;
    /// * a MUTATION does not ([`kill_cenv_narrowed`]) — row b32b, where
    ///   `h.push(1)` between the guard and the use leaves both engines silent;
    /// * it does NOT escape a BLOCK join — row b17b, where a guard inside
    ///   `[1].each do … end` leaves the post-block call firing on both engines —
    ///   though it does cross INTO one (row b28);
    /// * it STICKS: a later guard on the same local neither re-mints nor
    ///   collapses (rows b21/b34b). Upstream would re-narrow the widened carrier
    ///   to the new guard class (row b20a, where it fires `for String` and we
    ///   stay silent — a recorded coverage gap, never an FP).
    Widened,
}

/// One local's class assertion on ONE edge of a predicate, before any gate is
/// applied — the raw syntactic output of [`Typer::analyse_predicate`], stage
/// 3a-1. The gates (carrier ALLOW-list, `Dynamic`/`Top`, disjoint collapse,
/// review R3) are applied per-local by [`Typer::apply_guards`], which is why
/// this stays a pure syntactic fact.
#[derive(Clone, PartialEq, Eq, Debug)]
struct GuardFact {
    /// The class names the edge asserts the local is one OF. Length 1 for an
    /// atomic guard; longer only after an `||` truthy join, where the reference
    /// narrows to a UNION (`Hash | String`) that this slice cannot represent —
    /// so a multi-class fact never MINTS, and only ever feeds the `Bot`
    /// collapse, which needs every member to collapse (probes `k_bot_or_bot`,
    /// `k_bot_or_same` vs the must-still-fire `k_bot_or_cond`).
    classes: Vec<String>,
    /// `instance_of?` — the reference's `exact:` path, whose collapse condition
    /// is a bare name mismatch rather than a proven-disjoint pair.
    exact: bool,
    /// May this fact mint a [`ClassFact::Narrowed`]? `false` for `===`.
    mintable: bool,
    /// Stage 3a-3: for a [`GuardTarget::Chain`] fact, the arena id of the CHAIN
    /// CALL the predicate guarded (`h.last` in `h.last.is_a?(String)`). The
    /// `narrow_class_other` Dynamic/Top carrier gate is evaluated against THAT
    /// node's type, not against any local's — `h = [1, 2]; h.last.is_a?(String)`
    /// types the address `Integer`, which the reference collapses to `Bot`
    /// (probe `k_root_array_lit`, reference-silent). `None` for a
    /// [`GuardTarget::Local`] fact.
    chain_call: Option<NodeId>,
}

/// What a [`GuardFact`] asserts a class of: a bare local (stages 1-3a-1) or a
/// stable single-hop chain address (stage 3a-3).
///
/// `Chain(root, m)` is the port of the reference's `stable_chain_address`
/// (`narrowing.rb:1826`) restricted to LOCAL roots — an ivar root is declined
/// because the arena's `VariableRead` carries no name (spec row c7b, a recorded
/// coverage gap).
#[derive(Clone, PartialEq, Eq, Debug)]
enum GuardTarget {
    Local(String),
    Chain(String, String),
}

impl GuardTarget {
    /// The LOCAL whose rebind invalidates this target — the name itself for a
    /// local, the ROOT for a chain address.
    fn root(&self) -> &str {
        match self {
            GuardTarget::Local(n) | GuardTarget::Chain(n, _) => n,
        }
    }
}

/// One `when` condition / `in` class pattern, classified the way the
/// reference's `apply_case_when_condition` (`narrowing.rb:3254`) reads it.
/// The class decides BOTH the truthy-edge meet in the clause body AND
/// whether the clause's falsey edge may narrow further — `fully_narrowable`,
/// which an unread condition or an integer range switches off.
#[derive(Debug)]
enum CaseCond {
    /// `C === subject`: a static class/module constant resolved lexically
    /// (`lexical_class_name` — resolved, never declined: the MEET side reads
    /// a project-shadowed or unresolvable name as a class too, and the
    /// ordering demotes it), a `nil`/`true`/`false` literal
    /// (`SINGLETON_WHEN_LITERAL_CLASSES` → `NilClass`/`TrueClass`/
    /// `FalseClass`), or a range literal whose `range_target_class` is
    /// `Numeric`/`String`.
    Class(String),
    /// `k === subject` for an integer literal — value equality, read only on
    /// an integer-rooted carrier (`integer_rooted_type?`). The truthy meet is
    /// `narrow_integer_equal`, the falsey `narrow_integer_not_equal`;
    /// `fully_narrowable: true`. `(i64 value, normalized digit spelling)` —
    /// `None`/`String` for a Bignum.
    IntLit(Option<i64>, String),
    /// `(a..b) === subject` on an integer-rooted carrier —
    /// `integer_range_when_result`'s `fully_narrowable: false` (the
    /// complement is two-piece), so the clause hands the falsey edge the
    /// ENTRY scope back.
    IntRange(IntBound, IntBound),
    /// `/re/ === subject` — `fully_narrowable: true`; the truthy narrows by
    /// `to_str` membership (`regexp_case_equality_type`), the falsey keeps
    /// the accumulator.
    Regexp,
    /// `apply_case_when_condition` answered nil — calls, writes, splats and
    /// the literals no rule reads (`"s"`, `:s`, `1.5`, an interpolated
    /// regexp). A clause with an unread member answers `untyped` for its
    /// body and hands the entry scope to the falsey edge.
    Unread,
}

/// A `case_equality_integer_range` bound — an integer literal's value or the
/// open-ended infinity an absent endpoint supplies.
#[derive(Clone, Debug)]
enum IntBound {
    NegInf,
    /// `(i64 value, normalized signed-decimal spelling)` — `None` + spelling
    /// for a Bignum the `i64` cannot hold.
    Lit(Option<i64>, String),
    PosInf,
}

impl IntBound {
    /// `high -= 1` for a `...` range's literal upper bound
    /// (`case_equality_integer_range`, `narrowing.rb:3307`).
    fn dec(&self) -> IntBound {
        match self {
            IntBound::Lit(Some(v), _) => {
                let v = *v - 1;
                IntBound::Lit(Some(v), v.to_string())
            }
            IntBound::Lit(None, d) => IntBound::Lit(None, dec_int_spelling(d)),
            b => b.clone(),
        }
    }
}

/// A signed-decimal spelling minus one, for Bignum `...` bounds the `i64`
/// cannot hold.
fn dec_int_spelling(d: &str) -> String {
    if let Some(rest) = d.strip_prefix('-') {
        // -N - 1 = -(N + 1)
        let mut digits: Vec<u8> = rest.bytes().collect();
        let mut i = digits.len();
        loop {
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
                if i == 0 {
                    digits.insert(0, b'1');
                    break;
                }
            } else {
                digits[i] += 1;
                break;
            }
        }
        format!("-{}", String::from_utf8(digits).unwrap_or_default())
    } else if d == "0" {
        "-1".to_string()
    } else {
        let mut digits: Vec<u8> = d.bytes().collect();
        let mut i = digits.len();
        loop {
            i -= 1;
            if digits[i] == b'0' {
                digits[i] = b'9';
            } else {
                digits[i] -= 1;
                break;
            }
            if i == 0 {
                break;
            }
        }
        normalize_int_spelling(&String::from_utf8(digits).unwrap_or_default())
    }
}

/// Canonicalize a signed decimal spelling — strip leading zeros past the
/// sign (`"007"` → `"7"`, `"-0"`/`"00"` → `"0"`).
fn normalize_int_spelling(d: &str) -> String {
    let (neg, rest) = match d.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, d),
    };
    let rest = rest.trim_start_matches('0');
    let rest = if rest.is_empty() { "0" } else { rest };
    if neg && rest != "0" {
        format!("-{rest}")
    } else {
        rest.to_string()
    }
}

/// `a.cmp(b)` where `a` may be a Bignum spelling (always outside `i64`, so
/// its sign alone orders it against any `i64`).
fn cmp_int_i64(v: Option<i64>, digits: &str, b: i64) -> std::cmp::Ordering {
    match v {
        Some(a) => a.cmp(&b),
        None => {
            if digits.starts_with('-') {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        }
    }
}

/// `a.cmp(b)` for two integer values in `(i64, spelling)` form — Bignum vs
/// Bignum compares sign, magnitude length, then the digits themselves.
fn cmp_int_spellings(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let an = a.starts_with('-');
    let bn = b.starts_with('-');
    if an != bn {
        return if an { Less } else { Greater };
    }
    let (a, b) = (a.trim_start_matches('-'), b.trim_start_matches('-'));
    let ord = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
    if an { ord.reverse() } else { ord }
}

/// `scalar.cmp(bound)` for an integer-valued [`Scalar`] — a bound is never
/// Equal to an infinity, so only `Lit` compares.
fn cmp_scalar_bound(s: &Scalar, b: &IntBound) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match b {
        IntBound::NegInf => Greater,
        IntBound::PosInf => Less,
        IntBound::Lit(v, d) => match (s, v) {
            (Scalar::Int(a), Some(b)) => a.cmp(b),
            // An `i64` never equals a Bignum; the Bignum's sign orders them.
            (Scalar::Int(_), None) => {
                if d.starts_with('-') { Greater } else { Less }
            }
            (Scalar::BigInt(a), Some(b)) => cmp_int_i64(None, a.as_str(), *b),
            (Scalar::BigInt(a), None) => cmp_int_spellings(a, d),
            _ => Equal, // a non-integer scalar — unreachable by the caller gate
        },
    }
}

/// `lo <= scalar <= hi` for an integer-valued [`Scalar`].
fn int_scalar_in_bounds(s: &Scalar, lo: &IntBound, hi: &IntBound) -> bool {
    cmp_scalar_bound(s, lo) != std::cmp::Ordering::Less
        && cmp_scalar_bound(s, hi) != std::cmp::Ordering::Greater
}

/// Whether an `IntegerRange` carrier `[min, max]` is wholly outside
/// `[lo, hi]` — the `Bot` meet of `intersect_integer_range`.
fn int_ranges_disjoint(
    min: Option<i64>,
    max: Option<i64>,
    lo: &IntBound,
    hi: &IntBound,
) -> bool {
    // `max < lo`: the carrier sits wholly below the range.
    let below = match (max, lo) {
        (Some(m), IntBound::Lit(v, d)) => cmp_int_i64(*v, d, m) == std::cmp::Ordering::Greater,
        (Some(_), IntBound::PosInf) => true,
        _ => false,
    };
    // `min > hi`: the carrier sits wholly above it.
    let above = match (min, hi) {
        (Some(m), IntBound::Lit(v, d)) => cmp_int_i64(*v, d, m) == std::cmp::Ordering::Less,
        (Some(_), IntBound::NegInf) => true,
        _ => false,
    };
    below || above
}

/// Whether an `IntegerRange` `[min, max]` contains the integer literal
/// `(value, digits)`.
fn int_range_covers(
    min: Option<i64>,
    max: Option<i64>,
    value: Option<i64>,
    digits: &str,
) -> bool {
    let after_lo = match min {
        Some(m) => cmp_int_i64(value, digits, m) != std::cmp::Ordering::Less,
        None => true,
    };
    let before_hi = match max {
        Some(m) => cmp_int_i64(value, digits, m) != std::cmp::Ordering::Greater,
        None => true,
    };
    after_lo && before_hi
}

/// `constant.value == k` for a `when k` integer literal —
/// `narrow_integer_equal_constant` / `narrow_integer_not_equal`.
fn int_scalar_eq(s: &Scalar, value: Option<i64>, digits: &str) -> bool {
    match (s, value) {
        (Scalar::Int(v), Some(k)) => *v == k,
        (Scalar::BigInt(d), _) => cmp_int_spellings(d, digits) == std::cmp::Ordering::Equal,
        _ => false,
    }
}

/// One edge's guard facts in SOURCE ORDER. A `Vec` rather than a map because
/// [`Typer::apply_guards`] must apply a same-target collision sequentially to
/// reproduce the reference's nested scopes (see its doc comment).
type GuardMap = Vec<(GuardTarget, GuardFact)>;

/// A stable single-hop chain address — `(root local name, method name)`, the
/// key of the stage-3a-3 `chain_env`.
type ChainAddr = (String, String);

/// The class-narrowing fact environment threaded through the flow pass: the
/// per-LOCAL facts stages 1-3a-1 established, plus the stage-3a-3 per-CHAIN
/// facts keyed by [`ChainAddr`].
///
/// The two families are deliberately separate maps rather than one keyed union:
/// the invalidation rules differ (any mention of the ROOT kills every chain
/// rooted at it, while a local fact dies only on a write/mutation of that
/// local), and the MINT gate reads a different carrier (the chain call's own
/// type, not the local's `tenv` entry).
///
/// Both families carry the same [`ClassFact`] value. A chain `Bot` was
/// introduced by the 2026-08-09 chain-guard-meet slice: the reference's
/// sequential meet collapses a re-guarded chain address exactly as it collapses
/// a local, and "absent" could not express it — a THIRD guard re-minted against
/// the empty env and witnessed where the reference stays silent (probe
/// `chain_third`). `Bot` is a sentinel that survives every later guard and
/// suppresses the recorded use.
#[derive(Clone, Default, Debug)]
struct Facts {
    /// Per-local facts. Was the bare `cenv: HashMap<String, ClassFact>` before
    /// stage 3a-3; every existing rule reads and writes exactly this field.
    locals: HashMap<String, ClassFact>,
    /// Stage 3a-3: `(root, method) -> class fact`.
    chains: HashMap<ChainAddr, ClassFact>,
}

impl Facts {
    /// Invalidate everything a REBIND of `name` invalidates: the local's own
    /// fact and EVERY chain address rooted at it (the reference drops a chain
    /// narrowing inside `Scope#with_local`, `narrowing.rb:1800` — probe `c7g`,
    /// where the reference fires a DIFFERENT diagnostic off the rebound value
    /// and we must stay silent).
    fn kill_local(&mut self, name: &str) {
        self.locals.remove(name);
        self.chains.retain(|(root, _), _| root != name);
    }

    /// Invalidate every chain address rooted at `name`, leaving the local fact
    /// alone — the port of `invalidate_chain_after_call`
    /// (`indexed_narrowing.rb:151`), widened to any MENTION of the root (spec
    /// rows c7c/f23: the reference keeps the fact through an argument-position
    /// mention and we decline, a pure coverage loss).
    fn kill_chains_rooted_at(&mut self, name: &str) {
        self.chains.retain(|(root, _), _| root != name);
    }
}

/// The output of [`Typer::class_narrowing_pass`] — the two per-call-node
/// snapshot sets the rules layer consumes.
#[derive(Default, Debug)]
pub struct ClassNarrowing {
    /// `call node id -> narrowed class name C` for a bare-local receiver the
    /// pass narrowed from `Dynamic`/`Top`. Read by `check_narrowed_call`.
    pub calls: HashMap<NodeId, String>,
    /// Call node ids whose bare-local receiver is `Bot` under a disjoint guard.
    /// The rules layer emits NOTHING at these sites — the reference cannot,
    /// because `Bot` has no dispatch surface.
    pub dead: HashSet<NodeId>,
}

impl<'i> Typer<'i> {
    // -----------------------------------------------------------------------
    // `is_a?` / `case-when` class narrowing (census mechanism 1, spec
    // docs/notes/20260807-class-narrowing-slice-spec.md) + the disjoint-guard
    // suppression (docs/notes/20260808-disjoint-guard-suppression.md)
    // -----------------------------------------------------------------------

    /// Compute the per-call-node class-narrowing snapshot map: `call node id ->
    /// narrowed class name C` for every bare-local receiver whose local was
    /// narrowed from `Dynamic`/`Top` to `Nominal[C]` by an `is_a?`/`kind_of?`/
    /// `instance_of?` guard (`if`/`elsif`/`unless`/ternary — mirror of the
    /// reference's `narrow_class_other`, `narrowing.rb:2425`) or a single-
    /// static-constant `case`/`when` clause (`case_when_scopes`,
    /// `narrowing.rb:374`). The rules layer's `check_narrowed_call` fires
    /// `call.undefined-method` from this map — and ONLY that rule (spec pitfall
    /// 7: wiring more rules over a narrowed receiver is out of slice).
    ///
    /// ## FP-safety envelope (every decline load-bearing)
    ///
    /// Strict subset of the reference's preconditions, so every recorded use is
    /// one the reference also narrows:
    /// - **Predicate shape**: an atomic guard is `local.is_a?(C)` (or
    ///   `kind_of?`/`instance_of?`/`C === local`) — bare `LocalVariableRead`
    ///   operand, no safe-nav, no block, exactly one `ConstantRead` argument.
    ///   Since stage 3a-1 those compose through `&&`, `||` and `!`
    ///   ([`Typer::analyse_predicate`]); chains and ivar receivers still
    ///   decline (ADR-0038 "unmodeled ⇒ decline"). A `Logical` in a VALUE
    ///   position still MINTS nothing (stage 3a-2); since stage 3b-1 it no
    ///   longer clears facts either — its operands are descended so an OUTER
    ///   fact's uses are recorded, and only a rebind in its span kills.
    /// - **Lexical constant resolution**: `C` resolves at the predicate's
    ///   lexical prefix; a project declaration shadowing `C`
    ///   ([`SourceIndex::constant_shadowed`]) declines entirely — we never
    ///   narrow to a project nominal in this slice.
    /// - **Dynamic/Top carriers only** (`narrow_class_other`): the local's type
    ///   in the threaded env must be `Dynamic`/`Top` (or unbound ⇒ untyped).
    ///   `Nominal`/union/scalar carriers are untouched.
    /// - **Per-edge guard maps** (stage 3a-1 reworded this invariant): an
    ///   atomic guard still narrows the TRUTHY edge only — the then-branch for
    ///   `if`/ternary, the else-branch for `unless`. The falsey edge is no
    ///   longer categorically unnarrowed: `!`, and the `&&`/`||` edge algebra
    ///   above it, can SWAP a fact onto it (`if !v.is_a?(C) … else USE end`
    ///   fires on the reference). A `case`/`when` clause is UNCHANGED — it
    ///   narrows only under its own single-constant condition (multi-condition
    ///   unions decline; no falsey threading between clauses).
    /// - **Early-return propagation** (`eval_if:486`/`:495`), stage 3a-1 runs
    ///   it in BOTH directions: when exactly one branch terminates (final
    ///   statement `return`/`raise` — a conservative approximation), the
    ///   OPPOSITE edge's guard map applies to the statements after the
    ///   conditional; per local it is declined if ANY write to that local lands
    ///   inside the conditional's span, and it is skipped entirely when BOTH
    ///   branches terminate (the code after is unreachable and the reference
    ///   emits nothing there).
    /// - **Invalidation**: any write to the local (`LocalVariableWrite`/
    ///   `OpWrite`/`MultiWrite` target), a [`MUTATOR_METHODS`] receiver call,
    ///   or a mutated-argument position (the [`collect_flow_writes`]/
    ///   [`indexed_flow_writes`] span machinery) kills the fact; any unmodeled
    ///   statement clears ALL facts (decline backstop).
    /// - **Unmodeled statement forms** (stage 3b-1,
    ///   docs/notes/20260807-narrowing-stage3-spec.md): the arms enumerated in
    ///   [`Typer::class_flow_stmt`] DESCEND instead of declining — inert leaf
    ///   statements, ivar/gvar/cvar/constant writes, `begin`/`rescue` bodies, a
    ///   loop PREDICATE and statement-position `&&`/`||` — and the literal
    ///   containers in [`Typer::class_flow_expr`] likewise. Every one of them
    ///   records uses under facts that already exist and MINTS NOTHING. A
    ///   `while`/`until`/`for` BODY and survival past a `begin`/loop/`case`
    ///   stay declined.
    /// - **Block bodies**: facts do NOT enter a `block_body` (fresh fact env,
    ///   ADR-0038 §3) — the archetype's `value.deep_transform_keys! { … }`
    ///   receiver sits OUTSIDE the block and is recorded before descent; after
    ///   a block descent all outer facts are cleared (a capture may invisibly
    ///   reassign a local).
    /// - **Position gate** (docs/notes/20260807-block-narrowing-position-rule
    ///   .md): a block body and a `case`/`when` clause narrow ONLY from
    ///   statement position or an assignment RHS. Consumed as a call receiver,
    ///   as an argument, or as a `return` operand they narrow nothing — the
    ///   reference types those positions with `ExpressionTyper`, which threads
    ///   no scope. `if`/ternary is the documented exception: its branches
    ///   narrow in every position, and only the early-return propagation PAST
    ///   it is statement-only (review R2, above).
    ///
    /// ## The DISJOINT-guard suppression ([`ClassNarrowing::dead`])
    ///
    /// The same walk carries a SECOND, opposite-direction fact
    /// (docs/notes/20260808-disjoint-guard-suppression.md). Where the narrowing
    /// map covers "our carrier is COARSER than the reference's", this covers
    /// "our carrier is PRECISE and the reference's is `Bot`": the reference's
    /// `narrow_nominal_to_class` / `narrow_shape_to_class` /
    /// `narrow_constant_to_class` (`narrowing.rb:2381,2404,2364`) collapse a
    /// guarded local to `Bot` when the guard class is DISJOINT from the
    /// carrier's class — dispatch through `Bot` then witnesses nothing, so the
    /// reference is silent on every call whose receiver is that local, for
    /// EVERY rule (measured: `undefined-method`, `wrong-arity`,
    /// `argument-type-mismatch`). rigor-rs never narrowed a precise carrier, so
    /// `check_call` kept firing on the pre-guard type — a live FP.
    ///
    /// The suppression is bounded to what our side can PROVE, because here
    /// silence is the fix and an over-broad rule loses real diagnostics:
    /// - the carrier must map to a class name through
    ///   [`CoreIndex::class_name_of`] — the SAME function the undefined-method
    ///   rule dispatches on, so the class we suppress against is exactly the
    ///   class we would have witnessed against;
    /// - `is_a?`/`kind_of?`/`===` reach `Bot` only on
    ///   [`ClassOrdering::Disjoint`], i.e. both names resolve in the core index
    ///   AND both ancestor chains are complete. `Unknown` (an unresolvable or
    ///   project class, a truncated chain) reaches [`ClassFact::Widened`]
    ///   instead — a THIRD fact the `v0.3.4 -> v0.3.8` re-pin introduced
    ///   (upstream #533 item 4, `70ca7e74`), which suppresses like `Bot` but
    ///   ABSORBS at a join instead of being its identity. Before the re-pin the
    ///   `Unknown` arm suppressed NOTHING on a Nominal carrier, and the probe
    ///   corpus refuted every cheap proxy for the missing hierarchy fact
    ///   (`h = *spec` and `h = []; h << 1` witnessed on the reference under an
    ///   unknown guard class while `h = [1, 2]` did not) — upstream answered it
    ///   by widening ALL of them, and the divergence that survives is only which
    ///   CARRIER KIND is involved: a shaped carrier still collapses to `Bot`;
    /// - `instance_of?` suppresses on any NAME MISMATCH — the reference's
    ///   `exact:` path returns `Bot` unconditionally once the names differ
    ///   (`narrowing.rb:2384`, `subclass_of?:2440`), so no hierarchy fact is
    ///   needed;
    /// - a `case`/`when` clause suppresses only when EVERY condition is a
    ///   static constant that collapses (the reference unions the per-condition
    ///   narrowings, so `when Hash, Array` on an Array keeps the carrier);
    /// - once `Bot`, the local stays `Bot` on BOTH edges of any further guard
    ///   and past a nested conditional's join, and is killed only by a rebind
    ///   — the same invalidation the narrowing fact gets. `Widened` sticks the
    ///   same way, but a mutation does not kill it either, and it PROPAGATES out
    ///   of a branch join ([`propagate_widened`]) where `Bot` only survives one.
    ///
    /// [`CoreIndex::class_name_of`]: rigor_index::CoreIndex::class_name_of
    /// [`MUTATOR_METHODS`]: crate::MUTATOR_METHODS
    /// [`SourceIndex::constant_shadowed`]: crate::SourceIndex::constant_shadowed
    pub fn class_narrowing_pass(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> ClassNarrowing {
        let mut out = ClassNarrowing::default();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return out,
        };
        let mut writes = collect_flow_writes(ast);
        writes.extend(indexed_flow_writes(ast, self.source));
        // Every consumer below keeps its own in-`span` containment check —
        // the table's ORDER is not semantic — so sorting by start lets each
        // call slice it to `writes_within` instead of scanning O(file
        // writes) once per `if`/`case`/call (rigor-rs#424).
        writes.sort_by_key(|(w, _)| w.0);
        let mut tenv = TypeEnv::new();
        let mut cenv = Facts::default();
        let coarse = coarse_locals(ast, &body);
        self.class_flow_scope(
            ast, &body, &mut tenv, &mut cenv, &coarse, &writes, interner, &mut out, true,
        );
        out
    }

    /// The narrowed-call half of [`Typer::class_narrowing_pass`], for callers
    /// that only need `call node id -> narrowed class`.
    pub fn class_narrowing_snapshots(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> HashMap<NodeId, String> {
        self.class_narrowing_pass(ast, interner).calls
    }

    /// Thread `(tenv, cenv)` through a scope's statements in source order.
    /// `stmt_position` is the POSITION the whole statement list sits in (see
    /// [`Typer::class_flow_expr`]): a method/program body and the branch bodies
    /// of a statement-position conditional are statement position; the clause
    /// bodies of an expression-position `case`/ternary inherit `false`.
    #[allow(clippy::too_many_arguments)]
    fn class_flow_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        for &s in stmts {
            self.class_flow_stmt(ast, s, tenv, cenv, coarse, writes, interner, out, stmt_position);
        }
    }

    /// Apply one statement's effect on `(tenv, cenv)` and record narrowed uses.
    ///
    /// A statement lowered under a CROSSED block/lambda (a recovered child
    /// carrying [`LoweredAst::closure_bound_names`] — `super { |o| … }`) is
    /// processed on scratch envs with the closure's bound names dropped and
    /// their facts killed: inside the closure those names read the parameter
    /// (`Dynamic[top]`), never the shadowed outer binding/fact, and the
    /// closure's effects do not reach the enclosing scope (rigor-rs#137).
    #[allow(clippy::too_many_arguments)]
    fn class_flow_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        let bound = ast.closure_bound_names(id);
        if !bound.is_empty() {
            let mut t = tenv.clone();
            let mut c = cenv.clone();
            for name in bound {
                t.remove(name.as_str());
                c.kill_local(name.as_str());
            }
            return self.class_flow_stmt_inner(
                ast,
                id,
                &mut t,
                &mut c,
                coarse,
                writes,
                interner,
                out,
                stmt_position,
            );
        }
        self.class_flow_stmt_inner(
            ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position,
        )
    }

    /// The per-node half of [`Typer::class_flow_stmt`].
    #[allow(clippy::too_many_arguments)]
    fn class_flow_stmt_inner(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        match ast.get(id) {
            // An inert carrier (`defined?` operand, `END` / `BEGIN` body,
            // `super(…)` / `yield(…)` arguments) leaves the reference's scope
            // unchanged, so a write in it neither binds nor kills here
            // (rigor-rs#153). Its USES are still recorded under the facts in
            // force, as before: `yield v.use` / `super(v.use)` under a guard
            // fire on the reference (stage 3b-1 rows d23/g2). A recovery
            // carrier is still descended as a sequence — its reads and calls
            // are this pass's load-bearing `cache[v] ||= v.use` coverage (see
            // the leaf arm).
            Node::Statements { body, kind: StatementsKind::Inert, .. } => {
                for s in body.clone() {
                    match ast.get(s) {
                        Node::LocalVariableWrite { value, .. }
                        | Node::LocalVariableOpWrite { value, .. }
                        | Node::MultiWrite { value, .. } => {
                            let value = *value;
                            self.class_flow_expr(
                                ast, value, tenv, cenv, coarse, writes, interner, out, stmt_position,
                            );
                        }
                        _ => self.class_flow_stmt(
                            ast, s, tenv, cenv, coarse, writes, interner, out, stmt_position,
                        ),
                    }
                }
            }
            Node::Statements { body, .. } => {
                let body = body.clone();
                self.class_flow_scope(ast, &body, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            // An assignment RHS keeps the statement's own position (oracle
            // probes s8/p1 for `=`, x3 for a multi-write, x4 for an op-write:
            // the reference narrows a block/`case` on the RHS of all three).
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // Record uses in the RHS BEFORE rebinding — the RHS reads the
                // pre-write fact (`value = value.frobnicate if value.is_a?(…)`).
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, stmt_position);
                let vty = self.type_of(ast, value, tenv, interner);
                tenv.insert(Rc::from(name.as_str()), vty);
                // Rebinding invalidates the narrowing (probe a4; `scope.rb:194`).
                cenv.kill_local(&name);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, stmt_position);
                let rhs = self.type_of(ast, value, tenv, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    cenv.kill_local(&name);
                    tenv.insert(Rc::from(name.as_str()), ty);
                }
                // An `h[k]` index target MUTATES `h` through `[]=` on the
                // post-binding scope (rigor-rs#134): it kills a `Narrowed` /
                // chain fact like a `h.mut!(…)` call (`kill_cenv_narrowed`),
                // not a rebind's full `kill_local`.
                for (_, tspan, _) in targets.index_writes() {
                    widen_flow_writes(writes_within(writes, tspan).iter(), tspan, tenv, interner);
                    kill_cenv_narrowed(writes_within(writes, tspan), tspan, cenv);
                }
            }
            Node::LocalVariableOpWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, stmt_position);
                cenv.kill_local(&name);
                let u = interner.untyped();
                tenv.insert(Rc::from(name.as_str()), u);
            }
            Node::Call { .. } | Node::IndexWrite { .. } | Node::AttrWrite { .. } => {
                self.class_flow_expr(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            // A `return E` evaluates its values in the current facts (`return
            // value.frobnicate if …` must witness). No fact effect: statements
            // after a `return` in the same list are unreachable, and the
            // reference's evaluator threads the same scope past them.
            //
            // The operand is EXPRESSION position: the reference is silent on a
            // `case`/block narrowed under `return` (probes s12, p7), so a
            // returned value may READ an outer fact but never establishes one
            // of its own.
            Node::Return { values, .. } => {
                let values = values.clone();
                for v in values {
                    self.class_flow_expr(ast, v, tenv, cenv, coarse, writes, interner, out, false);
                }
            }
            Node::If { .. } => {
                self.class_flow_if(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            Node::Case { .. } => {
                self.class_flow_case(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                // Independent scope: fresh envs (INCLUDING a freshly computed
                // coarse-carrier set — local NAMES do not cross a `def`/`class`
                // boundary), no effect on the enclosing one.
                let body = body.clone();
                let mut t = TypeEnv::new();
                let mut c = Facts::default();
                let inner = coarse_locals(ast, &body);
                self.class_flow_scope(
                    ast, &body, &mut t, &mut c, &inner, writes, interner, out, true,
                );
            }
            // ---- stage 3b-1 (docs/notes/20260807-narrowing-stage3-spec.md) ----
            // Every arm below either DESCENDS to record uses under facts the
            // stage-1/2 machinery already established, or is a provable no-op.
            // NONE of them mints a fact.
            //
            // An INERT LEAF statement — a bare read or a literal. It binds
            // nothing, calls nothing, and (being a leaf) can contain no write,
            // so both halves of the `other` arm's decline are no-ops on it.
            // Routing it to `other` is what killed the whole recovered-carrier
            // op-assign family: `cache[v] ||= v.use` has no owned variant and
            // lowers through `collect_recoverable_children` into a `Statements`
            // carrier whose children are the bare reads `cache`, `v` and only
            // THEN the call, so the facts died before the call was reached
            // (probes d1/d2/d10a/d10b/d11/e3/e9). The literal/`ConstantRead`
            // members are load-bearing for `BeginRescue`, whose flat `body`
            // interleaves the protected statements with each clause's lowered
            // exception `ConstantRead`s (probe f7).
            //
            // Stage 3a-3 splits the bare LOCAL read out: it is still inert for
            // the per-local facts, but it MENTIONS a name that may be a chain
            // root, and this slice's invalidation kills a chain on any mention
            // (see the `class_flow_expr` arm for why that is a strict superset
            // of the reference's rule).
            Node::LocalVariableRead { name, .. } => {
                let name = name.clone();
                cenv.kill_chains_rooted_at(&name);
            }
            Node::ConstantRead { .. }
            | Node::VariableRead { .. }
            | Node::SelfExpr { .. }
            | Node::StringLit { .. }
            | Node::IntegerLit { .. }
            | Node::FloatLit { .. }
            | Node::SymbolLit { .. }
            | Node::NilLit { .. }
            | Node::TrueLit { .. }
            | Node::FalseLit { .. } => {}
            // `@x = E` / `$gx = E` / `@@cx = E` / `X = E`. None of these can
            // rebind a LOCAL, so every fact SURVIVES the statement (probes
            // e1/e6/e7/e8) — the half of the old `other` treatment that was
            // pure loss. A write nested in `E` still kills by span, exactly as
            // `other`'s `widen_flow_writes` + a `kill_cenv_writes` do.
            //
            // DESCENDING `E` to record the use (spec rows d4-d7) is DECLINED.
            // The spec's build measured it as the one arm that surfaces a
            // PRE-EXISTING carrier-fidelity gap: `narrow_class_other` narrows
            // Dynamic/Top carriers only, and rigor-rs types a `Node::Logical`
            // (and any project-method return that ends in one) as
            // `Dynamic[top]` where the reference produces a UNION — so the
            // reference's gate declines and ours does not. Two live FPs over
            // the standing sweep sat on this arm (gitlab-foss
            // `lib/ci/inputs/base_input.rb:30` via `spec_hash = spec || {}`,
            // `lib/gitlab/encrypted_configuration.rb:70` via a `deserialize`
            // whose body ends in `… || {}`); master emits the same FPs for a
            // bare-statement use, so the gap is orthogonal to this slice and
            // must be closed on the carrier side before d4-d7 can ship.
            Node::InstanceVariableWrite { span, .. }
            | Node::VariableWrite { span, .. }
            | Node::ConstantWrite { span, .. } => {
                let span = *span;
                widen_flow_writes(writes_within(writes, span).iter(), span, tenv, interner);
                kill_cenv_writes(writes_within(writes, span), span, cenv);
            }
            // `begin`/`rescue`/`else`/`ensure`. The flat `body` holds the
            // protected statements, each clause's exception constants and
            // clause body, the `else` body and the `ensure` body in source
            // order (`ast.rs:1516`), so ONE descent covers d19/f7/f8.
            //
            // A `rescue => e` capture REBINDS `e` with no `LocalVariableWrite`
            // node, so it is invisible to `collect_flow_writes`: kill the bound
            // names explicitly BEFORE descending (probe `rescuebind` — the
            // reference narrows the bound name to the exception class and says
            // `for StandardError`; keeping the stale fact would emit
            // `for String`, a live FP). Killing before the protected body costs
            // the coverage of a use that precedes the clause — a subset.
            //
            // Recording under exception paths is safe: facts are only KILLED
            // inside, never minted, and a runtime path that skips a rebind only
            // makes a recorded fact more true. Survival PAST the `begin` is
            // declined (widen + clear, exactly as `other` did) even though the
            // reference does keep it (probe post1) — unprobed at spec time and
            // a strict subset.
            Node::BeginRescue { body, clauses, span, .. } => {
                let (body, span) = (body.clone(), *span);
                let bound: Vec<String> =
                    clauses.iter().filter_map(|c| c.bound_name.clone()).collect();
                for name in &bound {
                    cenv.kill_local(name);
                }
                self.class_flow_scope(ast, &body, tenv, cenv, coarse, writes, interner, out, stmt_position);
                widen_flow_writes(writes_within(writes, span).iter(), span, tenv, interner);
                // The body was DESCENDED into `cenv` itself, so a rebind inside
                // already removed the fact: no edge evidence is needed and no
                // span kill either (probes `bot_in_begin`, `bot_after_begin`).
                join_cenv(cenv, &[]);
            }
            // `while`/`until`/`for`. The predicate (for `for`, the COLLECTION)
            // is evaluated ONCE before the body, in the enclosing scope — the
            // same EXPRESSION position an `if` predicate gets. The reference
            // fires on a narrowed use in a `while`/`until` predicate and in a
            // `for` collection, even when the `for` index rebinds that very
            // local (probes g1/g1b/g1c/g1d — the collection is evaluated before
            // the rebind).
            //
            // The BODY is DECLINED. `Node::Loop` did not distinguish `for`,
            // whose index rebinds the local and where the reference is measured
            // SILENT (probe f10a: `for v in list` then `v.use`), from
            // `while`/`until`, where it fires (f10b/d21). Descending would be a
            // live FP. The index names are now carried (`Node::Loop::index`,
            // rigor-rs#151) and reach this arm only through `writes`, so the
            // span kill below drops a fact the index rebinds; descending the
            // body is still a separate slice.
            Node::Loop { predicate, span, .. } => {
                let (predicate, span) = (*predicate, *span);
                if let Some(p) = predicate {
                    self.class_flow_expr(ast, p, tenv, cenv, coarse, writes, interner, out, false);
                }
                widen_flow_writes(writes_within(writes, span).iter(), span, tenv, interner);
                // The BODY is not descended, so a rebind inside it is invisible
                // to the edge evidence: keep the entry `Bot` but kill by span
                // (probe `bot_after_while`).
                join_cenv(cenv, &[]);
                kill_cenv_writes(writes_within(writes, span), span, cenv);
            }
            // A statement-position `&&`/`||`. Descend both operands to record
            // uses of an OUTER fact — valid on both edges, and the reference
            // records them (probes f1/f2). Establishing a fact FROM the
            // operands is stage 3a-2 and is NOT done here. The operands are
            // EXPRESSION position. Afterwards: `widen_flow_writes` keeps the
            // `tenv` effect byte-identical to the `other` arm this replaces,
            // and `kill_cenv_writes` drops the fact of any local the
            // conditionally-executed right operand may rebind (a fact with no
            // write in the span survives — probe f4b, where the reference also
            // keeps it).
            Node::Logical { left, right, span, .. } => {
                let (left, right, span) = (*left, *right, *span);
                self.class_flow_expr(ast, left, tenv, cenv, coarse, writes, interner, out, false);
                self.class_flow_expr(ast, right, tenv, cenv, coarse, writes, interner, out, false);
                widen_flow_writes(writes_within(writes, span).iter(), span, tenv, interner);
                kill_cenv_narrowed(writes_within(writes, span), span, cenv);
            }
            // Any other statement (`case`/`in`/lambda/range/…) is UNMODELED:
            // widen `tenv` for the locals it writes and CLEAR ALL facts
            // (decline backstop). No descent.
            other => {
                let span = other.span();
                widen_flow_writes(writes_within(writes, span).iter(), span, tenv, interner);
                // No descent, so no edge evidence: the entry `Bot` rides through
                // (nothing inside can widen it) and a rebind in the span kills.
                join_cenv(cenv, &[]);
                kill_cenv_writes(writes_within(writes, span), span, cenv);
            }
        }
    }

    /// Evaluate an expression for narrowed-local USES: record `call -> C` for a
    /// bare-local receiver in `cenv`, descend a block body with a FRESH fact
    /// env, and kill facts a call's contained writes/mutations invalidate.
    ///
    /// `stmt_position` carries the POSITION rule (docs/notes/20260807-block-
    /// narrowing-position-rule.md): a block body and a `case`/`when` clause
    /// narrow only when the construct sits in statement position or on an
    /// assignment RHS. Reaching an expression as a call RECEIVER, as an
    /// ARGUMENT, or as a `return` OPERAND drops it to `false`, and no
    /// narrowing is established anywhere beneath — the reference's
    /// `ExpressionTyper` threads no scope through those positions.
    #[allow(clippy::too_many_arguments)]
    fn class_flow_expr(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        match ast.get(id) {
            Node::Call { receiver, method: _, args, block_body, safe_nav, span, .. } => {
                let receiver = *receiver;
                let args = args.clone();
                let block_body = block_body.clone();
                let safe_nav = *safe_nav;
                let call_span = *span;
                // ---- stage 3a-3: chain-address bookkeeping ------------------
                // Is THIS call the pure address read of a LIVE chain fact
                // (`h.last` while `(h, "last")` is narrowed)? Two things hang
                // off the answer, and both are load-bearing:
                //
                //  * the read must NOT invalidate its own fact. The reference
                //    agrees — `invalidate_chain_after_call` runs at the
                //    STATEMENT `eval_call` and `h.last.frobnicate_zzz`'s outer
                //    receiver is a `CallNode`, not a stable root, so nothing is
                //    dropped and a second read narrows too (probe `f11`, the
                //    reference fires twice);
                //  * the root read beneath it must not trip the
                //    root-MENTION kill below.
                let live_address = stable_chain_address(ast, id)
                    .filter(|addr| cenv.chains.contains_key(addr));
                // Recurse the receiver first (a nested use like `a.b` in `a.b.c`).
                // A receiver is EXPRESSION position (probes s5-s7, s11, p3).
                // Skipped for a live address read: its receiver is the bare root
                // local, whose descent would kill the very fact being read.
                if let Some(r) = receiver {
                    if live_address.is_none() {
                        self.class_flow_expr(
                            ast, r, tenv, cenv, coarse, writes, interner, out, false,
                        );
                    }
                }
                // INVALIDATION, the strict superset of
                // `invalidate_chain_after_call` this slice specced: a call whose
                // receiver READS the root, other than the pure address read
                // above, drops every chain rooted there (probe `c7d`, `h.pop`,
                // reference-silent). The reference's own rule is narrower — it
                // fires only for a statement-position call — so ours declines
                // some rows it keeps; killing is always a subset.
                //
                // A call whose receiver is the ADDRESS (`h.last.strip`,
                // `h.last << y`) is NOT a root-receiver call and does not
                // invalidate, on either engine (probes `n_call_on_address`,
                // `n_address_receiver_call`, both reference=1).
                if live_address.is_none() {
                    if let Some(r) = receiver {
                        if let Node::LocalVariableRead { name, .. } = ast.get(r) {
                            cenv.kill_chains_rooted_at(name);
                        }
                    }
                }
                // Record the CHAIN use: `<addr>.<m>(…)` where `<addr>` carries a
                // live fact. The outer call's own arguments and block are
                // irrelevant — the reference narrows the RECEIVER expression
                // (`method_chain_narrowing_for` gates the node it types, which
                // is the address, not its caller), so `h.last.zzz(1)` and
                // `h.last.zzz { }` both fire (probes `m_use_with_args`,
                // `m_use_with_block`). Safe-nav on the outer call declines, as
                // everywhere in this slice.
                //
                // A `Bot` chain fact records into `out.dead` instead, exactly
                // as a `Bot` LOCAL fact does below: the reference's meet
                // collapsed the address, so it has no dispatch surface and the
                // rules layer must emit nothing there. Safe-nav does not gate
                // the `Bot` half (same reasoning as the local twin: the
                // safe-nav decline is about which SHAPES we narrow, not about
                // dispatch through `Bot`).
                if let Some(r) = receiver {
                    if let Some(addr) = stable_chain_address(ast, r) {
                        match cenv.chains.get(&addr) {
                            // `Widened` suppresses exactly as `Bot` does — the
                            // reference's carrier is `untyped`, which has no
                            // dispatch surface to witness on either.
                            Some(ClassFact::Bot | ClassFact::Widened) => {
                                out.dead.insert(id);
                            }
                            Some(ClassFact::Narrowed(c)) if !safe_nav => {
                                out.calls.insert(id, c.clone());
                            }
                            _ => {}
                        }
                    }
                }
                // Record the use: a plain (not safe-nav) call on a bare local
                // currently narrowed to `C`. Recorded BEFORE any invalidation
                // below — the receiver read happens before the call's effects
                // (`value.deep_transform_keys! { … }` must witness).
                //
                // The `Bot` fact is recorded on the SAME node key but under
                // safe-nav too: `narrow_*_to_class` collapsed the receiver, and
                // a safe-nav dispatch through `Bot` witnesses just as little as
                // a plain one (the safe-nav decline on `Narrowed` is about
                // which SHAPES the reference narrows, not about dispatch).
                if let Some(r) = receiver {
                    if let Node::LocalVariableRead { name, .. } = ast.get(r) {
                        match cenv.locals.get(name) {
                            // `Widened` suppresses like `Bot`, safe-nav
                            // included: the guard erased the carrier, so there
                            // is nothing to witness against (rows b1/b3/b4/
                            // b8/b9/b12, all reference-silent).
                            Some(ClassFact::Bot | ClassFact::Widened) => {
                                out.dead.insert(id);
                            }
                            Some(ClassFact::Narrowed(c)) if !safe_nav => {
                                out.calls.insert(id, c.clone());
                            }
                            _ => {}
                        }
                    }
                }
                // An argument is EXPRESSION position (probes s9, s13, p2).
                for a in &args {
                    self.class_flow_expr(ast, *a, tenv, cenv, coarse, writes, interner, out, false);
                }
                if !block_body.is_empty() {
                    // Block-scope discipline (ADR-0038 §3): descend with a FRESH
                    // fact env + inherited (cloned) `tenv`; afterwards clear ALL
                    // outer facts (a capture may invisibly reassign a local) and
                    // widen `tenv` for locals the block visibly writes.
                    //
                    // POSITION GATE (docs/notes/20260807-block-narrowing-
                    // position-rule.md): descend ONLY from statement position.
                    // The reference fires on a guard narrowed inside a block
                    // whose call is a statement or an assignment RHS (probes
                    // s1-s4, s8, s10, x3-x5 — safe-nav included, which is why
                    // PR #63's `if !safe_nav` decline was the wrong axis) and
                    // is SILENT once the call's value is consumed as a receiver
                    // (s5-s7, s11), as an argument (s9, s13) or as a `return`
                    // operand (s12). Skipping the descent drops every narrowed
                    // recording inside the block — a strict subset of the
                    // reference, never an FP; the conservative clear/widen
                    // effects below still apply either way.
                    //
                    // The `Bot` facts DO cross into the block: they are not a
                    // narrowing claim about a Dynamic carrier but the statement
                    // that the reference's guarded scope bound the local to
                    // `Bot`, and that scope is exactly what the block body runs
                    // under (probes `bot_into_block`, `bot_into_block_doend`).
                    // A rebind inside the block drops it from `bcenv`, so the
                    // join below drops it from the outer env too
                    // (`bot_block_rebind`, where the reference fires).
                    //
                    // A `Widened` fact crosses in for the same reason and is
                    // measured doing so: row b28 (`return unless
                    // h.is_a?(UnknownZzzClass)` then `[1].each { h.use }`) is
                    // reference-SILENT. It does NOT come back OUT, though — the
                    // block-call join below carries no `propagate_widened`, and
                    // row b17b (a guard INSIDE the block, then a use after it)
                    // fires on both engines.
                    //
                    // Stage 3a-3: CHAIN facts do NOT cross into a block. The
                    // reference does carry them (probe `n_into_block` fires),
                    // but a chain address is invalidated by a call on its root
                    // and a block body can invisibly reach the root through a
                    // capture — the same reason `Narrowed` locals stay out.
                    // A recorded coverage gap, never an FP.
                    let mut block_edge: Option<Facts> = None;
                    if stmt_position {
                        let mut btenv = tenv.clone();
                        let mut bcenv = Facts {
                            locals: cenv
                                .locals
                                .iter()
                                .filter(|(_, f)| {
                                    matches!(f, ClassFact::Bot | ClassFact::Widened)
                                })
                                .map(|(k, f)| (k.clone(), f.clone()))
                                .collect(),
                            chains: HashMap::new(),
                        };
                        self.class_flow_scope(
                            ast, &block_body, &mut btenv, &mut bcenv, coarse, writes, interner, out, true,
                        );
                        block_edge = Some(bcenv);
                    }
                    match block_edge {
                        Some(edge) => join_cenv(cenv, std::slice::from_ref(&edge)),
                        // Not descended (expression position): no edge evidence,
                        // so keep the entry `Bot` and kill by span instead.
                        None => {
                            join_cenv(cenv, &[]);
                            kill_cenv_writes(writes_within(writes, call_span), call_span, cenv);
                        }
                    }
                    widen_flow_writes(writes_within(writes, call_span).iter(), call_span, tenv, interner);
                }
                // Invalidation: kill the fact of every local with a recorded
                // write/mutation span INSIDE this call — covers a
                // `MUTATOR_METHODS` receiver (`value.merge!(…)`), a mutated
                // positional argument (`fill(value)`), and a write nested in an
                // argument (`f(value = x)`). A REBIND in the arguments was
                // already threaded by the expression-position write arms, so
                // what is left here is MUTATION, which cannot revive a `Bot`
                // (probe `bot_mutator_use`).
                kill_cenv_narrowed(writes_within(writes, call_span), call_span, cenv);
            }
            Node::Logical { left, right, span, .. } => {
                // `&&`/`||`. Stage 3b-1: the up-front `cenv.clear()` is GONE.
                // Recording a use is POSITION-INDEPENDENT — the reference fires
                // on a use of an outer fact inside a logical operand in
                // statement, argument and assignment-RHS position alike (probes
                // f1-f4). Establishing a fact from the operands stays out of
                // slice (3a-2), so nothing new is minted here; a
                // conditionally-executed rebind inside the operands is killed
                // by span, exactly as the `Call` arm does.
                let (left, right, span) = (*left, *right, *span);
                self.class_flow_expr(ast, left, tenv, cenv, coarse, writes, interner, out, false);
                self.class_flow_expr(ast, right, tenv, cenv, coarse, writes, interner, out, false);
                kill_cenv_narrowed(writes_within(writes, span), span, cenv);
            }
            // A write in EXPRESSION position (`f(value = x, value.frobnicate)`)
            // must thread its rebind IMMEDIATELY: Ruby evaluates arguments
            // left-to-right and the reference threads scope through them, so a
            // use AFTER the rebind reads the new binding — the post-call
            // `kill_cenv_writes` alone would leave the stale fact live for the
            // remaining sibling arguments (adversarial-review R1). Mirrors the
            // statement arms.
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, false);
                let vty = self.type_of(ast, value, tenv, interner);
                tenv.insert(Rc::from(name.as_str()), vty);
                cenv.kill_local(&name);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, false);
                let rhs = self.type_of(ast, value, tenv, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    cenv.kill_local(&name);
                    tenv.insert(Rc::from(name.as_str()), ty);
                }
                // An `h[k]` index target MUTATES `h` through `[]=` on the
                // post-binding scope (rigor-rs#134): kill only the
                // `Narrowed` / chain facts a mutation drops.
                for (_, tspan, _) in targets.index_writes() {
                    widen_flow_writes(writes_within(writes, tspan).iter(), tspan, tenv, interner);
                    kill_cenv_narrowed(writes_within(writes, tspan), tspan, cenv);
                }
            }
            Node::LocalVariableOpWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, false);
                cenv.kill_local(&name);
                let u = interner.untyped();
                tenv.insert(Rc::from(name.as_str()), u);
            }
            // A ternary is an expression-position `Node::If` (Prism parses it as
            // an IfNode); `propagate_if_branches` (`scope_indexer.rb:2742`)
            // gives it the same treatment as a statement `if` — EXCEPT the
            // early-return propagation, which is a STATEMENT-evaluator behavior
            // (`eval_if:481`) and unprobed for expression position (R2), so it
            // is gated off here.
            Node::If { .. } => {
                self.class_flow_if(ast, id, tenv, cenv, coarse, writes, interner, out, false);
            }
            // A `case` in an ASSIGNMENT RHS narrows its clause bodies like a
            // statement `case` (probe p1) — hence `stmt_position` rather than a
            // hard `false`; reached as a receiver/argument/`return` operand the
            // flag is already `false` and the clauses narrow nothing (p2, p3,
            // p7). Unlike `if`/ternary, which narrows its branches in EVERY
            // position (p4, p8) and so is left alone.
            Node::Case { .. } => {
                self.class_flow_case(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            // ---- stage 3b-1: pure-descent expression containers ------------
            // Recording a narrowed use is position-INDEPENDENT (probe f3), so
            // descending a literal container closes d14-d17 wherever it sits.
            // The elements are EXPRESSION position: a container is not a
            // statement list, so a BLOCK inside one is not descended (probe
            // blk4 — `x = [[1].map { … }]` — is a recorded coverage gap, not an
            // FP). Nothing is minted and nothing is cleared: an element cannot
            // rebind a local except through the expression-position write arms
            // above, which thread it immediately.
            Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
                let elements = elements.clone();
                for e in elements {
                    self.class_flow_expr(ast, e, tenv, cenv, coarse, writes, interner, out, false);
                }
            }
            Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
                let parts = parts.clone();
                for p in parts {
                    self.class_flow_expr(ast, p, tenv, cenv, coarse, writes, interner, out, false);
                }
            }
            // A `Statements` carrier (the `collect_recoverable_children`
            // recovery for `x = *use`, `x = (use rescue nil)`, …) or a
            // `begin`/`rescue` in expression position (`x = begin USE rescue
            // end`) is a STATEMENT LIST: hand it to the statement walker, which
            // carries the same position the assignment RHS has (probes
            // d25/g6/d20). The `BeginRescue` arm's bound-name kill and its
            // post-clear apply here too.
            Node::Statements { .. } | Node::BeginRescue { .. } => {
                self.class_flow_stmt(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            // `h[k] ||= v` / `h[k] &&= v` / `h[k] op= v` — a compound index
            // write. Its operands evaluate in EXPRESSION position and record
            // uses under the facts in force — the stage 3b-1
            // `cache[v] ||= v.use` shape (row d1), which the old recovered
            // `Statements` carrier descended and the owned variant must not
            // lose. The `[]=` store kills a narrowed fact on the receiver
            // exactly as the `[]=` `Call` arm does — `kill_cenv_narrowed` by
            // span: mutation, never a rebind.
            Node::IndexWrite {
                receiver,
                indices,
                value,
                span,
                ..
            } => {
                let (receiver, indices, value, wspan) =
                    (*receiver, indices.clone(), *value, *span);
                if let Some(r) = receiver {
                    self.class_flow_expr(
                        ast, r, tenv, cenv, coarse, writes, interner, out, false,
                    );
                }
                for i in &indices {
                    self.class_flow_expr(
                        ast, *i, tenv, cenv, coarse, writes, interner, out, false,
                    );
                }
                self.class_flow_expr(
                    ast, value, tenv, cenv, coarse, writes, interner, out, false,
                );
                kill_cenv_narrowed(writes_within(writes, wspan), wspan, cenv);
            }
            // `h.attr ||= v` / `h.attr &&= v` / `h.attr op= v` — a compound
            // ATTRIBUTE write. Receiver and RHS are expression-position
            // children. The WRITER (`attr=`) — when it names a shape mutator
            // and the write is `evaluated` — rebinds the receiver through
            // `widen_attribute_write` (`statement_evaluator.rb`), which drops
            // a `Narrowed` fact and every chain rooted at `h` exactly like a
            // mutator `Call` does (`widen_receiver_aliases` → `with_local`,
            // rigor-rs#343). A non-evaluated occurrence (typed operand,
            // deferred closure body, suppressed carrier) leaves the facts
            // alone.
            Node::AttrWrite {
                receiver,
                value,
                write_name,
                evaluated,
                span,
                ..
            } => {
                let (receiver, value, writer, evaluated, span) =
                    (*receiver, *value, write_name.clone(), *evaluated, *span);
                if let Some(r) = receiver {
                    self.class_flow_expr(
                        ast, r, tenv, cenv, coarse, writes, interner, out, false,
                    );
                }
                self.class_flow_expr(
                    ast, value, tenv, cenv, coarse, writes, interner, out, false,
                );
                // A lexical `break` argument's scope lands on the
                // locals-only break join — the writer's chain
                // invalidation applies nowhere (rigor-rs#363).
                if evaluated && is_shape_mutator(&writer) && !ast.in_locals_only_carrier(span) {
                    if let Some(Node::LocalVariableRead { name, .. }) =
                        receiver.map(|r| ast.get(r))
                    {
                        cenv.kill_local(name);
                    }
                }
            }
            // Stage 3a-3: a bare read of a chain ROOT anywhere OTHER than
            // beneath a live address read invalidates every chain rooted at it.
            // The reference does NOT — `invalidate_chain_after_call` only
            // matches a call whose RECEIVER is the root, so it keeps the fact
            // through `g(h)` and `other.push(h)` (probes `c7c_arg_mention`,
            // `f23_push`, `n_root_as_arg_to_mutator`, all reference=1). The
            // spec's decline set names this: we kill on ANY root mention,
            // paying coverage for an invalidation rule that is a strict
            // superset of the reference's and therefore cannot be an FP source.
            Node::LocalVariableRead { name, .. } => {
                let name = name.clone();
                cenv.kill_chains_rooted_at(&name);
            }
            _ => {}
        }
    }

    /// Narrow through one `if`/`unless`/ternary node. `stmt_position` is `true`
    /// only when the node is a direct STATEMENT (reached via
    /// [`Typer::class_flow_stmt`]): the early-return propagation is the
    /// statement evaluator's behavior (`eval_if:481`) and is unprobed for an
    /// expression-position conditional (`f(x.is_a?(C) ? x : raise)` —
    /// `propagate_if_branches` types the expression's branches, nothing shows
    /// the falsey edge propagating past it), so an expression-position `if`
    /// narrows its branches but never the statements after it (review R2).
    /// See [`Typer::class_narrowing_snapshots`] for the envelope.
    #[allow(clippy::too_many_arguments)]
    fn class_flow_if(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        let Node::If { predicate, then_body, else_body, is_unless, span } = ast.get(id) else {
            return;
        };
        let (predicate, is_unless, if_span) = (*predicate, *is_unless, *span);
        let then_body = then_body.clone();
        // Prism models an `else` clause as its own node, and the arena lowers an
        // `ElseNode` to a CLAUSE-LESS `BeginRescue` carrier (`ast.rs:1457`), so
        // `else_body` is the single-element `vec![carrier]`. Walking the carrier
        // as an ordinary statement would run the `BeginRescue` arm, whose
        // `join_cenv(cenv, &[])` blanket-wipes the edge's `Narrowed` facts — so
        // EVERY `if` with an `else` lost the incoming fact on its falsey edge,
        // whatever the else contained (probes `D_else_nil`/`E_both_nil`, where
        // the reference fires and master is silent). The carrier is not a real
        // `begin`: `subsequent` is only ever an `elsif` (an `If`, untouched
        // here) or an `else`, so unwrapping it is exact.
        let else_body = match else_body.as_slice() {
            [only] => match ast.get(*only) {
                Node::BeginRescue { body, ensure_body, clauses, .. }
                    if ensure_body.is_empty() && clauses.is_empty() =>
                {
                    body.clone()
                }
                _ => else_body.clone(),
            },
            _ => else_body.clone(),
        };
        // The predicate is evaluated first, in the current facts (its own calls
        // may read an OUTER narrowing) — EXPRESSION position.
        self.class_flow_expr(ast, predicate, tenv, cenv, coarse, writes, interner, out, false);
        // rigor-rs#451 — the `if` join is per-edge for the predicate's SUBJECT
        // locals too, not only its written names: `if h.empty?; end; h.frob`
        // reads `h` as the arms' joined (widened) type on the reference, not
        // `Array`. A local whose edge does not Keep enters its arm as
        // `Dynamic` (`if h.is_a?(Array); else; h.frob`'s
        // `non-empty-array[top]` is a shape the port declines to a widen —
        // coverage loss, never an FP); post-`if`, the join widens the name
        // unless BOTH edges keep it. A DECIDED predicate is the folded `if`
        // the dead-arm machinery owns — `[1].empty?` keeps `h` `[1]`.
        let predicate_subjects: Vec<String> = if self
            .known_polarity(ast, tenv, predicate, interner)
            .is_some()
        {
            Vec::new()
        } else {
            self.subjects_within(ast, predicate)
                .into_iter()
                .filter_map(|(s, _)| match s {
                    crate::loop_edges::Subject::Local(n) => Some(n),
                    _ => None,
                })
                .collect()
        };
        let edge_binds = |tenv: &TypeEnv, edge: bool, interner: &mut Interner| {
            let mut out = Vec::new();
            for name in &predicate_subjects {
                if out.iter().any(|(n, _)| n == name) {
                    continue;
                }
                let ty = tenv
                    .get(name.as_str())
                    .copied()
                    .unwrap_or_else(|| interner.untyped());
                // An untyped carrier is the GUARD map's domain — `rule
                // .is_a?(Hash)` narrows the param to `Hash` there while
                // `subject_edge` only speaks for a resolved carrier.
                if matches!(interner.get(ty), Type::Dynamic(_) | Type::Top) {
                    continue;
                }
                let e = self.subject_edge(
                    ast,
                    predicate,
                    &crate::loop_edges::Subject::Local(name.clone()),
                    ty,
                    edge,
                    false,
                    interner,
                );
                if e != crate::loop_edges::Edge::Keep {
                    out.push((name.clone(), interner.untyped()));
                }
            }
            out
        };
        // The FACT twin: inside an arm, a `Keep` edge RESTORES the entry
        // fact (`[1, 2].is_a?(Array)` is decided — the guard narrows
        // nothing, and `h.zz1` inside still witnesses `for [1, 2]`); a
        // non-Keep edge widens, the join of narrowed edges never being the
        // bare nominal the oracle leaves (`non-empty-array[top]` is the
        // shape the port declines) — rigor-rs#451.
        let edge_facts = |arm: &mut Facts,
                          saved: &HashMap<String, ClassFact>,
                          tenv: &TypeEnv,
                          edge: bool,
                          interner: &mut Interner| {
            for name in &predicate_subjects {
                let ty = tenv
                    .get(name.as_str())
                    .copied()
                    .unwrap_or_else(|| interner.untyped());
                // An untyped carrier is the GUARD map's domain — `rule
                // .is_a?(Hash)` narrows the param to `Hash` there while
                // `subject_edge` only speaks for a resolved carrier.
                if matches!(interner.get(ty), Type::Dynamic(_) | Type::Top) {
                    continue;
                }
                let e = self.subject_edge(
                    ast,
                    predicate,
                    &crate::loop_edges::Subject::Local(name.clone()),
                    ty,
                    edge,
                    false,
                    interner,
                );
                match e {
                    // A DEAD edge carries `Bot` — calls inside the arm are
                    // suppressed, and `propagate_widened` (which reads only
                    // `Widened`) never leaks it past the `if`
                    // (rigor-rs#451).
                    crate::loop_edges::Edge::Empty => {
                        arm.locals.insert(name.clone(), ClassFact::Bot);
                    }
                    crate::loop_edges::Edge::Keep => {
                        match saved.get(name) {
                            Some(fact) => {
                                arm.locals.insert(name.clone(), fact.clone());
                            }
                            None => {
                                arm.locals.remove(name);
                            }
                        }
                    }
                    _ => {
                        arm.locals.insert(name.clone(), ClassFact::Widened);
                    }
                }
            }
        };
        // The subject facts as they stood at `if` entry — `Keep` arms
        // restore from here (`apply_guards` rewrites a guard-named fact).
        // Only `predicate_subjects` names are ever read back, so this is a
        // filtered clone — a full `cenv.locals.clone()` costs O(file locals)
        // per `if` (rigor-rs#424).
        let saved_facts: HashMap<String, ClassFact> = predicate_subjects
            .iter()
            .filter_map(|n| cenv.locals.get(n.as_str()).map(|f| (n.clone(), f.clone())))
            .collect();
        // Stage 3a-1: the predicate yields a guard map for EACH edge (`&&`/`||`
        // /`!` recursion — [`Typer::analyse_predicate`]). A plain class guard
        // still yields facts on the truthy edge only, so the pre-3a-1 behavior
        // is the `(vec![g], vec![])` case of this.
        let (truthy_g, falsey_g) = self.analyse_predicate(ast, predicate, 64).unwrap_or_default();
        // Truthy edge: then-branch for `if`/ternary, else-branch for `unless`.
        let (truthy, falsey) =
            if is_unless { (&else_body, &then_body) } else { (&then_body, &else_body) };
        let (truthy_edge, truthy_t_env) = {
            let mut t = tenv.clone();
            for (name, ty) in edge_binds(tenv, true, interner) {
                t.insert(Rc::from(name.as_str()), ty);
            }
            let mut c = cenv.clone();
            self.apply_guards(&truthy_g, ast, tenv, &mut c, coarse, interner);
            edge_facts(&mut c, &saved_facts, tenv, true, interner);
            // The branch bodies INHERIT the conditional's own position: a block
            // inside an expression-position ternary branch narrows nothing
            // (probe x2), while a statement `if`'s branches stay statement
            // position (probe x5). The branch-internal narrowing itself is
            // never position-gated (p4, p8).
            self.class_flow_scope(ast, truthy, &mut t, &mut c, coarse, writes, interner, out, stmt_position);
            (c, t)
        };
        let falsey_edge_binds = edge_binds(tenv, false, interner);
        let (falsey_edge, falsey_t_env) = {
            // 3a-1: the falsey edge is no longer categorically unnarrowed. It
            // carries facts exactly when the predicate SWAPPED one onto it —
            // `!guard` (probes c4d/c4f), an `||` whose disjuncts' falsey maps
            // concatenate (`x_or_falsey_bang`), or an `&&` whose falsey maps
            // join on the SAME class (`b2_and_bang_same`). A bare
            // `local.is_a?(C)` still contributes NOTHING here (`c1g`).
            let mut t = tenv.clone();
            for (name, ty) in falsey_edge_binds {
                t.insert(Rc::from(name.as_str()), ty);
            }
            let mut c = cenv.clone();
            self.apply_guards(&falsey_g, ast, tenv, &mut c, coarse, interner);
            edge_facts(&mut c, &saved_facts, tenv, false, interner);
            self.class_flow_scope(ast, falsey, &mut t, &mut c, coarse, writes, interner, out, stmt_position);
            (c, t)
        };
        // A terminated arm never reaches the join — read by the write and
        // subject joins below and by `propagate_widened` (`return if
        // h.is_a?(Array)` leaves `h` unreachable on `[1, 2]` — oracle-silent,
        // rigor-rs#451 review).
        let truthy_terminates = !truthy.is_empty() && branch_terminates(ast, truthy);
        let falsey_terminates = !falsey.is_empty() && branch_terminates(ast, falsey);
        // Post-`if` TYPE join for the locals the arms wrote: the reference
        // unions the arm values (`v = nil if p` on `v = nil` keeps `nil`,
        // and `v.zz` fires `for nil` — rigor-rs#443), so each written name
        // joins its truthy-edge and falsey-edge env entries; a terminated
        // arm contributes the ENTRY type (its writes never ran), and a name
        // absent from one env was never bound on that edge (`untyped`, the
        // decline) — never the blanket `widen_flow_writes`, which threw the
        // joined type away. The FACT side stays conservative: a
        // branch-established `Narrowed` fact never survives a merge below.
        {
            let entry = tenv.clone();
            for (wspan, name) in writes_within(writes, if_span) {
                if !(if_span.0 <= wspan.0 && wspan.1 <= if_span.1) {
                    continue;
                }
                let mut absent = |env: &TypeEnv| {
                    env.get(name.as_str()).copied().unwrap_or_else(|| interner.untyped())
                };
                let t_ty = if truthy_terminates {
                    absent(&entry)
                } else {
                    absent(&truthy_t_env)
                };
                let f_ty = if falsey_terminates {
                    absent(&entry)
                } else {
                    absent(&falsey_t_env)
                };
                let j = rigor_types::Algebra::join(interner, t_ty, f_ty);
                tenv.insert(Rc::from(name.as_str()), j);
            }
        }
        // Stage 3a-3: `join_cenv` wipes every chain fact, so snapshot them for
        // the propagation's re-seed below (see there for why).
        // S2: the LOCAL twin of the same snapshot. See the re-seed below.
        let pre_join = cenv.clone();
        let (pre_join_chains, pre_join_locals) = (&pre_join.chains, &pre_join.locals);
        let edges = [truthy_edge, falsey_edge];
        join_cenv(cenv, &edges);
        // Join-retention slice: put back every pre-join fact BOTH edges left
        // untouched. Runs BEFORE the propagation below, so a restored fact is
        // what the carried guard map MEETS against (the propagation re-reads the
        // same snapshot for its own targets — idempotent on top of this). Both
        // branch bodies are always descended here, so the chain edges carry real
        // evidence and the chain half is enabled — gated on the predicate's own
        // local mentions (see `locals_in_span`).
        let predicate_locals = locals_in_span(self, ast, ast.get(predicate).span());
        retain_joined_facts(
            cenv,
            &pre_join,
            &edges,
            writes_within(writes, if_span),
            if_span,
            Some(&predicate_locals),
        );
        // … PLUS the #533 widening, which is the one fact that flows the other
        // way through the join: `untyped ∪ anything` is `untyped`, so a branch
        // that widened a local widens it for everything after the `if` (row
        // b2b). A TERMINATING edge is excluded — the code after the `if` never
        // runs on it (rows b24/b29/b33, all firing on both engines).
        let [truthy_edge, falsey_edge] = &edges;
        propagate_widened(
            cenv,
            &[
                (truthy_edge.clone(), truthy_terminates),
                (falsey_edge.clone(), falsey_terminates),
            ],
            writes_within(writes, if_span),
            if_span,
        );

        // … and the predicate's subject locals join their edge types: unless
        // BOTH edges keep the carrier, the post-`if` env widens it (`if
        // h.empty?; end; h.frob` is reference-silent). A local the arms
        // themselves rewrote keeps the write join's own answer.
        //
        // A terminated arm never reaches the join — for the subject its
        // edge reads `Empty` (`return if h.is_a?(Array)` leaves `h`
        // unreachable on `[1, 2]` — oracle-silent, rigor-rs#451 review).
        for name in &predicate_subjects {
            if writes_within(writes, if_span).iter().any(|(ws, n)| n == name && if_span.0 <= ws.0 && ws.1 <= if_span.1) {
                continue;
            }
            let ty = tenv
                .get(name.as_str())
                .copied()
                .unwrap_or_else(|| interner.untyped());
            if matches!(interner.get(ty), Type::Dynamic(_) | Type::Top) {
                continue;
            }
            let t = self.subject_edge(
                ast,
                predicate,
                &crate::loop_edges::Subject::Local(name.clone()),
                ty,
                true,
                false,
                interner,
            );
            let f = self.subject_edge(
                ast,
                predicate,
                &crate::loop_edges::Subject::Local(name.clone()),
                ty,
                false,
                false,
                interner,
            );
            use crate::loop_edges::Edge;
            let t_live = if truthy_terminates { Edge::Empty } else { t };
            let f_live = if falsey_terminates { Edge::Empty } else { f };
            // `T ∪ Bot = T`: an `Empty` edge — decided-away, or a
            // terminating arm — contributes nothing (`if h.is_a?(Hash)`
            // on `[1, 2]` re-joins the falsey `[1, 2]` alone, firing
            // after); only a live `Other` widens, and `Empty ∪ Empty`
            // makes the subject unreachable.
            match crate::loop_edges::join(t_live, f_live) {
                Edge::Other => {
                    let u = interner.untyped();
                    tenv.insert(Rc::from(name.as_str()), u);
                    cenv.locals.insert(name.clone(), ClassFact::Widened);
                }
                Edge::Keep => {
                    // `Keep ∪ Empty`: the decided edge contributes
                    // nothing — the subject re-joins on its entry type
                    // and its entry FACT (`[1, 2].is_a?(Array)` keeps
                    // `[1, 2]` after).
                    match saved_facts.get(name) {
                        Some(fact) => {
                            cenv.locals.insert(name.clone(), fact.clone());
                        }
                        None => {
                            cenv.locals.remove(name);
                        }
                    }
                }
                Edge::Empty => {
                    cenv.locals.insert(name.clone(), ClassFact::Bot);
                }
            }
        }

        // … EXCEPT the early-return propagation (`eval_if:486`/`:495`), which
        // 3a-1 runs in BOTH directions: a terminating FALSEY branch propagates
        // the truthy map (the `return unless guard` idiom a5/c1d), a
        // terminating TRUTHY branch the falsey map (`return if !guard` —
        // c4a/c4b/f22/t_c1d_or). Only in STATEMENT position (review R2), never
        // for a local rewritten inside the conditional's span, and never when
        // BOTH branches terminate: the statements after are then unreachable
        // and the reference emits nothing there (probe `t_both_terminate` — a
        // measured would-be FP).
        let rewritten = |local: &str| {
            writes_within(writes, if_span).iter().any(|(ws, n)| n == local && if_span.0 <= ws.0 && ws.1 <= if_span.1)
        };
        if stmt_position && truthy_terminates != falsey_terminates {
            let carried = if falsey_terminates { &truthy_g } else { &falsey_g };
            // A CHAIN target is filtered on its ROOT: a rebind of the root
            // inside the conditional's span invalidates the address just as a
            // rebind of the local invalidates a local fact (probe `c7g`, where
            // the reference fires a DIFFERENT diagnostic off the rebound value
            // and we must stay silent).
            let carried: GuardMap =
                carried.iter().filter(|(t, _)| !rewritten(t.root())).cloned().collect();
            // Re-seed the PRE-JOIN chain facts for exactly the addresses this
            // map touches. Without it a sequential disjoint re-guard of one
            // address (`return unless h.last.is_a?(String)` then `return unless
            // h.last.is_a?(Hash)`) would mint `Hash` against an EMPTY env and
            // witness where the reference has already collapsed to `Bot` —
            // probe `d_seq_two_returns_disjoint`, reference-silent. This is the
            // chain analogue of the LOCAL-side defect recorded in the
            // `next`/`break` build note (`s1_two_returns_sequential`), which
            // sits on the same `join_cenv`-before-propagation ordering; the
            // narrow re-seed fixes it for chains without touching that ordering.
            // Only the touched addresses are restored, so an untouched chain
            // fact still dies at the join (probe `n_escape_after_if`).
            //
            // S2 (2026-08-08) closes the LOCAL side of exactly that defect, on
            // the same narrow re-seed. `return unless v.is_a?(File::Stat)` then
            // `return unless v.is_a?(URI::HTTP)` is reference-SILENT (its scope
            // carries `File::Stat` into the second guard, which collapses it to
            // `Bot`), while rigor-rs minted `URI::HTTP` against the wiped env and
            // witnessed — probe r7. The same shape on two CORE names
            // (`Hash` then `String`) was already firing before this slice: the
            // pre-existing FP the `next`/`break` build note recorded as
            // `s1_two_returns_sequential`. Re-seeding lets the sequential-guard
            // MEET in `apply_guards` see the prior fact: a disjoint re-guard
            // reaches `Bot` (silence on both spellings), a subclass re-guard
            // refines instead of dropping (`seq_subclass` fires `for Integer`
            // on the reference). Only the locals this map touches are restored,
            // so an untouched fact still dies at the join.
            for (t, _) in &carried {
                match t {
                    GuardTarget::Chain(root, m) => {
                        let addr: ChainAddr = (root.clone(), m.clone());
                        if let Some(fact) = pre_join_chains.get(&addr) {
                            cenv.chains.insert(addr, fact.clone());
                        }
                    }
                    GuardTarget::Local(name) => {
                        if let Some(fact) = pre_join_locals.get(name) {
                            cenv.locals.insert(name.clone(), fact.clone());
                        }
                    }
                }
            }
            self.apply_guards(&carried, ast, tenv, cenv, coarse, interner);
        }
    }

    /// Apply one edge's guard map to the fact env `c`, IN SOURCE ORDER.
    ///
    /// Sequencing is load-bearing: the reference's `analyse_and` evaluates the
    /// right conjunct's truthy scope UNDER the left's, so `v.is_a?(String) &&
    /// v.is_a?(Hash)` re-narrows an already-`Nominal[String]` carrier and
    /// reaches `Bot` — the reference is SILENT in that branch (probe
    /// `a_same_local_disjoint_then`). Applying the entries one at a time
    /// against the WORKING env reproduces that through the R3 conflict rule
    /// below, where the spec's "b wins a same-local collision" would have been
    /// a live false positive.
    ///
    /// Per local, in order:
    /// 1. **The precise-carrier meet** ([`Typer::guard_meet_precise`]), tested
    ///    first and against `c`, so an earlier conjunct's fact sticks. One
    ///    unorderable member answers `Widened` and widens the whole edge (the
    ///    `v0.3.8` re-pin, upstream #533 item 4); otherwise `Bot` (PR #73) when
    ///    EVERY class in the fact's union collapses the local's carrier.
    /// 2. **`Narrowed`** when the fact is mintable (not `===`), carries exactly
    ///    ONE class (an `||` union is stage 3a-4 — the reference narrows to
    ///    `Hash | String` and we decline), the local passes the carrier
    ///    ALLOW-list (`coarse`, PR #72) and its current type is `Dynamic`/`Top`
    ///    (`narrow_class_other`). Both gates are PER-LOCAL: a compound predicate
    ///    may narrow one local and decline another (probe `a_two_locals_then`).
    /// 3. **Sequential-guard meet** (replacing the old review-R3 blanket drop
    ///    for LOCALS): a guard over an existing `Narrowed` fact narrows the
    ///    FACT's carrier the way the reference's `narrow_nominal_to_class`
    ///    narrows `Nominal[C]` — same class keeps, a subclass guard refines, a
    ///    superclass guard is a no-op, proven-disjoint reaches `Bot`, `exact`
    ///    collapses on name mismatch, an unresolvable ordering KEEPS the
    ///    carrier (`:unknown stays conservative`, `narrowing.rb:2388`), and an
    ///    `||` union meets per member. See the arm's comment for probe rows.
    ///
    /// ## Stage 3a-3 — [`GuardTarget::Chain`] entries
    ///
    /// A chain target follows the SAME steps — Bot short-circuit, R3 conflict,
    /// sequential MEET, mint — with two differences, each measured:
    /// - the `Bot` STEP has no `guard_meet_precise` half. A precise chain carrier
    ///   IS collapsed by the reference (`h = [1, 2]; h.last.is_a?(String)` is
    ///   reference-silent, probe `k_root_array_lit`), but we reproduce that by
    ///   DECLINING the mint — the carrier gate below reads the chain call's own
    ///   type, which is precise in exactly that case. A chain `Bot` therefore
    ///   only ever arises from the MEET, and once it does it sticks: it survives
    ///   every later guard and records the use into `ClassNarrowing::dead`
    ///   rather than `calls`. Before the 2026-08-09 slice `chains` could not
    ///   express it at all (the value type was a bare class name), and the
    ///   "absent" stand-in let a third guard re-mint — probe `chain_third`, a
    ///   live false positive;
    /// - the carrier gate reads `type_of(chain_call)` instead of the local's
    ///   `tenv` entry, and the PR #72 `coarse` ALLOW-list does NOT apply. The
    ///   allow-list exists because the reference's carrier for a `||`-bound
    ///   LOCAL is a union its `narrow_class_other` declines; the carrier here is
    ///   the DISPATCH RESULT off that union, which the reference narrows
    ///   normally — `h = a || b; h.last.is_a?(String)` fires on the reference
    ///   (probe `k_root_or_union`), so applying the allow-list would be pure
    ///   coverage loss with no FP to pay for it.
    ///
    /// A sequential re-guard of the SAME address runs the identical
    /// `narrow_nominal_to_class` meet the LOCAL arm runs — measured row for row
    /// against the oracle over the 2026-08-09 `chain_*` matrix, which found the
    /// chain family to behave EXACTLY like the local one
    /// (`class_narrowing_chain_guard_meet_matrix`). A disjoint pair reaches
    /// `Bot` and stays silent (`chain_disjoint`, and the `||` union
    /// `chain_or_disjoint`, which the pre-slice `classes.len() == 1` mint gate
    /// skipped outright — a live FP); a subclass guard REFINES
    /// (`chain_subclass` fires `for Integer`, where the old blind
    /// keep-if-equal-else-remove dropped it); a superclass guard is a no-op
    /// (`chain_superclass`); the `Unknown` split keeps a project-class carrier
    /// (`chain_projclass`/`chain_projsub`/`chain_projsub_or`) and drops an
    /// RBS-space pair (`chain_r7`).
    ///
    /// What stays declined is RECOGNITION, not the meet: `guard_predicate`
    /// requires a bare local operand, so `===` and `nil?` on a chain receiver
    /// never produce a chain target at all (`chain_caseeq_same`,
    /// `chain_caseeq_subclass`, `chain_nilq` — the reference fires, we stay
    /// silent, pure coverage).
    #[allow(clippy::too_many_arguments)]
    fn apply_guards(
        &self,
        guards: &GuardMap,
        ast: &LoweredAst,
        tenv: &TypeEnv,
        c: &mut Facts,
        coarse: &HashSet<String>,
        interner: &mut Interner,
    ) {
        // The classes each TARGET was already asserted to EARLIER IN THIS MAP.
        // Needed because a NON-mintable assertion (`===`, `nil?`) writes nothing
        // to `c`, yet the reference's carrier at the next conjunct is that class
        // — `String === v && v.is_a?(Hash)` and `v.nil? && v.is_a?(String)` are
        // both measured reference-silent and would otherwise witness.
        let mut asserted: Vec<(&GuardTarget, &[String])> = Vec::new();
        for (target, g) in guards {
            let prior = asserted
                .iter()
                .rev()
                .find(|(t, _)| *t == target)
                .map(|(_, cl)| *cl);
            asserted.push((target, &g.classes));
            let conflicts = prior.is_some_and(|p| p != g.classes.as_slice());
            match target {
                GuardTarget::Local(local) => {
                    // Step 1, three-valued since the re-pin: the guard's meet
                    // against a PRECISE carrier. One unorderable member WIDENS
                    // the whole edge (`untyped | anything = Dynamic`, upstream
                    // #533 item 4); otherwise the historical rule stands and the
                    // fact collapses only when EVERY member collapses.
                    let met: Vec<Option<ClassFact>> = g
                        .classes
                        .iter()
                        .map(|class| {
                            self.guard_meet_precise(local, class, g.exact, tenv, c, interner)
                        })
                        .collect();
                    if met.iter().any(|m| m.as_ref() == Some(&ClassFact::Widened)) {
                        c.locals.insert(local.clone(), ClassFact::Widened);
                        continue;
                    }
                    let collapses = !met.is_empty()
                        && met.iter().all(|m| m.as_ref() == Some(&ClassFact::Bot));
                    if collapses {
                        c.locals.insert(local.clone(), ClassFact::Bot);
                        continue;
                    }
                    if conflicts {
                        c.locals.remove(local);
                        continue;
                    }
                    // Sequential-guard meet: the env already carries a
                    // `Narrowed` fact for this local (an earlier statement's
                    // early-return propagation minted it, re-seeded past the
                    // join by S2), so the reference's carrier here is
                    // `Nominal[carrier]` and this guard narrows THAT, not
                    // `Dynamic` — `narrow_nominal_to_class`
                    // (`narrowing.rb:2381`), probed over the seq_* matrix in
                    // the 2026-08-08 sequential-guards note. The rule table
                    // lives on the helper, which the CHAIN arm shares
                    // unchanged. `Bot` here survives the join to SUPPRESS,
                    // where the S2 drop merely went silent.
                    if let Some(ClassFact::Narrowed(carrier)) = c.locals.get(local).cloned() {
                        let met = self.narrow_nominal_to_class(&carrier, g);
                        match met {
                            Some(f) => {
                                c.locals.insert(local.clone(), f);
                            }
                            None => {
                                c.locals.remove(local);
                            }
                        }
                        continue;
                    }
                    let mintable = g.mintable
                        && g.classes.len() == 1
                        && !coarse.contains(local)
                        && match tenv.get(local.as_str()) {
                            None => true, // unbound ⇒ untyped (Dynamic[top])
                            Some(&ty) => {
                                matches!(interner.get(ty), Type::Dynamic(_) | Type::Top)
                            }
                        };
                    if !mintable {
                        continue;
                    }
                    // No existing fact for the local by here: `Bot` was
                    // consumed by the collapse test, `Narrowed` by the meet.
                    c.locals.insert(local.clone(), ClassFact::Narrowed(g.classes[0].clone()));
                }
                GuardTarget::Chain(root, method) => {
                    let addr: ChainAddr = (root.clone(), method.clone());
                    // A collapsed address stays collapsed: the reference's
                    // scope carries `Bot` into every later guard and
                    // `narrow_nominal_to_class` cannot revive it (probe
                    // `chain_third`, String→Hash→String, reference-SILENT —
                    // a live FP on master, where "absent" let the third guard
                    // re-mint). The LOCAL twin of this short-circuit lives in
                    // `guard_meet_precise`, which answers `Bot` on an incoming
                    // `Bot` fact.
                    // A WIDENED address is equally final: the reference's carrier
                    // there is `untyped`, which no later guard in this slice
                    // re-narrows (the local twin of the same short-circuit lives
                    // in `guard_meet_precise`).
                    if matches!(
                        c.chains.get(&addr),
                        Some(ClassFact::Bot) | Some(ClassFact::Widened)
                    ) {
                        continue;
                    }
                    if conflicts {
                        c.chains.remove(&addr);
                        continue;
                    }
                    // Sequential-guard meet, the chain twin of the LOCAL arm
                    // above and the same `narrow_nominal_to_class`
                    // (`narrowing.rb:2381`): an existing fact means the
                    // reference's carrier here is `Nominal[carrier]`, not
                    // `Dynamic`. Oracle-probed over the 2026-08-09 chain_*
                    // matrix — a subclass guard REFINES (`chain_subclass` fires
                    // `for Integer`), a superclass guard is a no-op
                    // (`chain_superclass` fires `for Integer`), a disjoint pair
                    // or an all-disjoint `||` union reaches `Bot`
                    // (`chain_disjoint`, `chain_or_disjoint` — the latter a
                    // live FP on master, where the `classes.len() == 1` mint
                    // gate skipped the union guard entirely and the stale
                    // `String` fact survived to witness), and the `Unknown`
                    // split keeps a project-class carrier
                    // (`chain_projclass`/`chain_projsub`/`chain_projsub_or` all
                    // fire `for String`) while dropping an RBS-space pair our
                    // resolver cannot order (`chain_r7`).
                    if let Some(ClassFact::Narrowed(carrier)) = c.chains.get(&addr).cloned() {
                        match self.narrow_nominal_to_class(&carrier, g) {
                            Some(f) => {
                                c.chains.insert(addr, f);
                            }
                            None => {
                                c.chains.remove(&addr);
                            }
                        }
                        continue;
                    }
                    // No existing fact by here: `Bot` was consumed by the
                    // short-circuit, `Narrowed` by the meet. The MINT path is
                    // unchanged — `narrow_class_other`'s envelope, read off the
                    // CHAIN CALL.
                    let carrier_ok = g.chain_call.is_some_and(|n| {
                        let ty = self.type_of(ast, n, tenv, interner);
                        matches!(interner.get(ty), Type::Dynamic(_) | Type::Top)
                    });
                    let mintable = g.mintable && g.classes.len() == 1 && carrier_ok;
                    if !mintable {
                        continue;
                    }
                    c.chains.insert(addr, ClassFact::Narrowed(g.classes[0].clone()));
                }
            }
        }
    }

    /// The MEET of an already-`Nominal[carrier]` fact against one guard — the
    /// port of the reference's `narrow_nominal_to_class` (`narrowing.rb:2381`),
    /// shared by the LOCAL and CHAIN arms of [`Typer::apply_guards`].
    ///
    /// `Some(fact)` is the met fact, `None` a DROP (the caller removes the
    /// entry — neither witnessed nor collapsed). Same class keeps the fact
    /// (`seq_same`/`chain_same`, and `===` too — `seq_caseeq_same` fires
    /// `for String`); `instance_of?` collapses on a bare name mismatch BEFORE
    /// the hierarchy (`seq_exact_subclass`/`chain_exact_subclass` are
    /// reference-silent); a proven-disjoint pair reaches `Bot`; a SUBCLASS
    /// guard refines to the more specific class, mintable or not
    /// (`seq_subclass`/`chain_subclass` fire `for Integer`); a SUPERCLASS guard
    /// is a no-op (`seq_superclass`/`chain_superclass` fire `for Integer`, the
    /// carrier); `Unknown` splits on WHY (see the arm); a multi-class fact (an
    /// `||` union) meets per member.
    ///
    /// Extracted verbatim from the LOCAL arm by the 2026-08-09 chain-guard-meet
    /// slice — the chain family measures IDENTICALLY on the oracle across all
    /// 26 `chain_*` rows, so one implementation is the honest encoding.
    fn narrow_nominal_to_class(&self, carrier: &str, g: &GuardFact) -> Option<ClassFact> {
        match g.classes.as_slice() {
            [class] if class.as_str() == carrier => Some(ClassFact::Narrowed(carrier.to_string())),
            [_] if g.exact => Some(ClassFact::Bot),
            [class] => match self.index.class_ordering(carrier, class) {
                ClassOrdering::Equal | ClassOrdering::Subclass => {
                    Some(ClassFact::Narrowed(carrier.to_string()))
                }
                ClassOrdering::Superclass => Some(ClassFact::Narrowed(class.clone())),
                ClassOrdering::Disjoint => Some(ClassFact::Bot),
                // `Unknown` WIDENS (upstream #533 item 4, `70ca7e74`, ported at
                // the `v0.3.4 → v0.3.8` re-pin): `narrow_nominal_to_class`'s
                // `:unknown` arm now answers `untyped` — "the guard proved
                // membership in a class the environment cannot name, which
                // destroys the old knowledge".
                //
                // This replaces a two-way split the pin RETIRED. Until `v0.3.8`
                // the reference's `:unknown` KEPT the bound, so a PROJECT-class
                // guard kept the carrier and only an unorderable RBS-space pair
                // dropped. Re-measured at `ffb456b0`, all six of those rows are
                // now reference-SILENT and were live rigor-rs false positives:
                // `seq_projclass`/`seq_projsub`/`seq_projsub_or` and their
                // `chain_*` twins, each firing `for String` after a
                // `ProjKlass`/`ProjBare` re-guard. `seq_ns_unknown_drop` /
                // `chain_r7` (`File::Stat` then `URI::HTTP`) were already silent
                // through the DROP and stay silent through the widening.
                ClassOrdering::Unknown => Some(ClassFact::Widened),
            },
            classes => {
                // An `||` union meets PER MEMBER and unions the results
                // (`accumulate` over `analyse_or`): a disjoint member
                // contributes `Bot` (nothing), a superclass member the
                // carrier, a subclass member itself. Representable when one
                // class survives: `{Hash, String}` over a String carrier is
                // `Bot ∪ String` and the reference fires `for String` (probes
                // `seq_or_mixed`/`chain_or_mixed`); all-disjoint is `Bot`
                // (`seq_or_disjoint`/`chain_or_disjoint`). Two surviving
                // classes are a real union — drop.
                let mut survivors: Vec<&str> = Vec::new();
                let mut widened = false;
                for class in classes {
                    let met: Option<&str> = if g.exact {
                        (class.as_str() == carrier).then_some(carrier)
                    } else {
                        match self.index.class_ordering(carrier, class) {
                            ClassOrdering::Disjoint => None,
                            ClassOrdering::Superclass => Some(class.as_str()),
                            ClassOrdering::Equal | ClassOrdering::Subclass => Some(carrier),
                            // One unorderable member WIDENS THE WHOLE union: the
                            // reference unions the per-member narrowings and
                            // `untyped | anything` is `Dynamic` (the `Dynamic[T]`
                            // algebra absorbs). Measured: `h.is_a?(UnknownZzz) ||
                            // h.is_a?(Hash)` on an `Array.new` carrier is
                            // reference-silent (row b4) even though the `Hash`
                            // member alone would be `Bot`, and the retired
                            // keep-the-carrier split fired `for String` on
                            // `seq_projsub_or`/`chain_projsub_or`.
                            ClassOrdering::Unknown => {
                                widened = true;
                                None
                            }
                        }
                    };
                    if let Some(m) = met {
                        if !survivors.contains(&m) {
                            survivors.push(m);
                        }
                    }
                }
                match (widened, survivors.as_slice()) {
                    (true, _) => Some(ClassFact::Widened),
                    (false, []) => Some(ClassFact::Bot),
                    (false, [one]) => Some(ClassFact::Narrowed((*one).to_string())),
                    (false, _) => None,
                }
            }
        }
    }

    /// Analyse a whole predicate into its `(truthy, falsey)` guard maps — the
    /// port of the reference's `predicate_scopes` dispatch (`narrowing.rb:344`)
    /// for the three compound forms, stage 3a-1.
    ///
    /// - a class guard on a local → `([g], [])` (`analyse_class_predicate`,
    ///   `:1761`);
    /// - `!x` — a no-arg, no-block, non-safe-nav `Call` named `"!"`, which is
    ///   how prism lowers both `!` and `not` → the operand's pair SWAPPED
    ///   (`dispatch_unary_predicate`, `:1555`: `analyse(receiver)&.reverse`);
    /// - `&&` → truthy CONCATENATES (left then right, applied sequentially),
    ///   falsey JOINS (`analyse_and`, `:2631`);
    /// - `||` → truthy JOINS, falsey concatenates (`analyse_or`, `:2640`);
    /// - anything else → `None`, a whole-predicate decline.
    ///
    /// An UNRECOGNISED operand of `&&`/`||` contributes empty maps rather than
    /// killing the whole analysis — which is exactly the reference's behaviour:
    /// its truthy fallback is the other conjunct's scope (so any one recognised
    /// conjunct narrows, c1a–c1c) while its falsey JOIN against the unchanged
    /// scope yields nothing (c1g, f12, `u_and_or_falsey`). `depth` bounds the
    /// recursion; a predicate deeper than that declines.
    fn analyse_predicate(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        depth: u32,
    ) -> Option<(GuardMap, GuardMap)> {
        if depth == 0 {
            return None;
        }
        match ast.get(pred) {
            Node::Logical { left, right, is_and, .. } => {
                let (left, right, is_and) = (*left, *right, *is_and);
                // A named-capture `=~` (`/(?<v>a)/ =~ s`) BINDS `v` to `String`
                // in the reference and to nothing at all in the arena — prism's
                // `MatchWriteNode` has no lowering here, so the local reads as
                // an unbound (untyped) name and our gate would narrow it. That
                // is a measured FP (probe `matchwrite`), and the binding is
                // invisible, so the whole compound predicate declines. Only a
                // regex on the LEFT of `=~` binds; `v =~ /a/` (a local or ivar
                // receiver) does not and still narrows (`matchop_keep`).
                if regex_binding_match(ast, left, depth) || regex_binding_match(ast, right, depth)
                {
                    return None;
                }
                let a = self.analyse_predicate(ast, left, depth - 1);
                let b = self.analyse_predicate(ast, right, depth - 1);
                if a.is_none() && b.is_none() {
                    return None;
                }
                let (at, af) = a.unwrap_or_default();
                let (bt, bf) = b.unwrap_or_default();
                Some(if is_and {
                    ([at, bt].concat(), join_guards(&af, &bf))
                } else {
                    (join_guards(&at, &bt), [af, bf].concat())
                })
            }
            Node::Call { receiver: Some(r), method, args, block_body, safe_nav, .. }
                if method == "!" && args.is_empty() && block_body.is_empty() && !*safe_nav =>
            {
                let r = *r;
                self.analyse_predicate(ast, r, depth - 1).map(|(t, f)| (f, t))
            }
            _ => self.guard_predicate(ast, pred),
        }
    }

    /// Recognise ONE atomic predicate on a bare local, as the reference's
    /// `dispatch_call_simple` (`narrowing.rb:976`) does, and return its
    /// `(truthy, falsey)` guard maps.
    ///
    /// Four shapes, three of them NON-mintable — they exist so that a compound
    /// predicate knows the local was already pinned to a class, which is where
    /// the false positives live:
    /// - `local.is_a?(C)` / `kind_of?(C)` / `instance_of?(C)` — the mintable
    ///   guard (`analyse_class_predicate`, `:1761`). Bare `LocalVariableRead`
    ///   receiver, no safe-nav, no block, one `ConstantRead` argument with a
    ///   statically known, unshadowed name. Chains are stage 3a-3.
    /// - `C === local` — the reference narrows through it too (probe
    ///   `e3_case_eq_bang` fires `for String`), but minting would be new
    ///   coverage this slice does not claim. Recognising it non-mintably is
    ///   load-bearing anyway: `String === v && v.is_a?(Hash)` reaches `Bot` on
    ///   the reference and would otherwise witness `for Hash` (probe
    ///   `L_caseeq`, a measured FP), and `Hash === v` on an Array carrier feeds
    ///   the collapse (`e3_case_eq_bot`).
    /// - `local.nil?` — `analyse_nil_predicate` (`:2453`) pins `NilClass` on the
    ///   truthy edge (`narrow_nil_other`: a `Dynamic` carrier becomes
    ///   `Constant[nil]`, every precise one becomes `Bot`) and leaves the falsey
    ///   edge unchanged. Without it `v.nil? && v.is_a?(String)` witnesses
    ///   `for String` where the reference reaches `Bot` — measured FP `L_nilq`,
    ///   in BOTH conjunct orders and in the middle of a chain (`mid_nilq`).
    ///   `!v.nil? && v.is_a?(String)` keeps narrowing, because the swap puts the
    ///   `NilClass` fact on the edge the `&&` does not concatenate.
    /// - `local == nil` / `nil == local` (and `!=`, which swaps the pair) —
    ///   `analyse_equality_predicate` (`:1568`) meets the carrier with
    ///   `Constant[nil]`; on `Dynamic` that is a no-op (probe `L_eq_nil` fires,
    ///   so declining it costs coverage) but on an already-narrowed carrier it
    ///   is `Bot` (`R_eq_nil`, a measured FP). Equality against a NON-nil
    ///   literal attaches a relational fact and never pins a class (`L_eq_one`
    ///   / `R_eq_one` both fire) — not recognised, and measured safe.
    fn guard_predicate(&self, ast: &LoweredAst, pred: NodeId) -> Option<(GuardMap, GuardMap)> {
        let Node::Call { receiver: Some(r), method, args, block_body, safe_nav, span, .. } =
            ast.get(pred)
        else {
            return None;
        };
        if *safe_nav || !block_body.is_empty() {
            return None;
        }
        let nil_fact = |name: &str| {
            (GuardTarget::Local(name.to_string()), GuardFact {
                classes: vec!["NilClass".to_string()],
                exact: false,
                mintable: false,
                chain_call: None,
            })
        };
        match (method.as_str(), args.len()) {
            ("is_a?" | "kind_of?" | "instance_of?", 1) => {
                // S3: a guard on a name the PROJECT declares is not mintable
                // (this slice never narrows to a project nominal), but it must
                // still be SEEN — the reference resolves it and collapses a
                // shaped carrier against it (`v = [1, 2]` guarded by an
                // in-source `Proj::Thing` is reference-SILENT, probe r1g).
                // Declining the whole predicate, as this arm did before, left
                // that as a live false positive. `mintable: false` is the
                // existing carrier for "assert but do not narrow" (`===`,
                // `nil?`).
                let (class, mintable) = self.guard_constant(ast, args[0], *span)?;
                let exact = method == "instance_of?";
                // Stage 3a-3: the operand is EITHER a bare local (stages 1-2)
                // OR a stable single-hop chain address off a local root
                // (`analyse_class_predicate_on_chain`, `narrowing.rb:1805`).
                // Anything else — an ivar read, a two-hop chain, a hop with
                // arguments or a block, a safe-nav hop — declines, exactly as
                // `stable_chain_address` returns nil for it (probes `c7b_ivar`,
                // `m_two_hop`, `c7e_args_on_hop`, `m_block_on_hop`,
                // `m_safe_nav_hop`; the last three are reference-silent too,
                // the first two are recorded coverage gaps). A `(x)`
                // receiver is a `ParenthesesNode` upstream —
                // `analyse_class_predicate`/`stable_chain_address` decline
                // it too (rigor-rs#820).
                let (target, chain_call) = match ast.get(*r) {
                    Node::LocalVariableRead { name, .. } if !ast.paren_unwrapped(*r) => {
                        (GuardTarget::Local(name.clone()), None)
                    }
                    _ => {
                        let (root, m) = stable_chain_address(ast, *r)?;
                        (GuardTarget::Chain(root, m), Some(*r))
                    }
                };
                let g = (target, GuardFact {
                    classes: vec![class],
                    exact,
                    mintable,
                    chain_call,
                });
                Some((vec![g], Vec::new()))
            }
            ("===", 1) => {
                // `C === (x)` — the operand `(x)` is a `ParenthesesNode`,
                // not a `LocalVariableReadNode`: unread upstream
                // (rigor-rs#820).
                let Node::LocalVariableRead { name, .. } = ast.get(args[0]) else { return None };
                if ast.paren_unwrapped(args[0]) {
                    return None;
                }
                let class = self.resolved_static_constant(ast, *r, *span)?;
                let g = (GuardTarget::Local(name.clone()), GuardFact {
                    classes: vec![class],
                    exact: false,
                    mintable: false,
                    chain_call: None,
                });
                Some((vec![g], Vec::new()))
            }
            ("nil?", 0) => {
                // `(x).nil?` — the paren'd receiver is unread upstream
                // (rigor-rs#820).
                let Node::LocalVariableRead { name, .. } = ast.get(*r) else { return None };
                if ast.paren_unwrapped(*r) {
                    return None;
                }
                Some((vec![nil_fact(name)], Vec::new()))
            }
            ("==" | "!=", 1) => {
                // `x == (nil)` / `(nil) == x` are unread:
                // `equality_local_literal`'s `static_literal_type` reads the
                // `ParenthesesNode` itself and answers nil for it
                // (rigor-rs#820). So is a `(x)` local on either side — the
                // `is_a?(LocalVariableReadNode)` gate fails on it.
                let name = match (ast.get(*r), ast.get(args[0])) {
                    (Node::LocalVariableRead { name, .. }, Node::NilLit { .. })
                        if !ast.paren_unwrapped(*r) && !ast.paren_unwrapped(args[0]) =>
                    {
                        name
                    }
                    (Node::NilLit { .. }, Node::LocalVariableRead { name, .. })
                        if !ast.paren_unwrapped(*r) && !ast.paren_unwrapped(args[0]) =>
                    {
                        name
                    }
                    _ => return None,
                };
                let g = nil_fact(name);
                Some(if method == "==" {
                    (vec![g], Vec::new())
                } else {
                    (Vec::new(), vec![g])
                })
            }
            _ => None,
        }
    }

    /// What guarding `local` with `class_name` does to a PRECISE carrier in the
    /// reference: `Some(Bot)` collapses it, `Some(Widened)` erases it to
    /// `untyped`, `None` keeps it (the caller then falls through to the
    /// sequential meet / mint).
    ///
    /// `Bot`, four ways:
    /// 1. the local is ALREADY `Bot` — `narrow_class_other` / `narrow_other_class`
    ///    return `Bot` unchanged on both polarities, so a further guard cannot
    ///    revive it (probes `bot_then_match`, `bot_then_neg`);
    /// 2. `instance_of?` (`exact:`) with a name the carrier's class does not
    ///    equal — `narrow_nominal_to_class` returns `Bot` before consulting the
    ///    hierarchy at all, and `subclass_of?` degenerates to name equality for
    ///    the shape/constant carriers (`narrowing.rb:2384,2440`);
    /// 3. a PROVEN-disjoint pair. [`CoreIndex::class_ordering`] answers
    ///    `Disjoint` only when both names resolve AND both ancestor chains are
    ///    complete, so an unresolvable/project class or a truncated chain
    ///    answers `Unknown`;
    /// 4. a SHAPED carrier (`Constant`/`Tuple`/`HashShape`) under a
    ///    `Disjoint` or `Superclass` ordering — its helper
    ///    (`narrow_shape_to_class`, `:2508`) asks `subclass_of?` rather than
    ///    `disjoint?`, so anything that is not `Subclass`/`Equal` collapses
    ///    EXCEPT the `Unknown` that `declines_bot?` now intercepts.
    ///
    /// `Widened`, two ways: a NOMINAL carrier under an `Unknown` ordering
    /// (upstream #533 item 4) and — since `v0.3.9`'s `6cde8381` — a SHAPED one
    /// under the same ordering (#657 item 2). See [`ClassFact::Widened`].
    ///
    /// The carrier's class comes from [`CoreIndex::class_name_of`] — the same
    /// mapping `check_call` dispatches on, so the suppression is exactly
    /// co-extensive with the witness it removes. A carrier that mapping
    /// declines (`Dynamic`, `Top`, a union, a `Singleton`) meets to `None`.
    ///
    /// [`CoreIndex::class_name_of`]: rigor_index::CoreIndex::class_name_of
    /// [`CoreIndex::class_ordering`]: rigor_index::CoreIndex::class_ordering
    fn guard_meet_precise(
        &self,
        local: &str,
        class_name: &str,
        exact: bool,
        tenv: &TypeEnv,
        cenv: &Facts,
        interner: &Interner,
    ) -> Option<ClassFact> {
        match cenv.locals.get(local) {
            Some(ClassFact::Bot) => return Some(ClassFact::Bot),
            // A widened local stays widened for the rest of the edge (rows
            // b21/b34b).
            Some(ClassFact::Widened) => return Some(ClassFact::Widened),
            _ => {}
        }
        let &ty = tenv.get(local)?;
        // The carrier must be one the reference's `narrow_class_dispatch`
        // (`narrowing.rb:2311`) routes to a COLLAPSING helper. Its table is
        // Constant / Nominal / Union / Tuple / HashShape / Singleton, and
        // everything else — an `IntegerRange` included — falls through to
        // `narrow_other_class`, which returns the type UNCHANGED for anything
        // that is not Dynamic/Top. `Union` (per-member) and `Singleton` (via
        // `subclass_of?("Class", …)`) are declined here as unprobed.
        if !matches!(
            interner.get(ty),
            Type::Constant(_) | Type::Nominal { .. } | Type::Tuple(_) | Type::HashShape(_)
        ) {
            return None;
        }
        let carrier = self.index.class_name_of(interner, ty)?;
        if exact {
            // `instance_of?` is `Bot`-or-keep on EVERY carrier, widening
            // included: `narrow_nominal_to_class` returns `Bot` for `exact:`
            // before it ever consults the ordering (`narrowing.rb:2482`), which
            // is why row b13 is silent on both engines through `Bot`, not
            // through the new fact.
            return (carrier != class_name).then_some(ClassFact::Bot);
        }
        // S3 (2026-08-08): the collapse condition is per-CARRIER-KIND, and it is
        // NOT the same predicate for all of them — reading `narrow_class_dispatch`
        // (`narrowing.rb:2311`) and probing the oracle both say so.
        //
        // * `narrow_shape_to_class` (`:2403`) and `narrow_constant_to_class`
        //   (`:2364`) keep the carrier iff `subclass_of?(carrier, class_name)`,
        //   i.e. iff the ordering is `Subclass`/`Equal`. `Unknown` therefore
        //   COLLAPSES: a shaped carrier does not need the guard class to resolve.
        // * `narrow_nominal_to_class` (`:2480`) is different — it PRESERVES the
        //   bound on `Subclass` and, since `70ca7e74`, WIDENS on `Unknown`,
        //   collapsing only on `Disjoint`.
        //
        // Treating every carrier as the Nominal case (the pre-S3 code) left a
        // live false positive: `v = [1, 2]; return unless v.is_a?(File::Stat);
        // v.frobnicate_zzz` fired `for Array` where the reference is silent, and
        // it fired for an UNRESOLVABLE guard too. Measured against the pinned
        // reference on `[1, 2]` / `{a: 1}` carriers: `Enumerable`, `Object` and
        // `Array` guards all FIRE (subclass/equal ⇒ the shape survives) while
        // `File::Stat` and `Foo::Bar::Baz` are SILENT — which pins the condition
        // as `subclass_of?`, not as "any shaped carrier collapses".
        //
        // `v0.3.9` DOES move the shaped arms. `6cde8381` lifted
        // `narrow_constant_to_class`'s `:unknown ⇒ untyped` (#657) into a shared
        // `declines_bot?` and gave it to `narrow_shape_to_class` and
        // `narrow_singleton_to_class` too, so the `Unknown` ordering now widens
        // on every positive-edge carrier rather than collapsing. Rows b15b/b30b
        // measured the OLD behaviour (both engines firing on the call after the
        // `if`, through `Bot`'s join identity) and are the rows that retracted.
        // The SINGLETON half is not ported here: `guard_meet_precise` declines a
        // `Singleton` carrier outright, and probe t3 measures BOTH engines
        // silent on `case Widget when Meta` with `extend Meta` — there is no
        // witness to remove.
        let ordering = self.index.class_ordering(carrier, class_name);
        match interner.get(ty) {
            Type::Constant(_) | Type::Tuple(_) | Type::HashShape(_) => {
                match ordering {
                    ClassOrdering::Subclass | ClassOrdering::Equal => None,
                    // Upstream `6cde8381` (#657 item 2, `v0.3.9`) lifted the
                    // `Unknown` decline out of `narrow_constant_to_class` into
                    // the shared `declines_bot?` and gave it to EVERY
                    // positive-edge carrier: a `Tuple` / `HashShape` projects
                    // through `Array` / `Hash`, and a project module included
                    // into either leaves the ordering `Unknown` for exactly
                    // #657's reason. `Bot` asserts "this can never match", and
                    // only `Disjoint` is evidence for that.
                    ClassOrdering::Unknown => Some(ClassFact::Widened),
                    ClassOrdering::Disjoint | ClassOrdering::Superclass => Some(ClassFact::Bot),
                }
            }
            _ => match ordering {
                ClassOrdering::Disjoint => Some(ClassFact::Bot),
                ClassOrdering::Unknown => Some(ClassFact::Widened),
                ClassOrdering::Subclass | ClassOrdering::Equal | ClassOrdering::Superclass => None,
            },
        }
    }

    /// The statically-known name of a `ConstantRead`/`ConstantPath` node (both
    /// lower to `ConstantRead`), resolved lexically at `use_span`'s prefix —
    /// or `None` when the node is not a static constant (`dynamic_base`, the
    /// reference's `qualified_name_or_nil` answering nil — rigor-rs#670) OR a
    /// project declaration shadows the name ([`SourceIndex::constant_shadowed`]
    /// — decline entirely; this slice never narrows to a project nominal).
    ///
    /// S2 (2026-08-08): the name is additionally resolved to a QUALIFIED KEY
    /// here, at MINT time, because this is where the use site's lexical
    /// `enclosing_prefix` is available — the class-narrowing fact then carries
    /// a resolved key rather than the verbatim source spelling. Three spellings
    /// of the same class must reach the same key (probes p3a–p3d): a relative
    /// `HTTP` inside `module URI`, a qualified `URI::HTTP`, and an absolute
    /// `::URI::HTTP` all render `for URI::HTTP` on the reference.
    ///
    /// [`SourceIndex::constant_shadowed`]: crate::SourceIndex::constant_shadowed
    fn resolved_static_constant(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        use_span: rigor_parse::Span,
    ) -> Option<String> {
        let Node::ConstantRead { name, dynamic_base, rooted, .. } = ast.get(id) else {
            return None;
        };
        // A dynamic base (`k::String`, `self::String`, `self.class::String`)
        // is not a static class: the trailing name is a lenient render, and
        // the reference's `static_class_name` returns nil (rigor-rs#670). So
        // does a PARENTHESED operand: `static_class_name` reads the
        // `ParenthesesNode` itself and answers nil for it (`when (C)` narrows
        // neither edge upstream — rigor-rs#820), so the id the lowering
        // unwrapped stays unread in every `lexical_class_name` position this
        // resolves (`when` condition, `===` receiver).
        if name.is_empty() || *dynamic_base || ast.paren_unwrapped(id) {
            return None;
        }
        if *rooted {
            // `lexical_class_name`'s rooted arm keeps the de-rooted spelling
            // UN-WALKED — `when ::Stat` inside `class File` names top-level
            // `Stat`, never `File::Stat` (rigor-rs#672). Only a ONE-SEGMENT
            // top-level project class shadows it: `knows_class`'s leaf key
            // used to shadow `::M` for a nested `class A; class M`, and
            // `discovered_decls`'s qualified keys would shadow
            // `::Outer::Inner` — both are names the rooted operand narrows
            // to on the oracle (rigor-rs#672 rounds 6–7).
            if !name.contains("::") && self.source.knows_discovered_decl(name) {
                return None;
            }
            return Some(name.to_string());
        }
        let prefix = self.enclosing_prefix(use_span);
        if self.source.constant_shadowed(name, prefix) {
            return None;
        }
        Some(self.resolve_constant_as_written(name, prefix, false))
    }

    /// [`Typer::resolved_static_constant`] for the `is_a?`/`instance_of?` arm,
    /// which needs the SHADOWED case as a fact rather than as a decline:
    /// `(resolved qualified key, mintable)`. `mintable` is `false` exactly when
    /// the project declares the name in a lexically visible scope — the same
    /// predicate `resolved_static_constant` declines on — so the guard can
    /// collapse a precise carrier without ever narrowing TO a project nominal.
    pub(crate) fn guard_constant(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        use_span: rigor_parse::Span,
    ) -> Option<(String, bool)> {
        let Node::ConstantRead { name, dynamic_base, rooted, .. } = ast.get(id) else {
            return None;
        };
        // `is_a?((C))` is silent upstream: `rooted_class_predicate_name`
        // walks the same `static_class_name`, which reads the
        // `ParenthesesNode` — not the constant — and answers nil
        // (rigor-rs#820).
        if name.is_empty() || *dynamic_base || ast.paren_unwrapped(id) {
            return None;
        }
        if *rooted {
            // `rooted_class_predicate_name` (upstream #614, rigor-rs#672): a
            // `::C` operand names the TOP level only — `is_a?(::Stat)` inside
            // `class File` never narrows to `File::Stat`. It answers `C` when
            // `class_known_to_scope?` holds — the RBS registry / project
            // `sig/` (`constant_names_a_known_class`) OR an in-source class
            // (`knows_discovered_class`, the `discovered_classes` analogue:
            // `class`/`module` and meta-new writes like `Foo = Struct.new`)
            // — and DECLINES outright (nil → no fact) when neither surface
            // owns the name.
            //
            // The bundled answer wins the ORDER (rigor-rs#714):
            // `class_known_to_scope?` is an OR, so an in-source declaration
            // cannot veto a name the RBS also declares — dependabot's
            // `module Bundler; class Source; class Git` monkey patch must not
            // demote `instance_of?(::Bundler::Source::Git)` to assert-only,
            // or the then-arm loses every row the reference's narrowing
            // fires. The assert-only demotion stays for names ONLY the
            // project declares (the unrooted arm's lexical-shadow
            // `mintable: false`), and `knows_class`'s leaf key stays out of
            // both arms (`class A; class M` never shadows `::M`,
            // rigor-rs#672 round 6).
            if self.constant_names_a_known_class(name) {
                return Some((name.to_string(), true));
            }
            if self.source.knows_discovered_class(name) {
                return Some((name.to_string(), false));
            }
            return None;
        }
        let prefix = self.enclosing_prefix(use_span);
        let shadowed = self.source.constant_shadowed(name, prefix);
        Some((self.resolve_constant_as_written(name, prefix, false), !shadowed))
    }

    /// Resolve a guard constant's SOURCE SPELLING to the qualified key it names,
    /// by Ruby's (and RBS's) own rule: an ABSOLUTE reference (`::File::Stat`)
    /// drops its root marker and is looked up as written; a relative one is
    /// tried against each enclosing lexical scope innermost-outward, then at the
    /// root. The first scope that KNOWS the name wins — deterministic, so there
    /// is no residual ambiguity to decline on (the reference resolves the same
    /// way, probes q9/q9b).
    ///
    /// A name nothing knows is returned as written (minus the root marker): the
    /// witness gate declines it anyway (probes p2/p2b are reference-silent), and
    /// leaving it verbatim keeps `guard_meet_precise` seeing exactly what it saw
    /// before this slice.
    pub(crate) fn resolve_constant_as_written(
        &self,
        name: &str,
        prefix: &[String],
        rooted: bool,
    ) -> String {
        // `rooted` is `Node::ConstantRead`'s flag — the lowering renders `::C`
        // UNROOTED, so the `starts_with` sniff the old code relied on was dead
        // and a rooted operand walked the lexical ladder (rigor-rs#672).
        if !rooted {
            for depth in (1..=prefix.len()).rev() {
                let cand = format!("{}::{name}", prefix[..depth].join("::"));
                if self.constant_names_a_known_class(&cand) {
                    return cand;
                }
            }
        }
        name.to_string()
    }

    /// Whether `qname` names a class/module some AUTHORITATIVE surface knows —
    /// the bundled RBS qualified registry or the project's own `sig/`. In-source
    /// declarations are deliberately NOT consulted: the reference is silent on
    /// them for the ADR-0033 provenance reason (probes p4a/p4b/p5), so resolving
    /// to one could only ever change which name a declined witness carries.
    pub(crate) fn constant_names_a_known_class(&self, qname: &str) -> bool {
        self.index.knows_qualified_class(qname) || self.index.is_qualified_project_sig_class(qname)
    }

    /// `integer_rooted_type?` (`narrowing.rb:3310`) for a `case` subject's
    /// tenv carrier: a `Constant` Integer, an `IntegerRange`, a bare
    /// `Nominal[Integer]` — or, on the reference, an all-integer union,
    /// which this slice declines like every other union carrier.
    fn integer_rooted(&self, ty: TypeId, interner: &Interner) -> bool {
        match interner.get(ty) {
            Type::Constant(Scalar::Int(_) | Scalar::BigInt(_)) | Type::IntegerRange { .. } => {
                true
            }
            Type::Nominal { class, args } => {
                args.is_empty() && self.index.class_name_for_id(*class) == Some("Integer")
            }
            _ => false,
        }
    }

    /// One `when` condition's `===` class — `apply_case_when_condition`
    /// (`narrowing.rb:3254`) minus the subject-dependent parts the caller
    /// supplies as `integer_rooted`.
    fn case_cond_class(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        integer_rooted: bool,
        use_span: rigor_parse::Span,
    ) -> CaseCond {
        match ast.get(id) {
            Node::ConstantRead { name, dynamic_base, rooted, .. } => {
                // `(C)` is UNREAD — `lexical_class_name`'s
                // `static_class_name` answers nil for the `ParenthesesNode`
                // itself (rigor-rs#820). The literal arms below stay
                // paren-transparent by contrast: `when_condition_class`'s
                // other two sources (`case_equality_target_class`, the
                // nil/true/false table) and the integer paths all
                // `unwrap_parens` before they read.
                if name.is_empty() || *dynamic_base || ast.paren_unwrapped(id) {
                    return CaseCond::Unread;
                }
                // `lexical_class_name` — resolved at the use site's prefix
                // and NEVER declined: the MEET side reads a class target
                // where the mint gate would decline a project shadow.
                if *rooted {
                    CaseCond::Class(name.to_string())
                } else {
                    let prefix = self.enclosing_prefix(use_span);
                    CaseCond::Class(self.resolve_constant_as_written(name, prefix, false))
                }
            }
            Node::NilLit { .. } => CaseCond::Class("NilClass".to_string()),
            Node::TrueLit { .. } => CaseCond::Class("TrueClass".to_string()),
            Node::FalseLit { .. } => CaseCond::Class("FalseClass".to_string()),
            Node::RegexpLit { .. } => CaseCond::Regexp,
            Node::IntegerLit { value, digits, .. } if integer_rooted => CaseCond::IntLit(
                *value,
                digits
                    .clone()
                    .unwrap_or_else(|| value.map(|v| v.to_string()).unwrap_or_default()),
            ),
            Node::Range { left, right, exclude_end, .. } => {
                // `case_equality_integer_range`: both present endpoints are
                // integer literals (an absent one is an infinity). Only then
                // does `integer_rooted_type?` pick the RANGE answer; a
                // well-formed integer range on any other carrier falls
                // through to `when_condition_class`, where every endpoint
                // being an `IntegerNode` makes `range_target_class`
                // answer `Numeric`.
                let bound = |ep: &Option<NodeId>| -> Option<Option<(Option<i64>, String)>> {
                    match ep {
                        None => Some(None),
                        Some(n) => match ast.get(*n) {
                            Node::IntegerLit { value, digits, .. } => Some(Some((
                                *value,
                                digits
                                    .clone()
                                    .unwrap_or_else(|| {
                                        value.map(|v| v.to_string()).unwrap_or_default()
                                    }),
                            ))),
                            _ => None,
                        },
                    }
                };
                match (bound(left), bound(right)) {
                    (Some(lo), Some(hi)) => {
                        if integer_rooted {
                            let mk = |b: Option<(Option<i64>, String)>, inf: IntBound| match b {
                                None => inf,
                                Some((v, d)) => {
                                    IntBound::Lit(v, normalize_int_spelling(&d))
                                }
                            };
                            let lo = mk(lo, IntBound::NegInf);
                            let hi = mk(hi, IntBound::PosInf);
                            let hi = if *exclude_end { hi.dec() } else { hi };
                            CaseCond::IntRange(lo, hi)
                        } else {
                            CaseCond::Class("Numeric".to_string())
                        }
                    }
                    _ => {
                        // `range_target_class`: an `IntegerNode`/`FloatNode`
                        // endpoint answers `Numeric`, a `StringNode` one
                        // `String`; anything else is unread.
                        let numeric = |ep: &Option<NodeId>| {
                            ep.is_some_and(|n| {
                                matches!(
                                    ast.get(n),
                                    Node::IntegerLit { .. } | Node::FloatLit { .. }
                                )
                            })
                        };
                        let string = |ep: &Option<NodeId>| {
                            ep.is_some_and(|n| matches!(ast.get(n), Node::StringLit { .. }))
                        };
                        if numeric(left) || numeric(right) {
                            CaseCond::Class("Numeric".to_string())
                        } else if string(left) || string(right) {
                            CaseCond::Class("String".to_string())
                        } else {
                            CaseCond::Unread
                        }
                    }
                }
            }
            _ => CaseCond::Unread,
        }
    }

    /// The TRUTHY-edge meet one `when`/`in` condition applies to the
    /// subject's precise carrier — `None` keeps the carrier (the `met`
                /// members the union rides), `Some(Bot)` collapses,
    /// `Some(Widened)` is the `untyped` answer.
    fn case_cond_meet(
        &self,
        local: &str,
        cond: &CaseCond,
        tenv: &TypeEnv,
        cenv: &Facts,
        interner: &Interner,
    ) -> Option<ClassFact> {
        match cond {
            CaseCond::Class(name) => {
                self.guard_meet_precise(local, name, false, tenv, cenv, interner)
            }
            // `narrow_integer_equal` — a `Constant` collapses on any other
            // value, an `IntegerRange` when it cannot contain the literal;
            // a bare `Nominal[Integer]` re-pins to `Constant[k]` — a live
            // carrier either way.
            CaseCond::IntLit(value, digits) => match interner.get(*tenv.get(local)?) {
                Type::Constant(s) => {
                    (!int_scalar_eq(s, *value, digits)).then_some(ClassFact::Bot)
                }
                Type::IntegerRange { min, max } => {
                    (!int_range_covers(*min, *max, *value, digits)).then_some(ClassFact::Bot)
                }
                _ => None,
            },
            // `integer_range_when_result`'s truthy — the carrier intersected
            // with the range: a `Constant` outside collapses, an
            // `IntegerRange` disjoint from it collapses.
            CaseCond::IntRange(lo, hi) => match interner.get(*tenv.get(local)?) {
                Type::Constant(s) => {
                    (!int_scalar_in_bounds(s, lo, hi)).then_some(ClassFact::Bot)
                }
                Type::IntegerRange { min, max } => {
                    int_ranges_disjoint(*min, *max, lo, hi).then_some(ClassFact::Bot)
                }
                _ => None,
            },
            // `regexp_case_equality_member`: the member keeps whole when its
            // class `converts_with_to_str?` — String, a class whose
            // signature/ancestry/source declares `to_str`, or a class no
            // signature declares — and otherwise narrows to its
            // `String | Symbol` parts. A member collapses only when BOTH
            // projections do — a `Disjoint` ordering on each side; a
            // `Superclass` ordering still answers a live `Nominal[target]`.
            // The `to_str`-signature and declared-ancestry keeps this
            // approximation misses are coverage losses, never FPs.
            CaseCond::Regexp => {
                let ty = *tenv.get(local)?;
                let carrier = self.index.class_name_of(interner, ty)?;
                let disjoint = |target: &str| {
                    matches!(
                        self.index.class_ordering(carrier, target),
                        ClassOrdering::Disjoint
                    )
                };
                (disjoint("String") && disjoint("Symbol")).then_some(ClassFact::Bot)
            }
            CaseCond::Unread => None,
        }
    }

    /// The FALSEY-edge subtraction one condition applies — the clause-level
    /// `fully_narrowable` gate already excluded `IntRange` and `Unread`
    /// members and a `Regexp` contributes nothing, so this answers "does the
    /// carrier drop out of the edge entirely".
    fn case_cond_covers(
        &self,
        local: &str,
        cond: &CaseCond,
        tenv: &TypeEnv,
        interner: &Interner,
    ) -> bool {
        let Some(&ty) = tenv.get(local) else { return false };
        match cond {
            // `narrow_*_not_class` drops the carrier exactly when it IS the
            // target — `subclass_of?`'s `{subclass, equal}` — for every
            // carrier the positive-edge rules model (`narrow_other_class`'s
            // negative half keeps anything else UNCHANGED: a `Dynamic`, an
            // `IntegerRange`, a `Singleton`, a `Union`).
            CaseCond::Class(name) => {
                if !matches!(
                    interner.get(ty),
                    Type::Constant(_) | Type::Nominal { .. } | Type::Tuple(_) | Type::HashShape(_)
                ) {
                    return false;
                }
                let Some(carrier) = self.index.class_name_of(interner, ty) else {
                    return false;
                };
                matches!(
                    self.index.class_ordering(carrier, name),
                    ClassOrdering::Subclass | ClassOrdering::Equal
                )
            }
            // `narrow_integer_not_equal` removes the ONE literal value: a
            // `Constant[k]` drops out; a single-point `IntegerRange[k,k]`
            // empties; everything else keeps.
            CaseCond::IntLit(value, digits) => match interner.get(ty) {
                Type::Constant(s) => int_scalar_eq(s, *value, digits),
                Type::IntegerRange { min: Some(lo), max: Some(hi) } => {
                    lo == hi && *value == Some(*lo)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Narrow through one `case`/`when` node (statement or expression
    /// position) — reference `case_when_scopes`/`case_clause_scopes`
    /// (`narrowing.rb:519,583`), the strict-subset envelope:
    /// - the subject is a bare `LocalVariableRead` whose type is Dynamic/Top
    ///   (the `subject`/`bot_subject` pair);
    /// - a clause narrows the Dynamic subject ONLY when it has EXACTLY ONE
    ///   condition and that condition is a static constant, resolved
    ///   lexically, unshadowed (multi-condition unions — probe a6 — decline,
    ///   FP-safe);
    /// - the per-condition meet a PRECISE carrier takes in a clause body is
    ///   `apply_case_when_condition` (`narrowing.rb:3254`): class targets
    ///   (`when C`/`in C`, `nil`/`true`/`false`, non-integer ranges →
    ///   `Numeric`/`String`), an integer literal (`when k`), an integer range
    ///   (`when a..b`) and a regexp literal are each READ; anything else is
    ///   unread and an all-unread clause keeps the entry carrier while a
    ///   mixed one widens it (`case_when_body_type` → `untyped`);
    /// - the FALSEY edge threads clause to clause
    ///   (`eval_case_when_branches`, `statement_evaluator.rb:2489`): each
    ///   clause's body enters from the accumulated no-match scope and the
    ///   `else` — real, or a `case`/`when`'s implicit no-match arm — from
    ///   the last one. The subject is the only local that moves on it:
    ///   `narrow_not_class` drops a covered carrier to `Bot` once EVERY
    ///   condition of the covering clauses was narrowable
    ///   (`fully_narrowable`); a `case`/`in` WITHOUT `else` has no no-match
    ///   arm at all (`pattern_case_matches_every_path?`) — no else edge is
    ///   pushed and the subject's post-`case` fact is the arm-edge union;
    /// - **position gate** (`stmt_position`): a `case` reached as a call
    ///   receiver, as an argument, or as a `return` operand narrows NOTHING —
    ///   the reference is silent there (probes p2, p3, p7) while it fires in
    ///   statement position (p6) and on an assignment RHS (p1). This is the
    ///   same rule block bodies follow; `if`/ternary is the exception and
    ///   narrows in every position (p4, p8).
    #[allow(clippy::too_many_arguments)]
    fn class_flow_case(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        let Node::Case { predicate, branches, else_body, span } = ast.get(id) else {
            return;
        };
        let (predicate, case_span) = (*predicate, *span);
        let (branches, else_body) = (branches.clone(), else_body.clone());
        // The subject evaluates first, in the current facts — EXPRESSION
        // position (it is the `case`'s operand, not a statement).
        if let Some(p) = predicate {
            self.class_flow_expr(ast, p, tenv, cenv, coarse, writes, interner, out, false);
        }
        // The narrowing subject: a bare local currently Dynamic/Top — and only
        // in statement position / on an assignment RHS (p2, p3, p7 decline).
        let subject = predicate
            .filter(|_| stmt_position)
            .and_then(|p| match ast.get(p) {
                Node::LocalVariableRead { name, .. } => Some(name.clone()),
                _ => None,
            })
            .filter(|local| !coarse.contains(local))
            .filter(|local| match tenv.get(local.as_str()) {
                None => true, // unbound ⇒ untyped (Dynamic[top])
                Some(&ty) => matches!(interner.get(ty), Type::Dynamic(_) | Type::Top),
            });
        // The `Bot` subject: the same bare local under the same position gate
        // (probe `case_as_recv` — a `case` consumed as a call receiver narrows
        // NOTHING and the reference still fires there), but with the PRECISE
        // carrier the narrowing subject excludes. The per-clause collapse test
        // is applied below, per condition.
        let bot_subject = predicate.filter(|_| stmt_position).and_then(|p| match ast.get(p) {
            Node::LocalVariableRead { name, .. } => Some(name.clone()),
            _ => None,
        });
        // Join-retention slice: the pre-`case` facts, snapshot AFTER the subject
        // was evaluated (each clause clones `cenv` from exactly here).
        let pre_join = cenv.clone();
        // The threaded FALSEY edge — the reference's
        // `eval_case_when_branches` (`statement_evaluator.rb:2489`) walks the
        // clauses in source order, and each clause's body enters from the
        // scope the PRECEDING clauses' no-match edges left behind: a subject
        // whose carrier a `when`/`in` class test covers is `Bot` in every
        // later clause and in the `else`. `falsey` accumulates exactly that —
        // the `case`'s entry facts, each clause's condition effects, and the
        // subject's subtraction — so clause bodies clone from IT, not from
        // `cenv` (rigor-rs#710).
        let mut falsey = cenv.clone();
        // Were ALL branches descended? `edges` is the restore's only evidence
        // for the `in` arms as well as the `when`s — a carrier this slice
        // still cannot place leaves the chain half of the restore disabled.
        let mut all_descended = true;
        let mut edges: Vec<Facts> = Vec::new();
        // Parallel to `edges`: does that clause's body TERMINATE? Only
        // [`propagate_widened`] reads it — a `when` arm that returns widens
        // nothing for the code after the `case`.
        let mut edge_terminates: Vec<bool> = Vec::new();
        // The `case`/`in` flag `pattern_case_matches_every_path?` reads: an
        // all-`in` `case` without `else` has no no-match arm to join.
        let mut saw_when = false;
        let mut saw_in = false;
        // `when_test_rebinds_subject?` (#1963): once a `when` test writes the
        // subject, NO later clause narrows it — the variable no longer holds
        // the value the tests match.
        let mut subject_rebound = false;
        // `integer_rooted_type?` reads the subject's tenv carrier, which the
        // clause loop never rewrites — compute it once.
        let integer_rooted = bot_subject
            .as_ref()
            .and_then(|local| tenv.get(local.as_str()))
            .is_some_and(|&ty| self.integer_rooted(ty, interner));
        for br in branches {
            // `conditions` are the `===` operands the clause narrows by (an
            // `in` arm contributes its bare-class pattern, or nothing);
            // `body` is what descends under the truthy edge.
            let (conditions, body, in_arm): (Vec<NodeId>, Vec<NodeId>, Option<rigor_parse::Span>) =
                match ast.get(br) {
                    Node::When { conditions, body, .. } => {
                        saw_when = true;
                        (conditions.clone(), body.clone(), None)
                    }
                    // A `case`/`in` arm — a clause-less `BeginRescue` carrier
                    // whose FIRST body node is the lowered pattern
                    // (`[pattern, …stmts]`). A bare class pattern (`in C`,
                    // `in C::D`, `in ::C`, `in C => x` — the capture is
                    // unwrapped to the constant by the lowering) matches
                    // exactly when `subject.is_a?(C)`, so it narrows like a
                    // one-condition `when` — `case_clause_scopes` →
                    // `bare_class_pattern_node` (`narrowing.rb:583`). Every
                    // other pattern narrows neither edge (`[scope, scope]`),
                    // but its body still descends: the threaded falsey is
                    // what silences it after a covering clause.
                    Node::BeginRescue { body, clauses, ensure_body, has_else, span, .. }
                        if clauses.is_empty() && ensure_body.is_empty() && !*has_else =>
                    {
                        saw_in = true;
                        let conds: Vec<NodeId> = body
                            .first()
                            .filter(|&&p| matches!(ast.get(p), Node::ConstantRead { .. }))
                            .copied()
                            .into_iter()
                            .collect();
                        (conds, body.iter().skip(1).copied().collect(), Some(*span))
                    }
                    _ => {
                        all_descended = false;
                        continue;
                    }
                };
            let mut t = tenv.clone();
            let mut c = falsey.clone();
            // The `when` tests (and an `in` class pattern) run BEFORE the
            // body and before every later clause — `run_when_test` — so
            // their effects thread down the falsey spine: a condition that
            // writes kills what it rebinds for every later edge, not just
            // this body.
            for &cond in &conditions {
                self.class_flow_expr(ast, cond, &mut t, &mut c, coarse, writes, interner, out, false);
            }
            // …and they continue on the falsey edge itself.
            falsey = c.clone();
            if !subject_rebound {
                if let Some(local) = &bot_subject {
                    subject_rebound = conditions.iter().any(|&cond| {
                        writes_within(writes, ast.get(cond).span())
                            .iter()
                            .any(|(_, n)| n == local)
                    });
                }
            }
            // A `case`/`in` pattern's own bindings (`in [a, b]`, `in C => x`)
            // ride an `UnmodeledWrite` sharing the ARM's span — they rebind
            // on the match edge only, so the body clone forgets any incoming
            // fact they held while the falsey edge keeps it.
            if let Some(arm_span) = in_arm {
                for &(wspan, ref name) in writes_within(writes, arm_span) {
                    if wspan == arm_span {
                        c.kill_local(name);
                        t.remove(name.as_str());
                    }
                }
            }
            if let (Some(local), [only]) = (&subject, conditions.as_slice()) {
                if !subject_rebound {
                    if let Some(class) = self.resolved_static_constant(ast, *only, case_span) {
                        // Review R3: a clause conflicting with an existing
                        // DIFFERENT-class fact for the subject drops the stale fact
                        // and never inserts (the reference's carrier is a Nominal
                        // there, out of the Dynamic-only envelope). Same class keeps.
                        let fact = ClassFact::Narrowed(class);
                        if c.locals.get(local).is_some_and(|existing| existing != &fact) {
                            c.locals.remove(local);
                        } else {
                            c.locals.insert(local.clone(), fact);
                        }
                    }
                }
            } else if let Some(local) = &bot_subject {
                // `case x when C1, C2` runs `C1 === x || C2 === x` and the
                // reference UNIONS the per-condition narrowings
                // (`case_when_body_type`), so the clause body is `Bot` only
                // when EVERY condition collapses — `when Hash, Array` on an
                // Array keeps the carrier and still witnesses (probes
                // `case_multi_disj` vs `case_multi_mixed`). An empty clause
                // cannot occur; `all` over one condition is the single-constant
                // case, which lands here whenever `subject` declined it.
                //
                // A clause mixing a READ condition with an UNREAD one answers
                // `untyped` for the whole body (`case_when_body_type` — the
                // body may have been reached through the unread member), the
                // same answer one UNORDERABLE class gives (row b8,
                // `case h when UnknownZzzClass`). Row b30 is the shaped-carrier
                // control the widening must not swallow: a `[1, 2]` subject
                // stays `Bot` and the call after the `case` fires on both
                // engines.
                let mut classified: Vec<CaseCond> = Vec::with_capacity(conditions.len());
                let mut met: Vec<Option<ClassFact>> = Vec::with_capacity(conditions.len());
                let mut any_unread = false;
                for &cond in &conditions {
                    let cc = self.case_cond_class(ast, cond, integer_rooted, case_span);
                    match &cc {
                        CaseCond::Unread => any_unread = true,
                        _ => met.push(self.case_cond_meet(local, &cc, tenv, &c, interner)),
                    }
                    classified.push(cc);
                }
                if !subject_rebound {
                    if any_unread && !met.is_empty() {
                        c.locals.insert(local.clone(), ClassFact::Widened);
                    } else if !any_unread {
                        let all_collapse = !met.is_empty()
                            && met.iter().all(|m| m.as_ref() == Some(&ClassFact::Bot));
                        if met.iter().any(|m| m.as_ref() == Some(&ClassFact::Widened)) {
                            c.locals.insert(local.clone(), ClassFact::Widened);
                        } else if all_collapse {
                            c.locals.insert(local.clone(), ClassFact::Bot);
                        }
                    }
                }
                // The FALSEY edge the next clause (and `else`) enters from.
                // `case_when_types` returns `current` unless EVERY condition
                // was read AND narrowable (`fully_narrowable` — an integer
                // range's two-piece complement and any unread member hand
                // the entry back untouched). Class targets subtract by
                // `narrow_not_class`, an integer literal by
                // `narrow_integer_not_equal`; a regexp contributes nothing.
                if !subject_rebound
                    && classified.iter().all(|cc| {
                        matches!(
                            cc,
                            CaseCond::Class(_) | CaseCond::IntLit(..) | CaseCond::Regexp
                        )
                    })
                    && !matches!(
                        falsey.locals.get(local),
                        Some(ClassFact::Bot | ClassFact::Widened)
                    )
                {
                    for cc in &classified {
                        if self.case_cond_covers(local, cc, tenv, interner) {
                            falsey.locals.insert(local.clone(), ClassFact::Bot);
                            break;
                        }
                    }
                }
            }
            // Clause bodies INHERIT the `case`'s position: a block inside an
            // expression-position clause narrows nothing (probe x1).
            self.class_flow_scope(ast, &body, &mut t, &mut c, coarse, writes, interner, out, stmt_position);
            edges.push(c);
            edge_terminates.push(!body.is_empty() && branch_terminates(ast, &body));
        }
        // The `else` — a real one, or a `case`/`when`'s implicit no-match arm
        // (`eval_case_else` hands it the final `falsey_scope` either way).
        // A `case`/`in` WITHOUT `else` raises `NoMatchingPatternError` on no
        // match, so the continuation joins ONLY matched arms
        // (`pattern_case_matches_every_path?`) — no else edge exists there.
        if !saw_in || saw_when || !else_body.is_empty() {
            let mut t = tenv.clone();
            let mut c = falsey.clone();
            self.class_flow_scope(
                ast, &else_body, &mut t, &mut c, coarse, writes, interner, out, stmt_position,
            );
            edges.push(c);
            edge_terminates.push(!else_body.is_empty() && branch_terminates(ast, &else_body));
        }
        let case_in_no_else = saw_in && !saw_when && else_body.is_empty();
        widen_flow_writes(writes_within(writes, case_span).iter(), case_span, tenv, interner);
        // A `case`/`in` clause is not descended, so its rebinds are invisible to
        // the edge evidence — kill by span as well as by edge.
        join_cenv(cenv, &edges);
        // Join-retention slice: restore the pre-`case` facts the clauses left
        // untouched (`case_intervening`, reference-firing on master's silence).
        // The narrowing SUBJECT is excluded: the reference replaces its type per
        // clause and the post-`case` union is out of the Dynamic-only envelope
        // this pass models, so keeping our incoming fact there would be an
        // unprobed guess (probe `case_subject_is_target` — the reference fires,
        // we decline; coverage, never an FP).
        let subject_excl: Vec<String> = subject.iter().chain(bot_subject.iter()).cloned().collect();
        let joined_subject: Vec<(String, Option<ClassFact>)> =
            subject_excl.iter().map(|n| (n.clone(), cenv.locals.get(n).cloned())).collect();
        let predicate_locals = predicate.map(|p| locals_in_span(self, ast, ast.get(p).span()));
        retain_joined_facts(
            cenv,
            &pre_join,
            &edges,
            writes_within(writes, case_span),
            case_span,
            predicate_locals.as_ref().filter(|_| all_descended),
        );
        if case_in_no_else {
            // `unmatched_pattern_result`: the continuation joins ONLY the
            // matched-arm scopes — the subject's post-`case` fact is the
            // arm-edge union. `untyped` absorbs (`Widened ∪ T = Widened`),
            // `Bot` is the join identity, and a carrier that survives on ANY
            // edge keeps the union inhabited (the reference's union still
            // holds it — decline the rest, the verdict `check` renders
            // rides the surviving carrier).
            for name in &subject_excl {
                let mut any_widened = false;
                let mut all_bot = !edges.is_empty();
                for edge in &edges {
                    match edge.locals.get(name) {
                        Some(ClassFact::Widened) => any_widened = true,
                        Some(ClassFact::Bot) => {}
                        _ => all_bot = false,
                    }
                }
                if any_widened {
                    cenv.locals.insert(name.clone(), ClassFact::Widened);
                } else if all_bot {
                    cenv.locals.insert(name.clone(), ClassFact::Bot);
                } else {
                    cenv.locals.remove(name);
                }
            }
        } else {
            for (name, fact) in joined_subject {
                match fact {
                    Some(f) => cenv.locals.insert(name, f),
                    None => cenv.locals.remove(&name),
                };
            }
        }
        // AFTER the subject restore above, which would otherwise overwrite the
        // widening for the one local that most often carries it: the `case`
        // subject itself (row b27b — `case h when UnknownZzzClass` then a use
        // after the `case`, reference-silent). Row b30b is the control: a `[1,
        // 2]` subject collapses to `Bot`, which does NOT propagate, and both
        // engines fire after the `case`.
        let widened_edges: Vec<(Facts, bool)> =
            edges.iter().cloned().zip(edge_terminates.iter().copied()).collect();
        propagate_widened(cenv, &widened_edges, writes_within(writes, case_span), case_span);
        kill_cenv_writes(writes_within(writes, case_span), case_span, cenv);
    }
}

/// Whether a branch body's final statement EXITS the surrounding control flow
/// — a `return`, an argument-less `next`/`break`, or a receiverless `raise` (a
/// conservative approximation of the reference's
/// `branch_unconditionally_exits?`, `statement_evaluator.rb:2836`; missing a
/// termination only loses narrowing, never adds one). Descends the pure
/// statement carriers
/// (`Statements`, and a `BeginRescue` with no rescue clauses and no ensure —
/// the lowered `else`-clause / parenthesized-group shape; a real
/// `begin`/`rescue` declines, its tail `return` may be skipped by a raise
/// before it).
pub(crate) fn branch_terminates(ast: &LoweredAst, body: &[NodeId]) -> bool {
    match body.last() {
        Some(&last) => stmt_terminates(ast, last),
        None => false,
    }
}

fn stmt_terminates(ast: &LoweredAst, id: NodeId) -> bool {
    match ast.get(id) {
        Node::Return { .. } => true,
        // An argument-less `next`/`break` (`Node::Other`'s `jump` tag). The
        // reference accepts them unconditionally — no in-block gate, no
        // loop-body special case — and the probe matrix reproduces that:
        // `next`/`break` in a block (`p1`/`p2`/`p3`), in a `while`/`until` body
        // (`p5`/`p5b`/`p5c`), in a `lambda`/`define_method`/`loop`/`times`
        // block (`q15`/`q16`/`r2`/`q18`) all narrow past the guard, and the
        // loop-carried rebind AFTER the use (`p6`, `r11` — the shape 3b-1
        // declined loop BODIES over) still fires there. We reach a strict
        // subset of that: a `while`/`until` BODY is never descended
        // (`Node::Loop`), a fact never escapes the block (`join_cenv` keeps
        // only `Bot` — probes `p9`/`p9b`/`p13`/`q10`/`r13`, all
        // reference-silent), and a rebind BEFORE the use kills it (`q17`).
        Node::Other { jump: Some(_), .. } => true,
        // A VALUED `next e` / `break e` exits the branch exactly like the
        // argument-less forms — `branch_unconditionally_exits?` does not look
        // at `NextNode`/`BreakNode`'s arguments. `redo`/`retry` carriers stay
        // excluded (the reference's accepted set is `next`/`break`/`return`).
        Node::Statements { kind: StatementsKind::Jump(kind), .. } => {
            matches!(kind, JumpKind::Next | JumpKind::Break)
        }
        // `EXIT_CALL_NAMES` (`raise throw exit abort fail`,
        // statement_evaluator.rb:9666) plus `exit!`: it is absent from the
        // syntactic set but its Kernel signature returns `bot`, so
        // `branch_terminates?` catches it through the type half — the same
        // shape the `If`/`Case`/`BeginRescue` arms below approximate.
        Node::Call { receiver: None, method, .. }
            if matches!(
                method.as_str(),
                "raise" | "throw" | "exit" | "exit!" | "abort" | "fail"
            ) =>
        {
            true
        }
        Node::BeginRescue { body, ensure_body, clauses, .. }
            if clauses.is_empty() && ensure_body.is_empty() =>
        {
            branch_terminates(ast, body)
        }
        // `branch_unconditionally_exits?` recurses into `if`/`unless` only
        // when BOTH arms exit — an `if` with no else falls through on the
        // falsey edge, so it never qualifies
        // (statement_evaluator.rb:9688). Without this arm a folded
        // arm's effects still landed where the reference drops the whole
        // scope (`if true; x = "s"; if c; raise; else; raise; end; end`
        // answered `for "a"` on the oracle — rigor-rs#942 review).
        Node::If { then_body, else_body, .. } => {
            !else_body.is_empty()
                && branch_terminates(ast, then_body)
                && branch_terminates(ast, else_body)
        }
        // `branch_terminates?`'s Bot half, approximated syntactically: a
        // `case`/`case…in` whose every arm exits types `Bot`. The else
        // clause must be present for a `case`/`when` (a missing else falls
        // through to `nil`); a `case`/`in` — whose branches lower to
        // `BeginRescue` carriers — cannot fall through at all (a non-match
        // raises `NoMatchingPatternError`), so every branch body exiting
        // already types `Bot`. The else body must exit whenever present.
        Node::Case { branches, else_body, .. } => {
            let all_in_branches = !branches.is_empty()
                && branches
                    .iter()
                    .all(|&b| matches!(ast.get(b), Node::BeginRescue { .. }));
            let falls_through = else_body.is_empty() && !all_in_branches;
            !falls_through
                && (else_body.is_empty() || branch_terminates(ast, else_body))
                && branches.iter().all(|&b| stmt_terminates(ast, b))
        }
        // A `when` clause's exit is its body's — its conditions only pick
        // WHICH body runs, they cannot complete the branch themselves.
        Node::When { body, .. } => branch_terminates(ast, body),
        // A `begin … rescue … end` exits only when every live VALUE path
        // exits: every rescue clause, plus the normally-completing path —
        // which is the protected body's own statements with no `else`,
        // but the ELSE's statements when one is present (`else` replaces
        // the body's value, so `begin; nil; rescue; raise; else; raise;
        // end` is `bot` on the oracle even though `nil` never exits).
        // The else joins unconditionally: `else; nil` — even unreachable
        // behind an exiting body — keeps the begin non-`bot` and the arm
        // live, and an empty `else; end` still contributes `nil`
        // (rigor-rs#952). `ensure` runs on either path and contributes no
        // value, so it does not count toward exiting.
        Node::BeginRescue { main_body, body, clauses, ensure_body, has_else, .. }
            if !clauses.is_empty() =>
        {
            // The else statements ride the flat `body` after every
            // clause's exceptions+body and before `ensure_body`
            // (ast/builder/control.rs); a real `begin` admits an `else`
            // only beside a `rescue`, and `has_else` counts an empty one
            // — whose missing statements leave an empty slice, a `nil`
            // path `branch_terminates` declines.
            let normal_path_exits = if *has_else {
                let else_start = main_body.len()
                    + clauses
                        .iter()
                        .map(|c| c.exceptions.len() + c.body.len())
                        .sum::<usize>();
                let else_end = body.len().saturating_sub(ensure_body.len());
                branch_terminates(ast, body.get(else_start..else_end).unwrap_or(&[]))
            } else {
                branch_terminates(ast, main_body)
            };
            normal_path_exits
                && clauses
                    .iter()
                    .all(|c| branch_terminates(ast, &c.body))
        }
        // A recovery carrier's children are flattened out of expression
        // structure (`raise X rescue nil` recovers the `raise` call), so only a
        // real sequence ends where its last child does.
        Node::Statements { body, kind: StatementsKind::Sequence, .. } => branch_terminates(ast, body),
        _ => false,
    }
}

/// `branch_certainly_exits?` (statement_evaluator.rb:9712) — the STRICT,
/// syntactic-only exit test `if_join` applies to the body arm beside an
/// `else`: a `return`/`next`/`break`, a receiverless `raise`/`fail`/`exit`/
/// `exit!`/`abort`/`throw` call (the reference additionally checks
/// `!scope.project_method_name?` — a decline-side gap the flat index
/// cannot see here), a statement list or else/paren carrier whose last
/// statement exits, an `if`/`unless` whose BOTH arms certainly exit, and
/// a `case`/`case…in` with a present else whose every branch body
/// certainly exits (`case_certainly_exits?`). A `begin`/`rescue` is
/// deliberately NOT recognised — it is absent from the reference's case
/// list — and neither is the `Bot` half [`branch_terminates`] adds.
pub(crate) fn branch_certainly_exits(ast: &LoweredAst, body: &[NodeId]) -> bool {
    match body.last() {
        Some(&last) => stmt_certainly_exits(ast, last),
        None => false,
    }
}

fn stmt_certainly_exits(ast: &LoweredAst, id: NodeId) -> bool {
    match ast.get(id) {
        Node::Return { .. } => true,
        Node::Other { jump: Some(_), .. } => true,
        Node::Statements { kind: StatementsKind::Jump(kind), .. } => {
            matches!(kind, JumpKind::Next | JumpKind::Break)
        }
        // `CERTAIN_EXIT_NAMES` — `exit!` is here and NOT in the looser
        // `EXIT_CALL_NAMES` [`stmt_terminates`] mirrors
        // (statement_evaluator.rb:9666 vs :9743).
        Node::Call { receiver: None, method, .. }
            if matches!(
                method.as_str(),
                "raise" | "fail" | "exit" | "exit!" | "abort" | "throw"
            ) =>
        {
            true
        }
        Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
            branch_certainly_exits(ast, body)
        }
        // The carriers `branch_certainly_exits?` DOES unwrap — an
        // `ElseNode`'s statements and a `ParenthesesNode`'s body. A real
        // `begin` node is not in its case list, so only the two carrier
        // identities qualify.
        Node::BeginRescue { body, .. }
            if ast.is_else_carrier(id) || ast.is_paren_carrier(id) =>
        {
            branch_certainly_exits(ast, body)
        }
        Node::If { then_body, else_body, .. } => {
            !else_body.is_empty()
                && branch_certainly_exits(ast, then_body)
                && branch_certainly_exits(ast, else_body)
        }
        // `case_certainly_exits?` — an else clause must be PRESENT for
        // either case kind (`!node.else_clause.nil?`,
        // statement_evaluator.rb:9736) — a `case`/`in` without one is not
        // recognised even though it cannot match-fall-through.
        Node::Case { branches, else_body, .. } => {
            !else_body.is_empty()
                && branch_certainly_exits(ast, else_body)
                && branches.iter().all(|&b| match ast.get(b) {
                    Node::When { body, .. } => branch_certainly_exits(ast, body),
                    Node::BeginRescue { body, .. } => branch_certainly_exits(ast, body),
                    _ => false,
                })
        }
        _ => false,
    }
}

/// Is `id` a binding VALUE whose carrier BOTH engines type `Dynamic`/`Top`?
///
/// The allow-list half of the carrier-fidelity fix
/// (docs/notes/20260808-narrowing-carrier-fidelity-fp.md). `narrow_class_other`
/// narrows a `Dynamic`/`Top` carrier only, so "we narrow only Dynamic" is a
/// SUBSET rule exactly while `Dynamic` means the same thing in both engines —
/// and it does not: rigor-rs collapses to `Dynamic[top]` a long tail of
/// carriers the reference types precisely (a `Logical` union, a `Range`, a
/// `Proc`, `self`, a `case`/`if` union, `defined?`, a loop's `nil`, …). On each
/// of those our gate fires where theirs declines — a live false positive.
///
/// Enumerating the coarse carriers is a losing game (`__method__`, `proc { }`,
/// `binding` and `defined?` all hide inside the same `Call`/`Statements`
/// carriers as the safe shapes), so the gate is an ALLOW-list instead, and every
/// member is oracle-measured as FIRING on the reference:
///
/// - a bare local that is itself narrowable (a parameter, or bound to a member
///   of this list) — `ctrl_param`, `ctrl_unbound`, `blockparam`, `allow_kwarg`,
///   `allow_optarg`, `allow_restarg`, `allow_block_arg`;
/// - an `@ivar` / `@@cvar` / `$gvar` read — the reference types none of them
///   (`ivar_read`, `gvar_read`, `cvar_read`);
/// - a call THROUGH such a receiver: an untyped receiver resolves no method on
///   either side, so the result is untyped on both (`plain_call_dyn`,
///   `index_read`, `safenav`, `block_call_dyn`, `call_chain`, `call_ivar_recv`,
///   `call_gvar_recv`, `allow_param_index`). `self` is deliberately NOT a
///   narrowable receiver — the reference resolves an in-source method through
///   it and gets that method's real return (`call_self_recv` is an FP), and
///   neither is a `ConstantRead` (`recv_const_float`).
///
/// Everything else declines. That costs coverage on carriers the reference does
/// narrow (`yield`, `super`, an implicit-self call, a `begin`/`ensure` value, a
/// `case` with no `else`, a constant receiver) — a strict subset, never an FP.
fn narrowable_binding(ast: &LoweredAst, id: NodeId, coarse: &HashSet<String>, depth: u32) -> bool {
    if depth == 0 {
        return false;
    }
    match ast.get(id) {
        Node::LocalVariableRead { name, .. } => !coarse.contains(name),
        Node::VariableRead { .. } => true,
        Node::Call { receiver: Some(r), .. } => {
            narrowable_binding(ast, *r, coarse, depth - 1)
        }
        _ => false,
    }
}

/// The local names in ONE scope whose binding is not a [`narrowable_binding`] —
/// the set the `is_a?`/`case-when` gate refuses to narrow.
///
/// Scope-wide rather than flow-sensitive: a name coarse at ANY binding site in
/// the scope is coarse throughout it. That is deliberately conservative (a
/// rebind never resurrects narrowability) and needs no extra env threading.
/// The scope is delimited by its statements' byte range MINUS the range of every
/// nested `def`/`class`/`module`, whose locals are their own scope's business
/// (each gets its own `coarse_locals` call).
///
/// Three binding kinds enter the set:
/// - a `LocalVariableWrite` whose value is not a narrowable carrier;
/// - EVERY `LocalVariableOpWrite` (`h ||= {}` is a union on the reference —
///   probe `logical_orassign` — and our op-write arm types it `Dynamic[top]`);
/// - every `rescue => e` capture (the reference binds the exception CLASS).
///
/// `MultiWrite` targets are deliberately absent: destructuring loses precision
/// on both sides, and all three measured shapes (`multiwrite`, `mw_from_call`,
/// `mw_from_logical`) fire on the reference.
fn coarse_locals(ast: &LoweredAst, body: &[NodeId]) -> HashSet<String> {
    let mut coarse: HashSet<String> = HashSet::new();
    let Some(lo) = body.iter().map(|&s| ast.get(s).span().0).min() else {
        return coarse;
    };
    let hi = body.iter().map(|&s| ast.get(s).span().1).max().unwrap_or(lo);
    let nested: Vec<rigor_parse::Span> = ast
        .iter()
        .filter(|(_, n)| {
            matches!(
                n,
                Node::Definition { .. } | Node::ClassDef { .. } | Node::ModuleDef { .. }
            )
        })
        .map(|(_, n)| n.span())
        .filter(|s| lo <= s.0 && s.1 <= hi)
        .collect();
    let inside = |sp: rigor_parse::Span| {
        lo <= sp.0 && sp.1 <= hi && !nested.iter().any(|n| n.0 <= sp.0 && sp.1 <= n.1)
    };
    // (name, Some(value)) — narrowable iff the value is; (name, None) —
    // unconditionally coarse.
    let mut bindings: Vec<(String, Option<NodeId>)> = Vec::new();
    for (_, n) in ast.iter() {
        match n {
            Node::LocalVariableWrite { name, value, span, .. } if inside(*span) => {
                bindings.push((name.clone(), Some(*value)));
            }
            Node::LocalVariableOpWrite { name, span, .. } if inside(*span) => {
                bindings.push((name.clone(), None));
            }
            Node::BeginRescue { clauses, span, .. } if inside(*span) => {
                for c in clauses {
                    if let Some(b) = &c.bound_name {
                        bindings.push((b.clone(), None));
                    }
                }
            }
            _ => {}
        }
    }
    // Fixpoint: a name whose value reads a name that just became coarse is
    // itself coarse. Monotone (the set only grows), so it terminates.
    loop {
        let mut changed = false;
        for (name, value) in &bindings {
            if coarse.contains(name) {
                continue;
            }
            let ok = value.is_some_and(|v| narrowable_binding(ast, v, &coarse, 32));
            if !ok {
                coarse.insert(name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    coarse
}

/// The conservative branch join for a conditional / `case` / block call.
///
/// A [`ClassFact::Narrowed`] fact never survives a merge in this slice (the
/// reference's union of `Nominal[C]` with itself would, but the decline predates
/// this note and stays). A [`ClassFact::Bot`] fact present on ENTRY does
/// survive, because the reference's join is `Bot | Bot = Bot` (probes
/// `bot_after_inner`, `bot_after_begin`, `bot_after_block_call`) — but only
/// while EVERY edge still carries it. `edges` are the per-edge fact maps the
/// branches were walked with; an edge that REBOUND the local dropped the fact
/// from its own clone, and the reference's union of `Bot` with the new binding
/// is that binding, so the merged fact goes too (probes `bot_rebind_use`,
/// `bot_block_rebind`). An empty `edges` means the construct was not descended,
/// so there is no edge evidence and every entry `Bot` rides through — the
/// caller pairs that with a span-based [`kill_cenv_writes`].
///
/// Facts the EDGES established are not in `cenv` at all (edges walk clones), so
/// nothing minted inside a branch can leak out through here.
///
/// Stage 3a-3: EVERY chain fact dies at a join, unconditionally — a chain `Bot`
/// included, unlike the LOCAL `Bot` the retain below preserves. The reference
/// agrees that a chain narrowing established inside a branch does not escape it
/// (probe `n_escape_after_if`, reference-silent), and a `Bot` that escaped a
/// branch would SUPPRESS rather than merely go silent, so the blanket wipe is
/// the conservative side. The one place a chain fact must outlive the join is the
/// early-return propagation, which [`Typer::class_flow_if`] re-seeds explicitly
/// from a PRE-join snapshot — see its comment on the sequential-disjoint
/// hazard.
///
/// Join-retention slice (2026-08-09): the two CONDITIONAL callers
/// ([`Typer::class_flow_if`], [`Typer::class_flow_case`]) pair this wipe with
/// [`retain_joined_facts`], which puts back every pre-join fact the edges left
/// untouched. The wipe itself stays exactly as written, because the other four
/// callers pass NO edges (a construct that was not descended) and must keep the
/// blanket-clear + span-kill discipline.
fn join_cenv(cenv: &mut Facts, edges: &[Facts]) {
    cenv.locals.retain(|name, fact| {
        matches!(fact, ClassFact::Bot | ClassFact::Widened)
            && edges.iter().all(|edge| edge.locals.get(name) == Some(fact))
    });
    cenv.chains.clear();
}

/// Carry a [`ClassFact::Widened`] established INSIDE a branch back OUT of the
/// join — the half [`join_cenv`] structurally cannot do, since its `retain`
/// only ever keeps facts that were already in `cenv` before the branch.
///
/// This is the join rule of upstream #533 item 4 stated positively: `untyped`
/// ABSORBS at a join (`Dynamic ∪ Array = Dynamic`), so one edge widening a local
/// widens it for everything after the construct. Row b2b is the measurement —
/// `h.t1 if h.is_a?(UnknownZzzClass)` then a bare `h.t2`, where the reference is
/// silent on BOTH calls and rigor-rs fired on both — and rows b26c and b27b
/// repeat it past an intervening `if` and out of a `case`.
///
/// Two filters, each pinned by a must-still-fire row:
///
/// * a TERMINATING edge contributes nothing. `return if h.is_a?(UnknownZzzClass)`
///   widens only the path that returns; the code after the `if` runs on the
///   FALSEY edge, where the carrier is untouched, and both engines fire there
///   (rows b24/b29/b33). Leaking the widening out of the terminating edge would
///   silence all three.
/// * a local REBOUND anywhere inside the construct's span contributes nothing —
///   the reference's join is then the new binding, not the widening (row b18,
///   `if h.is_a?(U); h = Array.new; end`, firing on both engines). This mirrors
///   [`retain_joined_facts`]'s own `written` filter.
///
/// Only [`Typer::class_flow_if`] and [`Typer::class_flow_case`] call it. The
/// BLOCK join deliberately does not: row b17b measures a widening established
/// inside a block NOT escaping it.
fn propagate_widened(
    cenv: &mut Facts,
    edges: &[(Facts, bool)],
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
) {
    let written =
        |name: &str| writes.iter().any(|(ws, n)| n == name && span.0 <= ws.0 && ws.1 <= span.1);
    for (edge, terminates) in edges {
        if *terminates {
            continue;
        }
        for (name, fact) in &edge.locals {
            if *fact == ClassFact::Widened && !written(name) {
                cenv.locals.insert(name.clone(), ClassFact::Widened);
            }
        }
    }
}

/// Put back, after [`join_cenv`], every PRE-join fact that survived the
/// conditional untouched — the reference's `Scope#join` keeps a local's type
/// whenever both edges agree on it (`scope.rb:680`), and a fact minted BEFORE
/// the conditional is on both edges by construction. Master wiped them all, so
/// a fact died at ANY later `if`/`unless`/`case`, terminating or not, related or
/// not (the 2026-08-09 probe matrix: 10 of 14 rows diverged, every one a
/// coverage loss, plus one live FP — a disjoint re-guard AFTER an intervening
/// `if` minted against the wiped env and witnessed where the reference's meet
/// had already reached `Bot`).
///
/// A fact is restored only when EVERY edge still carries it IDENTICALLY. That
/// single test subsumes the spec's separate criteria:
///
/// - a REBIND inside a branch removed the fact from that edge's clone
///   (`branch_rebind_one_side` / `write_to_a_in_if`, both reference-silent for
///   us — the reference's real union `1 | String` is the separate widen gap);
/// - the conditional's OWN guard targets moved on at least one edge whenever the
///   guard did anything (`if a.is_a?(Hash)` after a `String` guard leaves `Bot`
///   on the truthy edge and `String` on the falsey edge), so the sequential-meet
///   and `Bot`-collapse results are never resurrected;
/// - a call on a chain ROOT inside a branch fired `invalidate_chain_after_call`
///   on that edge, which is invisible to `writes` (probe
///   `chain_call_on_root_in_branch`, reference-silent).
///
/// `writes` + `span` add the span-containment kill on top, for the rebinds an
/// edge cannot see — a `case`/`in` clause is not descended, so its rebinds are
/// invisible to the edge evidence (`case_in_rebinds_target`, reference-silent).
/// `chains` is the caller's gate for the same reason: only a construct whose
/// every branch was DESCENDED has trustworthy chain edges.
///
/// Facts the edges MINTED are absent from `pre` (edges walk clones of it), so
/// nothing established inside a branch escapes through here — the block/loop
/// escape rules (probes `n_escape_after_if`, p9/p13) are untouched.
fn retain_joined_facts(
    cenv: &mut Facts,
    pre: &Facts,
    edges: &[Facts],
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    chains: Option<&HashSet<String>>,
) {
    if edges.is_empty() {
        return;
    }
    let written =
        |name: &str| writes.iter().any(|(ws, n)| n == name && span.0 <= ws.0 && ws.1 <= span.1);
    for (name, fact) in &pre.locals {
        if written(name) {
            continue;
        }
        if edges.iter().all(|edge| edge.locals.get(name) == Some(fact)) {
            cenv.locals.insert(name.clone(), fact.clone());
        }
    }
    let Some(predicate_locals) = chains else { return };
    for (addr, fact) in &pre.chains {
        if written(&addr.0) || predicate_locals.contains(&addr.0) {
            continue;
        }
        if edges.iter().all(|edge| edge.chains.get(addr) == Some(fact)) {
            cenv.chains.insert(addr.clone(), fact.clone());
        }
    }
}

/// Every local NAME read or written anywhere inside `span` — used as the
/// chain-restore gate in [`retain_joined_facts`].
///
/// A conditional's PREDICATE gets no edge evidence of its own: the edges are
/// clones taken after it ran, so a predicate that narrowed a chain address in a
/// way [`Typer::analyse_predicate`] does not RECOGNISE leaves both edges
/// agreeing on the stale incoming fact. `guard_predicate` requires a bare LOCAL
/// operand, so `String === h.last` and `h.last.nil?` are not chain guards at
/// all — and `return unless String === h.last` after a disjoint `is_a?` guard is
/// reference-SILENT (row `chain_caseeq_disjoint`), so restoring there would be a
/// live false positive. Any mention of the ROOT in the predicate therefore
/// declines the restore for every address rooted at it, mirroring the
/// any-mention widening [`Facts::kill_chains_rooted_at`] already applies to
/// calls.
/// The local read/write NAMES inside `span` — the [`ReachIndex`] span scan
/// (the arena walk this used to run once per `if`/`case` predicate,
/// rigor-rs#424's residual quadratic).
fn locals_in_span(typer: &Typer<'_>, ast: &LoweredAst, span: rigor_parse::Span) -> HashSet<String> {
    let mut out = HashSet::new();
    for n in typer.reach_index(ast).nodes_within(ast, span) {
        let name = match ast.get(n) {
            Node::LocalVariableRead { name, .. }
            | Node::LocalVariableWrite { name, .. }
            | Node::LocalVariableOpWrite { name, .. } => name,
            _ => continue,
        };
        out.insert(name.clone());
    }
    out
}

/// The stable single-hop chain address of `id`, if it has one — the port of the
/// reference's `stable_chain_address` (`narrowing.rb:1826`) restricted to LOCAL
/// roots.
///
/// `Some((root, method))` iff `id` is a `Call` whose receiver is a bare
/// `LocalVariableRead`, with NO arguments, NO block and NO safe-nav. The
/// reference's ivar arm is DECLINED: the arena's `VariableRead` carries no name
/// (spec row `c7b`, a recorded coverage gap that needs a lowering change first).
///
/// Every other decline is measured reference-silent as well: arguments on the
/// hop (`c7e`), a block on the hop (`m_block_on_hop`), a two-hop chain
/// (`m_two_hop`). A safe-nav hop is the one exception — the reference fires
/// (`m_safe_nav_hop`) and we decline, matching `stable_chain_address`'s own
/// shape gate as ported plus the slice-wide safe-nav decline.
fn stable_chain_address(ast: &LoweredAst, id: NodeId) -> Option<ChainAddr> {
    let Node::Call { receiver: Some(r), method, args, block_body, safe_nav, .. } = ast.get(id)
    else {
        return None;
    };
    if *safe_nav || !args.is_empty() || !block_body.is_empty() {
        return None;
    }
    let Node::LocalVariableRead { name, .. } = ast.get(*r) else { return None };
    Some((name.clone(), method.clone()))
}

/// Does this predicate operand's subtree contain a `=~` whose RECEIVER is not a
/// bare variable read — i.e. the `/(?<name>…)/ =~ str` shape, which binds every
/// named capture group as a local?
///
/// Prism models it as a `MatchWriteNode`; the arena has no lowering for one, so
/// the bound locals appear only as unbound reads and the narrowing gate treats
/// them as untyped. The reference binds them to `String`, so a following
/// `v.is_a?(Hash)` reaches `Bot` there and witnesses nothing (probe
/// `matchwrite`). The binding is arena-INVISIBLE, so the only safe answer is to
/// decline the whole compound predicate. `v =~ /a/` — a variable receiver —
/// binds nothing and is deliberately excluded (`matchop_keep` fires on both
/// engines).
fn regex_binding_match(ast: &LoweredAst, id: NodeId, depth: u32) -> bool {
    if depth == 0 {
        return true; // out of budget ⇒ answer conservatively
    }
    match ast.get(id) {
        Node::Call { receiver, method, args, .. } => {
            if method == "=~"
                && !matches!(
                    receiver.map(|r| ast.get(r)),
                    Some(Node::LocalVariableRead { .. } | Node::VariableRead { .. })
                )
            {
                return true;
            }
            receiver.is_some_and(|r| regex_binding_match(ast, r, depth - 1))
                || args.iter().any(|&a| regex_binding_match(ast, a, depth - 1))
        }
        Node::Logical { left, right, .. } => {
            regex_binding_match(ast, *left, depth - 1)
                || regex_binding_match(ast, *right, depth - 1)
        }
        Node::Statements { body, .. } => {
            body.iter().any(|&s| regex_binding_match(ast, s, depth - 1))
        }
        _ => false,
    }
}

/// The JOIN of two edges' guard maps — the reference's `Scope#join`, which
/// unions the two scopes' types per local (`analyse_and`'s falsey edge,
/// `analyse_or`'s truthy edge; `narrowing.rb:2631,2640`).
///
/// A local absent from EITHER side carries its unchanged (un-narrowed) type
/// there, and a union with the unchanged type is the unchanged type — so the
/// join keeps only locals present on BOTH sides (probes `c1g`, `f12`,
/// `a_two_locals_else`, `b2_and_bang_two_locals`, `u_and_or_falsey`, all
/// reference-silent). Class names union: identical on both sides the fact
/// survives intact and MINTS (`b2_and_bang_same`, `x_or_same_class` both fire
/// on the reference); different, the reference narrows to a real union
/// (`Hash | String` — `b2_and_bang_diff`, `x_or_diff_class`) which stage 3a-4
/// would represent and this slice declines by keeping the fact un-mintable.
/// `exact` weakens to `is_a?` semantics (the STRONGER collapse requirement, so
/// the join never suppresses more than either side alone).
fn join_guards(a: &GuardMap, b: &GuardMap) -> GuardMap {
    let mut out: GuardMap = Vec::new();
    for (target, ga) in a {
        let Some((_, gb)) = b.iter().find(|(n, _)| n == target) else { continue };
        let mut classes = ga.classes.clone();
        for class in &gb.classes {
            if !classes.contains(class) {
                classes.push(class.clone());
            }
        }
        out.push((
            target.clone(),
            GuardFact {
                classes,
                exact: ga.exact && gb.exact,
                mintable: ga.mintable && gb.mintable,
                // Both sides address the SAME node when the target is a chain
                // (the join keys on the address), so either id is the carrier
                // gate's subject; `a`'s is taken for determinism.
                chain_call: ga.chain_call,
            },
        ));
    }
    out
}

/// Kill the class-narrowing fact of every local whose recorded write/mutation
/// span is contained in `span` — the `cenv` counterpart of
/// [`widen_flow_writes`].
fn kill_cenv_writes(
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    cenv: &mut Facts,
) {
    for (wspan, name) in writes {
        if span.0 <= wspan.0 && wspan.1 <= span.1 {
            cenv.kill_local(name);
        }
    }
}

/// [`kill_cenv_writes`] restricted to [`ClassFact::Narrowed`], for the sites
/// whose contents were DESCENDED (so a real rebind already removed the fact
/// through a write arm) and where the recorded span therefore stands for a
/// MUTATION — a `MUTATOR_METHODS` receiver, a mutated argument position.
/// A mutation widens a CARRIER, and neither `Bot` nor `Widened` has a carrier to
/// widen: the reference keeps `Bot` across `h.push(3)` and stays silent
/// afterwards (probe `bot_mutator_use`), and it keeps the #533 widening there
/// too (row b32b — a mutation between the guard and the use leaves both engines
/// silent). Only the narrowing fact dies here.
///
/// Stage 3a-3: a CHAIN fact rooted at the named local dies here unconditionally
/// — a chain `Bot` included (a mutated root invalidates the ADDRESS, so the
/// collapse no longer describes anything), and a mutation of the root is exactly the
/// "intervening call against the same root receiver" the reference drops on
/// (`invalidate_chain_after_call`; probe `n_root_mutator`, reference-silent).
fn kill_cenv_narrowed(
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    cenv: &mut Facts,
) {
    for (wspan, name) in writes {
        if span.0 <= wspan.0 && wspan.1 <= span.1 {
            cenv.kill_chains_rooted_at(name);
            if matches!(cenv.locals.get(name), Some(ClassFact::Narrowed(_))) {
                cenv.locals.remove(name);
            }
        }
    }
}
