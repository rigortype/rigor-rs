//! ADR-47 WD5 / upstream #627 (`d20d6f90`) — the **dead arm of a decidable Ruby
//! version guard reports nothing**.
//!
//! A multi-version gem selects an API generation with a guard the analyzer can
//! decide on its own runtime:
//!
//! ```ruby
//! if RUBY_VERSION < "2.7."
//!   append_cflags("-std=gnu99")     # never runs on the Ruby being checked with
//! end
//! ```
//!
//! The dead arm is honest code for an older Ruby, so a diagnostic inside it is a
//! false positive. Two reference files are ported here, verbatim in envelope:
//!
//! * `lib/rigor/inference/version_guard.rb` — [`verdict`], a PURE function of
//!   the lowered AST answering `:truthy` / `:falsey` for a decidable guard.
//! * `lib/rigor/analysis/check_rules/dead_version_guard_arms.rb` —
//!   [`filter_dead_version_guard_arms`], a SPAN FILTER dropping every diagnostic
//!   that lands inside a dead arm.
//!
//! The reference ALSO elides the dead arm's writes from the post-`if` scope
//! (`StatementEvaluator#eval_if`). That half is typing precision, not FP safety,
//! and is deliberately NOT ported (a recorded coverage gap: `x = 1; x = "s" if
//! RUBY_VERSION < "3.4"; x.typo` fires on the reference for `1` and is silent
//! here).
//!
//! # The reference Ruby
//!
//! The reference reads the ANALYZER's own `RUBY_VERSION` / `RUBY_ENGINE`
//! (`Configuration#target_ruby` is deliberately not consulted — it is a Prism
//! *parse* version). rigor-rs has no Ruby runtime to read them from, so the pair
//! is a compile-time constant ([`HOST_RUBY_VERSION`] / [`HOST_RUBY_ENGINE`]) with
//! an environment override:
//!
//! | variable | overrides | default |
//! |---|---|---|
//! | `RIGOR_RUBY_VERSION` | the `RUBY_VERSION` a version guard folds against | [`HOST_RUBY_VERSION`] |
//! | `RIGOR_RUBY_ENGINE`  | the `RUBY_ENGINE` a version guard folds against  | [`HOST_RUBY_ENGINE`] |
//!
//! (These are unrelated to `RIGOR_RUBY` / `RIGOR_NO_RUBY`, which pick the ADR-0036
//! sidecar posture — see `rigor_cli::ruby_mode`.) An EMPTY value is ignored, so
//! `RIGOR_RUBY_VERSION=` reads as "unset" exactly like the reference's
//! `runtime_value` rejecting an empty String.
//!
//! # What is folded (allow-list), and what deliberately is not
//!
//! Folded: ONE comparison call (`< <= > >= == !=`, no block, exactly one plain
//! argument) whose two operands are both readable —
//!
//! * a String literal;
//! * a BARE `RUBY_VERSION` read, compared with **String** semantics (lexical, so
//!   `"4.0.5" < "3.4"` is false and `RUBY_VERSION >= "3.10"` is famously false on
//!   3.10 — folding reproduces the program's real behaviour);
//! * a BARE `RUBY_ENGINE` read, **equality only**;
//! * `Gem::Version.new(<one of those>)` on **both** sides (a mixed wrapped/bare
//!   pair is never folded: `Gem::Version#<=>` answers nil for a String operand,
//!   so the program raises and there is no arm to pick).
//!
//! Not folded, both arms stay live: `RUBY_PLATFORM`, `<=>`, `!` / `&&` / `||`
//! compositions, `case` subjects, a value read through a local, two bare String
//! literals, and `.to_f` spellings. A `::RUBY_VERSION` (leading-`::`) spelling is
//! declined too: the reference accepts `RUBY_VERSION` only as a
//! `Prism::ConstantReadNode`, and the port's lowering collapses `::RUBY_VERSION`
//! into the same `ConstantRead` — so the bare-name arm re-checks the SOURCE
//! SPELLING (measured: the oracle fires on `… if ::RUBY_VERSION < "3.4"`).
//! `::Gem::Version.new(…)` on the other hand IS accepted, because the reference
//! reads that receiver through `qualified_name_or_nil`, which renders
//! `::Gem::Version` as `"Gem::Version"`.
//!
//! # `Psych::VERSION`: unreadable, so BOTH arms are dead
//!
//! The reference curates exactly one `X::VERSION` — `VERSION_CONSTANTS =
//! Set["Psych::VERSION"]` — and reads it out of its own runtime. rigor-rs has no
//! runtime, and which psych the host loads is not a property of the pin.
//!
//! DECLINING it (both arms live) is **not** a safe under-claim, and the probe
//! says so: `"abc".typo if Gem::Version.new(Psych::VERSION) <
//! Gem::Version.new("3.1.0")` is SILENT on the oracle at `ffb456b0` and would
//! FIRE here — a false positive, the exact shape of `mail/lib/mail/yaml.rb` that
//! motivated upstream #627. Baking a `HOST_PSYCH_VERSION` constant is worse: a
//! stale value picks the WRONG arm and reports inside the arm the oracle killed.
//!
//! So a curated-but-unreadable `X::VERSION` yields [`Verdict::Unreadable`] — "a
//! decidable-shaped guard whose value this port cannot read" — and the filter
//! drops BOTH arms. Whichever arm the reference kills, the port has already
//! dropped it; the other arm is a recorded coverage gap, never a false positive.
//! The set stays the reference's ALLOW-LIST: a non-curated `Foo::VERSION` is
//! plain-undecidable on both engines and keeps both arms live.

