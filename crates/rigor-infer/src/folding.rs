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
            "+" | "-" | "*" | "<" | "<=" | ">" | ">=" | "==" | "abs"
        ),
        "TrueClass" | "FalseClass" => matches!(method, "!" | "&" | "|" | "=="),
        "NilClass" => matches!(method, "!" | "&" | "|" | "=="),
        "Symbol" => matches!(method, "to_s" | "=="),
        "String" => matches!(
            method,
            "upcase" | "downcase" | "reverse" | "length" | "size" | "+" | "*"
                | "==" | "empty?"
        ),
        _ => false,
    }
}

/// The Ruby class name of a scalar literal — the receiver-class key used to gate
/// [`sidecar_foldable`]. `Bool`/`Nil` map to their singleton classes.
#[must_use]
pub fn scalar_class(s: &Scalar) -> &'static str {
    match s {
        Scalar::Int(_) => "Integer",
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
    matches!(
        (receiver_class, method),
        // Integer#to_s(base) — the Rust core folds only base-10 `to_s`; a base
        // argument routes here (`255.to_s(16) => "ff"`).
        ("Integer", "to_s")
        // String#% (format) — a pure, deterministic long-tail fold.
        | ("String", "%")
    )
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
}
