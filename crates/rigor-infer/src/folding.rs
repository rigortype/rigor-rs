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
//! non-determinism, a non-modeled method, a non-ASCII string, a divide-by-zero
//! — it returns `None`, and the dispatcher widens to the nominal type rather
//! than minting a spurious `Constant` (ADR-0023 tier-5). Integer overflow is
//! not a decline: an out-of-`i64` result promotes to the `Scalar::BigInt`
//! decimal spelling, matching the reference's arbitrary-precision `Constant`,
//! up to the reference's `INTEGER_FOLD_BIT_LIMIT` gate.
//!
// TODO(spec): the long tail (Float formatting, encoding-sensitive String,
// Rational/Complex, Date/Time, Regexp, `String#%`, `(1..10).first(3)`) goes to
// the Ruby sidecar (ADR-0008). Argument-count / argument-type validation and
// the full purity catalogue belong to the dispatcher, not this leaf.

use rigor_types::Scalar;

/// The reference's three fold results (`constant_folding.rb`): a folded
/// `Constant`, a `WidenedResult` — the result's CLASS when
/// `INTEGER_FOLD_BIT_LIMIT` keeps the value out of `Constant`, which
/// `build_constant_type` turns into `Nominal[class]` — or a decline.
///
/// rigor-rs#694: where the port once declined an oversized `**` / `<<` fold
/// to `Dynamic`, the reference answers `Nominal[Integer]` (or `[Rational]`
/// for a negative exponent) and `call.undefined-method` fires on it.
/// [`fold`] keeps its `Option<Scalar>` shape — only the callers that can
/// carry the nominal consume [`fold_outcome`] directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FoldOutcome {
    /// A `Constant[…]` result (`fold_result`'s `[value]`).
    Value(Scalar),
    /// A `WidenedResult`: the fold knows only the result's class —
    /// `"Integer"`, `"Rational"`, … — for a number past
    /// `INTEGER_FOLD_BIT_LIMIT`.
    Widened(&'static str),
    /// The fold cannot decide (`invoke_*`'s `nil`).
    Declined,
}

impl FoldOutcome {
    fn of(result: Option<Scalar>) -> Self {
        match result {
            Some(s) => FoldOutcome::Value(s),
            None => FoldOutcome::Declined,
        }
    }
}

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
/// (divide-by-zero, non-ASCII string, a fold past the reference's integer bit
/// limit) where Rust's result could diverge from Ruby's.
pub fn fold(receiver: &Scalar, method: &str, args: &[Scalar]) -> Option<Scalar> {
    match fold_outcome(receiver, method, args) {
        FoldOutcome::Value(s) => Some(s),
        FoldOutcome::Widened(_) | FoldOutcome::Declined => None,
    }
}

/// [`fold`] with the reference's third answer kept: a result past
/// `INTEGER_FOLD_BIT_LIMIT` reports `FoldOutcome::Widened` (the
/// `WidenedResult`'s class name) instead of `None`. The reference mints it
/// in exactly two places (`constant_folding.rb`) — `numeric_blow_up_class`,
/// the operand precheck, and `fold_result`'s `oversized_number?` after-fact
/// gate — mirrored here as [`numeric_blow_up_class`] and [`fold_result_gate`]
/// ([`big_scalar`] / [`rational_oversized`]), the only two functions that
/// mint a `Widened` (rigor-rs#766).
pub fn fold_outcome(receiver: &Scalar, method: &str, args: &[Scalar]) -> FoldOutcome {
    match (method, args) {
        // `nil?` answers from the pin's identity alone — `nil.nil?` is
        // `true`, every other scalar `false`. (`!` stays per-receiver —
        // `fold_bool`/`fold_nil` — matching the oracle's BOOL_UNARY /
        // NIL_UNARY boundary: `!"x"` types `bool` NOMINAL there, so both
        // the predicate fold AND the certainty fold decline — `q = "x";
        // if !q` is live on both arms and silent on the predicate.)
        ("nil?", []) => return FoldOutcome::Value(Scalar::Bool(matches!(receiver, Scalar::Nil))),
        // `!=` is the `==` fold negated — sharing the arm keeps the same
        // operand-kind declines.
        ("!=", [_]) => {
            return match fold(receiver, "==", args) {
                Some(Scalar::Bool(b)) => FoldOutcome::Value(Scalar::Bool(!b)),
                _ => FoldOutcome::Declined,
            };
        }
        _ => {}
    }
    match receiver {
        Scalar::Int(a) => fold_int(*a, method, args),
        // A Bignum folds the members whose answer needs only the decimal
        // spelling — sign, parity, magnitude, ordering — and declines
        // arithmetic, whose operand width the core does not model
        // (rigor-rs#682).
        Scalar::BigInt(d) => fold_bigint(d, method, args),
        Scalar::Float(a) => FoldOutcome::of(fold_float(*a, method, args)),
        Scalar::Bool(a) => FoldOutcome::of(fold_bool(*a, method, args)),
        Scalar::Nil => FoldOutcome::of(fold_nil(method, args)),
        Scalar::Sym(a) => FoldOutcome::of(fold_sym(a, method, args)),
        Scalar::Str(a) => FoldOutcome::of(fold_str(a, method, args)),
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
            "+" | "-" | "*" | "/" | "%" | "**" | "pow" | "&" | "|" | "^" | "<<" | ">>"
                | "<" | "<=" | ">" | ">=" | "=="
                | "abs" | "succ" | "pred" | "even?" | "odd?" | "zero?" | "to_s"
                | "+@" | "-@"
        ),
        "Float" => matches!(
            method,
            "+" | "-" | "*" | "<" | "<=" | ">" | ">=" | "==" | "abs" | "<=>"
                | "+@" | "-@"
        ),
        "TrueClass" | "FalseClass" => matches!(method, "!" | "&" | "|" | "=="),
        "NilClass" => matches!(method, "!" | "&" | "|" | "=="),
        "Symbol" => matches!(method, "to_s" | "=="),
        "String" => matches!(
            method,
            "upcase" | "downcase" | "reverse" | "length" | "size" | "+" | "*"
                | "==" | "empty?" | "[]" | "slice" | "byteslice" | "index"
                | "getbyte" | "rindex" | "byteindex" | "byterindex" | "+@"
                | "-@" | "to_s" | "to_str"
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
        && (matches!(args.first(), Some(Scalar::Int(w))
            if *w > crate::kernel_fold::STRING_FOLD_BYTE_LIMIT as i64)
            // A Bignum width exceeds the byte limit by construction.
            || matches!(args.first(), Some(Scalar::BigInt(_))))
}

/// Upstream v0.4.6 (#2125 / #2247, `constant_folding.rb` `OVERSIZED_STRING`):
/// whether a String-producing fold on a value-pinned receiver would build (or
/// is handed) a String past [`crate::kernel_fold::STRING_FOLD_BYTE_LIMIT`]
/// bytes. The reference declines the WHOLE fold there and a later tier
/// answers `literal-string` (or `literal-string & non-empty-string`), a
/// carrier the port does not model; the caller types the call untyped
/// instead, which only stays silent. The prechecks mirror
/// `string_blow_up?` (`*`, `+`), `format_blow_up?` (`%`),
/// `string_pad_blow_up?` (`center` / `ljust` / `rjust`) and
/// `string_unary_blow_up?` (a String receiver already at the limit).
pub fn string_fold_oversized(receiver: &Scalar, method: &str, args: &[Scalar]) -> bool {
    const LIMIT: usize = crate::kernel_fold::STRING_FOLD_BYTE_LIMIT;
    let Scalar::Str(recv) = receiver else {
        return false;
    };
    // `integer_operand`: an Integer, a finite Float's truncation; a Bignum is
    // past any limit (`None` = huge).
    let operand = |s: &Scalar| -> Option<Option<i128>> {
        match s {
            Scalar::Int(n) => Some(Some(i128::from(*n))),
            Scalar::BigInt(d) => Some(if d.starts_with('-') { Some(-1) } else { None }),
            Scalar::Float(f) if f.is_finite() => Some(Some(f.trunc().clamp(-1e30, 1e30) as i128)),
            _ => None,
        }
    };
    match (method, args) {
        ("*", [count]) => match operand(count) {
            Some(Some(n)) => n >= 0 && (recv.len() as i128) * n > LIMIT as i128,
            Some(None) => !recv.is_empty(),
            None => false,
        },
        ("+", [Scalar::Str(b)]) => recv.len() + b.len() > LIMIT,
        ("%", [arg]) => format_blows_up(recv, std::slice::from_ref(arg)),
        ("center" | "ljust" | "rjust", [width, ..]) => match operand(width) {
            Some(Some(w)) => w > LIMIT as i128,
            Some(None) => true,
            None => false,
        },
        (_, []) => recv.len() >= LIMIT,
        _ => false,
    }
}

/// `format_blow_up?`: a directive width / precision past the byte limit, or a
/// `*` width with an Integer argument past it.
pub fn format_blows_up(format: &str, args: &[Scalar]) -> bool {
    const LIMIT: u128 = crate::kernel_fold::STRING_FOLD_BYTE_LIMIT as u128;
    let bytes = format.as_bytes();
    let mut star = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        if bytes.get(i + 1) == Some(&b'%') {
            i += 2;
            continue;
        }
        // `%(?:<[^>]*>|\{[^}]*\}|[-+ 0#*$.\d])*`, with the `<…>` / `{…}`
        // names and `N$` positions removed before the digits are read.
        i += 1;
        let mut spec: Vec<u8> = Vec::new();
        loop {
            match bytes.get(i) {
                Some(b'<') | Some(b'{') => {
                    let close = if bytes[i] == b'<' { b'>' } else { b'}' };
                    match bytes[i + 1..].iter().position(|&b| b == close) {
                        Some(p) => i += p + 2,
                        None => break,
                    }
                }
                Some(&b) if b"-+ 0#*$.".contains(&b) || b.is_ascii_digit() => {
                    spec.push(b);
                    i += 1;
                }
                _ => break,
            }
        }
        // Drop `\d+\$` positions.
        let mut cleaned: Vec<u8> = Vec::new();
        let mut j = 0;
        while j < spec.len() {
            if spec[j].is_ascii_digit() {
                let start = j;
                while j < spec.len() && spec[j].is_ascii_digit() {
                    j += 1;
                }
                if spec.get(j) == Some(&b'$') {
                    j += 1;
                    continue;
                }
                cleaned.extend_from_slice(&spec[start..j]);
            } else {
                cleaned.push(spec[j]);
                j += 1;
            }
        }
        let mut k = 0;
        while k < cleaned.len() {
            if cleaned[k].is_ascii_digit() {
                let start = k;
                while k < cleaned.len() && cleaned[k].is_ascii_digit() {
                    k += 1;
                }
                let digits = &cleaned[start..k];
                let trimmed: &[u8] = {
                    let z = digits.iter().position(|&b| b != b'0').unwrap_or(digits.len());
                    &digits[z..]
                };
                if trimmed.len() > 5 {
                    return true;
                }
                let v: u128 = std::str::from_utf8(trimmed).ok().and_then(|t| t.parse().ok()).unwrap_or(0);
                if v > LIMIT {
                    return true;
                }
            } else {
                k += 1;
            }
        }
        star |= cleaned.contains(&b'*');
    }
    star && args.iter().any(|a| match a {
        Scalar::Int(n) => n.unsigned_abs() as u128 > LIMIT,
        Scalar::BigInt(_) => true,
        Scalar::Float(f) if f.is_finite() => f.trunc().abs() > LIMIT as f64,
        _ => false,
    })
}