use crate::Diagnostic;
use rigor_parse::{LoweredAst, Node, NodeId, Span};

/// The `RUBY_VERSION` a version guard folds against when `RIGOR_RUBY_VERSION` is
/// unset: the Ruby the parity oracle runs under on the standing harness host
/// (`ruby -e 'p RUBY_VERSION'` = `"4.0.5"`, 2026-09-09).
pub const HOST_RUBY_VERSION: &str = "4.0.5";

/// The `RUBY_ENGINE` twin of [`HOST_RUBY_VERSION`] (`RIGOR_RUBY_ENGINE`).
pub const HOST_RUBY_ENGINE: &str = "ruby";

/// The reference Ruby a version guard is decided against — the port's stand-in
/// for the reference reading its OWN `RUBY_VERSION` / `RUBY_ENGINE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RubyRuntime {
    pub version: String,
    pub engine: String,
}

impl Default for RubyRuntime {
    fn default() -> Self {
        RubyRuntime {
            version: HOST_RUBY_VERSION.to_string(),
            engine: HOST_RUBY_ENGINE.to_string(),
        }
    }
}

impl RubyRuntime {
    /// Resolve from explicit override strings — the testable half of
    /// [`RubyRuntime::from_env`]. An absent OR EMPTY override falls back to the
    /// host constant (the reference's `runtime_value` likewise rejects an empty
    /// String).
    pub fn from_overrides(version: Option<&str>, engine: Option<&str>) -> Self {
        let pick = |over: Option<&str>, default: &str| {
            over.map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(default)
                .to_string()
        };
        RubyRuntime {
            version: pick(version, HOST_RUBY_VERSION),
            engine: pick(engine, HOST_RUBY_ENGINE),
        }
    }

    /// The process-wide reference Ruby: `RIGOR_RUBY_VERSION` / `RIGOR_RUBY_ENGINE`
    /// over the host constants.
    pub fn from_env() -> Self {
        let version = std::env::var("RIGOR_RUBY_VERSION").ok();
        let engine = std::env::var("RIGOR_RUBY_ENGINE").ok();
        RubyRuntime::from_overrides(version.as_deref(), engine.as_deref())
    }
}

/// The decided branch of a version guard (the reference's `:truthy` / `:falsey`),
/// plus the port-only [`Verdict::Unreadable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The predicate is true on the reference Ruby ⇒ the `else`/`elsif` chain is
    /// dead (for an `unless`, the body is).
    Truthy,
    /// The predicate is false ⇒ the body is dead (for an `unless`, the `else`).
    Falsey,
    /// A version guard whose SHAPE the reference folds but whose value this port
    /// cannot read — today exactly `Psych::VERSION`. BOTH arms are treated as
    /// dead: the reference kills one of them and the port cannot tell which, so
    /// killing both is the only FP-safe answer (see the module docs).
    Unreadable,
}

// ---------------------------------------------------------------------------
// The filter (reference `DeadVersionGuardArms`)
// ---------------------------------------------------------------------------

/// Drop every diagnostic that lands inside the dead arm of a decidable version
/// guard, against the process's reference Ruby ([`RubyRuntime::from_env`]).
///
/// MUST run BEFORE `suppression_marker_diagnostics` joins the list: a malformed
/// `# rigor:disable` marker inside a dead arm is still a real authoring error, so
/// `suppression.*` is never dropped (the reference gets that from ORDER alone;
/// the explicit rule-id guard here makes the contract independent of the call
/// site's ordering).
pub fn filter_dead_version_guard_arms(
    diagnostics: Vec<Diagnostic>,
    ast: &LoweredAst,
    source: &str,
) -> Vec<Diagnostic> {
    filter_dead_version_guard_arms_with(diagnostics, ast, source, &RubyRuntime::from_env())
}

