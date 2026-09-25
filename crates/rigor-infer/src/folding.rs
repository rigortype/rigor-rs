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
                | "<" | "<=" | ">" | ">=" | "==" | "<=>"
                | "abs" | "succ" | "pred" | "even?" | "odd?" | "zero?" | "to_s"
        ),
        "Float" => matches!(
            method,
            "+" | "-" | "*" | "<" | "<=" | ">" | ">=" | "==" | "<=>" | "abs"
        ),
        "TrueClass" | "FalseClass" => matches!(method, "!" | "&" | "|" | "=="),
        "NilClass" => matches!(method, "!" | "&" | "|" | "=="),
        "Symbol" => matches!(method, "to_s" | "==" | "<=>"),
        "String" => matches!(
            method,
            "upcase" | "downcase" | "reverse" | "length" | "size" | "+" | "*"
                | "==" | "<=>" | "empty?" | "[]" | "slice" | "byteslice" | "index"
                | "rindex" | "byteindex" | "byterindex" | "getbyte"
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
            if *w > crate::kernel_fold::STRING_FOLD_BYTE_LIMIT as i64)
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
        // `rb_int_cmp`: `-1`/`0`/`1` against a numeric literal, `nil` against
        // NaN or anything else — one of the nilable lookups of issue #164.
        ("<=>", [s]) => return Some(int_cmp_scalar(a, s)),
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

// --- `<=>` ------------------------------------------------------------------
//
// Shared by the `Integer` / `Float` / `String` / `Symbol` `<=>` arms: the
// reference folds every one through `NUMERIC_BINARY` / `STRING_BINARY` /
// `SYMBOL_BINARY` by running the real method, so a fold here is exactly Ruby's
// answer — `-1`/`0`/`1` against a same-domain argument, `nil` against NaN or a
// non-comparable one (`1.0 <=> "x"` is `nil`, not a raise).

/// `rb_int_cmp` on a scalar argument.
fn int_cmp_scalar(a: i64, s: &Scalar) -> Scalar {
    match s {
        Scalar::Int(b) => Scalar::Int(a.cmp(b) as i64),
        Scalar::Float(b) => int_cmp_float(a, *b).map_or(Scalar::Nil, |o| Scalar::Int(o as i64)),
        _ => Scalar::Nil,
    }
}

/// `flo_cmp` on a scalar argument.
fn float_cmp_scalar(a: f64, s: &Scalar) -> Scalar {
    match s {
        Scalar::Float(b) => a.partial_cmp(b).map_or(Scalar::Nil, |o| Scalar::Int(o as i64)),
        Scalar::Int(b) => int_cmp_float(*b, a).map_or(Scalar::Nil, |o| Scalar::Int(o.reverse() as i64)),
        _ => Scalar::Nil,
    }
}

/// Order an `i64` against an `f64` the way Ruby's `rb_int_cmp` does — exactly,
/// so `9_007_199_254_740_993 <=> 9_007_199_254_740_992.0` answers `1` (a plain
/// `as f64` cast would collapse both to `2^53` and answer `0`). `None` on NaN.
fn int_cmp_float(a: i64, b: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering::*;
    if b.is_nan() {
        return None;
    }
    // `2^63` as f64; outside this interval the comparison is decided without
    // touching the fractional part (b is out of i64 range on that side).
    if b >= 9.223372036854776e18 {
        return Some(Less);
    }
    if b < -9.223372036854776e18 {
        return Some(Greater);
    }
    // b ∈ [-2^63, 2^63), so `b.trunc()` fits an i64 exactly.
    let trunc = b.trunc() as i64;
    match a.cmp(&trunc) {
        Equal => {
            let frac = b - trunc as f64;
            Some(if frac > 0.0 {
                Less
            } else if frac < 0.0 {
                Greater
            } else {
                Equal
            })
        }
        ord => Some(ord),
    }
}

// --- Float ------------------------------------------------------------------

