//! Rust-native constant folding for the conservative deterministic core
//! (ADR-0008).
//!
//! The reference reaches its precision by *executing real Ruby* on literal
//! values, gated by a purity allowlist. ADR-0008 splits that work on a hybrid
//! boundary: a **conservative Rust core** handles the cases where byte-exact
//! agreement with Ruby is trivially guaranteed (integer arithmetic / bitops /
//! comparisons, boolean & nil logic, symbol equality, pure ASCII string ops);
//! everything else (Float formatting, encoding-sensitive String, Rational,
//! Date/Time, Regexp, `String#%`) routes to the cached Ruby sidecar.
//!
//! This module is *only* the Rust core. Its hard contract is **zero false
//! positives**: [`fold`] returns `Some(result)` only when the result is
//! deterministic AND byte-exactly what Ruby would compute; on any doubt —
//! non-determinism, a non-modeled method, a non-ASCII string, overflow, a
//! divide-by-zero — it returns `None`, and the dispatcher widens to the nominal
//! type rather than minting a spurious `Constant` (ADR-0023 tier-5).
//!
// TODO(spec): the long tail (Float formatting, encoding-sensitive String,
// Rational/Complex, Date/Time, Regexp, `String#%`, `(1..10).first(3)`) goes to
// the Ruby sidecar (ADR-0008). Argument-count / argument-type validation and
// the full purity catalogue belong to the dispatcher, not this leaf.

use rigor_types::Scalar;

/// Fold a value-pinned method call on a literal receiver.
///
/// Given the receiver `Scalar`, the `method` name, and the already-typed
/// argument `Scalar`s, return `Some(result)` when the call is in the
/// conservative deterministic core and the computation is byte-exact, else
/// `None` (NEVER guess).
///
/// `None` is returned for: a non-modeled receiver class / method, the wrong
/// argument count, a non-`Constant` argument (the caller passes only the scalars
/// it could pin), a non-deterministic operation, or any boundary condition
/// (overflow, divide-by-zero, non-ASCII string) where Rust's result could
/// diverge from Ruby's.
pub fn fold(receiver: &Scalar, method: &str, args: &[Scalar]) -> Option<Scalar> {
    match receiver {
        Scalar::Int(a) => fold_int(*a, method, args),
        // A Bignum: every fold declines. The decimal spelling carries no `i64`
        // to compute on, and the sidecar path (ADR-0008) still reaches the few
        // `Integer` methods it models via `scalar_class` — never a guessed
        // value (rigor-rs#194).
        Scalar::BigInt(_) => None,
        Scalar::Float(a) => fold_float(*a, method, args),
        Scalar::Bool(a) => fold_bool(*a, method, args),
        Scalar::Nil => fold_nil(method, args),
        Scalar::Sym(a) => fold_sym(a, method, args),
        Scalar::Str(a) => fold_str(a, method, args),
    }
}

/// Whether a `(class, method)` pair is in the Rust-foldable catalogue at all.
/// This is the *foldability* decision (ADR-0008: decided in Rust); it does not
/// execute anything. Mirrors the arms of [`fold`]; kept in sync by hand.
///
// TODO(spec): derive this from a shared purity catalogue rather than mirroring
// the match arms (ADR-0008).
pub fn is_foldable(class: &str, method: &str) -> bool {
    match class {
        "Integer" => matches!(
            method,
            "+" | "-" | "*" | "/" | "%" | "**" | "&" | "|" | "^" | "<<" | ">>"
                | "<" | "<=" | ">" | ">=" | "=="
                | "abs" | "succ" | "pred" | "even?" | "odd?" | "zero?" | "to_s"
        ),
        "Float" => matches!(
            method,
            "+" | "-" | "*" | "<" | "<=" | ">" | ">=" | "==" | "abs" | "<=>"
        ),
        "TrueClass" | "FalseClass" => matches!(method, "!" | "&" | "|" | "=="),
        "NilClass" => matches!(method, "!" | "&" | "|" | "=="),
        "Symbol" => matches!(method, "to_s" | "=="),
        "String" => matches!(
            method,
            "upcase" | "downcase" | "reverse" | "length" | "size" | "+" | "*"
                | "==" | "empty?" | "[]" | "slice" | "byteslice" | "index"
                | "getbyte" | "rindex" | "byteindex" | "byterindex"
        ),
        _ => false,
    }
}

/// The Ruby class name of a scalar literal — the receiver-class key used to gate
/// [`sidecar_foldable`]. `Bool`/`Nil` map to their singleton classes.
#[must_use]
pub fn scalar_class(s: &Scalar) -> &'static str {
    match s {
        Scalar::Int(_) | Scalar::BigInt(_) => "Integer",
        Scalar::Float(_) => "Float",
        Scalar::Str(_) => "String",
        Scalar::Sym(_) => "Symbol",
        Scalar::Bool(true) => "TrueClass",
        Scalar::Bool(false) => "FalseClass",
        Scalar::Nil => "NilClass",
    }
}