/// [`filter_dead_version_guard_arms`] against an explicit reference Ruby.
pub fn filter_dead_version_guard_arms_with(
    diagnostics: Vec<Diagnostic>,
    ast: &LoweredAst,
    source: &str,
    runtime: &RubyRuntime,
) -> Vec<Diagnostic> {
    // The scan is a whole-file walk, so it is paid only when there is something
    // to drop (reference `filter`'s two early returns).
    if diagnostics.is_empty() {
        return diagnostics;
    }
    let arms = dead_arm_spans(ast, source, runtime);
    if arms.is_empty() {
        return diagnostics;
    }
    diagnostics
        .into_iter()
        .filter(|d| {
            d.rule_id.starts_with("suppression.")
                || !arms.iter().any(|arm| covers(*arm, d.start_offset))
        })
        .collect()
}

/// The source ranges of every dead version-guard arm in the file (reference
/// `DeadVersionGuardArms.scan`).
///
/// The reference's walk does not DESCEND into a dead arm; this one visits every
/// node, which is equivalent for the filter — a guard nested inside a dead arm
/// can only contribute ranges the dead arm already covers.
pub fn dead_arm_spans(ast: &LoweredAst, source: &str, runtime: &RubyRuntime) -> Vec<Span> {
    let mut arms = Vec::new();
    for (_, node) in ast.iter() {
        let Node::If { predicate, then_body, else_body, is_unless, .. } = node else {
            continue;
        };
        let Some(verdict) = verdict(ast, *predicate, source, runtime) else {
            continue;
        };
        // `unless` runs its body on the FALSEY edge, so the arms are swapped.
        // `else_body` holds AT MOST one lowered node — the `elsif` chain's
        // `IfNode` or the `else` clause's carrier — so its hull IS the
        // reference's `node.subsequent` / `node.else_clause` location.
        let (body_dead, subsequent_dead) = match (verdict, *is_unless) {
            (Verdict::Falsey, false) | (Verdict::Truthy, true) => (true, false),
            (Verdict::Truthy, false) | (Verdict::Falsey, true) => (false, true),
            (Verdict::Unreadable, _) => (true, true),
        };
        if body_dead {
            arms.extend(span_hull(ast, then_body));
        }
        if subsequent_dead {
            arms.extend(span_hull(ast, else_body));
        }
    }
    arms
}

/// The smallest span covering every node in `ids` (the reference's
/// `StatementsNode#location`), or `None` for an absent arm — `foo if
/// RUBY_VERSION >= "3.1"` has no `else`, and `if COND; end` has no body.
fn span_hull(ast: &LoweredAst, ids: &[NodeId]) -> Option<Span> {
    let mut it = ids.iter().map(|&id| ast.get(id).span());
    let first = it.next()?;
    Some(it.fold(first, |acc, s| (acc.0.min(s.0), acc.1.max(s.1))))
}

/// Whether a diagnostic anchored at `offset` falls inside `arm` (end EXCLUSIVE,
/// exactly like the reference's `covers?` — so a one-line guard
/// `RUBY_VERSION >= "3.1" ? a(1) : a(1, 2)` drops only the dead half).
fn covers(arm: Span, offset: usize) -> bool {
    arm.0 <= offset && offset < arm.1
}

// ---------------------------------------------------------------------------
// The verdict (reference `Inference::VersionGuard`)
// ---------------------------------------------------------------------------

/// The operand kinds the reference's `read_operand` produces. `LiteralString`
/// and `VersionString` and `Engine` all carry a plain Ruby String (the
/// `STRING_KINDS` set `Gem::Version.new` may wrap); `Engine` additionally
/// refuses every ORDERING comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Operand {
    /// A String literal in the source (`"3.4"`).
    LiteralString(String),
    /// A bare `RUBY_VERSION` read, resolved to the reference Ruby's version.
    VersionString(String),
    /// A bare `RUBY_ENGINE` read, resolved to the reference Ruby's engine.
    Engine(String),
    /// `Gem::Version.new(<readable>)`.
    GemVersion(GemVersion),
    /// A curated `X::VERSION` (`Psych::VERSION`): the reference's `:string` kind,
    /// value UNKNOWN to the port. See the module docs.
    UnreadableString,
    /// `Gem::Version.new(<unreadable>)`: the `:gem_version` kind, value unknown.
    UnreadableGemVersion,
}

/// The reference's operand KINDS, which decide comparability independently of the
/// values (`comparable_kinds?`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    LiteralString,
    VersionString,
    Engine,
    GemVersion,
}

impl Operand {
    fn kind(&self) -> Kind {
        match self {
            Operand::LiteralString(_) => Kind::LiteralString,
            Operand::VersionString(_) | Operand::UnreadableString => Kind::VersionString,
            Operand::Engine(_) => Kind::Engine,
            Operand::GemVersion(_) | Operand::UnreadableGemVersion => Kind::GemVersion,
        }
    }