/// `oversized_string?`: a String result past the byte limit.
pub fn oversized_string_result(value: &Scalar) -> bool {
    matches!(value, Scalar::Str(s) if s.len() > crate::kernel_fold::STRING_FOLD_BYTE_LIMIT)
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

fn fold_int(a: i64, method: &str, args: &[Scalar]) -> FoldOutcome {
    // Nullary, deterministic.
    match (method, args) {
        // `abs`/`succ`/`pred`/`<<` promote an out-of-`i64` result to
        // `Scalar::BigInt` like the binary ops below (rigor-rs#682).
        ("abs", []) => return FoldOutcome::Value(int_scalar(i128::from(a).abs())),
        ("succ", []) => return FoldOutcome::Value(int_scalar(i128::from(a) + 1)),
        ("pred", []) => return FoldOutcome::Value(int_scalar(i128::from(a) - 1)),
        ("even?", []) => return FoldOutcome::Value(Scalar::Bool(a % 2 == 0)),
        ("odd?", []) => return FoldOutcome::Value(Scalar::Bool(a % 2 != 0)),
        ("zero?", []) => return FoldOutcome::Value(Scalar::Bool(a == 0)),
        ("positive?", []) => return FoldOutcome::Value(Scalar::Bool(a > 0)),
        ("negative?", []) => return FoldOutcome::Value(Scalar::Bool(a < 0)),
        // `to_s` with no radix only (a radix arg changes the base — not folded
        // here to keep the core trivially byte-exact with Ruby's decimal form).
        ("to_s", []) => return FoldOutcome::Value(Scalar::Str(a.to_string())),
        // Unary `+x` / `-x` — the literal stays pinned (rigor-rs#462: `+"ab"`,
        // `-1` still read the nominal where the reference folds the scalar).
        ("+@", []) => return FoldOutcome::Value(Scalar::Int(a)),
        ("-@", []) => return FoldOutcome::Value(int_scalar(-i128::from(a))),
        _ => {}
    }

    // Comparisons also fold against a Bignum argument — the reference
    // compares integers exactly at any size (`5 < 99999999999999999999`).
    if let ("==" | "<" | "<=" | ">" | ">=", [b]) = (method, args) {
        if let Some(o) = int_scalar_cmp(&Scalar::Int(a), b) {
            return FoldOutcome::Value(Scalar::Bool(cmp_bool(method, o)));
        }
    }

    // `numeric_blow_up_class` (`constant_folding.rb:1910`) — the operand
    // precheck the `**`/`pow`/`<<`/`>>` folds consult first, like the
    // reference's `invoke_binary`: it answers the widened class from the
    // operands alone, so the huge value is never computed.
    if let ("**" | "pow" | "<<" | ">>", [b]) = (method, args) {
        if let Some(outcome) = numeric_blow_up_class(IntReceiver::Int(a), method, b) {
            return outcome;
        }
    }

    // `<<` / `>>` take a Bignum count too: past `i128` only its sign matters
    // (`1 >> 99999999999999999999` is `0`; `<<` blows the bit limit either
    // way it is reached) — same argument handling as [`fold_bigint`]'s.
    if let ("<<" | ">>", [b]) = (method, args) {
        let shift = match b {
            Scalar::Int(v) => i128::from(*v),
            Scalar::BigInt(d) => d
                .parse::<i128>()
                .unwrap_or(if d.starts_with('-') { i128::MIN } else { i128::MAX }),
            _ => return FoldOutcome::Declined,
        };
        return shift_result(a, if method == "<<" { shift } else { shift.saturating_neg() });
    }

    // `**` / `pow` — `numeric_blow_up_class` already answered the widened
    // cases (a Bignum exponent included); what remains is the compute — unit
    // bases, `checked_pow`, the decimal path through `big_scalar` /
    // `rational_oversized`.
    if let ("**" | "pow", [b]) = (method, args) {
        return int_pow_result(a, b);
    }

    // `Integer#pow(exp, mod)` — the reference's `invoke_ternary` path: `pow`
    // is a catalog `:leaf`, and there is NO `numeric_blow_up_class` precheck
    // for the two-arg shape — the result is bounded by |mod| anyway
    // (rigor-rs#769).
    if let ("pow", [e, m]) = (method, args) {
        return int_pow_mod_result(IntReceiver::Int(a), e, m);
    }

    // Binary on a single Integer argument.
    let b = match args {
        [Scalar::Int(b)] => *b,
        _ => return FoldOutcome::Declined,
    };
    match method {
        // Overflow promotes to the `Scalar::BigInt` decimal spelling — the
        // same carrier a Bignum literal pins (rigor-rs#194) — matching the
        // reference's arbitrary-precision `Constant` (rigor-rs#682). Every
        // i64 op result fits `i128`, so this never wraps and never declines.
        "+" => FoldOutcome::Value(int_scalar(i128::from(a) + i128::from(b))),
        "-" => FoldOutcome::Value(int_scalar(i128::from(a) - i128::from(b))),
        "*" => FoldOutcome::Value(int_scalar(i128::from(a) * i128::from(b))),

        // Ruby integer division floors toward negative infinity, and 0-divisor
        // raises. Decline on zero. `div_euclid` is NOT floor on a negative
        // divisor (`7 / -2` came out `-3` where Ruby floors to `-4` — the
        // pre-existing drift the review caught); truncate then adjust by the
        // sign-of-divisor rule,
        // the same rule `ruby_mod` applies. `i64::MIN / -1` promotes.
        "/" => {
            if b == 0 {
                FoldOutcome::Declined
            } else {
                let (a, b) = (i128::from(a), i128::from(b));
                let (q, r) = (a / b, a % b);
                FoldOutcome::Value(int_scalar(if r != 0 && (r < 0) != (b < 0) { q - 1 } else { q }))
            }
        }
        // Ruby `%` result takes the sign of the divisor (rem_euclid is for
        // positive b; use a sign-of-divisor adjustment).
        "%" => {
            if b == 0 {
                FoldOutcome::Declined
            } else {
                FoldOutcome::Value(Scalar::Int(ruby_mod(a, b)))
            }
        }
        "&" => FoldOutcome::Value(Scalar::Int(a & b)),
        "|" => FoldOutcome::Value(Scalar::Int(a | b)),
        "^" => FoldOutcome::Value(Scalar::Int(a ^ b)),
        // Bit reference `n[i]` — `0[0] -> 0`, `5[2] -> 1`. A negative
        // index reads `0`; past `i64` a non-negative receiver reads `0`
        // and a negative one its sign bit (Ruby's two's-complement
        // infinite width). `n[i, len]` declines here.
        "[]" => FoldOutcome::Value(Scalar::Int(if b < 0 { 0 } else { (a >> b.min(63)) & 1 })),
        _ => FoldOutcome::Declined,
    }
}

/// An Integer receiver as `numeric_blow_up_class` sees it — an `i64` pin or
/// the `Scalar::BigInt` decimal spelling — for the operand-only
/// `power_blow_up_class` / `shift_blow_up` checks.
#[derive(Clone, Copy)]
enum IntReceiver<'a> {
    Int(i64),
    Big(&'a str),
}

/// The reference's `numeric_blow_up_class` (`constant_folding.rb:1910`): the
/// operand precheck consulted at the head of the `**`/`pow`/`<<`/`>>` folds —
/// the `WidenedResult` the invoke answers when the fold would produce a
/// number past `INTEGER_FOLD_BIT_LIMIT`, read from the operands so the huge
/// value is never computed; `None` otherwise. With [`fold_result_gate`] (the
/// after-fact mirror) it is one of the two places `FoldOutcome::Widened` is
/// minted (rigor-rs#766).
fn numeric_blow_up_class(
    receiver: IntReceiver<'_>,
    method: &str,
    arg: &Scalar,
) -> Option<FoldOutcome> {
    let class = match method {
        "**" | "pow" => power_blow_up_class(receiver, arg)?,
        "<<" | ">>" if shift_blow_up(receiver, method, arg) => "Integer",
        _ => return None,
    };
    Some(FoldOutcome::Widened(class))
}

/// The reference's `power_blow_up_class` (:1921): `(power_base_bits(base) *
/// |exponent|) > INTEGER_FOLD_BIT_LIMIT` widens without computing —
/// `"Rational"` for a negative Integer exponent (an Integer base to a
/// negative power answers a Rational), `"Integer"` for a non-negative one.
/// `power_base_bits` is exact on an `i64` base (`|a|.bit_length - 1`, with
/// `0`'s count floored at 0 where the reference's is -1 — both never trip)
/// and a lower bound on a `BigInt` one, so a BigInt base widens only where
/// the reference's EXACT bound already trips (rigor-rs#694).
fn power_blow_up_class(base: IntReceiver<'_>, exponent: &Scalar) -> Option<&'static str> {
    let (exp_negative, exp_abs) = match exponent {
        Scalar::Int(v) => (*v < 0, u128::from(v.unsigned_abs())),
        // A Bignum exponent is knowable by sign alone — `bits >= 1` times an
        // unbounded count trips the limit on every non-unit base.
        Scalar::BigInt(d) => (d.starts_with('-'), u128::MAX),
        // A non-Integer exponent (`safe?` declines it before the gate).
        _ => return None,
    };
    let bits = match base {
        IntReceiver::Int(0) => 0,
        IntReceiver::Int(a) => u128::from(63 - a.unsigned_abs().leading_zeros()),
        IntReceiver::Big(d) => bigint_base_bits_lb(d),
    };
    if bits.saturating_mul(exp_abs) > u128::from(INTEGER_FOLD_BIT_LIMIT) {
        Some(if exp_negative { "Rational" } else { "Integer" })
    } else {
        None
    }
}

/// The reference's `shift_blow_up?` (:1952): `receiver.bit_length + shift >
/// INTEGER_FOLD_BIT_LIMIT`, `shift` signed by direction (`>>` subtracts).
/// Exact on an `i64` receiver; a `BigInt` receiver's lower bound trips only
/// where the exact bound already does — an unproven check still computes,
/// and the spelled result exits through [`big_scalar`]'s gate. A zero
/// receiver never trips, like the reference's `!receiver.zero?` guard; a
/// `BigInt` is never 0.
fn shift_blow_up(receiver: IntReceiver<'_>, method: &str, arg: &Scalar) -> bool {
    let shift = match arg {
        Scalar::Int(v) => i128::from(*v),
        // A Bignum count is knowable by sign alone at that size.
        Scalar::BigInt(d) => d
            .parse::<i128>()
            .unwrap_or(if d.starts_with('-') { i128::MIN } else { i128::MAX }),
        _ => return false,
    };
    let shift = if method == "<<" { shift } else { shift.saturating_neg() };
    let bits = match receiver {
        IntReceiver::Int(0) => return false,
        IntReceiver::Int(a) => i128::from(ruby_bit_length(a)),
        IntReceiver::Big(d) => i128::try_from(bigint_base_bits_lb(d)).unwrap_or(i128::MAX),
    };
    bits.saturating_add(shift) > i128::from(INTEGER_FOLD_BIT_LIMIT)
}

/// A lower bound on `floor(log2|v|)` — `power_base_bits` / `bit_length` on a
/// `Scalar::BigInt` — read off the digit count (`v >= 10^(digits-1)`), so
/// `numeric_blow_up_class` trips only where the reference's exact bound does.
fn bigint_base_bits_lb(d: &str) -> u128 {
    let digits = d.trim_start_matches('-').len().saturating_sub(1);
    (digits as f64 * std::f64::consts::LOG2_10) as u128
}

/// The compute side of `**`/`pow` on an `i64` base, reached once
/// `numeric_blow_up_class` passed the operands.
///
/// An Integer base to a negative power is a Rational — unmodeled — but
/// `fold_result`'s `oversized_number?` gates its DENOMINATOR
/// ([`rational_oversized`]): `3 ** -10000` is `for Rational` (the `1 *
/// 10000` estimate passes where `3**10000` is ~15850 bits). An
/// `i64`-overflowing positive exponent promotes to the `Scalar::BigInt`
/// decimal spelling (`10 ** 30` -> the 31-digit literal, rigor-rs#493 item
/// 7) through [`big_scalar`] — the after-fact gate widens what the estimate
/// under-counted (`2 ** 13606` estimates 13606 where the result is 13607
/// bits). `u32::try_from` guards `checked_pow`'s exponent: `b as u32` once
/// truncated `2 ** 4294967296` into `checked_pow(0)` -> `1` (PR #693
/// review); past `u32`, and on a Bignum exponent, only a unit base answers —
/// every other base already widened at the precheck. `pow` folds one-arg
/// identically (`2.pow(5)` -> `32`).
fn int_pow_result(a: i64, b: &Scalar) -> FoldOutcome {
    match b {
        Scalar::Int(b) => {
            if *b < 0 {
                // `0 ** -b` raises (declined on the reference); a `(±1) ** -b`
                // unit Rational declines like any Rational value here.
                if a == 0 {
                    return FoldOutcome::Declined;
                }
                return rational_oversized(decimal_pow(a, b.unsigned_abs()));
            }
            match u32::try_from(*b) {
                Ok(b32) => match a.checked_pow(b32) {
                    Some(v) => FoldOutcome::Value(Scalar::Int(v)),
                    // Only reachable for |a| >= 2 (`checked_pow` covers
                    // 0/±1), and the estimate already passed — the exact
                    // spelling still exits through `big_scalar`'s gate.
                    None => big_scalar(decimal_pow(a, u64::from(b32))),
                },
                // `b >= 2**32`: a unit base is the only answer left.
                Err(_) => match a {
                    0 => FoldOutcome::Value(Scalar::Int(0)),
                    1 => FoldOutcome::Value(Scalar::Int(1)),
                    -1 => FoldOutcome::Value(Scalar::Int(if b % 2 == 0 { 1 } else { -1 })),
                    _ => unreachable!("a non-unit base widened at numeric_blow_up_class"),
                },
            }
        }
        // A Bignum exponent: `0 ** -b` raises (declined on the reference),
        // `(±1) ** b` is its sign-parity unit; every other base widened at
        // the precheck.
        Scalar::BigInt(d) => match a {
            0 if d.starts_with('-') => FoldOutcome::Declined,
            0 => FoldOutcome::Value(Scalar::Int(0)),
            1 => FoldOutcome::Value(Scalar::Int(1)),
            -1 => FoldOutcome::Value(Scalar::Int(
                if d.as_bytes().last().is_some_and(|c| c % 2 == 0) { 1 } else { -1 },
            )),
            _ => unreachable!("a non-unit base widened at numeric_blow_up_class"),
        },
        // A Float exponent: `rb_int_pow` coerces to doubles (`2 ** 0.5` →
        // `1.4142135623730951`); a NaN — a Ruby Complex — declines like the
        // reference's `foldable_constant_value?` (rigor-rs#769).
        Scalar::Float(f) => float_pow_result(a as f64, *f),
        _ => FoldOutcome::Declined,
    }
}

/// `a ** e` through doubles — `rb_int_pow`/`rb_float_pow`'s coerce fallback
/// (`pow(a, b)` in C doubles). A NaN result is a Ruby Complex (`(-2) ** 0.5`),
/// which the reference's `foldable_constant_value?` declines; `±Infinity`,
/// `±0.0` and every finite value are foldable `Constant`s.
fn float_pow_result(a: f64, e: f64) -> FoldOutcome {
    let r = a.powf(e);
    if r.is_nan() {
        FoldOutcome::Declined
    } else {
        FoldOutcome::Value(Scalar::Float(r))
    }
}

/// A non-negative `pow(exp, mod)` exponent at any width — an `i64`'s bits or
/// a `BigInt`'s decimal-chunk bit stream. A negative exponent raises
/// `RangeError` under a modulus and is declined by the caller; a non-Integer
/// operand raises `TypeError` — both are `invoke_ternary`'s rescued declines.
enum PowExp {
    Small(u64),
    Big(Vec<u64>),
}

/// The exponent's bits, least-significant first — the stream
/// square-and-multiply consumes. `Small` reads the `u64`; `Big` halves the
/// little-endian base-1e9 chunks with [`decimal_shr_chunks`].
struct ExpBits(PowExp);

impl Iterator for ExpBits {
    type Item = bool;

    fn next(&mut self) -> Option<bool> {
        match &mut self.0 {
            PowExp::Small(e) => {
                if *e == 0 {
                    None
                } else {
                    let bit = *e & 1 == 1;
                    *e >>= 1;
                    Some(bit)
                }
            }
            PowExp::Big(chunks) => {
                if chunks.iter().all(|&c| c == 0) {
                    None
                } else {
                    let bit = chunks[0] & 1 == 1;
                    decimal_shr_chunks(chunks, 1);
                    Some(bit)
                }
            }
        }
    }
}

/// `Integer#pow(exp, mod)` — the reference's `invoke_ternary` fold
/// (`constant_folding.rb`: `pow` is a catalog `:leaf`). Modular
/// exponentiation, so the result has the divisor's sign and magnitude under
/// |mod| (`2.pow(5, -3)` → `-1`, `2.pow(20000, 97)` → `35`). Ruby raises on a
/// `0` modulus (`ZeroDivisionError`), a negative exponent (`RangeError`) and
/// a non-Integer operand (`TypeError`) — all three decline, matching the
/// reference's rescue-to-nil (rigor-rs#769).
fn int_pow_mod_result(base: IntReceiver<'_>, e: &Scalar, m: &Scalar) -> FoldOutcome {
    let exp = match e {
        Scalar::Int(v) if *v >= 0 => PowExp::Small(v.unsigned_abs()),
        Scalar::BigInt(d) if !d.starts_with('-') => PowExp::Big(decimal_chunks(d)),
        _ => return FoldOutcome::Declined,
    };
    match m {
        Scalar::Int(m) if *m != 0 => int_pow_mod_u128(base, exp, *m),
        Scalar::BigInt(d) => int_pow_mod_chunks(base, exp, d),
        _ => FoldOutcome::Declined,
    }
}

/// `a.pow(e, m)` with an `i64` modulus — the whole computation stays under
/// `u128` (`|m| <= 2**63`, so a product of residues is under `2**126`).
fn int_pow_mod_u128(base: IntReceiver<'_>, exp: PowExp, m: i64) -> FoldOutcome {
    let mm = u128::from((m as i128).unsigned_abs());
    let cur = match base {
        IntReceiver::Int(a) => i128::from(a).rem_euclid(mm as i128) as u128,
        IntReceiver::Big(d) => decimal_mod_u128(d, mm),
    };
    let mut r = 1 % mm;
    let mut bits = ExpBits(exp);
    let mut cur = cur;
    while let Some(bit) = bits.next() {
        if bit {
            r = r * cur % mm;
        }
        cur = cur * cur % mm;
    }
    // The result takes the divisor's sign (`ruby_mod` semantics): the
    // canonical residue `r` is the answer for `m > 0`, `r - |m|` for `m < 0`.
    let signed = if m > 0 || r == 0 {
        r as i128
    } else {
        r as i128 - mm as i128
    };
    // |signed| < |m| <= 2**63, so the cast cannot fail.
    FoldOutcome::Value(Scalar::Int(i64::try_from(signed).unwrap_or(i64::MAX)))
}

/// `|d| mod mm` by base-1e9 Horner reduction — the canonical (non-negative)
/// residue of a `Scalar::BigInt` receiver for [`int_pow_mod_u128`].
fn decimal_mod_u128(d: &str, m: u128) -> u128 {
    let chunks = decimal_chunks(d.trim_start_matches('-'));
    let mut r = 0u128;
    for &c in chunks.iter().rev() {
        r = (r * 1_000_000_000 + u128::from(c)) % m;
    }
    if d.starts_with('-') && r != 0 { m - r } else { r }
}

/// `a.pow(e, m)` with a `BigInt` modulus — chunk modular exponentiation. The
/// result is bounded by |m| and exits through [`big_scalar`]'s
/// [`INTEGER_FOLD_BIT_LIMIT`] gate like every spelled integer result
/// (rigor-rs#769). The work estimate (`exp_bits * m_words²`) is bounded — a
/// pathological modulus past it declines, the zero-FP-safe side.
fn int_pow_mod_chunks(base: IntReceiver<'_>, exp: PowExp, d: &str) -> FoldOutcome {
    // ~`log2(exp) * m_words` modmul word-ops — decline a computation the
    // reference's native bignum does invisibly but this port cannot afford.
    const WORK_LIMIT: u128 = 1 << 28;
    let exp_bits = match &exp {
        PowExp::Small(e) => 64 - e.leading_zeros() as usize,
        PowExp::Big(chunks) => chunks.len() * 30,
    };
    let m_mag = decimal_chunks(d.trim_start_matches('-'));
    if (exp_bits as u128).saturating_mul((m_mag.len() as u128).pow(2)) > WORK_LIMIT {
        return FoldOutcome::Declined;
    }
    let cur = match base {
        IntReceiver::Int(a) => {
            // |a| < 2**63 <= |m|: the canonical residue is `a` itself for
            // `a >= 0`, `|m| - |a|` for `a < 0`.
            let a_mag = decimal_chunks(&a.unsigned_abs().to_string());
            if a >= 0 {
                a_mag
            } else {
                chunks_sub(&m_mag, &a_mag)
            }
        }
        IntReceiver::Big(bd) => {
            let r = chunks_mod(&decimal_chunks(bd.trim_start_matches('-')), &m_mag);
            if bd.starts_with('-') && !chunks_is_zero(&r) {
                chunks_sub(&m_mag, &r)
            } else {
                r
            }
        }
    };
    let mut r = chunks_mod(&[1], &m_mag);
    let mut bits = ExpBits(exp);
    let mut cur = cur;
    while let Some(bit) = bits.next() {
        if bit {
            r = chunks_modmul(&r, &cur, &m_mag);
        }
        cur = chunks_modmul(&cur, &cur, &m_mag);
    }
    if d.starts_with('-') && !chunks_is_zero(&r) {
        big_scalar(format!("-{}", chunks_to_decimal(false, &chunks_sub(&m_mag, &r))))
    } else {
        big_scalar(chunks_to_decimal(false, &r))
    }
}

/// The reference's `INTEGER_FOLD_BIT_LIMIT` (`constant_folding.rb`, #2115):
/// `floor(STRING_FOLD_BYTE_LIMIT * log2(10))` — a folded Integer's maximum bit
/// length. Past it the reference widens the fold to `Nominal[Integer]` (never
/// the literal); this core declines instead, which lands on the same nominal
/// through the RBS signature.
const INTEGER_FOLD_BIT_LIMIT: u64 = 13606;

/// `2 ** INTEGER_FOLD_BIT_LIMIT` in decimal — the magnitude boundary the
/// reference's `oversized_number?` applies to a folded result.
static POW2_BIT_LIMIT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| decimal_shl(1, INTEGER_FOLD_BIT_LIMIT));

/// An integer result that spills `i64` becomes the `Scalar::BigInt` decimal
/// spelling — the same carrier a Bignum literal pins (rigor-rs#194) — matching
/// the reference's arbitrary-precision `Constant`.
fn int_scalar(v: i128) -> Scalar {
    match i64::try_from(v) {
        Ok(v) => Scalar::Int(v),
        Err(_) => Scalar::BigInt(v.to_string()),
    }
}

/// Ruby's `Integer#bit_length`: for a negative receiver it counts the bits of
/// `|v| - 1` (two's-complement magnitude), so `-8.bit_length == 3`.
fn ruby_bit_length(v: i64) -> u64 {
    let mag = if v < 0 { v.unsigned_abs() - 1 } else { v as u64 };
    64 - mag.leading_zeros() as u64
}

/// `a << shift` (`a >> -shift`) for a signed count at arbitrary size.
///
/// A negative count is an arithmetic floor shift saturating at `0` / `-1`.
/// A positive count is `a * 2**shift`: `numeric_blow_up_class` (the
/// reference's `shift_blow_up?`) already widened a count past
/// [`INTEGER_FOLD_BIT_LIMIT`], so the compute here is bounded — the spelled
/// result still exits through [`big_scalar`]'s gate.
fn shift_result(a: i64, shift: i128) -> FoldOutcome {
    if shift < 0 {
        let r = u64::try_from(-shift).unwrap_or(u64::MAX);
        return FoldOutcome::Value(Scalar::Int(if r >= 64 {
            if a < 0 {
                -1
            } else {
                0
            }
        } else {
            a >> r
        }));
    }
    if a == 0 {
        return FoldOutcome::Value(Scalar::Int(0));
    }
    let bits = u128::from(ruby_bit_length(a));
    // `bits + shift <= INTEGER_FOLD_BIT_LIMIT` holds after the precheck
    // (exact for an i64 receiver); the clamp is defensive — a count at the
    // boundary still spells a result `big_scalar` gates to the same answer.
    let shift = (shift as u64).min(INTEGER_FOLD_BIT_LIMIT + 1);
    if bits + u128::from(shift) <= 127 {
        FoldOutcome::Value(int_scalar(i128::from(a) << shift))
    } else {
        FoldOutcome::Value(Scalar::BigInt(decimal_shl(a, shift)))
    }
}

/// `a * 2**shift` spelled in decimal — digit-vector doubling in base-1e9
/// chunks, sign carried separately. Callers bound `shift` by
/// [`INTEGER_FOLD_BIT_LIMIT`], so this is at most ~13.6k doublings of a
/// ~450-chunk vector (single-digit ms).
fn decimal_shl(a: i64, shift: u64) -> String {
    let negative = a < 0;
    let mut v = a.unsigned_abs();
    let mut chunks: Vec<u64> = Vec::new();
    while v > 0 {
        chunks.push(v % 1_000_000_000);
        v /= 1_000_000_000;
    }
    decimal_shl_chunks(&mut chunks, shift);
    chunks_to_decimal(negative, &chunks)
}

/// `v * 2**shift` in place on little-endian base-1e9 chunks (30 bits at a
/// time — a chunk product stays under `u64`).
fn decimal_shl_chunks(chunks: &mut Vec<u64>, mut shift: u64) {
    const CHUNK: u64 = 1_000_000_000;
    if chunks.is_empty() {
        return;
    }
    while shift > 0 {
        let step = shift.min(30);
        let mul = 1u64 << step;
        let mut carry = 0u64;
        for c in chunks.iter_mut() {
            let cur = *c * mul + carry;
            *c = cur % CHUNK;
            carry = cur / CHUNK;
        }
        while carry > 0 {
            chunks.push(carry % CHUNK);
            carry /= CHUNK;
        }
        shift -= step;
    }
}

/// `v / 2**shift` (floor) in place on little-endian base-1e9 chunks.
/// Early-exits once the vector is exhausted, so a huge count saturates to `0`.
fn decimal_shr_chunks(chunks: &mut Vec<u64>, mut shift: u64) {
    const CHUNK: u64 = 1_000_000_000;
    while shift > 0 && chunks.iter().any(|&c| c != 0) {
        let step = shift.min(30);
        let div = 1u64 << step;
        let mut rem = 0u64;
        for c in chunks.iter_mut().rev() {
            let cur = rem * CHUNK + *c;
            *c = cur / div;
            rem = cur % div;
        }
        while chunks.last() == Some(&0) {
            chunks.pop();
        }
        shift -= step;
    }
}

/// `v - 1` in place on little-endian base-1e9 chunks (`v >= 1`).
fn decimal_dec_chunks(chunks: &mut Vec<u64>) {
    const CHUNK: u64 = 1_000_000_000;
    let mut i = 0;
    while i < chunks.len() {
        if chunks[i] > 0 {
            chunks[i] -= 1;
            break;
        }
        chunks[i] = CHUNK - 1;
        i += 1;
    }
    while chunks.last() == Some(&0) {
        chunks.pop();
    }
}

/// Parse a decimal magnitude string into little-endian base-1e9 chunks.
fn decimal_chunks(mag: &str) -> Vec<u64> {
    let mut chunks = Vec::new();
    let mut i = mag.len();
    while i > 0 {
        let start = i.saturating_sub(9);
        chunks.push(mag[start..i].parse().unwrap_or(0));
        i = start;
    }
    chunks
}

/// Render little-endian base-1e9 chunks as a canonical decimal spelling.
fn chunks_to_decimal(negative: bool, chunks: &[u64]) -> String {
    if chunks.is_empty() {
        return "0".to_string();
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    let mut it = chunks.iter().rev();
    if let Some(top) = it.next() {
        out.push_str(&top.to_string());
    }
    for c in it {
        out.push_str(&format!("{c:09}"));
    }
    out
}

/// Order two canonical decimal spellings (an optional `-` then digits — the
/// only spellings `Scalar::BigInt` and `i64::to_string` ever produce).
/// Leading zeros and `-0` are normalised defensively.
fn decimal_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn norm(s: &str) -> (bool, &str) {
        let (neg, digits) = (s.starts_with('-'), s.trim_start_matches('-'));
        let digits = digits.trim_start_matches('0');
        (neg && !digits.is_empty(), if digits.is_empty() { "0" } else { digits })
    }
    let (a_neg, a) = norm(a);
    let (b_neg, b) = norm(b);
    let mag = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
    match (a_neg, b_neg) {
        (false, false) => mag,
        (true, true) => mag.reverse(),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
    }
}

/// Whether a folded result's decimal spelling is past
/// [`INTEGER_FOLD_BIT_LIMIT`] — the port's mirror of the reference's
/// `oversized_number?` result gate. Ruby's `bit_length` on a negative counts
/// `|v| - 1`, so a negative magnitude overflows the limit one step later.
fn decimal_oversized(spelling: &str) -> bool {
    let mag = spelling.trim_start_matches('-');
    if spelling.starts_with('-') {
        decimal_cmp(mag, &POW2_BIT_LIMIT) == std::cmp::Ordering::Greater
    } else {
        decimal_cmp(mag, &POW2_BIT_LIMIT) != std::cmp::Ordering::Less
    }
}

/// True when every base-1e9 chunk is zero.
fn chunks_is_zero(chunks: &[u64]) -> bool {
    chunks.iter().all(|&c| c == 0)
}

/// Order two little-endian base-1e9 chunk vectors by magnitude (leading
/// zero words ignored).
fn chunks_cmp(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    fn len(a: &[u64]) -> usize {
        a.iter().rposition(|&c| c != 0).map_or(0, |p| p + 1)
    }
    let (la, lb) = (len(a), len(b));
    la.cmp(&lb).then_with(|| {
        (0..la)
            .rev()
            .map(|i| a[i].cmp(&b[i]))
            .find(|o| *o != std::cmp::Ordering::Equal)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// `a - b` on little-endian base-1e9 chunks — caller guarantees `a >= b`.
fn chunks_sub(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = a.to_vec();
    out.resize(a.len().max(b.len()), 0);
    chunks_sub_shift(&mut out, b, 0);
    out
}

/// `u -= p * CHUNK**shift` in place — caller guarantees `p << shift <= u`.
fn chunks_sub_shift(u: &mut Vec<u64>, p: &[u64], shift: usize) {
    const CHUNK: u64 = 1_000_000_000;
    let mut borrow = 0u64;
    for i in shift..u.len() {
        // `sub` can reach CHUNK (`p`-word CHUNK-1 plus a borrow) — `u[i]` is
        // always below it, so the borrow arm is the only reachable one then.
        let sub = p.get(i - shift).copied().unwrap_or(0) + borrow;
        if u[i] >= sub {
            u[i] -= sub;
            borrow = 0;
        } else {
            u[i] = u[i] + CHUNK - sub;
            borrow = 1;
        }
    }
    while u.last() == Some(&0) {
        u.pop();
    }
}

/// `a * f` on little-endian base-1e9 chunks for a `u128` factor
/// (normalisation factors, quotient digits — always below `2 * CHUNK`).
fn chunks_mul_small(a: &[u64], f: u128) -> Vec<u64> {
    const CHUNK: u128 = 1_000_000_000;
    let mut out = Vec::with_capacity(a.len() + 2);
    let mut carry = 0u128;
    for &c in a {
        let cur = u128::from(c) * f + carry;
        out.push((cur % CHUNK) as u64);
        carry = cur / CHUNK;
    }
    while carry > 0 {
        out.push((carry % CHUNK) as u64);
        carry /= CHUNK;
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// `a / f` (exact) on little-endian base-1e9 chunks for a `u128` divisor —
/// the denormalisation step of [`chunks_mod`].
fn chunks_div_small(a: &[u64], f: u128) -> Vec<u64> {
    const CHUNK: u128 = 1_000_000_000;
    let mut out = vec![0u64; a.len()];
    let mut r = 0u128;
    for i in (0..a.len()).rev() {
        let cur = r * CHUNK + u128::from(a[i]);
        out[i] = (cur / f) as u64;
        r = cur % f;
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// `a * b` on little-endian base-1e9 chunks — schoolbook, `u128`
/// accumulation.
fn chunks_mul(a: &[u64], b: &[u64]) -> Vec<u64> {
    const CHUNK: u128 = 1_000_000_000;
    let mut out = vec![0u64; a.len() + b.len() + 1];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u128;
        for (j, &y) in b.iter().enumerate() {
            let cur = u128::from(out[i + j]) + u128::from(x) * u128::from(y) + carry;
            out[i + j] = (cur % CHUNK) as u64;
            carry = cur / CHUNK;
        }
        let mut k = i + b.len();
        while carry > 0 {
            let cur = u128::from(out[k]) + carry;
            out[k] = (cur % CHUNK) as u64;
            carry = cur / CHUNK;
            k += 1;
        }
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// `a mod m` on little-endian base-1e9 chunks (`m` non-zero) — the division
/// half of [`chunks_modmul`]. Each quotient digit is estimated from the
/// leading words after normalising the divisor's top word past `CHUNK/2`
/// (Knuth's bound: the estimate overshoots by at most 2, so the fixup loop
/// is bounded).
fn chunks_mod(a: &[u64], m: &[u64]) -> Vec<u64> {
    const CHUNK: u128 = 1_000_000_000;
    if chunks_cmp(a, m) == std::cmp::Ordering::Less {
        return a.to_vec();
    }
    if m.len() == 1 {
        let d = u128::from(m[0]);
        let mut r = 0u128;
        for &c in a.iter().rev() {
            r = (r * CHUNK + u128::from(c)) % d;
        }
        return if r == 0 { vec![] } else { vec![r as u64] };
    }
    // Normalise so `v`'s top word is >= CHUNK/2.
    let f = CHUNK / (u128::from(*m.last().unwrap_or(&1)) + 1);
    let v = chunks_mul_small(m, f);
    let mut u = chunks_mul_small(a, f);
    let n = v.len();
    let qlen = u.len() + 1 - n;
    for j in (0..qlen).rev() {
        // `u` shrinks below `j + n` as the high quotient digits resolve —
        // both top words read 0 past its end.
        let num = u128::from(u.get(j + n).copied().unwrap_or(0)) * CHUNK
            + u128::from(u.get(j + n - 1).copied().unwrap_or(0));
        let mut qd = (num / u128::from(v[n - 1])).min(CHUNK - 1);
        let mut prod = chunks_mul_small(&v, qd);
        // `prod << j <= u` iff `prod <= u >> j` (the `j` low words of `u`
        // only shrink the difference further).
        while chunks_cmp(&prod, &u[j.min(u.len())..]) == std::cmp::Ordering::Greater {
            qd -= 1;
            prod = chunks_mul_small(&v, qd);
        }
        if qd > 0 {
            chunks_sub_shift(&mut u, &prod, j);
        }
    }
    // Denormalise the remainder.
    let r = chunks_div_small(&u, f);
    debug_assert!(chunks_cmp(&r, m) == std::cmp::Ordering::Less);
    r
}

/// `x * y mod m` on little-endian base-1e9 chunks.
fn chunks_modmul(x: &[u64], y: &[u64], m: &[u64]) -> Vec<u64> {
    chunks_mod(&chunks_mul(x, y), m)
}

/// `|d| ** e` spelled in decimal — the `Scalar::BigInt` counterpart of
/// [`decimal_pow`]. `e` is bounded by the caller's `power_blow_up` estimate
/// (a lower bound, so an oversized result is still possible): the spelled
/// result exits through [`big_scalar`]'s gate, and the loop stops once the
/// spelling provably passes [`INTEGER_FOLD_BIT_LIMIT`] — 457 base-1e9 words
/// hold at least `9*456` digits ≈ 13.6k bits — answering the boundary
/// spelling the gate widens (rigor-rs#769).
fn chunks_pow(d: &str, e: u64) -> String {
    const WORD_CAP: usize = 457;
    let base = decimal_chunks(d);
    let mut acc: Vec<u64> = vec![1];
    for _ in 0..e {
        acc = chunks_mul(&acc, &base);
        if acc.len() > WORD_CAP {
            // A spelling strictly past the limit — oversized under the
            // negative-magnitude gate too (which compares `>`, not `>=`).
            return decimal_shl(1, INTEGER_FOLD_BIT_LIMIT + 1);
        }
    }
    chunks_to_decimal(false, &acc)
}

/// Ordering of two integer scalars at any width — `i64` pairs compare
/// directly, `BigInt`s through their decimal spellings.
fn int_scalar_cmp(a: &Scalar, b: &Scalar) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Scalar::Int(x), Scalar::Int(y)) => Some(x.cmp(y)),
        (Scalar::Int(x), Scalar::BigInt(d)) => Some(decimal_cmp(&x.to_string(), d)),
        (Scalar::BigInt(d), Scalar::Int(y)) => Some(decimal_cmp(d, &y.to_string())),
        (Scalar::BigInt(x), Scalar::BigInt(y)) => Some(decimal_cmp(x, y)),
        _ => None,
    }
}

/// The boolean a comparison method name encodes over an [`Ordering`].
fn cmp_bool(method: &str, o: std::cmp::Ordering) -> bool {
    match method {
        "==" => o == std::cmp::Ordering::Equal,
        "<" => o == std::cmp::Ordering::Less,
        "<=" => o != std::cmp::Ordering::Greater,
        ">" => o == std::cmp::Ordering::Greater,
        ">=" => o != std::cmp::Ordering::Less,
        _ => unreachable!("cmp_bool on {method}"),
    }
}

/// `d ± 1` spelled in decimal on a canonical BigInt spelling — the caller
/// canonicalises it with [`big_scalar`] (`(-9223372036854775808).succ` ->
/// the `Scalar::Int` `-9223372036854775807`).
fn decimal_step(d: &str, up: bool) -> String {
    let negative = d.starts_with('-');
    let mut mag: Vec<u8> = d.trim_start_matches('-').bytes().collect();
    // `up` grows the magnitude on a positive, shrinks it on a negative.
    if negative == up {
        let mut i = mag.len();
        while i > 0 {
            i -= 1;
            if mag[i] > b'0' {
                mag[i] -= 1;
                break;
            }
            mag[i] = b'9';
        }
    } else {
        let mut i = mag.len();
        loop {
            if i == 0 {
                mag.insert(0, b'1');
                break;
            }
            i -= 1;
            if mag[i] < b'9' {
                mag[i] += 1;
                break;
            }
            mag[i] = b'0';
        }
    }
    let mag = String::from_utf8(mag).unwrap_or_default();
    let mag = mag.trim_start_matches('0');
    if negative && !mag.is_empty() && mag != "0" {
        format!("-{mag}")
    } else if mag.is_empty() {
        "0".to_string()
    } else {
        mag.to_string()
    }
}

/// `fold_result`'s `oversized_number?` gate (`constant_folding.rb:1695`)
/// applied to a decimal-spelled integer result: `Scalar::Int` when it fits,
/// `FoldOutcome::Widened` past `INTEGER_FOLD_BIT_LIMIT` (rigor-rs#694),
/// `Scalar::BigInt` otherwise. With [`numeric_blow_up_class`] (the operand
/// precheck) it is one of the two places a `Widened` is minted (rigor-rs#766).
fn fold_result_gate(spelling: &str, widened_class: &'static str) -> FoldOutcome {
    match spelling.parse::<i64>() {
        Ok(v) => FoldOutcome::Value(Scalar::Int(v)),
        Err(_) if decimal_oversized(spelling) => FoldOutcome::Widened(widened_class),
        Err(_) => FoldOutcome::Value(Scalar::BigInt(spelling.to_string())),
    }
}

/// Canonicalise a folded integer result's decimal spelling through
/// [`fold_result_gate`]. Every integer-producing BigInt arm exits through
/// here so a result past [`INTEGER_FOLD_BIT_LIMIT`] never mints a `Constant`
/// the reference does not emit — `x.succ` on a 5001-digit literal is
/// `Integer` on the oracle, not a one-larger literal (PR #693 review).
fn big_scalar(spelling: String) -> FoldOutcome {
    fold_result_gate(&spelling, "Integer")
}

/// `oversized_number?`'s Rational branch (`constant_folding.rb:1859`): a
/// folded `a ** -b` result's DENOMINATOR gates like an Integer result —
/// `3 ** -10000` is `for Rational` (the `1 * 10000` estimate passed where
/// `3**10000` is ~15850 bits). Ruby's Rational normalises the sign into the
/// numerator, so the denominator always gates as a positive magnitude. A
/// fitting denominator declines like the unmodeled Rational value does.
fn rational_oversized(denominator: String) -> FoldOutcome {
    match fold_result_gate(denominator.trim_start_matches('-'), "Rational") {
        FoldOutcome::Value(_) => FoldOutcome::Declined,
        outcome => outcome,
    }
}

/// Folds on a `Scalar::BigInt` receiver (rigor-rs#682). Only the members
/// whose answer reads straight off the decimal spelling — predicates, sign
/// ops, `to_s`, `succ`/`pred`, integer comparisons, and `<<`/`>>` — are
/// folded; other binary arithmetic (`+`, `*`, `&`, …) needs the operand
/// width the core does not model and declines to the RBS answer, the
/// zero-FP-safe side. `**`/`pow` still declines to fold — but the WIDENED
/// answer is decidable at the `numeric_blow_up_class` precheck
/// (rigor-rs#694, [`bigint_pow_result`]).
fn fold_bigint(d: &str, method: &str, args: &[Scalar]) -> FoldOutcome {
    // `numeric_blow_up_class` at the head — the same operand precheck
    // `fold_int` consults.
    if let ("**" | "pow" | "<<" | ">>", [b]) = (method, args) {
        if let Some(outcome) = numeric_blow_up_class(IntReceiver::Big(d), method, b) {
            return outcome;
        }
    }
    let negative = d.starts_with('-');
    match (method, args) {
        ("abs", []) => big_scalar(d.trim_start_matches('-').to_string()),
        ("-@", []) => big_scalar(if negative { d[1..].to_string() } else { format!("-{d}") }),
        ("+@", []) => big_scalar(d.to_string()),
        ("succ", []) => big_scalar(decimal_step(d, true)),
        ("pred", []) => big_scalar(decimal_step(d, false)),
        ("even?", []) => FoldOutcome::Value(Scalar::Bool(d.as_bytes().last().is_some_and(|c| c % 2 == 0))),
        ("odd?", []) => FoldOutcome::Value(Scalar::Bool(d.as_bytes().last().is_some_and(|c| c % 2 == 1))),
        ("zero?", []) => FoldOutcome::Value(Scalar::Bool(false)), // a BigInt is never 0.
        ("positive?", []) => FoldOutcome::Value(Scalar::Bool(!negative)),
        ("negative?", []) => FoldOutcome::Value(Scalar::Bool(negative)),
        ("to_s", []) => FoldOutcome::Value(Scalar::Str(d.to_string())),
        ("==" | "<" | "<=" | ">" | ">=", [b]) => {
            match int_scalar_cmp(&Scalar::BigInt(d.to_string()), b) {
                Some(o) => FoldOutcome::Value(Scalar::Bool(cmp_bool(method, o))),
                None => FoldOutcome::Declined,
            }
        }
        ("<<" | ">>", [b]) => {
            // A shift count that does not fit `i64` is only knowable by
            // sign — a Bignum count's parse overflows `i128` too.
            let shift = match b {
                Scalar::Int(v) => i128::from(*v),
                Scalar::BigInt(d) => d
                    .parse::<i128>()
                    .unwrap_or(if d.starts_with('-') { i128::MIN } else { i128::MAX }),
                _ => return FoldOutcome::Declined,
            };
            bigint_shift(d, if method == "<<" { shift } else { shift.saturating_neg() })
        }
        ("**" | "pow", [b]) => bigint_pow_result(d, b),
        // `Integer#pow(exp, mod)` on a BigInt receiver — the reference folds
        // it through the same `invoke_ternary` catalog leaf (rigor-rs#769).
        ("pow", [e, m]) => int_pow_mod_result(IntReceiver::Big(d), e, m),
        _ => FoldOutcome::Declined,
    }
}

/// The compute side of `**`/`pow` on a `Scalar::BigInt` base, reached once
/// `numeric_blow_up_class` passed: `v ** 0 == 1` at any width, `v ** 1 == v`,
/// and any result small enough to survive `fold_result`'s
/// `oversized_number?` gate is spelled by [`chunks_pow`] and exits through
/// [`big_scalar`] — `(10**20) ** 2` answers the `10**40` literal like the
/// reference (rigor-rs#769). A negative exponent's Rational DENOMINATOR is
/// spelled the same way for [`rational_oversized`]'s gate (`(10**20) ** -1`
/// declines like the fitting-denominator `i64` rows do). A Float exponent
/// coerces the receiver through `rb_big2dbl` into [`float_pow_result`]. A
/// Bignum exponent already widened at the precheck (|v| >= 2**63 is never a
/// unit base), so it is unreachable here.
fn bigint_pow_result(d: &str, arg: &Scalar) -> FoldOutcome {
    match arg {
        Scalar::Int(e) if *e < 0 => {
            rational_oversized(chunks_pow(d.trim_start_matches('-'), e.unsigned_abs()))
        }
        Scalar::Int(e) => {
            let spelled = chunks_pow(d.trim_start_matches('-'), *e as u64);
            if d.starts_with('-') && *e % 2 == 1 {
                big_scalar(format!("-{spelled}"))
            } else {
                big_scalar(spelled)
            }
        }
        Scalar::Float(f) => match d.parse::<f64>() {
            Ok(v) => float_pow_result(v, *f),
            Err(_) => FoldOutcome::Declined,
        },
        _ => FoldOutcome::Declined,
    }
}

/// `v << shift` (`v >> -shift`) on a `Scalar::BigInt` receiver — the decimal
/// counterpart of [`shift_result`]. A left shift is computed in chunks —
/// `numeric_blow_up_class` already widened a count past
/// [`INTEGER_FOLD_BIT_LIMIT`] (the reference's `WidenedResult`,
/// rigor-rs#694); a right shift floors (`-v >> r` is `-(floor((|v|-1) /
/// 2**r)) - 1`) and saturates at `0` / `-1`.
fn bigint_shift(d: &str, shift: i128) -> FoldOutcome {
    let negative = d.starts_with('-');
    if shift < 0 {
        let r = shift.unsigned_abs().min(u128::from(u64::MAX)) as u64;
        let mut chunks = decimal_chunks(d.trim_start_matches('-'));
        if negative {
            decimal_dec_chunks(&mut chunks);
        }
        decimal_shr_chunks(&mut chunks, r);
        let mag = chunks_to_decimal(false, &chunks);
        return if !negative {
            big_scalar(mag)
        } else if mag == "0" {
            FoldOutcome::Value(Scalar::Int(-1))
        } else {
            big_scalar(decimal_step(&format!("-{mag}"), false))
        };
    }
    // The precheck bounded the count; the clamp is defensive — a spelled
    // result exits through `big_scalar`'s gate either way.
    let s = u64::try_from(shift).unwrap_or(u64::MAX).min(INTEGER_FOLD_BIT_LIMIT + 1);
    let mut chunks = decimal_chunks(d.trim_start_matches('-'));
    decimal_shl_chunks(&mut chunks, s);
    big_scalar(chunks_to_decimal(negative, &chunks))
}

/// Ruby's `Integer#%`: the result has the sign of the divisor.
/// `wrapping_rem` because `i64::MIN % -1` panics where Ruby answers `0`
/// (rigor-rs#682).
fn ruby_mod(a: i64, b: i64) -> i64 {
    let r = a.wrapping_rem(b);
    if r != 0 && (r < 0) != (b < 0) {
        r + b
    } else {
        r
    }
}

// --- Float ------------------------------------------------------------------

fn fold_float(a: f64, method: &str, args: &[Scalar]) -> Option<Scalar> {
    match (method, args) {
        ("+@", []) => return Some(Scalar::Float(a)),
        ("-@", []) => return Some(Scalar::Float(-a)),
        ("zero?", []) => return Some(Scalar::Bool(a == 0.0)),
        ("positive?", []) => return Some(Scalar::Bool(a > 0.0)),
        ("negative?", []) => return Some(Scalar::Bool(a < 0.0)),
        _ => {}
    }
    if method == "abs" && args.is_empty() {
        return Some(Scalar::Float(a.abs()));
    }
    if method == "<=>" {
        return float_cmp(a, args);
    }
    // `Float#**` (`rb_float_pow`, NUMERIC_BINARY's `:**`): Integer and Float
    // exponents fold in doubles; a Bignum exponent converts through
    // `rb_big2dbl` (`±Infinity` past f64 — the same rounding Rust's
    // `str::parse::<f64>` performs, so `2.0 ** (10**30)` answers `Infinity`
    // like the oracle). `Float#pow` is deliberately NOT folded: it is not in
    // NUMERIC_BINARY and the catalog gates it off a Float receiver — the
    // oracle answers `undefined method 'pow'` (rigor-rs#769). A NaN — a
    // Ruby Complex — declines like the reference's
    // `foldable_constant_value?`.
    if method == "**" {
        let e = match args {
            [Scalar::Float(e)] => Some(*e),
            [Scalar::Int(e)] => Some(*e as f64),
            [Scalar::BigInt(d)] => d.parse::<f64>().ok(),
            _ => None,
        };
        return e.and_then(|e| {
            let r = a.powf(e);
            (!r.is_nan()).then_some(Scalar::Float(r))
        });
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
        // `+s`/`-s`/`to_s`/`to_str` keep the string carrier — the literal
        // stays pinned for the message (`for "ab"`, rigor-rs#462).
        ("+@" | "-@" | "to_s" | "to_str", []) => {
            return Some(Scalar::Str(a.to_string()))
        }
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

/// Issues #164/#178 — the literal calls whose tier-1 fold OWNS the answer:
/// a call that pins every argument but does not fold must not fall through
/// to the tier-3 flat slot. Each nilable String lookup (`String?` for `[]` /
/// `slice` / `byteslice`, `Integer?` for `index` / `getbyte` / `rindex` /
/// `byteindex` / `byterindex`) can answer `nil` — a substring miss, an
/// out-of-range index or Range — and the reference, which folds these by
/// running the method, is silent downstream where the flat slot's bare
/// `String` / `Integer` fires `call.undefined-method` on the chained call.
/// The same for a fold the port cannot reproduce (a multibyte receiver, a
/// Float index) or where Ruby raises (a wrong arity) and the reference
/// declines to the `String | nil` / `Integer | nil` join. The caller answers
/// `Dynamic` instead: a coverage loss, never a false positive.
/// rigor-rs#499 — an unfolded lookup answers `Dynamic`, never the flat
/// nominal: zero-arg `s.index` landed on bare `Integer`, collapsing under
/// `narrow_falsey` in `s.index &&= 1` to `1` where the oracle keeps
/// `Integer | nil` and stays silent downstream.
pub fn declines_unfolded(recv: &Scalar, method: &str) -> bool {
    is_str_lookup(recv, method) || matches!((recv, method), (Scalar::Float(_), "<=>"))
}

/// The lookups #178 added to `declines_unfolded`. A compound index/attr
/// write's internal `current` read (`s[0] ||= nil`'s `s[0]`) keeps the
/// flat nominal instead — the stored-slot record joins it with the rhs
/// (`String | nil`), which the `[]` interception then hands to later
/// reads. Declining it would erase the record to `Dynamic`.
pub fn declines_str_index_family(recv: &Scalar, method: &str) -> bool {
    matches!(recv, Scalar::Str(_))
        && matches!(method, "[]" | "slice" | "byteslice" | "index")
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

/// `a ** b` as a decimal-spelled [`Scalar::BigInt`] operand — digit-vector
/// long multiplication, sign carried separately. Used only for results past
/// `i64` (the caller tries `checked_pow` first) and negative-exponent
/// denominators. `a` is small (the foldable receivers are i64 literals) and
/// `bits(|a|) * b <= INTEGER_FOLD_BIT_LIMIT`, so the loop is bounded by
/// ~19*13606 digits worst-case.
fn decimal_pow(a: i64, b: u64) -> String {
    if a == 0 {
        // `0 ** b` never overflows; unreachable in practice.
        return "0".to_string();
    }
    let negative = a < 0 && b % 2 == 1;
    let base: u128 = u128::from(a.unsigned_abs());
    // digits little-endian (digits[0] = ones place). The multiply runs in
    // `u128`: `9 * i64::MAX` alone exceeds `u64` (the wrap the PR #693 review
    // caught on `9223372036854775807 ** 2`).
    let mut digits: Vec<u8> = vec![1];
    for _ in 0..b {
        let mut carry: u128 = 0;
        for d in digits.iter_mut() {
            let cur = u128::from(*d) * base + carry;
            *d = (cur % 10) as u8;
            carry = cur / 10;
        }
        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
        if digits.len() > 80_000 {
            // Paranoia bound — unreachable inside the caller's `bits * b`
            // bound — but a result this big certainly widens like a measured
            // one: spell the bit-limit boundary for the `fold_result_gate`
            // it exits through.
            return POW2_BIT_LIMIT.clone();
        }
    }
    let mut out = String::with_capacity(digits.len() + 1);
    if negative {
        out.push('-');
    }
    for d in digits.iter().rev() {
        out.push((b'0' + d) as char);
    }
    out
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
        // Divide by zero and negative exponent decline.
        assert_eq!(fold(&Scalar::Int(1), "/", &[Scalar::Int(0)]), None);
        assert_eq!(fold(&Scalar::Int(2), "**", &[Scalar::Int(-1)]), None);
        // Mixed-type argument declines (caller only pins same-kind scalars).
        assert_eq!(fold(&Scalar::Int(1), "+", &[Scalar::Str("x".into())]), None);
    }

    /// rigor-rs#682 — every expectation is the reference's measured fold (it
    /// runs real Ruby, so overflow promotes to a Bignum `Constant`, never an
    /// `i64` wrap).
    #[test]
    fn promotes_integer_overflow_to_bigint() {
        let i = Scalar::Int;
        let big = |d: &str| Scalar::BigInt(d.to_string());
        // `checked_shl` guarded only the count: `2 << 63` used to wrap to `0`.
        assert_eq!(fold(&i(2), "<<", &[i(63)]), Some(big("18446744073709551616")));
        assert_eq!(fold(&i(1), "<<", &[i(62)]), Some(i(4611686018427387904)));
        assert_eq!(fold(&i(1), "<<", &[i(63)]), Some(big("9223372036854775808")));
        assert_eq!(fold(&i(-1), "<<", &[i(62)]), Some(i(-4611686018427387904)));
        // Shifts past i64 still fold inside the reference's bit limit.
        assert_eq!(fold(&i(1), "<<", &[i(100)]), Some(big("1267650600228229401496703205376")));
        assert_eq!(fold(&i(-8), "<<", &[i(97)]), Some(big("-1267650600228229401496703205376")));
        // Negative counts are the opposite direction, saturating at 0 / -1.
        assert_eq!(fold(&i(2), "<<", &[i(-1)]), Some(i(1)));
        assert_eq!(fold(&i(2), "<<", &[i(-100)]), Some(i(0)));
        assert_eq!(fold(&i(8), ">>", &[i(100)]), Some(i(0)));
        assert_eq!(fold(&i(-8), ">>", &[i(100)]), Some(i(-1)));
        assert_eq!(fold(&i(8), ">>", &[i(-2)]), Some(i(32)));
        // A left-shifted `>>` promotes too.
        assert_eq!(fold(&i(8), ">>", &[i(-100)]), Some(big("10141204801825835211973625643008")));
        // Arithmetic promotes.
        assert_eq!(fold(&i(i64::MAX), "+", &[i(1)]), Some(big("9223372036854775808")));
        assert_eq!(fold(&i(i64::MIN), "-", &[i(1)]), Some(big("-9223372036854775809")));
        assert_eq!(fold(&i(4611686018427387904), "*", &[i(4)]), Some(big("18446744073709551616")));
        assert_eq!(fold(&i(i64::MIN), "/", &[i(-1)]), Some(big("9223372036854775808")));
        // Ruby `/` floors: `7 / -2` is -4, `div_euclid`'s -3 was a drift
        // (pre-existing; PR #693 review).
        assert_eq!(fold(&i(7), "/", &[i(-2)]), Some(i(-4)));
        assert_eq!(fold(&i(-7), "/", &[i(-2)]), Some(i(3)));
        assert_eq!(fold(&i(-7), "/", &[i(2)]), Some(i(-4)));
        assert_eq!(fold(&i(7), "/", &[i(2)]), Some(i(3)));
        // An exponent at/past `u32::MAX` must not truncate into `checked_pow`:
        // `2 ** 4294967296` is `Integer` on the oracle, never `1`.
        assert_eq!(fold(&i(2), "**", &[i(4294967296)]), None);
        assert_eq!(fold(&i(-2), "**", &[i(4294967297)]), None);
        assert_eq!(fold(&i(0), "**", &[i(4294967296)]), Some(i(0)));
        assert_eq!(fold(&i(1), "**", &[i(4294967296)]), Some(i(1)));
        assert_eq!(fold(&i(-1), "**", &[i(4294967297)]), Some(i(-1)));
        assert_eq!(fold(&i(-1), "**", &[i(4294967296)]), Some(i(1)));
        // `decimal_pow`'s inner multiply runs in `u128`: `9 * i64::MAX`
        // exceeds `u64` — the wrap gave a wrong literal (PR #693 review).
        assert_eq!(
            fold(&i(i64::MAX), "**", &[i(2)]),
            Some(big("85070591730234615847396907784232501249"))
        );
        // `<<`/`>>` take a Bignum count — only its sign matters at that size.
        assert_eq!(fold(&i(1), ">>", &[big("99999999999999999999")]), Some(i(0)));
        assert_eq!(fold(&i(-8), ">>", &[big("99999999999999999999")]), Some(i(-1)));
        assert_eq!(fold(&i(1), "<<", &[big("-99999999999999999999")]), Some(i(0)));
        assert_eq!(fold(&i(5), "<<", &[big("99999999999999999999")]), None);
        assert_eq!(fold(&i(5), ">>", &[big("-99999999999999999999")]), None);
        assert_eq!(fold(&i(i64::MIN), "abs", &[]), Some(big("9223372036854775808")));
        assert_eq!(fold(&i(i64::MAX), "succ", &[]), Some(big("9223372036854775808")));
        assert_eq!(fold(&i(i64::MIN), "pred", &[]), Some(big("-9223372036854775809")));
        assert_eq!(fold(&i(i64::MIN), "-@", &[]), Some(big("9223372036854775808")));
        // `i64::MIN % -1` panicked on the raw `%`; Ruby answers 0.
        assert_eq!(fold(&i(i64::MIN), "%", &[i(-1)]), Some(i(0)));
        assert_eq!(fold(&i(i64::MIN), "%", &[i(3)]), Some(i(1)));
    }

    /// The reference's `INTEGER_FOLD_BIT_LIMIT` / `oversized_number?` gates,
    /// applied to shifts and `**` (`shift_blow_up?` is `receiver.bit_length +
    /// shift`; a negative receiver counts `|v| - 1` bits).
    #[test]
    fn declines_integer_folds_past_the_bit_limit() {
        let i = Scalar::Int;
        // 1 + 13606 = 13607 > 13606: the reference widens to Nominal[Integer],
        // which declining reaches through the RBS `Integer#<<` signature.
        assert_eq!(fold(&i(1), "<<", &[i(13606)]), None);
        // 1 + 13605 = 13606: still folded.
        let Some(Scalar::BigInt(d)) = fold(&i(1), "<<", &[i(13605)]) else {
            panic!("1 << 13605 folds");
        };
        assert_eq!(d, decimal_shl(1, 13605));
        assert_eq!(d.len(), 4096);
        // A negative receiver counts one fewer bit: -8 is 3 bits, so
        // -8 << 13603 (result bit length 13606) still folds.
        assert!(matches!(fold(&i(-8), "<<", &[i(13603)]), Some(Scalar::BigInt(_))));
        assert_eq!(fold(&i(-8), "<<", &[i(13604)]), None);
        // `**` uses the lower-bound estimate `floor(log2|a|) * b` …
        assert_eq!(fold(&i(3), "**", &[i(13607)]), None);
        // … and the result gate when the estimate passes: 10**4096 renders
        // 4097 digits (bit length 13607) where the estimate said 12288.
        assert_eq!(fold(&i(10), "**", &[i(4096)]), None);
        // 2**13606 estimates to 13606 but the result has bit length 13607.
        assert_eq!(fold(&i(2), "**", &[i(13606)]), None);
        let Some(Scalar::BigInt(d)) = fold(&i(2), "**", &[i(13605)]) else {
            panic!("2 ** 13605 folds");
        };
        assert_eq!(d, decimal_shl(1, 13605));
    }

    /// rigor-rs#694 — the reference's `WidenedResult` answers, which `fold`
    /// flattens into `None`: `fold_outcome` keeps the nominal class.
    #[test]
    fn widened_outcomes_past_the_bit_limit() {
        let i = Scalar::Int;
        // Positive exponents past the limit widen to `Nominal[Integer]`.
        assert_eq!(
            fold_outcome(&i(2), "**", &[i(20000)]),
            FoldOutcome::Widened("Integer")
        );
        assert_eq!(
            fold_outcome(&i(2), "pow", &[i(20000)]),
            FoldOutcome::Widened("Integer")
        );
        assert_eq!(
            fold_outcome(&i(1), "<<", &[i(13606)]),
            FoldOutcome::Widened("Integer")
        );
        // `2.pow(5)` folds like `2 ** 5` — `for 32` on the oracle.
        assert_eq!(
            fold_outcome(&i(2), "pow", &[i(5)]),
            FoldOutcome::Value(Scalar::Int(32))
        );
        // A negative exponent is a Rational: the estimate's widen is
        // `Nominal[Rational]` (`2 ** -20000`) and the result gate widens on
        // the denominator too (`3 ** -10000` — the `1 * 10000` estimate
        // passes but `3**10000` is ~15850 bits). A small exponent still
        // declines — the Rational itself is not foldable.
        assert_eq!(
            fold_outcome(&i(2), "**", &[i(-20000)]),
            FoldOutcome::Widened("Rational")
        );
        assert_eq!(
            fold_outcome(&i(3), "**", &[i(-10000)]),
            FoldOutcome::Widened("Rational")
        );
        assert_eq!(fold_outcome(&i(2), "**", &[i(-3)]), FoldOutcome::Declined);
        // A unit base never blows the estimate (`floor(log2 1) == 0`).
        assert_eq!(
            fold_outcome(&i(1), "**", &[i(20000)]),
            FoldOutcome::Value(Scalar::Int(1))
        );
        assert_eq!(
            fold_outcome(&i(-1), "**", &[i(20001)]),
            FoldOutcome::Value(Scalar::Int(-1))
        );
    }

    #[test]
    fn folds_bigint_predicates_and_comparisons() {
        let i = Scalar::Int;
        let big = |d: &str| Scalar::BigInt(d.to_string());
        let n = big("99999999999999999999");
        assert_eq!(fold(&n, "==", &[i(0)]), Some(Scalar::Bool(false)));
        assert_eq!(fold(&n, "!=", &[i(0)]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, ">", &[i(i64::MAX)]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, "<", &[big("100000000000000000000")]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, "==", &[n.clone()]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, "even?", &[]), Some(Scalar::Bool(false)));
        assert_eq!(fold(&n, "odd?", &[]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, "zero?", &[]), Some(Scalar::Bool(false)));
        assert_eq!(fold(&n, "positive?", &[]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, "negative?", &[]), Some(Scalar::Bool(false)));
        assert_eq!(fold(&n, "to_s", &[]), Some(Scalar::Str("99999999999999999999".into())));
        // `-99999999999999999999` is a canonical negative BigInt spelling
        // (`-9223372036854775808` is not — it fits `i64`, so a produced
        // spelling that fits demotes to `Scalar::Int`).
        let neg = big("-99999999999999999999");
        assert_eq!(fold(&neg, "negative?", &[]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&neg, "<", &[i(0)]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&neg, "succ", &[]), Some(big("-99999999999999999998")));
        assert_eq!(fold(&neg, "pred", &[]), Some(big("-100000000000000000000")));
        assert_eq!(fold(&neg, "abs", &[]), Some(n.clone()));
        assert_eq!(fold(&neg, "-@", &[]), Some(n.clone()));
        assert_eq!(fold(&n, "succ", &[]), Some(big("100000000000000000000")));
        assert_eq!(fold(&big("9223372036854775808"), "pred", &[]), Some(i(i64::MAX)));
        assert_eq!(fold(&big("-9223372036854775809"), "succ", &[]), Some(i(i64::MIN)));
        assert_eq!(fold(&big("9223372036854775808"), "-@", &[]), Some(i(i64::MIN)));
        // Bignum `<<`/`>>` fold in place — same semantics as `shift_result`.
        assert_eq!(fold(&n, "<<", &[i(1)]), Some(big("199999999999999999998")));
        assert_eq!(fold(&n, ">>", &[i(1)]), Some(big("49999999999999999999")));
        assert_eq!(fold(&n, ">>", &[i(64)]), Some(i(5)));
        assert_eq!(fold(&n, ">>", &[i(1000)]), Some(i(0)));
        assert_eq!(fold(&neg, ">>", &[i(1)]), Some(big("-50000000000000000000")));
        assert_eq!(fold(&neg, ">>", &[i(63)]), Some(i(-11)));
        assert_eq!(fold(&neg, ">>", &[i(64)]), Some(i(-6)));
        assert_eq!(fold(&neg, ">>", &[i(1000)]), Some(i(-1)));
        assert_eq!(fold(&neg, "<<", &[i(1)]), Some(big("-199999999999999999998")));
        // A huge count by sign: `big << -huge` saturates, `big << +huge`
        // widens (the reference's `WidenedResult` — the RBS `Integer` here).
        let huge = big("99999999999999999999999999999999999999");
        assert_eq!(fold(&n, "<<", &[big("-99999999999999999999999999999999999999")]), Some(i(0)));
        assert_eq!(fold(&neg, "<<", &[big("-99999999999999999999999999999999999999")]), Some(i(-1)));
        assert_eq!(fold(&n, "<<", &[huge.clone()]), None);
        assert_eq!(fold(&n, ">>", &[huge.clone()]), Some(i(0)));
        assert_eq!(fold(&n, ">>", &[big("-99999999999999999999999999999999999999")]), None);
        // Bignum arithmetic (besides shifts) and cross-kind `==` decline
        // (the RBS answer).
        assert_eq!(fold(&n, "+", &[i(1)]), None);
        assert_eq!(fold(&n, "*", &[i(2)]), None);
        assert_eq!(fold(&n, "==", &[Scalar::Str("x".into())]), None);
    }

    /// The `oversized_number?` result gate applies on a BigInt receiver too:
    /// a literal past `INTEGER_FOLD_BIT_LIMIT` is still a `Constant` on both
    /// engines, but a fold whose RESULT stays past the limit widens to
    /// `Integer` on the oracle — never a minted `Constant` (PR #693 review).
    #[test]
    fn declines_bigint_results_past_the_bit_limit() {
        let i = Scalar::Int;
        let huge = Scalar::BigInt("9".repeat(5001));
        for m in ["succ", "pred", "abs", "-@", "+@"] {
            assert_eq!(fold(&huge, m, &[]), None, "{m} on an oversized receiver");
        }
        assert_eq!(fold(&huge, ">>", &[i(1)]), None);
        assert_eq!(fold(&huge, "<<", &[i(1)]), None);
        assert_eq!(fold(&huge, ">>", &[i(20000)]), Some(i(0)));
        // … while predicates and comparisons on it still fold (non-Integer
        // results are never gated).
        assert_eq!(fold(&huge, "even?", &[]), Some(Scalar::Bool(false)));
        assert_eq!(fold(&huge, ">", &[i(0)]), Some(Scalar::Bool(true)));
        // … and an `i64` receiver compares against a Bignum argument.
        assert_eq!(fold(&i(5), "<", &[huge.clone()]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&i(5), "==", &[huge]), Some(Scalar::Bool(false)));
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
        assert!(sidecar_blows_up("center", &[Scalar::BigInt("99999999999999999999".into())]));
        assert!(!sidecar_blows_up("tr", &[Scalar::Int(9999)]));
    }

    /// rigor-rs#769 — the reference folds `Integer#pow(exp, mod)` through
    /// `invoke_ternary` (a catalog `:leaf` with no `numeric_blow_up_class`
    /// precheck), `BigInt`-receiver `**` computes small results, and
    /// `Float#**` folds in doubles. Every expectation is oracle-verified.
    #[test]
    fn folds_pow_variants() {
        let i = Scalar::Int;
        let big = |d: &str| Scalar::BigInt(d.to_string());
        // `pow(exp, mod)` — modular exponentiation, sign of the divisor.
        assert_eq!(fold(&i(2), "pow", &[i(20000), i(97)]), Some(i(35)));
        assert_eq!(fold(&i(2), "pow", &[i(5), i(3)]), Some(i(2)));
        assert_eq!(fold(&i(2), "pow", &[i(5), i(-3)]), Some(i(-1)));
        assert_eq!(fold(&i(2), "pow", &[i(4), i(-7)]), Some(i(-5)));
        assert_eq!(fold(&i(2), "pow", &[i(20000), i(-97)]), Some(i(-62)));
        assert_eq!(fold(&i(-7), "pow", &[i(3), i(5)]), Some(i(2)));
        assert_eq!(fold(&i(-4), "pow", &[i(3), i(7)]), Some(i(6)));
        assert_eq!(fold(&i(2), "pow", &[i(0), i(3)]), Some(i(1)));
        assert_eq!(fold(&i(2), "pow", &[i(0), i(1)]), Some(i(0)));
        // Raising rows decline like the reference's rescue: a `0` modulus
        // (ZeroDivisionError) and a negative exponent (RangeError).
        assert_eq!(fold(&i(2), "pow", &[i(5), i(0)]), None);
        assert_eq!(fold(&i(2), "pow", &[i(-1), i(97)]), None);
        // Non-Integer operands decline (TypeError).
        assert_eq!(fold(&i(2), "pow", &[Scalar::Float(0.5), i(3)]), None);
        assert_eq!(fold(&i(2), "pow", &[i(5), Scalar::Float(3.0)]), None);
        // BigInt receiver / exponent / modulus.
        assert_eq!(
            fold(&big("100000000000000000000"), "pow", &[i(3), i(97)]),
            Some(i(47))
        );
        assert_eq!(
            fold(&i(2), "pow", &[big("10000000000000000000000000000000000000000"), i(97)]),
            Some(i(61))
        );
        assert_eq!(
            fold(&i(2), "pow", &[i(7), big("1000000000000000000000000000000")]),
            Some(i(128))
        );
        assert_eq!(
            fold(&i(3), "pow", &[i(100), big("-10000000000000000000000000")]),
            Some(big("-8870234378727297892477999"))
        );
        assert_eq!(
            fold(
                &big("1000000000000000000000000000000"),
                "pow",
                &[i(9), big("9999999999999999999999999999999999999999999999999999")]
            ),
            Some(i(10000000000))
        );
        // BigInt `**` computes a result under the bit limit (`10**20) ** 2`).
        assert_eq!(
            fold(&big("100000000000000000000"), "**", &[i(2)]),
            Some(big("10000000000000000000000000000000000000000"))
        );
        assert_eq!(
            fold(&big("-100000000000000000000"), "**", &[i(3)]),
            Some(big("-1000000000000000000000000000000000000000000000000000000000000"))
        );
        assert_eq!(
            fold(&big("-100000000000000000000"), "**", &[i(2)]),
            Some(big("10000000000000000000000000000000000000000"))
        );
        // A fitting Rational denominator declines like the `i64` rows; an
        // oversized one widens (both read `None` through `fold`).
        assert_eq!(fold(&big("100000000000000000000"), "**", &[i(-1)]), None);
        assert_eq!(fold(&big("100000000000000000000"), "**", &[i(-200)]), None);
        assert_eq!(fold(&big("100000000000000000000"), "**", &[i(0)]), Some(i(1)));
        // `Float#**` folds in doubles — Infinity / underflow / signed zero
        // results included; `pow` on a Float does NOT fold (undefined
        // method on the oracle).
        let f = Scalar::Float;
        assert_eq!(fold(&f(2.0), "**", &[f(5.0)]), Some(f(32.0)));
        assert_eq!(fold(&f(2.0), "**", &[i(5)]), Some(f(32.0)));
        assert_eq!(fold(&f(2.0), "**", &[i(-3)]), Some(f(0.125)));
        assert_eq!(fold(&f(-2.0), "**", &[i(-3)]), Some(f(-0.125)));
        assert_eq!(fold(&f(2.0), "**", &[i(20000)]), Some(f(f64::INFINITY)));
        assert_eq!(fold(&f(0.0), "**", &[f(-5.0)]), Some(f(f64::INFINITY)));
        assert_eq!(
            fold(&f(2.0), "**", &[big("1000000000000000000000000000000")]),
            Some(f(f64::INFINITY))
        );
        assert_eq!(
            fold(&f(0.5), "**", &[big("1000000000000000000000000000000")]),
            Some(f(0.0))
        );
        // A NaN — a Ruby Complex — declines.
        assert_eq!(fold(&f(-2.0), "**", &[f(0.5)]), None);
        assert_eq!(fold(&f(2.0), "pow", &[f(5.0)]), None);
        // Integer `**` / `pow` with a Float exponent coerce to doubles.
        assert_eq!(
            fold(&i(2), "**", &[f(0.5)]),
            Some(Scalar::Float(2.0f64.powf(0.5)))
        );
        assert_eq!(
            fold(&i(2), "pow", &[f(0.5)]),
            Some(Scalar::Float(2.0f64.powf(0.5)))
        );
        assert_eq!(fold(&i(-2), "**", &[f(0.5)]), None);
        assert_eq!(
            fold(&big("100000000000000000000"), "**", &[f(0.5)]),
            Some(f(1e20f64.powf(0.5)))
        );
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