/// Whether a `(receiver class, method)` is safe to route to the Ruby sidecar
/// (ADR-0008): a pure, deterministic long-tail fold the Rust core deliberately
/// declines, whose result the reference folds identically (so routing it cannot
/// diverge — parity-safe). Deliberately a SMALL, harness-verified subset; grows
/// as each method is confirmed against the reference. Note this is disjoint from
/// the Rust core: a method the Rust core already folds never reaches the sidecar
/// (the core wins first in the dispatcher).
#[must_use]
pub fn sidecar_foldable(receiver_class: &str, method: &str) -> bool {
    // Every entry has been verified to fold IDENTICALLY in the reference (which
    // also executes real Ruby), so routing it cannot diverge. All are pure +
    // deterministic and return a scalar carrier; a non-scalar arg simply fails to
    // pin (the fold is never attempted) and a non-scalar result declines.
    matches!(
        (receiver_class, method),
        // Integer — base-N formatting + number theory (Rust core is base-10 only).
        ("Integer", "to_s" | "gcd")
        // Float — rounding family (Rust core has none of these).
        | ("Float", "round")
        // String — the format + transform long tail.
        | ("String", "%" | "center" | "ljust" | "rjust" | "tr" | "sub" | "strip")
    )
}

/// Reference `string_pad_blow_up?` (`constant_folding.rb`): `center` / `ljust` /
/// `rjust` with a width past `STRING_FOLD_BYTE_LIMIT` does not fold. The sidecar
/// would otherwise build the string, and a 3e9 width took 70 s and 10 GB.
pub fn sidecar_blows_up(method: &str, args: &[Scalar]) -> bool {
    matches!(method, "center" | "ljust" | "rjust")
        && matches!(args.first(), Some(Scalar::Int(w))
            if *w > crate::kernel_fold::STRING_FOLD_BYTE_LIMIT as i64
        // A Bignum width exceeds the byte limit by construction.
            || matches!(args.first(), Some(Scalar::BigInt(_))))
}

/// Executes a purity-gated fold the Rust core declined, by running the real Ruby
/// method (ADR-0008 — the Ruby sidecar). Injected into the [`crate::Typer`] so
/// the pure `rigor-infer` crate never itself does IO / spawns a process; the
/// implementor (the CLI's sidecar client) owns that. `None` = declined /
/// unavailable (the dispatcher then widens to the nominal type — sound subset).
pub trait RubyFolder {
    /// Execute `receiver.method(*args)` on scalar literals, returning the result
    /// scalar or `None`. The caller has already confirmed [`sidecar_foldable`].
    fn fold(&self, receiver: &Scalar, method: &str, args: &[Scalar]) -> Option<Scalar>;
}

// --- Integer ----------------------------------------------------------------

fn fold_int(a: i64, method: &str, args: &[Scalar]) -> Option<Scalar> {
    // Nullary, deterministic.
    match (method, args) {
        ("abs", []) => return a.checked_abs().map(Scalar::Int),
        ("succ", []) => return a.checked_add(1).map(Scalar::Int),
        ("pred", []) => return a.checked_sub(1).map(Scalar::Int),
        ("even?", []) => return Some(Scalar::Bool(a % 2 == 0)),
        ("odd?", []) => return Some(Scalar::Bool(a % 2 != 0)),
        ("zero?", []) => return Some(Scalar::Bool(a == 0)),
        // `to_s` with no radix only (a radix arg changes the base — not folded
        // here to keep the core trivially byte-exact with Ruby's decimal form).
        ("to_s", []) => return Some(Scalar::Str(a.to_string())),
        _ => {}
    }

    // Binary on a single Integer argument.
    let b = match args {
        [Scalar::Int(b)] => *b,
        _ => return None,
    };
    match method {
        // Overflow -> None (Ruby promotes to Bignum; we don't model Bignum, so
        // declining preserves byte-exactness).
        "+" => a.checked_add(b).map(Scalar::Int),
        "-" => a.checked_sub(b).map(Scalar::Int),
        "*" => a.checked_mul(b).map(Scalar::Int),
        // Ruby integer division floors toward negative infinity, and 0-divisor
        // raises. Decline on zero; use floor (Euclidean-flavoured) division.
        "/" => {
            if b == 0 {
                None
            } else {
                a.checked_div_euclid(b).map(Scalar::Int)
            }
        }
        // Ruby `%` result takes the sign of the divisor (rem_euclid is for
        // positive b; use a sign-of-divisor adjustment).
        "%" => {
            if b == 0 {
                None
            } else {
                Some(Scalar::Int(ruby_mod(a, b)))
            }
        }
        // Exponent: decline negative exponents (Ruby yields a Rational) and
        // anything that overflows.
        "**" => {
            if b < 0 || b > u32::MAX as i64 {
                None
            } else {
                a.checked_pow(b as u32).map(Scalar::Int)
            }
        }
        "&" => Some(Scalar::Int(a & b)),
        "|" => Some(Scalar::Int(a | b)),
        "^" => Some(Scalar::Int(a ^ b)),
        // Shifts: decline out-of-range counts (Ruby has unbounded precision).
        "<<" => {
            if (0..64).contains(&b) {
                a.checked_shl(b as u32).map(Scalar::Int)
            } else {
                None
            }
        }
        ">>" => {
            if (0..64).contains(&b) {
                Some(Scalar::Int(a >> b))
            } else {
                None
            }
        }
        "<" => Some(Scalar::Bool(a < b)),
        "<=" => Some(Scalar::Bool(a <= b)),
        ">" => Some(Scalar::Bool(a > b)),
        ">=" => Some(Scalar::Bool(a >= b)),
        "==" => Some(Scalar::Bool(a == b)),
        _ => None,
    }
}