    fn is_unreadable(&self) -> bool {
        matches!(self, Operand::UnreadableString | Operand::UnreadableGemVersion)
    }
}

/// `Inference::VersionGuard::VERSION_CONSTANTS` — the reference's curated
/// `X::VERSION` allow-list, read from ITS runtime and unreadable from here.
const VERSION_CONSTANTS: &[&str] = &["Psych::VERSION"];

/// `:truthy` / `:falsey` for a decidable version-guard predicate, `None` when the
/// guard is undecidable (both arms stay live — the pre-existing behaviour).
pub fn verdict(
    ast: &LoweredAst,
    predicate: NodeId,
    source: &str,
    runtime: &RubyRuntime,
) -> Option<Verdict> {
    let Node::Call { receiver, method, args, block_body, args_all_plain, .. } = ast.get(predicate)
    else {
        return None;
    };
    if !is_comparison(method) {
        return None;
    }
    // Reference: `node.block`, `node.receiver.nil?`, and `arguments.length == 1`.
    // `args_all_plain` stands in for "one ORDINARY positional argument": the
    // lowering does not keep splat / keyword / block-pass arguments in `args`, so
    // without it `RUBY_VERSION < *pair` would read as a one-argument call.
    if !block_body.is_empty() || !*args_all_plain || args.len() != 1 {
        return None;
    }
    let receiver = (*receiver)?;
    let left = read_operand(ast, receiver, source, runtime)?;
    let right = read_operand(ast, args[0], source, runtime)?;
    decide(&left, &right, method)
}

/// The comparison operators whose result is a branch verdict. `<=>` is excluded
/// on purpose (it yields -1/0/1, not a verdict).
fn is_comparison(method: &str) -> bool {
    matches!(method, "<" | "<=" | ">" | ">=" | "==" | "!=")
}

fn is_equality(method: &str) -> bool {
    matches!(method, "==" | "!=")
}

/// Reads one side of the comparison, or `None` when it is not readable.
fn read_operand(
    ast: &LoweredAst,
    node: NodeId,
    source: &str,
    runtime: &RubyRuntime,
) -> Option<Operand> {
    match ast.get(node) {
        Node::StringLit { value, .. } => Some(Operand::LiteralString(value.clone())),
        Node::ConstantRead { name, span } => {
            // The reference reads `RUBY_VERSION` / `RUBY_ENGINE` only from a
            // `Prism::ConstantReadNode` — a BARE name — and a curated
            // `X::VERSION` only from a `Prism::ConstantPathNode`. The port's
            // lowering collapses both into this one variant, so re-derive "bare"
            // from the rendered name plus the source spelling: a `ConstantPathNode`
            // either renders a `::` separator (`Psych::VERSION`) or is spelt with a
            // leading `::` (`::RUBY_VERSION` renders as bare `"RUBY_VERSION"`, and
            // only the source slice tells the two apart).
            let bare = !name.contains("::") && source.get(span.0..span.1) == Some(name.as_str());
            if bare {
                match name.as_str() {
                    "RUBY_VERSION" => Some(Operand::VersionString(runtime.version.clone())),
                    "RUBY_ENGINE" => Some(Operand::Engine(runtime.engine.clone())),
                    _ => None,
                }
            } else if VERSION_CONSTANTS.contains(&name.as_str()) {
                // A curated `X::VERSION`, readable by the reference and not by
                // the port: a decidable SHAPE with an unknown value.
                Some(Operand::UnreadableString)
            } else {
                None
            }
        }
        Node::Call { receiver, method, args, block_body, args_all_plain, .. } => {
            // `Gem::Version.new(<readable>)`. The inner operand must be a plain
            // version String, and `Gem::Version.correct?` must accept it — a
            // malformed literal raises at runtime, so it keeps both arms live.
            if method != "new" || !block_body.is_empty() || !*args_all_plain || args.len() != 1 {
                return None;
            }
            let recv = (*receiver)?;
            // `Gem::Version` and `::Gem::Version` both render as `"Gem::Version"`,
            // and the reference accepts both (`qualified_name_or_nil`).
            match ast.get(recv) {
                Node::ConstantRead { name, .. } if name == "Gem::Version" => {}
                _ => return None,
            }
            let inner = read_operand(ast, args[0], source, runtime)?;
            let text = match &inner {
                Operand::LiteralString(s) | Operand::VersionString(s) | Operand::Engine(s) => s,
                // The reference cannot check `Gem::Version.correct?` on a value
                // the port never read, so the whole guard stays unreadable.
                Operand::UnreadableString => return Some(Operand::UnreadableGemVersion),
                // `Gem::Version.new(Gem::Version.new(…))` is not a String kind.
                Operand::GemVersion(_) | Operand::UnreadableGemVersion => return None,
            };
            GemVersion::parse(text).map(Operand::GemVersion)
        }
        _ => None,
    }
}