fn fold_float(a: f64, method: &str, args: &[Scalar]) -> Option<Scalar> {
    if method == "abs" && args.is_empty() {
        return Some(Scalar::Float(a.abs()));
    }
    // `flo_cmp`: `-1`/`0`/`1` against a numeric literal, `nil` against NaN or
    // a non-numeric argument (`1.0 <=> "x"`) — issue #164's head row.
    if let ("<=>", [s]) = (method, args) {
        return Some(float_cmp_scalar(a, s));
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
        // `Symbol#<=>` (`SYMBOL_BINARY`): orders against another Symbol by its
        // text, `nil` against anything else.
        ("<=>", [Scalar::Sym(b)]) => Some(Scalar::Int(a.cmp(b.as_str()) as i64)),
        ("<=>", [_]) => Some(Scalar::Nil),
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
        // `String#<=>` (`STRING_BINARY`): byte-order comparison — for UTF-8
        // strings that is also codepoint order, so `str::cmp` is byte-exact.
        ("<=>", [Scalar::Str(b)]) => Some(Scalar::Int(a.cmp(b.as_str()) as i64)),
        ("<=>", [_]) => Some(Scalar::Nil),
        ("[]" | "slice" | "byteslice" | "index" | "rindex" | "byteindex"
        | "byterindex" | "getbyte", _) => fold_str_lookup(a, method, args).into_value(),
        _ => None,
    }
}

/// Whether `method` on `recv` is one of the String lookups, which can fold to
/// `nil` and so change the receiver's class downstream.
pub fn is_str_lookup(recv: &Scalar, method: &str) -> bool {
    matches!(recv, Scalar::Str(_))
        && matches!(
            method,
            "[]" | "slice" | "byteslice" | "index" | "rindex" | "byteindex"
                | "byterindex" | "getbyte"
        )
}

/// Whether `method` on a pinned `recv` is a nilable `<=>` — folds to
/// `-1`/`0`/`1` or `nil`, and whose flat RBS slot drops the nil arm just like
/// the String lookups. Covers `Integer` / `Float` (`NUMERIC_BINARY`), `String`
/// (`STRING_BINARY`) and `Symbol` (`SYMBOL_BINARY`) receivers.
pub fn is_nilable_cmp(recv: &Scalar, method: &str) -> bool {
    method == "<=>"
        && matches!(recv, Scalar::Int(_) | Scalar::Float(_) | Scalar::Str(_) | Scalar::Sym(_))
}

/// Whether `method` on a pinned `recv` can fold to `nil`, which changes the
/// result's CLASS downstream — the stale-local decline gate keys on this: the
/// flat env keeps a local's first literal across `<<` / `+=` / branch / block
/// writes, so a nil-producing fold must not trust a value read from it.
pub fn fold_can_go_nil(recv: &Scalar, method: &str) -> bool {
    is_str_lookup(recv, method) || is_nilable_cmp(recv, method)
}

/// The outcome of a String lookup fold on pinned scalar arguments.
///
/// The reference folds these by *executing* the real method behind a purity
/// allowlist (`STRING_BINARY` + the `purity: leaf` catalog entries), so every
/// arm below must answer exactly what CRuby answers — including `nil` — or
/// signal why it cannot.
#[derive(Debug)]
pub enum LookupFold {
    /// Byte-exact Ruby answer — the caller mints the `Constant`.
    Value(Scalar),
    /// The pinned argument list is a shape Ruby RAISES on (`TypeError`,
    /// `ArgumentError`, `RangeError`). The reference's fold rescues, then its
    /// RBS tier answers the `C?` union — on which no negative rule fires.
    /// The caller declines to `Dynamic` (silent), never the flat `C`.
    Raises,
    /// The fold is computable but the RESULT cannot ride a `Scalar` — only a
    /// `byteslice` that lands inside a multibyte character (Ruby returns the
    /// invalid-byte String; `Scalar::Str` cannot hold it). The caller keeps
    /// the flat RBS answer: `for String` where the reference says `for
    /// "<byte>"` — same row.
    Decline,
}

impl LookupFold {
    /// `Some` for [`LookupFold::Value`], `None` otherwise — the view the
    /// generic [`fold`] path takes of a lookup.
    fn into_value(self) -> Option<Scalar> {
        match self {
            LookupFold::Value(s) => Some(s),
            _ => None,
        }
    }
}