/// Ruby's `Integer#%`: the result has the sign of the divisor.
fn ruby_mod(a: i64, b: i64) -> i64 {
    let r = a % b;
    if r != 0 && (r < 0) != (b < 0) {
        r + b
    } else {
        r
    }
}

// --- Float ------------------------------------------------------------------

fn fold_float(a: f64, method: &str, args: &[Scalar]) -> Option<Scalar> {
    if method == "abs" && args.is_empty() {
        return Some(Scalar::Float(a.abs()));
    }
    if method == "<=>" {
        return float_cmp(a, args);
    }
    // Binary on a single Float argument. We deliberately do NOT fold
    // Float op Integer (mixed coercion) here — that stays simple and exact.
    let b = match args {
        [Scalar::Float(b)] => *b,
        _ => return None,
    };
    let res = match method {
        "+" => Scalar::Float(a + b),
        "-" => Scalar::Float(a - b),
        "*" => Scalar::Float(a * b),
        "<" => Scalar::Bool(a < b),
        "<=" => Scalar::Bool(a <= b),
        ">" => Scalar::Bool(a > b),
        ">=" => Scalar::Bool(a >= b),
        "==" => Scalar::Bool(a == b),
        // `/` is intentionally excluded: Float division by zero yields
        // ±Infinity/NaN whose downstream display is sidecar territory (ADR-0008).
        _ => return None,
    };
    Some(res)
}

/// `Float#<=>` (`flo_cmp`), which the reference folds by calling it
/// (`NUMERIC_BINARY`). An Integer operand compares EXACTLY, not through a lossy
/// `i64 -> f64` cast: `9007199254740992.0 <=> 9007199254740993` is `-1`. A
/// non-numeric operand has no `coerce`, so Ruby answers `nil`. A NaN or infinite
/// receiver/operand declines: the reference never holds a NaN `Constant`
/// (`foldable_constant_value?`), and a non-finite port scalar is not proven to
/// be one the reference also pins.
fn float_cmp(a: f64, args: &[Scalar]) -> Option<Scalar> {
    if !a.is_finite() {
        return None;
    }
    let ord = |o: std::cmp::Ordering| Scalar::Int(o as i64);
    match args {
        [Scalar::Float(b)] if b.is_finite() => a.partial_cmp(b).map(ord),
        [Scalar::Float(_)] => None,
        [Scalar::Int(b)] => {
            // 2^63 is exactly representable; every finite `a` below it and at or
            // above -2^63 truncates to an in-range i64 without loss.
            const TWO_63: f64 = 9_223_372_036_854_775_808.0;
            if a >= TWO_63 {
                return Some(Scalar::Int(1));
            }
            if a < -TWO_63 {
                return Some(Scalar::Int(-1));
            }
            let whole = a.trunc();
            Some(ord((whole as i64).cmp(b).then(if a > whole {
                std::cmp::Ordering::Greater
            } else if a < whole {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            })))
        }
        [Scalar::Str(_) | Scalar::Sym(_) | Scalar::Bool(_) | Scalar::Nil] => Some(Scalar::Nil),
        _ => None,
    }
}

// --- Bool -------------------------------------------------------------------

fn fold_bool(a: bool, method: &str, args: &[Scalar]) -> Option<Scalar> {
    if method == "!" && args.is_empty() {
        return Some(Scalar::Bool(!a));
    }
    match (method, args) {
        // `true & nil` -> false, `false | nil` -> false: nil is falsey in
        // boolean logic. We model both Bool and Nil right operands.
        ("&", [b]) => Some(Scalar::Bool(a && truthy(b)?)),
        ("|", [b]) => Some(Scalar::Bool(a || truthy(b)?)),
        ("==", [Scalar::Bool(b)]) => Some(Scalar::Bool(a == *b)),
        // `true == 1` etc. is well-defined (false) but we keep `==` to same-kind
        // operands in the core to stay trivially exact.
        _ => None,
    }
}