/// Applies the operator with the semantics that actually run (reference
/// `decide` + `comparable_kinds?` + `apply`).
fn decide(left: &Operand, right: &Operand, operator: &str) -> Option<Verdict> {
    let (lk, rk) = (left.kind(), right.kind());
    // `:gem_version` only ever compares with another `:gem_version` — a mixed
    // wrapped/bare pair raises at runtime and has no live arm to pick. The String
    // kinds compare with each other EXCEPT two bare literals, which is a constant
    // comparison rather than a version guard.
    if lk == Kind::GemVersion || rk == Kind::GemVersion {
        if lk != rk {
            return None;
        }
    } else if lk == Kind::LiteralString && rk == Kind::LiteralString {
        return None;
    }
    // An engine name only ever answers equality.
    if (lk == Kind::Engine || rk == Kind::Engine) && !is_equality(operator) {
        return None;
    }
    // A curated-but-unreadable operand: the shape folds on the reference, the
    // value does not reach the port ⇒ both arms are dead here.
    if left.is_unreadable() || right.is_unreadable() {
        return Some(Verdict::Unreadable);
    }
    let result = match (left, right) {
        (Operand::GemVersion(a), Operand::GemVersion(b)) => a.cmp_rubygems(b),
        (l, r) => string_of(l).cmp(string_of(r)),
    };
    let truthy = match operator {
        "<" => result.is_lt(),
        "<=" => result.is_le(),
        ">" => result.is_gt(),
        ">=" => result.is_ge(),
        "==" => result.is_eq(),
        "!=" => result.is_ne(),
        _ => return None,
    };
    Some(if truthy { Verdict::Truthy } else { Verdict::Falsey })
}

fn string_of(operand: &Operand) -> &str {
    match operand {
        Operand::LiteralString(s) | Operand::VersionString(s) | Operand::Engine(s) => s,
        // Unreachable: `decide` answers `Unreadable` / declines before it gets here.
        Operand::GemVersion(_) | Operand::UnreadableString | Operand::UnreadableGemVersion => "",
    }
}

// ---------------------------------------------------------------------------
// A RubyGems-compatible version comparator
// ---------------------------------------------------------------------------

/// One segment of a RubyGems version: a digit run (`Integer`) or a letter run
/// (`String`). `Gem::Version#_segments` is `@version.scan(/[0-9]+|[a-z]+/i)`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    Num(u128),
    Str(String),
}

/// A parsed `Gem::Version`, reduced to its `canonical_segments`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GemVersion {
    /// `@version` after `strip.gsub("-", ".pre.")` — the `<=>` fast path compares
    /// it before the segments.
    normalized: String,
    canonical: Vec<Seg>,
}

impl GemVersion {
    /// `Gem::Version.correct?(text)` then `Gem::Version.new(text)`, or `None`
    /// when RubyGems would reject the string (the reference's
    /// `return nil unless ::Gem::Version.correct?(inner.last)`).
    fn parse(text: &str) -> Option<GemVersion> {
        if !correct(text) {
            return None;
        }
        // `Gem::Version#initialize`: `version.to_s.strip.gsub("-", ".pre.")`.
        let normalized = text.trim().replace('-', ".pre.");
        let segments = segments(&normalized)?;
        Some(GemVersion { normalized, canonical: canonical_segments(segments) })
    }

    /// `Gem::Version#<=>`, verbatim in envelope: equal fast paths, then an
    /// element-wise walk over both canonical segment lists zero-padded to the
    /// longer one, where a String segment (a prerelease) sorts BELOW a numeric.
    fn cmp_rubygems(&self, other: &GemVersion) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        if self.normalized == other.normalized || self.canonical == other.canonical {
            return Ordering::Equal;
        }
        let limit = self.canonical.len().max(other.canonical.len());
        for i in 0..limit {
            let zero = Seg::Num(0);
            let lhs = self.canonical.get(i).unwrap_or(&zero);
            let rhs = other.canonical.get(i).unwrap_or(&zero);
            if lhs == rhs {
                continue;
            }
            return match (lhs, rhs) {
                (Seg::Str(_), Seg::Num(_)) => Ordering::Less,
                (Seg::Num(_), Seg::Str(_)) => Ordering::Greater,
                (Seg::Num(a), Seg::Num(b)) => a.cmp(b),
                (Seg::Str(a), Seg::Str(b)) => a.as_str().cmp(b.as_str()),
            };
        }
        Ordering::Equal
    }
}