/// `NUM2LONG` on a scalar argument: an Integer is itself; a Float truncates
/// toward zero (`"abc".getbyte(1.5)` is `getbyte(1)`, probed); everything else
/// — plus a non-finite or out-of-`long`-range Float — raises in Ruby, so the
/// fold answers `None` and the caller takes the `Raises` arm.
fn to_int(s: &Scalar) -> Option<i64> {
    match s {
        Scalar::Int(i) => Some(*i),
        // `2^63` bounds: `trunc` fits `i64` iff it lies in [-2^63, 2^63).
        Scalar::Float(f)
            if f.is_finite() && *f >= -9.223372036854776e18 && *f < 9.223372036854776e18 =>
        {
            Some(f.trunc() as i64)
        }
        _ => None,
    }
}

/// First byte position of `needle` in `hay` (`None` = not found); an empty
/// needle matches at position 0, as CRuby's `rb_str_index` does.
fn byte_find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Last byte position `<= cap` where `needle` starts in `hay`. `rindex` /
/// `byterindex` bound the match's START, not its end — `"abcd".rindex("bcd", 1)`
/// is `1` even though the match extends past position 1 (probed). An empty
/// needle matches at `cap` itself.
fn byte_rfind(hay: &[u8], needle: &[u8], cap: usize) -> Option<usize> {
    let cap = cap.min(hay.len());
    if needle.is_empty() {
        return Some(cap);
    }
    (0..=cap)
        .rev()
        .find(|&i| hay.get(i..i + needle.len()) == Some(needle))
}

/// Byte offset of the `n`-th char of `a` (`n` may be the char count — answers
/// `a.len()` then, the position "just past the end" `index` / `rindex` use).
fn char_to_byte(a: &str, n: usize) -> usize {
    a.char_indices().nth(n).map_or(a.len(), |(b, _)| b)
}

/// Char index of a byte offset produced by `byte_find` / `byte_rfind` on a
/// `&str` needle: a UTF-8 needle can only start on a char boundary, so this
/// always lands exactly.
fn byte_to_char_pos(a: &str, byte: usize) -> usize {
    a.char_indices().take_while(|(b, _)| *b < byte).count()
}

/// `String#getbyte` — byte-domain, so it folds on any receiver. A negative
/// index counts from the end; out of range is `nil`.
fn fold_getbyte(a: &str, i: i64) -> Scalar {
    let len = a.len() as i64;
    let i = if i < 0 { i + len } else { i };
    if !(0..len).contains(&i) {
        Scalar::Nil
    } else {
        Scalar::Int(a.as_bytes()[i as usize] as i64)
    }
}

/// `String#index` — char-domain. A negative offset counts from the end; an
/// offset outside `0..=len` is `nil` (offset `len` can still match `""`).
fn fold_index(a: &str, sub: &str, offset: i64) -> Scalar {
    let len = a.chars().count() as i64;
    let offset = if offset < 0 { offset + len } else { offset };
    if !(0..=len).contains(&offset) {
        return Scalar::Nil;
    }
    let from = char_to_byte(a, offset as usize);
    match byte_find(&a.as_bytes()[from..], sub.as_bytes()) {
        Some(p) => Scalar::Int(byte_to_char_pos(a, from + p) as i64),
        None => Scalar::Nil,
    }
}

/// `String#rindex` — char-domain twin of [`fold_index`]: last occurrence whose
/// START is at or before `pos` (negative counts from the end, still negative
/// is `nil`, `pos >= len` searches the whole string).
fn fold_rindex(a: &str, sub: &str, pos: i64) -> Scalar {
    let len = a.chars().count() as i64;
    let pos = if pos < 0 { pos + len } else { pos };
    if pos < 0 {
        return Scalar::Nil;
    }
    match byte_rfind(a.as_bytes(), sub.as_bytes(), char_to_byte(a, pos.min(len) as usize)) {
        Some(p) => Scalar::Int(byte_to_char_pos(a, p) as i64),
        None => Scalar::Nil,
    }
}

/// `String#byteindex` — byte-domain twin of [`fold_index`]. The offset is in
/// bytes and may legitimately land inside a multibyte character, so the search
/// runs on `as_bytes`, never on `&str` slices.
fn fold_byteindex(a: &str, sub: &str, offset: i64) -> Scalar {
    let len = a.len() as i64;
    let offset = if offset < 0 { offset + len } else { offset };
    if !(0..=len).contains(&offset) {
        return Scalar::Nil;
    }
    match byte_find(&a.as_bytes()[offset as usize..], sub.as_bytes()) {
        Some(p) => Scalar::Int(offset + p as i64),
        None => Scalar::Nil,
    }
}