/// The boolean value of a scalar for `&`/`|` logic, but ONLY for the operands
/// the core models exactly: an explicit `Bool` or `nil`. Anything else returns
/// `None` (decline) rather than assuming Ruby's general truthiness.
fn truthy(s: &Scalar) -> Option<bool> {
    match s {
        Scalar::Bool(b) => Some(*b),
        Scalar::Nil => Some(false),
        _ => None,
    }
}

// --- Nil --------------------------------------------------------------------

fn fold_nil(method: &str, args: &[Scalar]) -> Option<Scalar> {
    match (method, args) {
        ("!", []) => Some(Scalar::Bool(true)), // !nil == true
        ("&", [_]) => Some(Scalar::Bool(false)), // nil & x == false (for any x)
        ("|", [b]) => Some(Scalar::Bool(truthy(b)?)),          // nil | x == !!x
        ("==", [Scalar::Nil]) => Some(Scalar::Bool(true)),
        ("==", [_]) => Some(Scalar::Bool(false)),
        _ => None,
    }
}

// --- Symbol -----------------------------------------------------------------

fn fold_sym(a: &str, method: &str, args: &[Scalar]) -> Option<Scalar> {
    match (method, args) {
        ("to_s", []) => Some(Scalar::Str(a.to_string())),
        ("==", [Scalar::Sym(b)]) => Some(Scalar::Bool(a == b)),
        ("==", [_]) => Some(Scalar::Bool(false)),
        _ => None,
    }
}

// --- String -----------------------------------------------------------------

fn fold_str(a: &str, method: &str, args: &[Scalar]) -> Option<Scalar> {
    // ASCII-only gate: `upcase`/`downcase`/`reverse` diverge from Ruby on
    // multibyte / locale-sensitive input, so decline anything non-ASCII and
    // route it to the sidecar (ADR-0008).
    match (method, args) {
        ("length" | "size", []) => return Some(Scalar::Int(a.chars().count() as i64)),
        ("empty?", []) => return Some(Scalar::Bool(a.is_empty())),
        ("upcase", []) if a.is_ascii() => return Some(Scalar::Str(a.to_ascii_uppercase())),
        ("downcase", []) if a.is_ascii() => return Some(Scalar::Str(a.to_ascii_lowercase())),
        ("reverse", []) if a.is_ascii() => {
            return Some(Scalar::Str(a.chars().rev().collect()))
        }
        _ => {}
    }
    match (method, args) {
        ("+", [Scalar::Str(b)]) => Some(Scalar::Str(format!("{a}{b}"))),
        ("*", [Scalar::Int(n)]) => {
            // Ruby raises on a negative count; decline. Cap repeats so a huge
            // literal can't blow memory in the analyzer.
            if *n < 0 || *n > 4096 {
                None
            } else {
                Some(Scalar::Str(a.repeat(*n as usize)))
            }
        }
        ("==", [Scalar::Str(b)]) => Some(Scalar::Bool(a == b)),
        ("==", [_]) => Some(Scalar::Bool(false)),
        (
            "[]" | "slice" | "byteslice" | "index" | "getbyte" | "rindex" | "byteindex"
            | "byterindex",
            _,
        ) => fold_str_lookup(a, method, args),
        _ => None,
    }
}

/// Whether `method` on `recv` is one of the String lookups, which can fold to
/// `nil` and so change the receiver's class downstream.
pub fn is_str_lookup(recv: &Scalar, method: &str) -> bool {
    matches!(recv, Scalar::Str(_))
        && matches!(
            method,
            "[]" | "slice"
                | "byteslice"
                | "index"
                | "getbyte"
                | "rindex"
                | "byteindex"
                | "byterindex"
        )
}

/// Issue #164 — the literal calls whose tier-1 fold OWNS the answer: a call
/// that pins every argument but does not fold must not fall through to the
/// tier-3 flat slot. Each returns `Integer?`, and the reference, which folds
/// these by running the method, either lands on a value the port does not model
/// (a multibyte receiver, a Float index) or, when Ruby raises (`"abc".getbyte("a")`,
/// a wrong arity), declines to the `Integer | nil` RBS join no rule fires on.
/// The flat slot's bare `Integer` is wrong both ways, so the caller answers
/// `Dynamic` instead: a coverage loss, never a false positive.
pub fn declines_unfolded(recv: &Scalar, method: &str) -> bool {
    match recv {
        Scalar::Str(_) => matches!(method, "getbyte" | "rindex" | "byteindex" | "byterindex"),
        Scalar::Float(_) => method == "<=>",
        _ => false,
    }
}