/// `Gem::Version.correct?` — `ANCHORED_VERSION_PATTERN`, hand-rolled:
/// `\A\s*([0-9]+(\.[0-9a-zA-Z]+)*(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?)?\s*\z`.
/// The whole version part is OPTIONAL, so the empty string is "correct".
fn correct(text: &str) -> bool {
    let body = text.trim();
    if body.is_empty() {
        return true;
    }
    // Split off the optional `-<prerelease>` tail at the FIRST `-`.
    let (head, tail) = match body.find('-') {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    // head: `[0-9]+(\.[0-9a-zA-Z]+)*`
    let mut parts = head.split('.');
    let Some(first) = parts.next() else { return false };
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    for part in parts {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return false;
        }
    }
    // tail: `[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*`
    if let Some(tail) = tail {
        for part in tail.split('.') {
            if part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return false;
            }
        }
    }
    true
}

/// `Gem::Version#_segments`: `@version.scan(/[0-9]+|[a-z]+/i)`, digit runs
/// interned as integers. `None` when a digit run overflows `u128` — a version
/// no real gem carries, declined rather than folded wrong.
fn segments(normalized: &str) -> Option<Vec<Seg>> {
    let bytes = normalized.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            out.push(Seg::Num(normalized[start..i].parse::<u128>().ok()?));
        } else if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            out.push(Seg::Str(normalized[start..i].to_string()));
        } else {
            i += 1;
        }
    }
    Some(out)
}