/// `String#byterindex` — byte-domain twin of [`fold_rindex`].
fn fold_byterindex(a: &str, sub: &str, offset: i64) -> Scalar {
    let len = a.len() as i64;
    let offset = if offset < 0 { offset + len } else { offset };
    if offset < 0 {
        return Scalar::Nil;
    }
    match byte_rfind(a.as_bytes(), sub.as_bytes(), offset.min(len) as usize) {
        Some(p) => Scalar::Int(p as i64),
        None => Scalar::Nil,
    }
}

/// `String#[]` / `#slice` / `#byteslice` / `#index` / `#rindex` / `#byteindex`
/// / `#byterindex` / `#getbyte` on pinned scalars (issue #121 step 1, extended
/// by issue #164).
///
/// The folds are char-exact for `[]` / `slice` / `index` / `rindex` and
/// byte-exact for `byteslice` / `byteindex` / `byterindex` / `getbyte`, so
/// they hold on multibyte receivers too — a result only stops being
/// representable when `byteslice` cuts inside a character
/// ([`LookupFold::Decline`]). Everything else a pinned-scalar argument can do
/// is a Ruby raise (`TypeError` / `ArgumentError` / `RangeError`), which the
/// reference rescues into the withholding `C?` union — [`LookupFold::Raises`].
/// Non-scalar argument kinds (`Range`, `Regexp`) never reach this function:
/// they do not pin, and the dispatch site handles them.
pub fn fold_str_lookup(a: &str, method: &str, args: &[Scalar]) -> LookupFold {
    let char_len = a.chars().count() as i64;
    // `rb_str_subpos` in characters (`[]` / `slice`): a negative start counts
    // from the end; a start past the end or a negative count is `nil`; a start
    // AT the end is `""`.
    let substr_chars = |start: i64, count: i64| -> Scalar {
        let start = if start < 0 { start + char_len } else { start };
        if count < 0 || start < 0 || start > char_len {
            return Scalar::Nil;
        }
        Scalar::Str(a.chars().skip(start as usize).take(count as usize).collect())
    };
    // The byte-domain twin for `byteslice`. The result may not be valid UTF-8
    // (a mid-character cut); `str::get` answers `None` there, which surfaces
    // as `Decline`, not a bogus `Scalar::Str`.
    let substr_bytes = |start: i64, count: i64| -> LookupFold {
        let len = a.len() as i64;
        let start = if start < 0 { start + len } else { start };
        if count < 0 || start < 0 || start > len {
            return LookupFold::Value(Scalar::Nil);
        }
        let end = start.saturating_add(count).min(len);
        match a.get(start as usize..end as usize) {
            Some(s) => LookupFold::Value(Scalar::Str(s.to_owned())),
            None => LookupFold::Decline,
        }
    };
    match (method, args) {
        ("getbyte", [s]) => match to_int(s) {
            Some(i) => LookupFold::Value(fold_getbyte(a, i)),
            None => LookupFold::Raises,
        },
        ("[]" | "slice", [s]) => match s {
            Scalar::Str(sub) => LookupFold::Value(
                if a.contains(sub.as_str()) { Scalar::Str(sub.clone()) } else { Scalar::Nil },
            ),
            _ => match to_int(s) {
                Some(i) => {
                    let i = if i < 0 { i + char_len } else { i };
                    LookupFold::Value(match a.chars().nth(i as usize).filter(|_| i >= 0) {
                        Some(c) => Scalar::Str(c.to_string()),
                        None => Scalar::Nil,
                    })
                }
                None => LookupFold::Raises,
            },
        },
        ("byteslice", [s]) => match to_int(s) {
            Some(i) => substr_bytes(i, 1),
            None => LookupFold::Raises,
        },
        ("[]" | "slice", [s, t]) => match (to_int(s), to_int(t)) {
            (Some(start), Some(count)) => LookupFold::Value(substr_chars(start, count)),
            _ => LookupFold::Raises,
        },
        ("byteslice", [s, t]) => match (to_int(s), to_int(t)) {
            (Some(start), Some(count)) => substr_bytes(start, count),
            _ => LookupFold::Raises,
        },
        ("index" | "rindex" | "byteindex" | "byterindex", [Scalar::Str(sub)]) => {
            let v = match method {
                "index" => fold_index(a, sub, 0),
                "rindex" => fold_rindex(a, sub, a.chars().count() as i64),
                "byteindex" => fold_byteindex(a, sub, 0),
                _ => fold_byterindex(a, sub, a.len() as i64),
            };
            LookupFold::Value(v)
        }
        ("index" | "rindex" | "byteindex" | "byterindex", [Scalar::Str(sub), off]) => {
            match to_int(off) {
                Some(o) => {
                    let v = match method {
                        "index" => fold_index(a, sub, o),
                        "rindex" => fold_rindex(a, sub, o),
                        "byteindex" => fold_byteindex(a, sub, o),
                        _ => fold_byterindex(a, sub, o),
                    };
                    LookupFold::Value(v)
                }
                None => LookupFold::Raises,
            }
        }
        // 0-arg and over-arity calls raise `ArgumentError`; any other pinned
        // argument kind raises `TypeError`. Both are silent on the reference
        // (its fold rescues into the `C?` union), so decline rather than the
        // flat `Integer` / `String` the tier-3 slot would mint.
        _ => LookupFold::Raises,
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

    /// Issue #164 — the nilable lookups. Every expectation is the pinned
    /// reference's measured fold (and plain Ruby's answer); a hit answers the
    /// constant, a miss answers `nil`.
    #[test]
    fn folds_nilable_string_lookups() {
        let s = |v: &str| Scalar::Str(v.into());
        let i = Scalar::Int;
        let abc = s("abc");
        let cases: &[(&str, Vec<Scalar>, Scalar)] = &[
            ("getbyte", vec![i(0)], i(97)),
            ("getbyte", vec![i(-1)], i(99)),
            ("getbyte", vec![i(2)], i(99)),
            ("getbyte", vec![i(3)], Scalar::Nil),
            ("getbyte", vec![i(9)], Scalar::Nil),
            ("getbyte", vec![i(-4)], Scalar::Nil),
            ("getbyte", vec![Scalar::Float(1.5)], i(98)), // Float truncates
            ("getbyte", vec![Scalar::Float(9.9)], Scalar::Nil),
            ("rindex", vec![s("b")], i(1)),
            ("rindex", vec![s("z")], Scalar::Nil),
            ("rindex", vec![s("")], i(3)),
            ("rindex", vec![s("b"), i(0)], Scalar::Nil),
            ("rindex", vec![s("b"), i(-1)], i(1)),
            ("rindex", vec![s("a"), i(0)], i(0)),
            ("rindex", vec![s("ca"), i(2)], Scalar::Nil),
            ("byteindex", vec![s("b")], i(1)),
            ("byteindex", vec![s("z")], Scalar::Nil),
            ("byteindex", vec![s("b"), i(2)], Scalar::Nil),
            ("byteindex", vec![s("c"), i(-1)], i(2)),
            ("byterindex", vec![s("b")], i(1)),
            ("byterindex", vec![s("z")], Scalar::Nil),
            ("byterindex", vec![s("b"), i(-2)], i(1)),
            ("byterindex", vec![s("c"), i(4)], i(2)), // offset past end searches all
        ];
        for (method, args, want) in cases {
            assert_eq!(fold(&abc, method, args).as_ref(), Some(want), "{method}{args:?}");
        }
        // `"abcabc".rindex("ca", 2)` bounds the match's START, not its end.
        assert_eq!(fold(&s("abcabc"), "rindex", &[s("ca"), i(2)]), Some(i(2)));
        // Byte-domain lookups answer BYTE offsets on multibyte receivers.
        let hel = s("héllo");
        assert_eq!(fold(&hel, "index", &[s("l")]), Some(i(2)));
        assert_eq!(fold(&hel, "byteindex", &[s("l")]), Some(i(3)));
        assert_eq!(fold(&hel, "rindex", &[s("l")]), Some(i(3)));
        assert_eq!(fold(&hel, "byterindex", &[s("l")]), Some(i(4)));
        assert_eq!(fold(&hel, "getbyte", &[i(2)]), Some(i(0xa9)));
        assert_eq!(fold(&hel, "[]", &[i(1)]), Some(s("é")));
        assert_eq!(fold(&abc, "index", &[s("é")]), Some(Scalar::Nil));
        assert_eq!(fold(&abc, "index", &[s("b"), Scalar::Float(1.5)]), Some(i(1)));
    }

    /// A pinned argument list Ruby raises on is [`LookupFold::Raises`] — the
    /// reference rescues into the `C?` union (silent on negative rules), so
    /// the dispatch site declines to `Dynamic`. `fold` still surfaces `None`.
    #[test]
    fn string_lookups_raise_on_ill_typed_pins() {
        let s = |v: &str| Scalar::Str(v.into());
        let abc = s("abc");
        let cases: &[(&str, Vec<Scalar>)] = &[
            ("byteslice", vec![s("a")]),
            ("[]", vec![Scalar::Nil]),
            ("[]", vec![Scalar::Sym("b".into())]),
            ("[]", vec![Scalar::Bool(true)]),
            ("index", vec![Scalar::Int(1)]),
            ("index", vec![s("b"), s("x")]),
            ("index", vec![]),
            ("index", vec![s("b"), Scalar::Int(0), Scalar::Int(1)]),
            ("rindex", vec![Scalar::Int(1)]),
            ("rindex", vec![s("b"), Scalar::Nil]),
            ("getbyte", vec![s("x")]),
            ("getbyte", vec![Scalar::Nil]),
            ("getbyte", vec![Scalar::Float(f64::NAN)]),
            ("getbyte", vec![Scalar::Float(1e19)]), // out of long range
            ("getbyte", vec![Scalar::Int(0), Scalar::Int(1)]),
        ];
        for (method, args) in cases {
            match fold_str_lookup("abc", method, args) {
                LookupFold::Raises => {}
                other => panic!("{method}{args:?}: expected Raises, got {other:?}"),
            }
            assert_eq!(fold(&abc, method, args), None);
        }
        // A `byteslice` that lands inside a multibyte character is
        // `LookupFold::Decline` — the byte string cannot ride `Scalar::Str`.
        match fold_str_lookup("héllo", "byteslice", &[Scalar::Int(2)]) {
            LookupFold::Decline => {}
            other => panic!("expected Decline, got {other:?}"),
        }
    }

    /// Issue #164 — `<=>` folds: `-1`/`0`/`1` against a same-domain argument,
    /// `nil` against NaN or a non-comparable one.
    #[test]
    fn folds_nilable_cmp() {
        let i = Scalar::Int;
        assert_eq!(fold(&Scalar::Float(1.0), "<=>", &[i(2)]), Some(i(-1)));
        assert_eq!(fold(&Scalar::Float(1.0), "<=>", &[i(1)]), Some(i(0)));
        assert_eq!(fold(&Scalar::Float(2.0), "<=>", &[i(1)]), Some(i(1)));
        assert_eq!(fold(&Scalar::Float(1.0), "<=>", &[Scalar::Str("x".into())]), Some(Scalar::Nil));
        assert_eq!(fold(&Scalar::Float(1.0), "<=>", &[Scalar::Float(f64::NAN)]), Some(Scalar::Nil));
        assert_eq!(fold(&i(1), "<=>", &[i(2)]), Some(i(-1)));
        assert_eq!(fold(&i(1), "<=>", &[Scalar::Str("x".into())]), Some(Scalar::Nil));
        assert_eq!(fold(&i(1), "<=>", &[Scalar::Float(f64::NAN)]), Some(Scalar::Nil));
        // `rb_int_cmp` is exact across the f64 boundary, not a plain cast.
        assert_eq!(
            fold(&i(9_007_199_254_740_993), "<=>", &[Scalar::Float(9_007_199_254_740_992.0)]),
            Some(i(1))
        );
        let a = Scalar::Str("a".into());
        assert_eq!(fold(&a, "<=>", &[Scalar::Str("b".into())]), Some(i(-1)));
        assert_eq!(fold(&a, "<=>", &[i(1)]), Some(Scalar::Nil));
        let sym = Scalar::Sym("a".into());
        assert_eq!(fold(&sym, "<=>", &[Scalar::Sym("b".into())]), Some(i(-1)));
        assert_eq!(fold(&sym, "<=>", &[Scalar::Str("a".into())]), Some(Scalar::Nil));
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