/// Whether a stale pinned value could change the fold's CLASS (a lookup's
/// `nil`, or `Float#<=>`'s `nil` against a receiver a later write retypes), so
/// the tier-1 fold must decline when the call reads a local.
pub fn is_nilable_fold(recv: &Scalar, method: &str) -> bool {
    is_str_lookup(recv, method) || matches!((recv, method), (Scalar::Float(_), "<=>"))
}

/// `String#[]` / `#slice` / `#byteslice` / `#index` on pinned scalars (issue
/// #121 step 1), and `#getbyte` / `#rindex` / `#byteindex` / `#byterindex`
/// (issue #164). The reference folds them all by running the real method, so a
/// fold here must be exactly Ruby's answer — including `nil` for an index out of
/// range or an absent substring, which the reference witnesses on as `nil`.
///
/// ASCII-only on BOTH sides, so a character offset is a byte offset: that makes
/// `byteslice` coincide with `slice` and keeps every arm trivially byte-exact.
/// Everything else declines and leaves the RBS answer standing — a `Float` index
/// or offset (Ruby truncates it), a `Range` or `Regexp` argument (not a
/// [`Scalar`]), a multibyte string, and every argument kind Ruby raises on
/// (`"abc".byteslice("a")`, `"abc"[nil]`, `"abc".index(1)`).
fn fold_str_lookup(a: &str, method: &str, args: &[Scalar]) -> Option<Scalar> {
    if !a.is_ascii() {
        return None;
    }
    let len = a.len() as i64;
    // Ruby's `rb_str_subpos`: a negative start counts from the end; a start past
    // the end, or a negative length, is `nil`; a start AT the end is `""`.
    let substr = |start: i64, count: i64| -> Scalar {
        let start = if start < 0 { start + len } else { start };
        if count < 0 || start < 0 || start > len {
            return Scalar::Nil;
        }
        let end = start.saturating_add(count).min(len);
        Scalar::Str(a[start as usize..end as usize].to_owned())
    };
    match (method, args) {
        ("[]" | "slice" | "byteslice", [Scalar::Int(i)]) => {
            let i = if *i < 0 { *i + len } else { *i };
            Some(if (0..len).contains(&i) {
                Scalar::Str(a[i as usize..=i as usize].to_owned())
            } else {
                Scalar::Nil
            })
        }
        ("[]" | "slice" | "byteslice", [Scalar::Int(start), Scalar::Int(count)]) => {
            Some(substr(*start, *count))
        }
        ("[]" | "slice", [Scalar::Str(sub)]) if sub.is_ascii() => {
            Some(if a.contains(sub.as_str()) { Scalar::Str(sub.clone()) } else { Scalar::Nil })
        }
        // `rb_str_getbyte`: a negative index counts from the end; out of range
        // is `nil`. Only an Integer index — Ruby truncates a Float and raises
        // on anything else.
        ("getbyte", [Scalar::Int(i)]) => {
            let i = if *i < 0 { *i + len } else { *i };
            Some(if (0..len).contains(&i) {
                Scalar::Int(i64::from(a.as_bytes()[i as usize]))
            } else {
                Scalar::Nil
            })
        }
        // `byteindex` is `index` counted in bytes (`rb_str_byteindex_m`: a
        // negative offset counts from the end, one outside `0..=len` is `nil`),
        // and on an ASCII receiver bytes are characters. Its only extra failure,
        // an offset off a character boundary, cannot happen here.
        ("index" | "byteindex", [Scalar::Str(sub)]) if sub.is_ascii() => {
            Some(a.find(sub.as_str()).map_or(Scalar::Nil, |p| Scalar::Int(p as i64)))
        }
        // `rb_str_rindex_m` / `rb_str_byterindex_m`: the start defaults to the
        // end; a negative one counts from the end (`nil` if still negative); one
        // past the end clamps to it. The match is the LAST one starting at or
        // before it, so an empty needle answers the start itself.
        ("rindex" | "byterindex", [Scalar::Str(sub), rest @ ..]) if sub.is_ascii() => {
            let start = match rest {
                [] => len,
                [Scalar::Int(p)] => {
                    let p = if *p < 0 { *p + len } else { *p };
                    if p < 0 {
                        return Some(Scalar::Nil);
                    }
                    p.min(len)
                }
                _ => return None,
            };
            let slen = sub.len() as i64;
            if slen > len {
                return Some(Scalar::Nil);
            }
            let end = (start + slen).min(len) as usize;
            Some(a[..end].rfind(sub.as_str()).map_or(Scalar::Nil, |p| Scalar::Int(p as i64)))
        }
        ("index" | "byteindex", [Scalar::Str(sub), Scalar::Int(offset)]) if sub.is_ascii() => {
            let offset = if *offset < 0 { *offset + len } else { *offset };
            if !(0..=len).contains(&offset) {
                return Some(Scalar::Nil);
            }
            let from = offset as usize;
            Some(a[from..].find(sub.as_str()).map_or(Scalar::Nil, |p| Scalar::Int((from + p) as i64)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_integer_arithmetic() {
        assert_eq!(fold(&Scalar::Int(1), "+", &[Scalar::Int(2)]), Some(Scalar::Int(3)));
        assert_eq!(fold(&Scalar::Int(7), "%", &[Scalar::Int(3)]), Some(Scalar::Int(1)));
        // Ruby `%` sign-of-divisor: -7 % 3 == 2.
        assert_eq!(fold(&Scalar::Int(-7), "%", &[Scalar::Int(3)]), Some(Scalar::Int(2)));
        // Floor division: -7 / 2 == -4.
        assert_eq!(fold(&Scalar::Int(-7), "/", &[Scalar::Int(2)]), Some(Scalar::Int(-4)));
        assert_eq!(fold(&Scalar::Int(2), "**", &[Scalar::Int(10)]), Some(Scalar::Int(1024)));
        assert_eq!(fold(&Scalar::Int(-5), "abs", &[]), Some(Scalar::Int(5)));
        assert_eq!(fold(&Scalar::Int(4), "even?", &[]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&Scalar::Int(3), "to_s", &[]), Some(Scalar::Str("3".into())));
    }

    #[test]
    fn declines_unsound_integer_cases() {
        // Divide by zero, negative exponent, and overflow all decline.
        assert_eq!(fold(&Scalar::Int(1), "/", &[Scalar::Int(0)]), None);
        assert_eq!(fold(&Scalar::Int(2), "**", &[Scalar::Int(-1)]), None);
        assert_eq!(fold(&Scalar::Int(i64::MAX), "+", &[Scalar::Int(1)]), None);
        // Mixed-type argument declines (caller only pins same-kind scalars).
        assert_eq!(fold(&Scalar::Int(1), "+", &[Scalar::Str("x".into())]), None);
    }

    #[test]
    fn folds_string_ascii_ops() {
        assert_eq!(fold(&Scalar::Str("hi".into()), "upcase", &[]), Some(Scalar::Str("HI".into())));
        assert_eq!(fold(&Scalar::Str("HELLO".into()), "downcase", &[]), Some(Scalar::Str("hello".into())));
        assert_eq!(fold(&Scalar::Str("hello".into()), "length", &[]), Some(Scalar::Int(5)));
        assert_eq!(fold(&Scalar::Str("ab".into()), "+", &[Scalar::Str("cd".into())]), Some(Scalar::Str("abcd".into())));
        assert_eq!(fold(&Scalar::Str("ab".into()), "*", &[Scalar::Int(3)]), Some(Scalar::Str("ababab".into())));
        assert_eq!(fold(&Scalar::Str("".into()), "empty?", &[]), Some(Scalar::Bool(true)));
    }

    #[test]
    fn declines_non_ascii_case_ops() {
        // upcase on multibyte declines (locale-sensitive -> sidecar), but
        // length still folds (codepoint count is exact).
        assert_eq!(fold(&Scalar::Str("café".into()), "upcase", &[]), None);
        assert_eq!(fold(&Scalar::Str("café".into()), "length", &[]), Some(Scalar::Int(4)));
    }

    #[test]
    fn folds_bool_nil_symbol_logic() {
        assert_eq!(fold(&Scalar::Bool(true), "!", &[]), Some(Scalar::Bool(false)));
        assert_eq!(fold(&Scalar::Bool(true), "&", &[Scalar::Nil]), Some(Scalar::Bool(false)));
        assert_eq!(fold(&Scalar::Nil, "!", &[]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&Scalar::Sym("foo".into()), "to_s", &[]), Some(Scalar::Str("foo".into())));
        assert_eq!(
            fold(&Scalar::Sym("a".into()), "==", &[Scalar::Sym("a".into())]),
            Some(Scalar::Bool(true))
        );
    }

    /// Issue #121 step 1 — every expectation is the pinned reference's measured
    /// fold (and plain Ruby's answer).
    #[test]
    fn folds_string_lookups() {
        let s = |v: &str| Scalar::Str(v.into());
        let abc = s("abc");
        let i = Scalar::Int;
        let cases: &[(&str, Vec<Scalar>, Scalar)] = &[
            ("[]", vec![i(0)], s("a")),
            ("[]", vec![i(-1)], s("c")),
            ("[]", vec![i(3)], Scalar::Nil),
            ("[]", vec![i(99)], Scalar::Nil),
            ("[]", vec![i(-4)], Scalar::Nil),
            ("[]", vec![i(1), i(2)], s("bc")),
            ("[]", vec![i(3), i(1)], s("")),
            ("[]", vec![i(4), i(1)], Scalar::Nil),
            ("[]", vec![i(-2), i(5)], s("bc")),
            ("[]", vec![i(-4), i(1)], Scalar::Nil),
            ("[]", vec![i(1), i(-1)], Scalar::Nil),
            ("[]", vec![s("b")], s("b")),
            ("[]", vec![s("z")], Scalar::Nil),
            ("slice", vec![i(0)], s("a")),
            ("slice", vec![i(1), i(1)], s("b")),
            ("slice", vec![s("bc")], s("bc")),
            ("byteslice", vec![i(0)], s("a")),
            ("byteslice", vec![i(-1)], s("c")),
            ("byteslice", vec![i(5)], Scalar::Nil),
            ("byteslice", vec![i(1), i(2)], s("bc")),
            ("index", vec![s("b")], i(1)),
            ("index", vec![s("z")], Scalar::Nil),
            ("index", vec![s("b"), i(1)], i(1)),
            ("index", vec![s("b"), i(2)], Scalar::Nil),
            ("index", vec![s(""), i(3)], i(3)),
            ("index", vec![s(""), i(4)], Scalar::Nil),
            ("index", vec![s("c"), i(-1)], i(2)),
            ("index", vec![s("a"), i(-9)], Scalar::Nil),
        ];
        for (method, args, want) in cases {
            assert_eq!(fold(&abc, method, args).as_ref(), Some(want), "{method}{args:?}");
        }
        assert_eq!(fold(&s(""), "index", &[s("")]), Some(i(0)));
    }

    #[test]
    fn string_lookups_decline_what_they_cannot_pin() {
        let s = |v: &str| Scalar::Str(v.into());
        let abc = s("abc");
        // Ruby raises on each of these, so the reference does not fold them.
        assert_eq!(fold(&abc, "byteslice", &[s("a")]), None);
        assert_eq!(fold(&abc, "[]", &[Scalar::Nil]), None);
        assert_eq!(fold(&abc, "[]", &[Scalar::Sym("b".into())]), None);
        assert_eq!(fold(&abc, "[]", &[Scalar::Bool(true)]), None);
        assert_eq!(fold(&abc, "index", &[Scalar::Int(1)]), None);
        // Ruby truncates a Float; the core leaves that to the RBS answer.
        assert_eq!(fold(&abc, "[]", &[Scalar::Float(1.5)]), None);
        assert_eq!(fold(&abc, "index", &[s("b"), Scalar::Float(1.5)]), None);
        // Multibyte: a character offset is not a byte offset.
        assert_eq!(fold(&s("héllo"), "[]", &[Scalar::Int(1)]), None);
        assert_eq!(fold(&s("héllo"), "index", &[s("l")]), None);
        assert_eq!(fold(&abc, "index", &[s("é")]), None);
    }

    /// Issue #164 — each expectation is plain Ruby's answer, which the pinned
    /// reference folds to.
    #[test]
    fn folds_nilable_byte_and_reverse_lookups() {
        let s = |v: &str| Scalar::Str(v.into());
        let abc = s("abc");
        let i = Scalar::Int;
        let nil = Scalar::Nil;
        let cases: &[(&str, Vec<Scalar>, Scalar)] = &[
            ("getbyte", vec![i(0)], i(97)),
            ("getbyte", vec![i(-3)], i(97)),
            ("getbyte", vec![i(3)], nil.clone()),
            ("getbyte", vec![i(9)], nil.clone()),
            ("getbyte", vec![i(-9)], nil.clone()),
            ("rindex", vec![s("b")], i(1)),
            ("rindex", vec![s("z")], nil.clone()),
            ("rindex", vec![s("")], i(3)),
            ("rindex", vec![s(""), i(1)], i(1)),
            ("rindex", vec![s("c"), i(-1)], i(2)),
            ("rindex", vec![s("a"), i(-3)], i(0)),
            ("rindex", vec![s("a"), i(-4)], nil.clone()),
            ("rindex", vec![s("c"), i(9)], i(2)),
            ("rindex", vec![s("c"), i(1)], nil.clone()),
            ("rindex", vec![s("abcd")], nil.clone()),
            ("byterindex", vec![s("b")], i(1)),
            ("byterindex", vec![s("z")], nil.clone()),
            ("byterindex", vec![s("c"), i(-1)], i(2)),
            ("byterindex", vec![s(""), i(5)], i(3)),
            ("byteindex", vec![s("b")], i(1)),
            ("byteindex", vec![s("z")], nil.clone()),
            ("byteindex", vec![s("c"), i(-1)], i(2)),
            ("byteindex", vec![s("b"), i(5)], nil.clone()),
            ("byteindex", vec![s(""), i(3)], i(3)),
        ];
        for (method, args, want) in cases {
            assert_eq!(fold(&abc, method, args).as_ref(), Some(want), "{method}{args:?}");
        }
        let abcabc = s("abcabc");
        assert_eq!(fold(&abcabc, "rindex", &[s("bc"), i(3)]), Some(i(1)));
        assert_eq!(fold(&abcabc, "rindex", &[s("bc"), i(4)]), Some(i(4)));
        assert_eq!(fold(&s(""), "rindex", &[s("")]), Some(i(0)));
        // Declines: multibyte, a non-Integer index, a raising argument kind.
        assert_eq!(fold(&s("é"), "getbyte", &[i(0)]), None);
        assert_eq!(fold(&s("éa"), "rindex", &[s("a")]), None);
        assert_eq!(fold(&abc, "getbyte", &[Scalar::Float(1.9)]), None);
        assert_eq!(fold(&abc, "getbyte", &[s("a")]), None);
        assert_eq!(fold(&abc, "rindex", &[Scalar::Sym("b".into())]), None);
        assert_eq!(fold(&abc, "rindex", &[s("b"), Scalar::Float(1.0)]), None);
    }

    #[test]
    fn folds_float_spaceship() {
        let f = Scalar::Float;
        let i = Scalar::Int;
        assert_eq!(fold(&f(1.0), "<=>", &[i(2)]), Some(i(-1)));
        assert_eq!(fold(&f(2.0), "<=>", &[i(2)]), Some(i(0)));
        assert_eq!(fold(&f(-0.5), "<=>", &[i(0)]), Some(i(-1)));
        assert_eq!(fold(&f(1.5), "<=>", &[f(1.0)]), Some(i(1)));
        // Exact, not through an `as f64` cast of the Integer.
        assert_eq!(fold(&f(9_007_199_254_740_992.0), "<=>", &[i(9_007_199_254_740_993)]), Some(i(-1)));
        assert_eq!(fold(&f(1e19), "<=>", &[i(i64::MAX)]), Some(i(1)));
        assert_eq!(fold(&f(-1e19), "<=>", &[i(i64::MIN)]), Some(i(-1)));
        for other in [Scalar::Str("x".into()), Scalar::Sym("x".into()), Scalar::Nil, Scalar::Bool(true)] {
            assert_eq!(fold(&f(1.0), "<=>", &[other]), Some(Scalar::Nil));
        }
        assert_eq!(fold(&f(f64::NAN), "<=>", &[i(1)]), None);
        assert_eq!(fold(&f(1.0), "<=>", &[f(f64::INFINITY)]), None);
        assert_eq!(fold(&f(1.0), "<=>", &[]), None);
    }

    #[test]
    fn unknown_method_declines() {
        assert_eq!(fold(&Scalar::Int(1), "sample", &[]), None);
        assert_eq!(fold(&Scalar::Str("x".into()), "lenght", &[]), None);
    }

    #[test]
    fn is_foldable_mirrors_fold() {
        assert!(is_foldable("Integer", "+"));
        assert!(is_foldable("String", "upcase"));
        assert!(!is_foldable("Array", "sample"));
        assert!(!is_foldable("String", "gsub")); // sidecar territory
    }

    #[test]
    fn sidecar_foldable_is_reference_verified_subset() {
        // Every entry folds identically in the reference (real Ruby both sides).
        assert!(sidecar_foldable("Integer", "to_s"));
        assert!(sidecar_foldable("Integer", "gcd"));
        assert!(sidecar_foldable("Float", "round"));
        assert!(sidecar_foldable("String", "%"));
        assert!(sidecar_foldable("String", "center"));
        assert!(sidecar_foldable("String", "rjust"));
        // Non-deterministic / unverified stay OUT (never route what could diverge).
        assert!(!sidecar_foldable("Array", "sample"));
        assert!(!sidecar_foldable("Integer", "+")); // Rust core owns it
        assert!(!sidecar_foldable("String", "gsub")); // unverified — not routed
    }

    #[test]
    fn sidecar_declines_a_pad_past_the_byte_limit() {
        assert!(!sidecar_blows_up("center", &[Scalar::Int(4096)]));
        assert!(sidecar_blows_up("center", &[Scalar::Int(4097)]));
        assert!(sidecar_blows_up("ljust", &[Scalar::Int(3_000_000_000), Scalar::Str("-".into())]));
        assert!(!sidecar_blows_up("tr", &[Scalar::Int(9999)]));
    }

    #[test]
    fn scalar_class_maps_carriers() {
        assert_eq!(scalar_class(&Scalar::Int(1)), "Integer");
        assert_eq!(scalar_class(&Scalar::Float(1.0)), "Float");
        assert_eq!(scalar_class(&Scalar::Str("x".into())), "String");
        assert_eq!(scalar_class(&Scalar::Bool(true)), "TrueClass");
        assert_eq!(scalar_class(&Scalar::Nil), "NilClass");
    }
}