/// `Gem::Version#canonical_segments`: split at the FIRST String segment, drop
/// each half's TRAILING numeric zeros, concatenate.
fn canonical_segments(segments: Vec<Seg>) -> Vec<Seg> {
    let split = segments.iter().position(|s| matches!(s, Seg::Str(_))).unwrap_or(segments.len());
    let (numeric, alpha) = segments.split_at(split);
    let trim = |part: &[Seg]| {
        let mut end = part.len();
        while end > 0 && part[end - 1] == Seg::Num(0) {
            end -= 1;
        }
        part[..end].to_vec()
    };
    let mut out = trim(numeric);
    out.extend(trim(alpha));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn ast_of(src: &str) -> (LoweredAst, String) {
        let bytes = src.as_bytes().to_vec();
        let result = parse(&bytes);
        (lower(&result), src.to_string())
    }

    /// The verdict of the FIRST `if`/`unless`/ternary in `src`.
    fn first_verdict(src: &str, rt: &RubyRuntime) -> Option<Verdict> {
        let (ast, source) = ast_of(src);
        ast.iter().find_map(|(_, n)| match n {
            Node::If { predicate, .. } => Some(verdict(&ast, *predicate, &source, rt)),
            _ => None,
        })?
    }

    fn host() -> RubyRuntime {
        RubyRuntime::default()
    }

    #[test]
    fn ruby_version_compares_lexically_like_a_string() {
        // f1 / f2 / f15 / f16: the host is 4.0.5 / "ruby".
        assert_eq!(first_verdict("a if RUBY_VERSION < \"2.7.\"", &host()), Some(Verdict::Falsey));
        assert_eq!(first_verdict("a if RUBY_VERSION >= \"3.0\"", &host()), Some(Verdict::Truthy));
        assert_eq!(first_verdict("a if RUBY_VERSION == \"4.0.5\"", &host()), Some(Verdict::Truthy));
        assert_eq!(first_verdict("a if \"2.7\" > RUBY_VERSION", &host()), Some(Verdict::Falsey));
        // Lexical, not numeric: "4.0.5" < "3.4" is false, and the famous
        // `"3.10" < "3.9"` is TRUE under String semantics.
        let rt = RubyRuntime::from_overrides(Some("3.10.0"), None);
        assert_eq!(first_verdict("a if RUBY_VERSION < \"3.9\"", &rt), Some(Verdict::Truthy));
    }

    #[test]
    fn ruby_engine_answers_equality_only() {
        assert_eq!(
            first_verdict("a unless RUBY_ENGINE == \"ruby\"", &host()),
            Some(Verdict::Truthy)
        );
        assert_eq!(first_verdict("a if RUBY_ENGINE == \"jruby\"", &host()), Some(Verdict::Falsey));
        assert_eq!(first_verdict("a if RUBY_ENGINE != \"ruby\"", &host()), Some(Verdict::Falsey));
        // ORDERING on an engine name is not a version guard.
        assert_eq!(first_verdict("a if RUBY_ENGINE < \"ruby\"", &host()), None);
    }

    #[test]
    fn the_undecidable_shapes_keep_both_arms_live() {
        // f6 / f8 / f9 / f10 / f17, plus `<=>`, two bare literals and Psych.
        for src in [
            "a if RUBY_PLATFORM =~ /java/",
            "a if RUBY_VERSION.to_f < 3.4",
            "a if RUBY_VERSION < \"3.4\" && ENV[\"X\"]",
            "a if RUBY_VERSION < \"3.4\" || RUBY_ENGINE == \"jruby\"",
            "v = RUBY_VERSION\na if v < \"3.4\"",
            "a if (RUBY_VERSION <=> \"3.4\") < 0",
            "a if \"a\" < \"b\"",
            "a if ::RUBY_VERSION < \"3.4\"",
            "a if Foo::VERSION < \"5.0\"",
            "a if defined?(Ractor)",
            "a if !(RUBY_VERSION < \"3.4\")",
        ] {
            assert_eq!(first_verdict(src, &host()), None, "{src}");
        }
    }

    /// The reference READS `Psych::VERSION` out of its own runtime, so declining
    /// it would report inside the arm the oracle killed. Both arms die instead.
    #[test]
    fn a_curated_but_unreadable_version_constant_kills_both_arms() {
        assert_eq!(
            first_verdict("a if Psych::VERSION < \"5.0\"", &host()),
            Some(Verdict::Unreadable)
        );
        assert_eq!(
            first_verdict("a if ::Psych::VERSION >= \"3.1.0\"", &host()),
            Some(Verdict::Unreadable)
        );
        assert_eq!(
            first_verdict(
                "a if Gem::Version.new(Psych::VERSION) < Gem::Version.new(\"3.1.0\")",
                &host()
            ),
            Some(Verdict::Unreadable)
        );
        // Kind rules still apply: a wrapped/bare mix and an ordering against an
        // engine name decline, unreadable operand or not.
        assert_eq!(
            first_verdict("a if Gem::Version.new(Psych::VERSION) < \"3.1.0\"", &host()),
            None
        );
        assert_eq!(first_verdict("a if Psych::VERSION < RUBY_ENGINE", &host()), None);
        // …and BOTH arms of such a guard are dropped.
        let src = "if Psych::VERSION >= \"3.1.0\"\n  a\nelse\n  b\nend\n";
        let (ast, source) = ast_of(src);
        assert_eq!(dead_arm_spans(&ast, &source, &host()).len(), 2);
    }

    #[test]
    fn gem_version_folds_on_both_sides_only() {
        // f7: 4.0.5 < 3.4 is FALSE under version semantics too.
        assert_eq!(
            first_verdict(
                "a if Gem::Version.new(RUBY_VERSION) < Gem::Version.new(\"3.4\")",
                &host()
            ),
            Some(Verdict::Falsey)
        );
        assert_eq!(
            first_verdict(
                "a if Gem::Version.new(RUBY_VERSION) >= Gem::Version.new(\"3.4\")",
                &host()
            ),
            Some(Verdict::Truthy)
        );
        // `::Gem::Version` renders the same and is accepted.
        assert_eq!(
            first_verdict(
                "a if ::Gem::Version.new(RUBY_VERSION) >= ::Gem::Version.new(\"3.4\")",
                &host()
            ),
            Some(Verdict::Truthy)
        );
        // A MIXED pair raises at runtime — never folded.
        assert_eq!(
            first_verdict("a if Gem::Version.new(RUBY_VERSION) < \"3.4\"", &host()),
            None
        );
        assert_eq!(
            first_verdict("a if RUBY_VERSION < Gem::Version.new(\"3.4\")", &host()),
            None
        );
        // A malformed literal `Gem::Version.new` would raise on.
        assert_eq!(
            first_verdict(
                "a if Gem::Version.new(RUBY_VERSION) < Gem::Version.new(\"not a version\")",
                &host()
            ),
            None
        );
    }

    /// The RubyGems comparator, pinned against `Gem::Version` itself.
    #[test]
    fn rubygems_comparator_matches_gem_version() {
        let cases: &[(&str, &str, std::cmp::Ordering)] = &[
            ("4.0.5", "3.4", std::cmp::Ordering::Greater),
            ("3.10", "3.9", std::cmp::Ordering::Greater),
            ("1.0", "1.0.0", std::cmp::Ordering::Equal),
            ("1.0.0", "1.0", std::cmp::Ordering::Equal),
            ("3.1.0.pre1", "3.1.0", std::cmp::Ordering::Less),
            ("3.1.0", "3.1.0.pre1", std::cmp::Ordering::Greater),
            ("1.0.a", "1.0", std::cmp::Ordering::Less),
            ("1.0.b", "1.0.a", std::cmp::Ordering::Greater),
            ("1.0-rc1", "1.0", std::cmp::Ordering::Less),
            ("", "0", std::cmp::Ordering::Equal),
            ("2", "10", std::cmp::Ordering::Less),
            ("1.0.0.0", "1", std::cmp::Ordering::Equal),
            ("1.a.0", "1.a", std::cmp::Ordering::Equal),
            ("1.0.pre.1", "1.0.pre", std::cmp::Ordering::Greater),
            ("1.9.3", "1.10.0", std::cmp::Ordering::Less),
            ("2.7.0", "2.7", std::cmp::Ordering::Equal),
            ("1.0.0-beta", "1.0.0", std::cmp::Ordering::Less),
            ("1.0.0.beta", "1.0.0.alpha", std::cmp::Ordering::Greater),
            ("4.0.5", "4.0.5", std::cmp::Ordering::Equal),
            ("0.0.1", "0", std::cmp::Ordering::Greater),
            ("1.0.0.rc.1", "1.0.0.rc", std::cmp::Ordering::Greater),
        ];
        for (a, b, want) in cases {
            let va = GemVersion::parse(a).unwrap_or_else(|| panic!("correct?({a:?})"));
            let vb = GemVersion::parse(b).unwrap_or_else(|| panic!("correct?({b:?})"));
            assert_eq!(va.cmp_rubygems(&vb), *want, "{a} <=> {b}");
        }
    }

    #[test]
    fn gem_version_correct_rejects_what_rubygems_rejects() {
        for ok in ["1", "1.0", "1.0.0", "3.1.0.pre1", "1.0-rc1", "  1.2  ", ""] {
            assert!(correct(ok), "correct?({ok:?}) should be true");
        }
        for bad in ["not a version", "a.1", "1..2", "1.0.", "-1", "1.0 2.0"] {
            assert!(!correct(bad), "correct?({bad:?}) should be false");
        }
    }

    #[test]
    fn env_overrides_flip_the_verdict() {
        // f1's twin: on 3.3.0 the `< "3.4"` guard is LIVE, not dead.
        let rt = RubyRuntime::from_overrides(Some("3.3.0"), None);
        assert_eq!(first_verdict("a if RUBY_VERSION < \"3.4\"", &rt), Some(Verdict::Truthy));
        assert_eq!(first_verdict("a if RUBY_VERSION < \"3.4\"", &host()), Some(Verdict::Falsey));
        // An empty or absent override reads as "unset".
        assert_eq!(RubyRuntime::from_overrides(None, None), RubyRuntime::default());
        assert_eq!(RubyRuntime::from_overrides(Some(""), Some("  ")), RubyRuntime::default());
        let jruby = RubyRuntime::from_overrides(None, Some("jruby"));
        assert_eq!(jruby.engine, "jruby");
        assert_eq!(jruby.version, HOST_RUBY_VERSION);
        assert_eq!(first_verdict("a if RUBY_ENGINE == \"jruby\"", &jruby), Some(Verdict::Truthy));
    }

    /// The one-line ternary drops only the dead HALF — the reason `covers` is an
    /// offset range and not a line number.
    #[test]
    fn a_ternary_drops_only_the_dead_half() {
        let src = "RUBY_VERSION < \"3.4\" ? a : b\n";
        let (ast, source) = ast_of(src);
        let arms = dead_arm_spans(&ast, &source, &host());
        assert_eq!(arms.len(), 1);
        let (start, end) = arms[0];
        assert_eq!(&src[start..end], "a");
    }

    #[test]
    fn an_elsif_chain_decides_each_link_on_its_own() {
        // f14: `>= "3.4"` is TRUE on the host, so the whole `elsif` chain is dead.
        let src = "if RUBY_VERSION >= \"3.4\"\n  a\nelsif RUBY_VERSION >= \"3.0\"\n  b\nend\n";
        let (ast, source) = ast_of(src);
        let arms = dead_arm_spans(&ast, &source, &host());
        assert_eq!(arms.len(), 1);
        let (start, end) = arms[0];
        assert!(src[start..end].starts_with("elsif"), "{:?}", &src[start..end]);
        assert!(src[start..end].contains("b"));
    }

    #[test]
    fn suppression_diagnostics_survive_a_dead_arm() {
        let src = "if RUBY_VERSION < \"3.4\"\n  a.b\nend\n";
        let (ast, source) = ast_of(src);
        let inside = src.find("a.b").expect("offset");
        let diag = |rule: &'static str| Diagnostic {
            rule_id: rule,
            start_offset: inside,
            end_offset: inside + 3,
            message: String::new(),
            severity: crate::Severity::Warning,
            source_family: "builtin",
            receiver_type: None,
            method_name: None,
        };
        let kept = filter_dead_version_guard_arms_with(
            vec![diag("call.undefined-method"), diag("suppression.unknown-rule")],
            &ast,
            &source,
            &host(),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].rule_id, "suppression.unknown-rule");
    }
}
