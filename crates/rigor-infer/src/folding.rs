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
    /// A `Tuple[Constant…]` result — the reference's `lift_array_result` /
    /// `try_fold_divmod` shape (`big.divmod(3)` -> `[q, r]`, `digits`,
    /// `gcdlcm`). Elements are already result-gated by the producer.
    Tuple(Vec<Scalar>),
    /// A `WidenedResult`: the fold knows only the result's class —
    /// `"Integer"`, `"Rational"`, … — for a number past
    /// `INTEGER_FOLD_BIT_LIMIT`.
    Widened(&'static str),
    /// The fold SUCCEEDED on the reference but produced a Constant the
    /// port's `Scalar` cannot hold — `Rational` / `Complex` results from
    /// `to_r` / `to_c` / `rationalize` / `quo` on an Integer receiver
    /// (rigor-rs#877). The reference's pinned Rational/Complex carrier
    /// stays silent downstream (`x = 5.to_r; x.zzz` is silent there), so
    /// the honest answer is untyped — NOT the RBS nominal, which would
    /// fire `call.undefined-method` the reference never emits (a `quo`
    /// call that RAISES declines instead, letting the RBS `Rational`
    /// answer stand like the reference's).
    Unrepresentable,
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
        FoldOutcome::Widened(_)
        | FoldOutcome::Tuple(_)
        | FoldOutcome::Unrepresentable
        | FoldOutcome::Declined => None,
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
    // A `Rational` / `Complex` constant the port's `Scalar` cannot hold —
    // the reference folds `to_r` / `to_c` / `rationalize` on an Integer
    // literal and `to_r` / `rationalize` on a Float literal to a pinned
    // carrier that stays SILENT downstream (`x = 5.to_r; x.zzz` never
    // witnesses there), so untyped is the honest answer — the RBS
    // `Rational`/`Complex` nominal would fire a `call.undefined-method`
    // the oracle never emits (rigor-rs#877). `Integer#to_c` folds there
    // (`INTEGER_UNARY`) but `Float#to_c` does NOT — its RBS `Complex`
    // answer stands (`1.5.to_c` -> `for Complex` on the oracle).
    if args.is_empty()
        && match (receiver, method) {
            (Scalar::Int(_) | Scalar::BigInt(_), "to_r" | "to_c" | "rationalize")
            | (Scalar::Float(_), "to_r" | "rationalize") => true,
            _ => false,
        }
    {
        return FoldOutcome::Unrepresentable;
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
                | "<" | "<=" | ">" | ">=" | "==" | "<=>"
                | "div" | "modulo" | "ceildiv" | "remainder" | "fdiv" | "quo"
                | "lcm" | "gcd" | "divmod" | "gcdlcm" | "digits"
                | "allbits?" | "anybits?" | "nobits?"
                | "floor" | "ceil" | "round" | "truncate"
                | "abs" | "succ" | "pred" | "even?" | "odd?" | "zero?" | "to_s"
                | "+@" | "-@"
                // BigInt-receiver only so far (rigor-rs#877): the i64
                // unary family is the issue's own deferred slice.
                | "~" | "bit_length" | "next" | "magnitude" | "to_i" | "to_int"
                | "to_f" | "nonzero?" | "finite?" | "infinite?" | "inspect"
                | "abs2" | "to_r" | "to_c" | "rationalize"
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

    // A Bignum or Float operand on `+`/`-`/`*` — `int + big` promotes past
    // `i64`, `int + 1.5` coerces to a Float — computes through the same
    // arbitrary-width path a BigInt receiver uses (rigor-rs#695). Other
    // operand kinds still decline through the `Scalar::Int` arm below.
    if let ("+" | "-" | "*", [b]) = (method, args) {
        if matches!(b, Scalar::BigInt(_) | Scalar::Float(_)) {
            return int_arith(&Scalar::Int(a), method, b);
        }
    }

    // The rest of the reference's Integer binary catalogue — shared with
    // the Bignum receiver's fold (rigor-rs#877): `int_binary` covers the
    // members `fold_int` never had (`div`, `ceildiv`, `remainder`, `fdiv`,
    // `<=>`, `lcm`, `gcd`, `divmod`, `gcdlcm`, `digits`, the bit-test
    // predicates, the decimal rounders) and the operands the i64 fast path
    // cannot take (a Bignum or Float operand on the existing `\`/`%`/`&`/
    // `|`/`^`/`[]` — `3 & big`, `5 % 2.5`). The i64-unary members the
    // issue's carve-out defers (`~`, `bit_length`, `next`, `magnitude`,
    // `to_i`, `finite?`, `nonzero?`, …) stay declined here.
    if let (m, [b]) = (method, args) {
        if matches!(
            m,
            "div" | "modulo" | "ceildiv" | "remainder" | "fdiv" | "quo" | "<=>"
                | "lcm" | "gcd" | "divmod" | "gcdlcm" | "digits" | "allbits?"
                | "anybits?" | "nobits?" | "floor" | "ceil" | "round" | "truncate"
        ) || (matches!(b, Scalar::BigInt(_) | Scalar::Float(_))
            && matches!(m, "/" | "%" | "&" | "|" | "^" | "[]"))
        {
            return int_binary(&Scalar::Int(a), m, b);
        }
    }

    // `digits` (base 10) — the spelling's own characters reversed into
    // little-endian place values; an `i64` never reaches the
    // `lift_array_result` cap of 32 (19 digits max). A negative receiver
    // raises `Math::DomainError` and declines to `Array[Integer]`
    // (rigor-rs#877).
    if method == "digits" && args.is_empty() {
        if a < 0 {
            return FoldOutcome::Declined;
        }
        return FoldOutcome::Tuple(
            a.to_string()
                .bytes()
                .rev()
                .map(|c| Scalar::Int(i64::from(c - b'0')))
                .collect(),
        );
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

/// `(exp_bits, shift_words)` — the estimated bit count a square-and-multiply
/// iterates and the per-bit [`decimal_shr_chunks`] pass width a `PowExp`
/// pays. `Small` reads its bits straight out of the `u64` (shift width 0);
/// `Big` halves the whole chunk vector per bit — `exp_bits * shift_words`
/// word-ops the modmul estimate used to leave uncounted (rigor-rs#850).
fn pow_exp_cost(exp: &PowExp) -> (usize, usize) {
    match exp {
        PowExp::Small(e) => (64 - e.leading_zeros() as usize, 0),
        PowExp::Big(chunks) => (chunks.len() * 30, chunks.len()),
    }
}

/// One fold's estimated word-ops: `exp_bits` square-and-multiply steps at
/// `modmul_words` word-ops each, plus `shift_words` per iterated bit for the
/// `ExpBits` chunk-halving a `PowExp::Big` exponent pays (rigor-rs#850).
/// `modmul_words` is charged once per bit although a step is really a
/// squaring plus ~half a multiply (~1.5 modmuls); the per-path limits'
/// measured calibrations absorb that constant factor.
fn powmod_work(exp: &PowExp, modmul_words: usize) -> u128 {
    let (exp_bits, shift_words) = pow_exp_cost(exp);
    (exp_bits as u128)
        .saturating_mul((modmul_words as u128).saturating_add(shift_words as u128))
}

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

/// The chunk path's budget of [`powmod_work`] units — a `chunks_modmul`
/// word-op costs ~17ns in release (the per-digit `chunks_mul_small` alloc,
/// `chunks_cmp` and fixup), so `1 << 24` bounds a fold at ~0.3s and still
/// passes an RSA-2048 shape (~2k exp bits * ~69² words ≈ 9.9M work). `1 << 28`
/// once let a single fold run ~4.5s (rigor-rs#837). NOT interchangeable with
/// [`POWMOD_U128_WORK_LIMIT`]: the paths' per-unit costs differ ~50×.
const POWMOD_WORK_LIMIT: u128 = 1 << 24;

/// The u128 path's budget: its modmul is a handful of `u128` ops (~8
/// word-equivalents) and its `ExpBits` shift word-op is a cheap
/// `decimal_shr_chunks` pass (~0.3ns — a 360k-digit exponent ran ~48G
/// word-ops ≈ 13s ungated, rigor-rs#850). `1 << 30` bounds a fold at ~0.9s
/// (measured at the ~53k-digit boundary, where the shift term dominates)
/// and still passes that exponent.
const POWMOD_U128_WORK_LIMIT: u128 = 1 << 30;

/// `a.pow(e, m)` with an `i64` modulus — the whole computation stays under
/// `u128` (`|m| <= 2**63`, so a product of residues is under `2**126`).
fn int_pow_mod_u128(base: IntReceiver<'_>, exp: PowExp, m: i64) -> FoldOutcome {
    // The estimate's only large term is the `ExpBits` shift — a `PowExp::Big`
    // exponent costs `exp_bits * exp_chunks` word-ops before any modmul. A
    // `Small` exponent iterates at most 64 O(1) bits, so it never gates.
    if powmod_work(&exp, 8) > POWMOD_U128_WORK_LIMIT {
        return FoldOutcome::Declined;
    }
    let mm = (m as i128).unsigned_abs();
    let mut cur = match base {
        IntReceiver::Int(a) => i128::from(a).rem_euclid(mm as i128) as u128,
        IntReceiver::Big(d) => decimal_mod_u128(d, mm),
    };
    let mut r = 1 % mm;
    for bit in ExpBits(exp) {
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
    // |signed| < |m| <= 2**63, so the cast cannot fail — a violated
    // invariant is a bug, not a foldable value (rigor-rs#837).
    FoldOutcome::Value(Scalar::Int(
        i64::try_from(signed).expect("pow-mod residue is under |m| <= 2**63"),
    ))
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
/// (rigor-rs#769). The [`powmod_work`] estimate is bounded — a pathological
/// exponent or modulus past it declines, the zero-FP-safe side.
fn int_pow_mod_chunks(base: IntReceiver<'_>, exp: PowExp, d: &str) -> FoldOutcome {
    // `exp_bits * (m_words² + shift_words)` — decline a computation the
    // reference's native bignum does invisibly but this port cannot afford.
    // The `m_words²` term is the `chunks_modmul` estimate; `shift_words` is
    // the `ExpBits` chunk-halving a `PowExp::Big` exponent pays per iterated
    // bit, which the estimate once omitted — its worst case under the modmul
    // term alone (exp_bits = C·30, m_words = 3) was a ~32s fold
    // (rigor-rs#850).
    let m_mag = decimal_chunks(d.trim_start_matches('-'));
    if powmod_work(&exp, m_mag.len().saturating_pow(2)) > POWMOD_WORK_LIMIT {
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
    let mut cur = cur;
    for bit in ExpBits(exp) {
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
    for (i, word) in u.iter_mut().enumerate().skip(shift) {
        // `sub` can reach CHUNK (`p`-word CHUNK-1 plus a borrow) — `u[i]` is
        // always below it, so the borrow arm is the only reachable one then.
        let sub = p.get(i - shift).copied().unwrap_or(0) + borrow;
        if *word >= sub {
            *word -= sub;
            borrow = 0;
        } else {
            *word += CHUNK - sub;
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
/// half of [`chunks_modmul`].
fn chunks_mod(a: &[u64], m: &[u64]) -> Vec<u64> {
    chunks_divmod(a, m).1
}

/// `a divmod m` on little-endian base-1e9 chunks (`m` non-zero) — the
/// quotient digits the loop always computed, now returned with the
/// remainder for the `/` `%` `div` `modulo` `ceildiv` `remainder` `divmod`
/// `digits` `lcm`/`gcd` folds (rigor-rs#877). Each quotient digit is
/// estimated from the leading words after normalising the divisor's top
/// word past `CHUNK/2` (Knuth's bound: the estimate overshoots by at most
/// 2, so the fixup loop is bounded).
fn chunks_divmod(a: &[u64], m: &[u64]) -> (Vec<u64>, Vec<u64>) {
    const CHUNK: u128 = 1_000_000_000;
    if chunks_cmp(a, m) == std::cmp::Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if m.len() == 1 {
        let d = u128::from(m[0]);
        let mut q = vec![0u64; a.len()];
        let mut r = 0u128;
        for i in (0..a.len()).rev() {
            let cur = r * CHUNK + u128::from(a[i]);
            q[i] = (cur / d) as u64;
            r = cur % d;
        }
        while q.last() == Some(&0) {
            q.pop();
        }
        return (q, if r == 0 { vec![] } else { vec![r as u64] });
    }
    // Normalise so `v`'s top word is >= CHUNK/2.
    let f = CHUNK / (u128::from(*m.last().unwrap_or(&1)) + 1);
    let v = chunks_mul_small(m, f);
    let mut u = chunks_mul_small(a, f);
    let n = v.len();
    let qlen = u.len() + 1 - n;
    let mut q = vec![0u64; qlen];
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
        q[j] = qd as u64;
        if qd > 0 {
            chunks_sub_shift(&mut u, &prod, j);
        }
    }
    while q.last() == Some(&0) {
        q.pop();
    }
    // Denormalise the remainder.
    let r = chunks_div_small(&u, f);
    debug_assert!(chunks_cmp(&r, m) == std::cmp::Ordering::Less);
    (q, r)
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

/// Folds on a `Scalar::BigInt` receiver (rigor-rs#682). The members whose
/// answer reads straight off the decimal spelling — predicates, sign ops,
/// `to_s`, `succ`/`pred`, integer comparisons, and `<<`/`>>` — are folded,
/// and `+`/`-`/`*` compute through the arbitrary-width chunk path
/// [`int_arith`] (rigor-rs#695); division, modulo and the bit ops (`&`,
/// `|`, `^`, `[]`, `~`) still decline to the RBS answer, the zero-FP-safe
/// side. `**`/`pow` still declines to fold — but the WIDENED answer is
/// decidable at the `numeric_blow_up_class` precheck (rigor-rs#694,
/// [`bigint_pow_result`]).
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
        // `to_i`/`to_int`/`floor`/`ceil`/`round`/`truncate`/`nonzero?`
        // answer the receiver itself on the oracle — the pin re-enters
        // through `big_scalar`'s `oversized_number?` gate like the
        // reference's `fold_result` (a past-limit literal widens to
        // `Integer` on `to_i` too).
        ("to_i" | "to_int" | "floor" | "ceil" | "round" | "truncate" | "nonzero?", []) => {
            big_scalar(d.to_string())
        }
        // `next` is `succ`'s alias; `magnitude` is `abs`'s.
        ("next", []) => big_scalar(decimal_step(d, true)),
        ("magnitude", []) => big_scalar(d.trim_start_matches('-').to_string()),
        // `finite?` is `true` on every Integer; `infinite?` is `nil`.
        ("finite?", []) => FoldOutcome::Value(Scalar::Bool(true)),
        ("infinite?", []) => FoldOutcome::Value(Scalar::Nil),
        ("inspect", []) => FoldOutcome::Value(Scalar::Str(d.to_string())),
        // `to_f` is `rb_big2dbl` — a correct round-to-nearest `f64`,
        // `Infinity` past the representable range (foldable — only NaN
        // is not).
        ("to_f", []) => match d.parse::<f64>() {
            Ok(v) if v.is_nan() => FoldOutcome::Declined,
            Ok(v) => FoldOutcome::Value(Scalar::Float(v)),
            Err(_) => FoldOutcome::Declined,
        },
        // `~v` is `-(v + 1)` — `decimal_step` does the magnitude work:
        // positive `v` negates its `v + 1`, negative `-(A)` answers
        // `A - 1`.
        ("~", []) => big_scalar(if negative {
            decimal_step(d.trim_start_matches('-'), false)
        } else {
            format!("-{}", decimal_step(d, true))
        }),
        // `bit_length` counts the magnitude's bits on a positive
        // receiver; a negative `-(A)` counts `(A - 1)`'s — `(-8)` is
        // `...1000`, three significant bits (`(-4)` is `...100`, two).
        ("bit_length", []) => {
            let limbs = chunks_to_limbs(&decimal_chunks(d.trim_start_matches('-')));
            let limbs = if negative { limbs_dec(&limbs) } else { limbs };
            let bits = limbs.iter().rposition(|&w| w != 0).map_or(0, |p| {
                (p as i64) * 32 + (32 - limbs[p].leading_zeros() as i64)
            });
            FoldOutcome::Value(Scalar::Int(bits))
        }
        // `abs2` is `v * v` — the `*` path's bit-length bound widens a
        // past-limit square up front.
        ("abs2", []) => int_arith(&Scalar::BigInt(d.to_string()), "*", &Scalar::BigInt(d.to_string())),
        // `digits` (base 10) — the decimal spelling's own characters,
        // reversed into little-endian place values. A negative receiver
        // raises `Math::DomainError` and declines to `Array[Integer]`;
        // `lift_array_result` caps at 32 elements.
        ("digits", []) => {
            if negative || d.trim_start_matches('-').len() > 32 {
                FoldOutcome::Declined
            } else {
                FoldOutcome::Tuple(
                    d.bytes().rev().map(|c| Scalar::Int(i64::from(c - b'0'))).collect(),
                )
            }
        }
        // `+`/`-`/`*` at arbitrary width — integer operands combine as
        // sign + magnitude chunks, a Float operand coerces through
        // `rb_big2dbl` (rigor-rs#695).
        ("+" | "-" | "*", [b]) => int_arith(&Scalar::BigInt(d.to_string()), method, b),
        // The rest of the reference's Integer binary catalogue — the
        // `/`/`%` family, `<=>`, bit ops and tests, gcd/lcm, `fdiv`/`quo`,
        // `digits(base)`, the decimal rounders, `divmod`/`gcdlcm` — over
        // the shared operand layer (rigor-rs#877).
        (
            "/" | "div" | "%" | "modulo" | "ceildiv" | "remainder" | "fdiv" | "quo"
            | "<=>" | "&" | "|" | "^" | "[]" | "lcm" | "gcd" | "divmod" | "gcdlcm"
            | "digits" | "allbits?" | "anybits?" | "nobits?" | "floor" | "ceil"
            | "round" | "truncate",
            [b],
        ) => int_binary(&Scalar::BigInt(d.to_string()), method, b),
        _ => FoldOutcome::Declined,
    }
}

/// `a <op> b` (`+` / `-` / `*`) on integer scalars at any width — the
/// `Scalar::BigInt` arithmetic `fold_bigint` declined before (rigor-rs#695).
///
/// Integer operands are sign + little-endian base-1e9 chunks — the spelling
/// [`decimal_chunks`] / [`chunks_mul`] / [`chunks_to_decimal`] already carry,
/// so no new representation. `+`/`-` cost O(width); `*` is schoolbook
/// O(w_a·w_b), so a bit-length lower bound on the operands answers
/// `FoldOutcome::Widened` up front when it already clears
/// [`INTEGER_FOLD_BIT_LIMIT`] — the `WidenedResult` the reference's
/// `oversized_number?` mints, without spelling the product out. A spelled
/// result exits through [`big_scalar`]'s gate like every integer result.
///
/// A Float operand coerces the integer operand the way `rb_big2dbl` /
/// C's `(double)v` do (`big + 1.5` -> `1.0e20`, `1 + 1.5` -> `2.5`). A NaN —
/// `inf * -0.0` is a Ruby Complex — declines like the reference's
/// `foldable_constant_value?`, which never holds a NaN `Constant`.
fn int_arith(a: &Scalar, method: &str, b: &Scalar) -> FoldOutcome {
    if let Scalar::Float(f) = b {
        let a = match a {
            Scalar::Int(v) => *v as f64,
            Scalar::BigInt(d) => match d.parse::<f64>() {
                Ok(v) => v,
                Err(_) => return FoldOutcome::Declined,
            },
            _ => return FoldOutcome::Declined,
        };
        let r = match method {
            "+" => a + f,
            "-" => a - f,
            "*" => a * f,
            _ => return FoldOutcome::Declined,
        };
        return if r.is_nan() {
            FoldOutcome::Declined
        } else {
            FoldOutcome::Value(Scalar::Float(r))
        };
    }
    let Some((a_neg, a_mag)) = int_operand_chunks(a) else {
        return FoldOutcome::Declined;
    };
    let Some((b_neg, b_mag)) = int_operand_chunks(b) else {
        return FoldOutcome::Declined;
    };
    let (neg, mag) = match method {
        "+" => signed_chunk_sum(a_neg, &a_mag, b_neg, &b_mag),
        "-" => signed_chunk_sum(a_neg, &a_mag, !b_neg, &b_mag),
        "*" => {
            // `0 * big` is `0` — check before the bit-length bound, which
            // would otherwise widen the certain zero.
            if chunks_is_zero(&a_mag) || chunks_is_zero(&b_mag) {
                return FoldOutcome::Value(Scalar::Int(0));
            }
            // `floor(log2 |v|)` lower bounds: `|a·b| >= 2**(lb_a + lb_b)`,
            // so the product's `bit_length` clears the limit the moment
            // the bounds do (`v >= 2**13606` is `bit_length 13607`).
            if chunks_bits_lb(&a_mag).saturating_add(chunks_bits_lb(&b_mag))
                >= u128::from(INTEGER_FOLD_BIT_LIMIT)
            {
                return FoldOutcome::Widened("Integer");
            }
            (a_neg != b_neg, chunks_mul(&a_mag, &b_mag))
        }
        _ => return FoldOutcome::Declined,
    };
    big_scalar(chunks_to_decimal(neg, &mag))
}

/// An integer scalar as (negative, little-endian base-1e9 magnitude chunks)
/// for [`int_arith`]. `Scalar::BigInt` is never 0; a `Scalar::Int(0)`
/// magnitude parses to the all-zero chunks [`chunks_is_zero`] reads.
fn int_operand_chunks(s: &Scalar) -> Option<(bool, Vec<u64>)> {
    match s {
        Scalar::Int(v) => Some((*v < 0, decimal_chunks(&v.unsigned_abs().to_string()))),
        Scalar::BigInt(d) => {
            Some((d.starts_with('-'), decimal_chunks(d.trim_start_matches('-'))))
        }
        _ => None,
    }
}

/// The signed `a + b` combine over magnitude chunks: equal signs add the
/// magnitudes, opposite signs subtract the smaller and keep the larger's
/// sign. `b_neg` arrives already adjusted for `-` (the caller flips it).
fn signed_chunk_sum(a_neg: bool, a: &[u64], b_neg: bool, b: &[u64]) -> (bool, Vec<u64>) {
    if a_neg == b_neg {
        (a_neg, chunks_add(a, b))
    } else {
        match chunks_cmp(a, b) {
            std::cmp::Ordering::Greater => (a_neg, chunks_sub(a, b)),
            std::cmp::Ordering::Less => (b_neg, chunks_sub(b, a)),
            std::cmp::Ordering::Equal => (false, Vec::new()),
        }
    }
}

/// `a + b` on little-endian base-1e9 chunks.
fn chunks_add(a: &[u64], b: &[u64]) -> Vec<u64> {
    const CHUNK: u64 = 1_000_000_000;
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0u64;
    for i in 0..a.len().max(b.len()) {
        let cur = a.get(i).copied().unwrap_or(0) + b.get(i).copied().unwrap_or(0) + carry;
        out.push(cur % CHUNK);
        carry = cur / CHUNK;
    }
    while carry > 0 {
        out.push(carry % CHUNK);
        carry /= CHUNK;
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// A lower bound on `floor(log2 |v|)` for a base-1e9 chunk vector —
/// `v >= 1e9**(len-1)`, the chunk analogue of [`bigint_base_bits_lb`].
fn chunks_bits_lb(chunks: &[u64]) -> u128 {
    const LOG2_1E9: f64 = 29.897_352_854_986_263;
    (chunks.len().saturating_sub(1) as f64 * LOG2_1E9) as u128
}

// --- Shared integer binary ops (rigor-rs#877) -------------------------------
//
// The rest of the reference's Integer fold family — `NUMERIC_BINARY`, the
// catalog leaves and `INTEGER_ARRAY_*` — over the same sign + base-1e9
// chunk operands [`int_operand_chunks`] already produces. Shared by the
// `Scalar::Int` and `Scalar::BigInt` receivers: a `Scalar::Int` receiver
// only reaches here for the members `fold_int` never had or for operands
// past `i64`, and a Bignum operand always rides the same code (the
// promotion `+`/`-`/`*` took through [`int_arith`]).

/// `a <op> b` on integer scalars at any width — the members of the
/// reference's Integer binary catalogue the port declined before
/// (rigor-rs#877). A `Float` operand coerces the way `invoke_binary`'s
/// `public_send` does (`big / 3.0` -> `3.33…e19`, `big <=> 1e30` -> `-1`);
/// a non-numeric operand declines to the RBS tier, EXCEPT `<=>`, which
/// pins `nil` on the oracle (`big <=> "s"` -> `for nil`).
fn int_binary(a: &Scalar, method: &str, b: &Scalar) -> FoldOutcome {
    if let Scalar::Float(f) = b {
        return int_binary_float(a, method, *f);
    }
    let Some((a_neg, a_mag)) = int_operand_chunks(a) else {
        return FoldOutcome::Declined;
    };
    let Some((b_neg, b_mag)) = int_operand_chunks(b) else {
        // `Integer#<=>` on a non-numeric pins `Constant[nil]` on the oracle
        // (`big <=> "s"` -> `for nil`); every other member raises and the
        // RBS tier answers (`& "s"` -> argument-type-mismatch + `Integer`).
        return if method == "<=>" {
            FoldOutcome::Value(Scalar::Nil)
        } else {
            FoldOutcome::Declined
        };
    };
    match method {
        "+" | "-" | "*" => int_arith(a, method, b),
        "/" | "div" | "%" | "modulo" | "ceildiv" | "remainder" | "divmod" => {
            // A `0` divisor raises on the oracle: the fold declines and the
            // RBS answer stands (`for Integer`) — `flow.always-raises`
            // fires separately on `/` `%` `div` `modulo` `divmod`
            // (`ceildiv`/`remainder` are not in `INTEGER_RAISING_OPERATORS`).
            if chunks_is_zero(&b_mag) {
                FoldOutcome::Declined
            } else {
                int_divmod_family(a_neg, &a_mag, method, b_neg, &b_mag)
            }
        }
        "fdiv" => {
            let (Some(af), Some(bf)) = (scalar_to_f64(a), scalar_to_f64(b)) else {
                return FoldOutcome::Declined;
            };
            let r = af / bf;
            if r.is_nan() {
                FoldOutcome::Declined
            } else {
                FoldOutcome::Value(Scalar::Float(r))
            }
        }
        // `Integer#quo` on an Integer operand answers a Rational — a
        // carrier `Scalar` cannot hold (the pinned constant is silent
        // downstream on the oracle). A zero divisor raises → Declined.
        "quo" => {
            if chunks_is_zero(&b_mag) {
                FoldOutcome::Declined
            } else {
                FoldOutcome::Unrepresentable
            }
        }
        "<=>" => FoldOutcome::Value(Scalar::Int(match (a_neg, b_neg) {
            (false, true) => 1,
            (true, false) => -1,
            _ => {
                let o = chunks_cmp(&a_mag, &b_mag);
                let o = if a_neg { o.reverse() } else { o };
                match o {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }
            }
        })),
        "&" | "|" | "^" => {
            let (neg, limbs) = limbs_bitop(
                a_neg,
                &chunks_to_limbs(&a_mag),
                b_neg,
                &chunks_to_limbs(&b_mag),
                method,
            );
            big_scalar(chunks_to_decimal(neg, &limbs_to_chunks(&limbs)))
        }
        "[]" => {
            // Bit reference `v[i]` — two's-complement infinite width. A
            // negative index reads `0`; past the magnitude a positive
            // receiver reads `0`, a negative one the sign bit (`(-v)[i]`
            // is bit `i` of `~(v-1)`). A Bignum index is always past.
            let a_limbs = chunks_to_limbs(&a_mag);
            match b {
                Scalar::Int(i) if *i < 0 => FoldOutcome::Value(Scalar::Int(0)),
                Scalar::Int(i) => FoldOutcome::Value(Scalar::Int(i64::from(
                    if a_neg {
                        1 - limb_bit(&limbs_dec(&a_limbs), *i as u64)
                    } else {
                        limb_bit(&a_limbs, *i as u64)
                    },
                ))),
                Scalar::BigInt(_) => FoldOutcome::Value(Scalar::Int(i64::from(a_neg))),
                _ => FoldOutcome::Declined,
            }
        }
        "allbits?" | "anybits?" | "nobits?" => {
            let a_limbs = chunks_to_limbs(&a_mag);
            let b_limbs = chunks_to_limbs(&b_mag);
            let (m_neg, m_limbs) = limbs_bitop(a_neg, &a_limbs, b_neg, &b_limbs, "&");
            let masked = limbs_is_zero(&m_limbs) && !m_neg;
            let all = limbs_eq_signed(m_neg, &m_limbs, b_neg, &b_limbs);
            let yes = match method {
                "allbits?" => all,
                "anybits?" => !masked,
                _ => masked,
            };
            FoldOutcome::Value(Scalar::Bool(yes))
        }
        // `gcd` is bounded by its operands but a pinned operand can
        // already sit past `INTEGER_FOLD_BIT_LIMIT` — spell through
        // `big_scalar`'s oversized gate like every result.
        "gcd" => big_scalar(chunks_to_decimal(false, &chunks_gcd(&a_mag, &b_mag))),
        "lcm" => int_lcm(&a_mag, &b_mag),
        "gcdlcm" => {
            let g = chunks_gcd(&a_mag, &b_mag);
            match int_lcm_inner(&a_mag, &b_mag, &g) {
                None => FoldOutcome::Declined,
                Some(l) => {
                    let (Some(gs), Some(ls)) = (mag_scalar_opt(&g), mag_scalar_opt(&l))
                    else {
                        return FoldOutcome::Declined;
                    };
                    FoldOutcome::Tuple(vec![gs, ls])
                }
            }
        }
        "digits" => {
            // `digits(base)` — base < 2 or a negative receiver raises (the
            // reference's guard declines to `Array[Integer]`); the
            // `lift_array_result` cap declines past 32 digits.
            if a_neg || b_neg || b_mag.is_empty() {
                return FoldOutcome::Declined;
            }
            int_digits(&a_mag, &b_mag)
        }
        "floor" | "ceil" | "round" | "truncate" => {
            int_decimal_round(a_neg, &a_mag, method, b_neg, &b_mag)
        }
        _ => FoldOutcome::Declined,
    }
}

/// `Integer#/`-family sign rules over magnitude chunks — the Knuth-D
/// quotient [`chunks_mod`] already computed and discarded (rigor-rs#877).
/// `/`/`div` floor toward `-inf`, `%`/`modulo` take the divisor's sign,
/// `ceildiv` rounds toward `+inf`, `remainder` truncates toward `0`, and
/// `divmod` lifts `[floor-q, sign-of-b-r]` to a Tuple of pinned constants.
fn int_divmod_family(
    a_neg: bool,
    a_mag: &[u64],
    method: &str,
    b_neg: bool,
    b_mag: &[u64],
) -> FoldOutcome {
    let (q, r) = chunks_divmod(a_mag, b_mag);
    let rem_nz = !chunks_is_zero(&r);
    match method {
        "/" | "div" => {
            // Floor division: a quotient's sign is the operand-sign
            // product; opposite signs with a nonzero remainder step the
            // quotient one further negative (`-5 / 3` -> `-2`, `-5 / -3`
            // -> `1`).
            let (neg, mag) = if a_neg != b_neg {
                if rem_nz {
                    (true, chunks_add(&q, &[1]))
                } else {
                    (!chunks_is_zero(&q), q)
                }
            } else {
                (false, q)
            };
            big_scalar(chunks_to_decimal(neg, &mag))
        }
        "%" | "modulo" => {
            if !rem_nz {
                return FoldOutcome::Value(Scalar::Int(0));
            }
            // Sign of the DIVISOR: opposite signs give `r = |b| - rem`.
            let (neg, mag) = if a_neg != b_neg {
                (b_neg, chunks_sub(b_mag, &r))
            } else {
                (b_neg, r)
            };
            big_scalar(chunks_to_decimal(neg, &mag))
        }
        "ceildiv" => {
            // Ceil division: same signs with a nonzero remainder step the
            // quotient one further positive; opposite signs truncate
            // toward `0` (`-big.ceildiv(4)` -> `-24999999999999999999`).
            let (neg, mag) = if a_neg == b_neg {
                (
                    false,
                    if rem_nz { chunks_add(&q, &[1]) } else { q },
                )
            } else {
                (!chunks_is_zero(&q), q)
            };
            big_scalar(chunks_to_decimal(neg, &mag))
        }
        "remainder" => {
            // Truncated — sign of the dividend.
            let neg = a_neg && rem_nz;
            big_scalar(chunks_to_decimal(neg, &r))
        }
        "divmod" => {
            // `[floor-q, sign-of-b-r]` — `lift_array_result`'s per-element
            // `foldable_constant_value?` declines the PAIR when either
            // element spills `INTEGER_FOLD_BIT_LIMIT`, so spell each
            // through the oversized gate first.
            let (q_neg, q_mag) = if a_neg != b_neg {
                if rem_nz {
                    (true, chunks_add(&q, &[1]))
                } else {
                    (!chunks_is_zero(&q), q)
                }
            } else {
                (false, q)
            };
            let (r_neg, r_mag) = if !rem_nz {
                (false, Vec::new())
            } else if a_neg != b_neg {
                (b_neg, chunks_sub(b_mag, &r))
            } else {
                (b_neg, r)
            };
            let (Some(qs), Some(rs)) = (
                spelled_scalar_opt(&chunks_to_decimal(q_neg, &q_mag)),
                spelled_scalar_opt(&chunks_to_decimal(r_neg, &r_mag)),
            ) else {
                return FoldOutcome::Declined;
            };
            FoldOutcome::Tuple(vec![qs, rs])
        }
        _ => FoldOutcome::Declined,
    }
}

/// `v.digits(base)` — base-b place values little-endian, as pinned Integer
/// constants, capped at the reference's `lift_array_result` limit of 32
/// elements (`STRING_ARRAY_LIFT_LIMIT`; a longer expansion declines to the
/// `Array[Integer]` RBS answer — `big.digits(2)` is 67 long).
fn int_digits(a_mag: &[u64], b_mag: &[u64]) -> FoldOutcome {
    // The base must be >= 2; `b_mag` holds its chunks (a Bignum base is
    // always >= 2, and a `0`/`1` base declines via this length check —
    // `chunks_cmp` reads `[0]`/`[1]` magnitudes directly).
    if chunks_cmp(b_mag, &[2]) == std::cmp::Ordering::Less {
        return FoldOutcome::Declined;
    }
    const LIFT_LIMIT: usize = 32;
    let mut mag = a_mag.to_vec();
    let mut out: Vec<Scalar> = Vec::new();
    while !chunks_is_zero(&mag) {
        if out.len() >= LIFT_LIMIT {
            return FoldOutcome::Declined;
        }
        let (q, r) = chunks_divmod(&mag, b_mag);
        // A digit is always < base; the base itself fits i64 for any
        // single-chunk value and a wider base makes this the last digit.
        out.push(Scalar::Int(r.first().copied().unwrap_or(0) as i64));
        mag = q;
    }
    FoldOutcome::Tuple(out)
}

/// `floor(nd)` / `ceil(nd)` / `round(nd)` / `truncate(nd)` on an integer
/// receiver — the catalog leaf's decimal rounding: `nd >= 0` answers the
/// receiver itself, `nd < 0` rounds the magnitude to a multiple of
/// `10**-nd` (toward `-inf`, `+inf`, half-up, or `0` respectively).
/// Spelled on the decimal digits — an integer's rounding never needs the
/// chunk machinery.
fn int_decimal_round(
    a_neg: bool,
    a_mag: &[u64],
    method: &str,
    b_neg: bool,
    b_mag: &[u64],
) -> FoldOutcome {
    // `ndigits` is a C `int` — `5.round(2147483648)` and `round(-2147483649)`
    // raise `RangeError` on the oracle, which declines to the RBS `Integer`
    // answer. A `|nd|` past `i64` (a Bignum precision) is certainly past
    // `int`.
    let Ok(nd) = chunks_to_decimal(false, b_mag).parse::<i64>() else {
        return FoldOutcome::Declined;
    };
    let nd = if b_neg { -nd } else { nd };
    if !(i32::MIN as i64..=i32::MAX as i64).contains(&nd) {
        return FoldOutcome::Declined;
    }
    if nd >= 0 {
        // `nd >= 0` — every member answers the receiver.
        return big_scalar(chunks_to_decimal(a_neg, a_mag));
    }
    let mag_str = chunks_to_decimal(false, a_mag);
    // `k = -nd`, the dropped decimal places.
    let k = usize::try_from(-nd).unwrap_or(usize::MAX);
    if k > mag_str.len() {
        // The whole magnitude drops: the rounding sides answer `0`, and
        // `floor` on a negative / `ceil` on a positive keeps a spelled
        // `10**k` (`10**4096` is 13612 bits — past
        // `INTEGER_FOLD_BIT_LIMIT` — so a `k` past 4096 is the
        // reference's `WidenedResult`). `0.ceil(-k)` is `0`.
        return match method {
            "floor" if a_neg => int_round_ten_pow(k, true),
            "ceil" if !a_neg && !chunks_is_zero(a_mag) => int_round_ten_pow(k, false),
            _ => FoldOutcome::Value(Scalar::Int(0)),
        };
    }
    let keep = mag_str.len() - k;
    let (kept, dropped) = mag_str.split_at(keep);
    let bump = match method {
        // `round` — half up (Ruby's default `half: :up`): the dropped
        // head at exactly `10**(k-1)` is `>= 5` only when the dropped
        // span is `k` digits long and starts `5`-`9` (`5.round(-20)`
        // drops 1 digit past a 20-digit span — far under half).
        "round" => dropped.len() == k && dropped.as_bytes().first().is_some_and(|&c| c >= b'5'),
        // `floor` — toward `-inf`: only a NEGATIVE receiver rounds its
        // magnitude up, and only on a nonzero drop; `ceil` mirrors.
        "floor" => a_neg && dropped.bytes().any(|c| c != b'0'),
        "ceil" => !a_neg && dropped.bytes().any(|c| c != b'0'),
        // `truncate` — toward `0`, never bumps.
        _ => false,
    };
    // The kept prefix incremented by one, spelled in place — a carry
    // rollover gains a digit (`99.round(-1)` -> `100`, and an all-dropped
    // `5.round(-1)` spells `10`).
    let mut prefix: Vec<u8> = if bump {
        let mut p = kept.as_bytes().to_vec();
        let mut i = p.len();
        loop {
            if i == 0 {
                p.insert(0, b'1');
                break;
            }
            i -= 1;
            if p[i] < b'9' {
                p[i] += 1;
                break;
            }
            p[i] = b'0';
        }
        p
    } else {
        kept.as_bytes().to_vec()
    };
    if prefix.is_empty() {
        return FoldOutcome::Value(Scalar::Int(0));
    }
    prefix.resize(prefix.len() + k, b'0');
    let spelled = String::from_utf8(prefix).unwrap_or_default();
    if a_neg {
        big_scalar(format!("-{spelled}"))
    } else {
        big_scalar(spelled)
    }
}

/// `±10**k` — a whole-magnitude-dropped `floor`/`ceil` result. `10**4096`
/// is 13612 bits, past `INTEGER_FOLD_BIT_LIMIT`, so a `k` past 4096 is the
/// reference's `WidenedResult` without spelling; below it, spell and let
/// `big_scalar`'s oversized gate decide.
fn int_round_ten_pow(k: usize, negative: bool) -> FoldOutcome {
    if k > 4096 {
        return FoldOutcome::Widened("Integer");
    }
    let spelled = format!("{}1{}", if negative { "-" } else { "" }, "0".repeat(k));
    big_scalar(spelled)
}

/// `a.gcd(b)` — Euclid on magnitude chunks through [`chunks_mod`]; the
/// result is always non-negative (`gcd(a, 0) = |a|`).
fn chunks_gcd(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (mut x, mut y) = (a.to_vec(), b.to_vec());
    while !chunks_is_zero(&y) {
        let r = chunks_mod(&x, &y);
        x = y;
        y = r;
    }
    x
}

/// `a.lcm(b)` — `|a| / gcd * |b|`, `0` when either operand is `0` (Ruby's
/// `lcm` never goes negative). Spelled through [`big_scalar`]'s oversized
/// gate like every integer result.
fn int_lcm(a_mag: &[u64], b_mag: &[u64]) -> FoldOutcome {
    if chunks_is_zero(a_mag) || chunks_is_zero(b_mag) {
        return FoldOutcome::Value(Scalar::Int(0));
    }
    match int_lcm_inner(a_mag, b_mag, &chunks_gcd(a_mag, b_mag)) {
        Some(l) => big_scalar(chunks_to_decimal(false, &l)),
        None => FoldOutcome::Declined,
    }
}

/// The lcm magnitude chunks, or `None` when the spelled result would
/// spill `INTEGER_FOLD_BIT_LIMIT` — used by both `lcm` and `gcdlcm`, where
/// an oversized element declines the WHOLE pair (`lift_array_result`'s
/// per-element `foldable_constant_value?`).
fn int_lcm_inner(a_mag: &[u64], b_mag: &[u64], g: &[u64]) -> Option<Vec<u64>> {
    if chunks_is_zero(a_mag) || chunks_is_zero(b_mag) {
        return Some(Vec::new());
    }
    let (q, _) = chunks_divmod(a_mag, g);
    let l = chunks_mul(&q, b_mag);
    let spelled = chunks_to_decimal(false, &l);
    if decimal_oversized(&spelled) {
        None
    } else {
        Some(l)
    }
}

/// An unsigned magnitude as an Integer scalar — `Scalar::Int` when it
/// fits, `Scalar::BigInt` otherwise (never oversized: `gcd` is bounded by
/// an operand).
fn mag_scalar(mag: &[u64]) -> Scalar {
    match mag_scalar_opt(mag) {
        Some(s) => s,
        None => Scalar::BigInt(chunks_to_decimal(false, mag)),
    }
}

/// [`mag_scalar`] returning `None` when the spelling spills
/// `INTEGER_FOLD_BIT_LIMIT` — `lift_array_result`'s per-element gate.
fn mag_scalar_opt(mag: &[u64]) -> Option<Scalar> {
    spelled_scalar_opt(&chunks_to_decimal(false, mag))
}

/// A signed decimal spelling as an Integer scalar, `None` past
/// `INTEGER_FOLD_BIT_LIMIT` — the per-element `foldable_constant_value?`
/// gate of `lift_array_result` / `divmod_result_to_pair`.
fn spelled_scalar_opt(spelling: &str) -> Option<Scalar> {
    if let Ok(v) = spelling.parse::<i64>() {
        return Some(Scalar::Int(v));
    }
    if decimal_oversized(spelling) {
        return None;
    }
    Some(Scalar::BigInt(spelling.to_string()))
}

/// An integer scalar as `f64` — `rb_big2dbl` / C's `(double)v` rounding,
/// `None` for a non-numeric scalar.
fn scalar_to_f64(s: &Scalar) -> Option<f64> {
    match s {
        Scalar::Int(v) => Some(*v as f64),
        Scalar::BigInt(d) => d.parse::<f64>().ok(),
        Scalar::Float(f) => Some(*f),
        _ => None,
    }
}

/// `a <op> b` where `b` is a `Scalar::Float` — the `public_send` coercion
/// the reference performs (`rb_big2dbl` / `f` directly). The div family
/// coerces to `f64` (`big / 3.0` -> `3.33…e19`); the bit ops, gcd/lcm,
/// bit-test predicates, `digits` and the decimal rounders raise a
/// `TypeError` on the oracle and decline to the RBS answer
/// (`argument-type-mismatch` where it fires — `big & 3.0`).
fn int_binary_float(a: &Scalar, method: &str, f: f64) -> FoldOutcome {
    let a_f = scalar_to_f64(a);
    match method {
        "+" | "-" | "*" => int_arith(a, method, &Scalar::Float(f)),
        "/" | "fdiv" | "quo" => {
            let Some(a_f) = a_f else { return FoldOutcome::Declined };
            let r = a_f / f;
            if r.is_nan() {
                FoldOutcome::Declined
            } else {
                FoldOutcome::Value(Scalar::Float(r))
            }
        }
        "div" | "ceildiv" => {
            let Some(a_f) = a_f else { return FoldOutcome::Declined };
            let q = a_f / f;
            // `div(0.0)` raises ZeroDivisionError, `ceildiv(1e-300)`
            // FloatDomainError — non-finite declines like the oracle's
            // rescue, which the RBS `Integer` answer then stands.
            if !q.is_finite() {
                return FoldOutcome::Declined;
            }
            let q = if method == "div" { q.floor() } else { q.ceil() };
            big_scalar(format!("{q:.0}"))
        }
        "%" | "modulo" => {
            let Some(a_f) = a_f else { return FoldOutcome::Declined };
            // CRuby `flodivmod`: `fmod` then the divisor-sign adjustment —
            // a `-0.0` keeps its sign (`-big % 2.5` -> `-0.0`).
            let mut m = a_f % f;
            if m != 0.0 && a_f * f < 0.0 {
                m += f;
            }
            if m.is_nan() {
                FoldOutcome::Declined
            } else {
                FoldOutcome::Value(Scalar::Float(m))
            }
        }
        "remainder" => {
            let Some(a_f) = a_f else { return FoldOutcome::Declined };
            let r = a_f % f; // fmod — sign of the dividend.
            if r.is_nan() {
                FoldOutcome::Declined
            } else {
                FoldOutcome::Value(Scalar::Float(r))
            }
        }
        "divmod" => {
            let Some(a_f) = a_f else { return FoldOutcome::Declined };
            // `flodivmod` — `fmod`, the divisor-sign adjustment, then the
            // floor quotient spelled as an Integer (`big.divmod(3.0)` ->
            // `[33333333333333331968, 1.0]`).
            if f == 0.0 || !a_f.is_finite() {
                return FoldOutcome::Declined;
            }
            let mut m = a_f % f;
            if m != 0.0 && a_f * f < 0.0 {
                m += f;
            }
            if m.is_nan() {
                return FoldOutcome::Declined;
            }
            let div = ((a_f - m) / f).floor();
            if !div.is_finite() {
                return FoldOutcome::Declined;
            }
            let Some(qs) = spelled_scalar_opt(&format!("{div:.0}")) else {
                return FoldOutcome::Declined;
            };
            FoldOutcome::Tuple(vec![qs, Scalar::Float(m)])
        }
        "<=>" => {
            // Exact `Integer <=> Float` — `rb_big_cmp` compares the
            // infinite-precision value, so `9007199254740993 <=> 9007199254740992.0`
            // is `1` where an `f64` round-trip would say `0`.
            let Some((a_neg, a_mag)) = int_operand_chunks(a) else {
                return FoldOutcome::Declined;
            };
            match int_cmp_f64(a_neg, &chunks_to_limbs(&a_mag), f) {
                Some(std::cmp::Ordering::Less) => FoldOutcome::Value(Scalar::Int(-1)),
                Some(std::cmp::Ordering::Equal) => FoldOutcome::Value(Scalar::Int(0)),
                Some(std::cmp::Ordering::Greater) => FoldOutcome::Value(Scalar::Int(1)),
                None => FoldOutcome::Declined,
            }
        }
        "[]" => {
            // A Float index truncates (`big[1.9]` reads bit 1); a negative
            // one reads `0`.
            let Some((a_neg, a_mag)) = int_operand_chunks(a) else {
                return FoldOutcome::Declined;
            };
            if !(f.is_finite() && f >= 0.0) {
                return if f.is_finite() {
                    FoldOutcome::Value(Scalar::Int(0))
                } else {
                    FoldOutcome::Declined
                };
            }
            let i = f as u64;
            let a_limbs = chunks_to_limbs(&a_mag);
            let bit = if a_neg {
                1 - limb_bit(&limbs_dec(&a_limbs), i)
            } else {
                limb_bit(&a_limbs, i)
            };
            FoldOutcome::Value(Scalar::Int(i64::from(bit)))
        }
        "floor" | "ceil" | "round" | "truncate" => {
            // A Float `ndigits` coerces via `to_i` — `big.round(-5.0)` is
            // `big.round(-5)`, and an out-of-range Float saturates the way
            // `rb_to_int` does (`1e300` -> self, `-1e300` -> the
            // all-dropped arm decides `0` or the `10**k` Widened).
            let Some((a_neg, a_mag)) = int_operand_chunks(a) else {
                return FoldOutcome::Declined;
            };
            if !f.is_finite() {
                return FoldOutcome::Declined;
            }
            // `ndigits` is a C `int` — past `i32` the call raises
            // `RangeError` (`5.round(1e18)`), which declines to the RBS
            // `Integer` answer.
            let nd = f as i64;
            if !(i32::MIN as i64..=i32::MAX as i64).contains(&nd) {
                return FoldOutcome::Declined;
            }
            int_decimal_round(
                a_neg,
                &a_mag,
                method,
                nd < 0,
                &decimal_chunks(&nd.unsigned_abs().to_string()),
            )
        }
        _ => FoldOutcome::Declined,
    }
}

/// Exact `v <=> f` for an integer `v` of sign `a_neg` and u32-limb
/// magnitude `a_limbs` — the `rb_big_cmp` semantics `Integer#<=>` executes
/// on the oracle. `f` is `m * 2**e` exactly; past `2**53` a spelled f64
/// still loses to the bignum it rounds from (`99999999999999999999 <=>
/// 99999999999999999999.0` -> `-1`). `None` on NaN (unreachable — a NaN
/// literal never pins).
fn int_cmp_f64(a_neg: bool, a_limbs: &[u32], f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering::*;
    if f.is_nan() {
        return None;
    }
    let f_neg = f.is_sign_negative() && f != 0.0;
    let a_zero = limbs_is_zero(a_limbs);
    // Sign first — a zero reads positive on both sides (`-0.0 == 0.0`).
    match (a_neg && !a_zero, f_neg) {
        (true, false) => return Some(Less),
        (false, true) => return Some(Greater),
        _ => {}
    }
    if a_zero && f == 0.0 {
        return Some(Equal);
    }
    // Magnitude: `f = m * 2**e` (m <= 2**53, m odd unless f is a power of
    // two at the subnormal floor).
    let bits = f.abs().to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if raw_exp == 0 {
        (frac, -1074i64)
    } else {
        (frac | (1u64 << 52), raw_exp - 1075)
    };
    let mag_ord = if e >= 0 {
        // `f` is an integer `m << e` — compare magnitudes directly.
        let mut f_limbs = vec![m as u32, (m >> 32) as u32];
        while f_limbs.last() == Some(&0) {
            f_limbs.pop();
        }
        limbs_shl(&mut f_limbs, e as u64);
        limbs_cmp(a_limbs, &f_limbs)
    } else {
        // `f = m / 2**k`: compare `v` to `floor(f)`; an exact floor is
        // equal only when the shifted-out fraction is zero — past `k = 64`
        // the whole mantissa is fraction (`0 <=> 1e-320` is `-1`).
        let k = (-e) as u64;
        let floor = if k >= 64 { 0u64 } else { m >> k };
        let frac_nz = if k >= 64 { m != 0 } else { (m & ((1u64 << k) - 1)) != 0 };
        let floor_limbs = [floor as u32, (floor >> 32) as u32];
        match limbs_cmp(a_limbs, &floor_limbs) {
            Equal if frac_nz => Less,
            o => o,
        }
    };
    Some(if a_neg && !a_zero { mag_ord.reverse() } else { mag_ord })
}

// --- u32-limb bit ops -------------------------------------------------------

/// Little-endian u32 limbs for base-1e9 chunks — the two's-complement bit
/// view the `&`/`|`/`^`/`[]`/`bit_length` folds need.
fn chunks_to_limbs(chunks: &[u64]) -> Vec<u32> {
    const BASE: u64 = 1_000_000_000;
    let mut limbs: Vec<u32> = Vec::new();
    for &c in chunks.iter().rev() {
        // `limbs = limbs * 1e9 + c` — schoolbook over a u64 carry.
        let mut carry = c;
        for w in limbs.iter_mut() {
            let cur = u64::from(*w) * BASE + carry;
            *w = cur as u32;
            carry = cur >> 32;
        }
        while carry > 0 {
            limbs.push(carry as u32);
            carry >>= 32;
        }
    }
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    limbs
}

/// Little-endian base-1e9 chunks for u32 limbs — the way back from a bit
/// op (a result's width is bounded by its operands', so this never spells
/// more than the inputs).
fn limbs_to_chunks(limbs: &[u32]) -> Vec<u64> {
    const CHUNK: u128 = 1_000_000_000;
    let mut out: Vec<u64> = Vec::new();
    for &w in limbs.iter().rev() {
        // `out = out * 2**32 + w`.
        let mut carry = u128::from(w);
        for c in out.iter_mut() {
            let cur = u128::from(*c) * (1u128 << 32) + carry;
            *c = (cur % CHUNK) as u64;
            carry = cur / CHUNK;
        }
        while carry > 0 {
            out.push((carry % CHUNK) as u64);
            carry /= CHUNK;
        }
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

fn limbs_is_zero(a: &[u32]) -> bool {
    a.iter().all(|&w| w == 0)
}

fn limbs_cmp(a: &[u32], b: &[u32]) -> std::cmp::Ordering {
    fn len(a: &[u32]) -> usize {
        a.iter().rposition(|&w| w != 0).map_or(0, |p| p + 1)
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

fn limbs_eq_signed(a_neg: bool, a: &[u32], b_neg: bool, b: &[u32]) -> bool {
    a_neg == b_neg && limbs_cmp(a, b) == std::cmp::Ordering::Equal
}

/// `a << shift` in place on little-endian u32 limbs.
fn limbs_shl(a: &mut Vec<u32>, shift: u64) {
    if a.is_empty() || shift == 0 {
        return;
    }
    let words = (shift / 32) as usize;
    let bits = (shift % 32) as u32;
    if bits > 0 {
        let mut carry = 0u32;
        for w in a.iter_mut() {
            let cur = (*w << bits) | carry;
            carry = *w >> (32 - bits);
            *w = cur;
        }
        if carry > 0 {
            a.push(carry);
        }
    }
    if words > 0 {
        let mut shifted = vec![0u32; words];
        shifted.extend_from_slice(a);
        *a = shifted;
    }
}

/// `a - 1` on a nonzero u32-limb magnitude — the `v - 1` a negative
/// operand's two's-complement expansion reads (`-v` is `~(v-1)`).
fn limbs_dec(a: &[u32]) -> Vec<u32> {
    let mut out = a.to_vec();
    for w in out.iter_mut() {
        if *w > 0 {
            *w -= 1;
            break;
        }
        *w = u32::MAX;
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// `a + 1` on a u32-limb magnitude — closes `~(x)` back to `-x` (`~t` on
/// the computed limbs is `-(t + 1)`).
fn limbs_inc(a: &[u32]) -> Vec<u32> {
    let mut out = a.to_vec();
    let mut carry = 1u64;
    for w in out.iter_mut() {
        let cur = u64::from(*w) + carry;
        *w = cur as u32;
        carry = cur >> 32;
        if carry == 0 {
            break;
        }
    }
    if carry > 0 {
        out.push(carry as u32);
    }
    out
}

fn limbs_and(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = a.iter().zip(b).map(|(&x, &y)| x & y).collect();
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

fn limbs_or(a: &[u32], b: &[u32]) -> Vec<u32> {
    (0..a.len().max(b.len()))
        .map(|i| a.get(i).copied().unwrap_or(0) | b.get(i).copied().unwrap_or(0))
        .collect()
}

fn limbs_xor(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = (0..a.len().max(b.len()))
        .map(|i| a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0))
        .collect();
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// `a & ~b` — `a`'s bits where `b` is clear, bounded by `a`'s width (the
/// complement's infinite ones past `b`'s span keep `a`).
fn limbs_andnot(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = a
        .iter()
        .enumerate()
        .map(|(i, &x)| x & !b.get(i).copied().unwrap_or(0))
        .collect();
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

fn limb_bit(a: &[u32], i: u64) -> u32 {
    a.get((i / 32) as usize)
        .map(|w| (w >> (i % 32)) & 1)
        .unwrap_or(0)
}

/// `a <op> b` as two's-complement bit ops over sign + u32-limb magnitudes —
/// returns `(negative, magnitude)`. A negative operand `-v` expands as
/// `~(v-1)`, so each result reduces to positive limb ops plus one
/// complement step (rigor-rs#877):
///
/// - `&` — `-a & b` is `b & ~(a-1)` = `b &~ (a-1)`; `-a & -b` is
///   `~((a-1) | (b-1))` = `-(((a-1) | (b-1)) + 1)`.
/// - `|` — `-a | b` is `~((a-1) &~ b)` = `-(((a-1) &~ b) + 1)`; `-a | -b`
///   is `~((a-1) & (b-1))` = `-(((a-1) & (b-1)) + 1)`.
/// - `^` — `-a ^ b` is `~((a-1) ^ b)` = `-(((a-1) ^ b) + 1)`; `-a ^ -b`
///   is `(a-1) ^ (b-1)` (the complements cancel).
fn limbs_bitop(a_neg: bool, a: &[u32], b_neg: bool, b: &[u32], op: &str) -> (bool, Vec<u32>) {
    let at = if a_neg { limbs_dec(a) } else { a.to_vec() };
    let bt = if b_neg { limbs_dec(b) } else { b.to_vec() };
    match (op, a_neg, b_neg) {
        ("&", false, false) => (false, limbs_and(a, b)),
        ("&", true, false) => (false, limbs_andnot(b, &at)),
        ("&", false, true) => (false, limbs_andnot(a, &bt)),
        ("&", true, true) => (true, limbs_inc(&limbs_or(&at, &bt))),
        ("|", false, false) => (false, limbs_or(a, b)),
        ("|", true, false) => (true, limbs_inc(&limbs_andnot(&at, b))),
        ("|", false, true) => (true, limbs_inc(&limbs_andnot(&bt, a))),
        ("|", true, true) => (true, limbs_inc(&limbs_and(&at, &bt))),
        ("^", false, false) => (false, limbs_xor(a, b)),
        ("^", true, false) => (true, limbs_inc(&limbs_xor(&at, b))),
        ("^", false, true) => (true, limbs_inc(&limbs_xor(a, &bt))),
        ("^", true, true) => (false, limbs_xor(&at, &bt)),
        _ => unreachable!("limbs_bitop on {op}"),
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
    // A NaN result declines (`foldable_constant_value?`), like `**` above:
    // the reference never holds a `Constant[NaN]` (`NaN == NaN` is `false`,
    // so it would corrupt type equality) — `1e400 - 1e400` and
    // `Float::INFINITY - Float::INFINITY` type `Float` there, not `NaN`.
    match res {
        Scalar::Float(f) if f.is_nan() => None,
        _ => Some(res),
    }
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
    is_str_lookup(recv, method)
        || matches!(
            (recv, method),
            (Scalar::Float(_) | Scalar::Int(_) | Scalar::BigInt(_), "<=>")
        )
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
    is_str_lookup(recv, method)
        || matches!(
            (recv, method),
            (Scalar::Float(_) | Scalar::Int(_) | Scalar::BigInt(_), "<=>")
        )
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
        // Cross-kind `==` declines (the RBS answer).
        assert_eq!(fold(&n, "==", &[Scalar::Str("x".into())]), None);
    }

    /// rigor-rs#695 — `+`/`-`/`*` on a `Scalar::BigInt` receiver, or with a
    /// BigInt operand, fold exactly like the reference's arbitrary-precision
    /// `public_send`. `/`, `%` and the bit ops still decline.
    #[test]
    fn folds_bigint_arithmetic() {
        let i = Scalar::Int;
        let big = |d: &str| Scalar::BigInt(d.to_string());
        let n = big("99999999999999999999");
        let neg = big("-99999999999999999999");
        // Integer operand, either direction.
        assert_eq!(fold(&n, "+", &[i(1)]), Some(big("100000000000000000000")));
        assert_eq!(fold(&n, "-", &[i(3)]), Some(big("99999999999999999996")));
        assert_eq!(fold(&n, "*", &[i(3)]), Some(big("299999999999999999997")));
        assert_eq!(fold(&i(1), "+", &[n.clone()]), Some(big("100000000000000000000")));
        assert_eq!(fold(&i(5), "-", &[n.clone()]), Some(big("-99999999999999999994")));
        assert_eq!(fold(&i(3), "*", &[n.clone()]), Some(big("299999999999999999997")));
        // BigInt operand both ways, all sign combinations.
        assert_eq!(fold(&n, "+", &[n.clone()]), Some(big("199999999999999999998")));
        assert_eq!(fold(&n, "-", &[big("99999999999999999998")]), Some(i(1)));
        assert_eq!(fold(&n, "-", &[n.clone()]), Some(i(0)));
        assert_eq!(fold(&neg, "+", &[i(1)]), Some(big("-99999999999999999998")));
        assert_eq!(fold(&neg, "-", &[i(1)]), Some(big("-100000000000000000000")));
        assert_eq!(fold(&neg, "*", &[i(-2)]), Some(big("199999999999999999998")));
        assert_eq!(fold(&neg, "*", &[i(2)]), Some(big("-199999999999999999998")));
        assert_eq!(fold(&neg, "+", &[n.clone()]), Some(i(0)));
        // Zero factors and results demote to `Scalar::Int`.
        assert_eq!(fold(&n, "*", &[i(0)]), Some(i(0)));
        assert_eq!(fold(&i(0), "*", &[n.clone()]), Some(i(0)));
        // A Float operand coerces through `rb_big2dbl` / `(double)v`.
        assert_eq!(fold(&n, "+", &[Scalar::Float(1.5)]), Some(Scalar::Float(1e20)));
        assert_eq!(fold(&i(1), "+", &[Scalar::Float(1.5)]), Some(Scalar::Float(2.5)));
        // `inf * -0.0` is a Ruby NaN — declined (never a `Constant`).
        let inf_recv = big(&"9".repeat(400));
        assert_eq!(fold(&inf_recv, "*", &[Scalar::Float(-0.0)]), None);
        // `/`, `%`, and the bit ops now fold at arbitrary width
        // (rigor-rs#877); a non-numeric operand still declines.
        assert_eq!(fold(&n, "/", &[i(3)]), Some(big("33333333333333333333")));
        assert_eq!(fold(&n, "%", &[i(3)]), Some(i(0)));
        assert_eq!(fold(&n, "&", &[i(3)]), Some(i(3)));
        assert_eq!(fold(&n, "+", &[Scalar::Str("x".into())]), None);
    }

    /// `big * big` (and `int * big`) past `INTEGER_FOLD_BIT_LIMIT` answers
    /// the reference's `WidenedResult` without spelling the product
    /// (rigor-rs#695).
    #[test]
    fn widens_bigint_products_past_the_bit_limit() {
        let i = Scalar::Int;
        let big = |d: &str| Scalar::BigInt(d.to_string());
        let wide = big(&"9".repeat(5000));
        assert_eq!(fold_outcome(&wide, "*", &[i(9)]), FoldOutcome::Widened("Integer"));
        assert_eq!(fold(&wide, "*", &[i(9)]), None);
        let half = big(&"9".repeat(2500));
        assert_eq!(
            fold_outcome(&half, "*", &[half.clone()]),
            FoldOutcome::Widened("Integer")
        );
        // `+`/`-` on an oversized receiver still compute then gate the
        // same way (`big_scalar` — fold() sees the Widened as None).
        assert_eq!(fold(&wide, "+", &[i(1)]), None);
        assert_eq!(fold(&wide, "-", &[wide.clone()]), Some(Scalar::Int(0)));
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

    /// The shared integer binary catalogue (rigor-rs#877): the `/`/`%`
    /// sign rules, `ceildiv`/`remainder`/`fdiv`, `<=>` (including the exact
    /// Bignum-vs-f64 compare), bit ops as two's-complement limb work,
    /// `[]`, the bit-test predicates, `gcd`/`lcm`, `divmod`/`gcdlcm`/
    /// `digits` as `FoldOutcome::Tuple`, and the decimal rounders — over
    /// the sign + base-1e9 operand layer, shared by `Scalar::Int` and
    /// `Scalar::BigInt` receivers. Every expected spelling was probed
    /// against the oracle.
    #[test]
    fn folds_the_integer_binary_catalogue() {
        let i = Scalar::Int;
        let big = |d: &str| Scalar::BigInt(d.to_string());
        let n = big("99999999999999999999");
        let neg = big("-99999999999999999999");
        let m = big("888888888888888888888888");
        // `/`/`div` floor, `ceildiv` toward +inf, `remainder` truncated,
        // `%`/`modulo` in the divisor's sign.
        assert_eq!(fold(&n, "/", &[i(3)]), Some(big("33333333333333333333")));
        // `-big / 4` floors a -2.5e19 quotient one further negative;
        // `1e20` happens to be divisible by 3 (digit sum 180).
        assert_eq!(fold(&neg, "/", &[i(4)]), Some(big("-25000000000000000000")));
        assert_eq!(fold(&neg, "/", &[i(-3)]), Some(big("33333333333333333333")));
        assert_eq!(fold(&n, "div", &[i(4)]), Some(big("24999999999999999999")));
        assert_eq!(fold(&n, "ceildiv", &[i(4)]), Some(big("25000000000000000000")));
        assert_eq!(fold(&neg, "ceildiv", &[i(4)]), Some(big("-24999999999999999999")));
        assert_eq!(fold(&n, "%", &[i(3)]), Some(i(0)));
        assert_eq!(fold(&neg, "%", &[i(4)]), Some(i(1)));
        assert_eq!(fold(&n, "%", &[i(-4)]), Some(i(-1)));
        assert_eq!(fold(&n, "remainder", &[i(4)]), Some(i(3)));
        assert_eq!(fold(&neg, "remainder", &[i(4)]), Some(i(-3)));
        // A zero divisor declines (the RBS tier stands + the raise).
        for m_ in ["/", "div", "%", "modulo", "ceildiv", "remainder", "divmod"] {
            assert_eq!(fold(&n, m_, &[i(0)]), None, "{m_} by zero");
        }
        // `divmod`/`gcdlcm`/`digits` are Tuple outcomes.
        assert_eq!(
            fold_outcome(&n, "divmod", &[i(3)]),
            FoldOutcome::Tuple(vec![big("33333333333333333333"), i(0)])
        );
        assert_eq!(
            fold_outcome(&neg, "divmod", &[i(4)]),
            FoldOutcome::Tuple(vec![big("-25000000000000000000"), i(1)])
        );
        assert_eq!(
            fold_outcome(&n, "gcdlcm", &[i(4)]),
            FoldOutcome::Tuple(vec![i(1), big("399999999999999999996")])
        );
        assert_eq!(
            fold_outcome(&i(5), "digits", &[]),
            FoldOutcome::Tuple(vec![i(5)])
        );
        assert_eq!(
            fold_outcome(&i(5), "digits", &[i(3)]),
            FoldOutcome::Tuple(vec![i(2), i(1)])
        );
        // `digits` declines on a bad base, a negative receiver, or a
        // >32-element expansion (`STRING_ARRAY_LIFT_LIMIT`).
        assert_eq!(fold(&n, "digits", &[i(0)]), None);
        assert_eq!(fold(&n, "digits", &[i(1)]), None);
        assert_eq!(fold(&neg, "digits", &[]), None);
        assert_eq!(fold(&n, "digits", &[i(2)]), None);
        // `fdiv` / `quo` / `/` on a Float operand coerce to `f64`;
        // `quo` on an Integer operand is `Unrepresentable` (Rational).
        assert_eq!(fold(&n, "fdiv", &[i(4)]), Some(Scalar::Float(2.5e19)));
        assert_eq!(fold(&n, "/", &[Scalar::Float(3.0)]), Some(Scalar::Float(1e20 / 3.0)));
        assert_eq!(fold(&n, "quo", &[Scalar::Float(4.0)]), Some(Scalar::Float(2.5e19)));
        assert_eq!(fold_outcome(&n, "quo", &[i(4)]), FoldOutcome::Unrepresentable);
        assert_eq!(fold(&n, "quo", &[i(0)]), None); // raises -> RBS
        // `<=>` — int/int, exact int/f64 (no `rb_big2dbl` loss on either
        // side), and `nil` on a non-numeric.
        assert_eq!(fold(&n, "<=>", &[i(3)]), Some(i(1)));
        assert_eq!(fold(&i(5), "<=>", &[i(3)]), Some(i(1)));
        assert_eq!(fold(&i(5), "<=>", &[i(5)]), Some(i(0)));
        assert_eq!(fold(&i(5), "<=>", &[i(6)]), Some(i(-1)));
        assert_eq!(
            fold(&i(9007199254740993), "<=>", &[Scalar::Float(9007199254740992.0)]),
            Some(i(1))
        );
        assert_eq!(
            fold(&n, "<=>", &[Scalar::Float(99999999999999999999.0)]),
            Some(i(-1))
        );
        assert_eq!(fold(&n, "<=>", &[Scalar::Str("a".into())]), Some(Scalar::Nil));
        // Bit ops — two's-complement infinite width (spellings probed).
        assert_eq!(fold(&n, "&", &[m.clone()]), Some(i(4846537325548899896)));
        assert_eq!(fold(&n, "^", &[m]), Some(big("888979195814237791089095")));
        assert_eq!(fold(&neg, "&", &[i(3)]), Some(i(1)));
        assert_eq!(fold(&neg, "|", &[i(3)]), Some(big("-99999999999999999997")));
        assert_eq!(fold(&n, "^", &[i(-1)]), Some(big("-100000000000000000000")));
        assert_eq!(fold(&n, "~", &[]), Some(big("-100000000000000000000")));
        assert_eq!(fold(&neg, "~", &[]), Some(big("99999999999999999998")));
        // `[]` — bit reference, negative index `0`, sign bit past the
        // magnitude, Float index truncates.
        assert_eq!(fold(&n, "[]", &[i(5)]), Some(i(1)));
        assert_eq!(fold(&neg, "[]", &[i(5)]), Some(i(0)));
        assert_eq!(fold(&n, "[]", &[i(-3)]), Some(i(0)));
        assert_eq!(fold(&i(5), "[]", &[i(100)]), Some(i(0)));
        assert_eq!(fold(&i(-5), "[]", &[i(100)]), Some(i(1)));
        assert_eq!(fold(&n, "[]", &[Scalar::Float(1.9)]), Some(i(1)));
        // Bit-test predicates.
        assert_eq!(fold(&n, "allbits?", &[i(3)]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, "anybits?", &[i(3)]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, "nobits?", &[i(3)]), Some(Scalar::Bool(false)));
        assert_eq!(fold(&i(5), "allbits?", &[i(3)]), Some(Scalar::Bool(false)));
        // gcd / lcm are non-negative magnitudes; `gcd(a, 0) = |a|`.
        assert_eq!(fold(&n, "gcd", &[big("888888888888888888888888")]), Some(i(3333)));
        assert_eq!(fold(&n, "gcd", &[i(0)]), Some(big("99999999999999999999")));
        assert_eq!(fold(&n, "lcm", &[i(6)]), Some(big("199999999999999999998")));
        assert_eq!(fold(&i(0), "lcm", &[i(5)]), Some(i(0)));
        // Decimal rounders — `nd >= 0` answers the receiver; `nd < 0`
        // rounds toward -inf / +inf / half-up / 0.
        assert_eq!(fold(&n, "round", &[i(-5)]), Some(big("100000000000000000000")));
        assert_eq!(fold(&n, "round", &[i(2)]), Some(big("99999999999999999999")));
        assert_eq!(fold(&neg, "floor", &[i(-5)]), Some(big("-100000000000000000000")));
        assert_eq!(fold(&n, "floor", &[i(-5)]), Some(big("99999999999999900000")));
        assert_eq!(fold(&neg, "ceil", &[i(-5)]), Some(big("-99999999999999900000")));
        assert_eq!(fold(&n, "truncate", &[i(-5)]), Some(big("99999999999999900000")));
        assert_eq!(fold(&i(5), "round", &[i(-1)]), Some(i(10)));
        assert_eq!(fold(&i(99), "round", &[i(-1)]), Some(i(100)));
        assert_eq!(fold(&i(5), "round", &[i(-20)]), Some(i(0)));
        assert_eq!(fold(&i(-25), "round", &[i(-1)]), Some(i(-30)));
        // Shared on an i64 receiver (the issue's deferred unary family
        // stays declined — `~`, `to_i`, `bit_length`, `next`, `magnitude`,
        // `nonzero?`, `finite?`, `infinite?`, `inspect`, `abs2`).
        assert_eq!(fold(&i(5), "ceildiv", &[i(3)]), Some(i(2)));
        assert_eq!(fold(&i(5), "remainder", &[i(3)]), Some(i(2)));
        assert_eq!(fold(&i(5), "fdiv", &[i(2)]), Some(Scalar::Float(2.5)));
        assert_eq!(
            fold_outcome(&i(5), "divmod", &[i(3)]),
            FoldOutcome::Tuple(vec![i(1), i(2)])
        );
        assert_eq!(fold(&i(5), "gcd", &[i(0)]), Some(i(5)));
        assert_eq!(fold(&i(5), "lcm", &[i(3)]), Some(i(15)));
        assert_eq!(fold(&i(5), "to_i", &[]), None);
        assert_eq!(fold(&i(5), "~", &[]), None);
        assert_eq!(fold(&i(5), "nonzero?", &[]), None);
        // BigInt unary family — self-returning members re-gate through
        // `big_scalar`; the rest spell themselves.
        assert_eq!(fold(&n, "to_i", &[]), Some(big("99999999999999999999")));
        assert_eq!(fold(&n, "floor", &[]), Some(big("99999999999999999999")));
        assert_eq!(fold(&n, "nonzero?", &[]), Some(big("99999999999999999999")));
        assert_eq!(fold(&n, "next", &[]), Some(big("100000000000000000000")));
        assert_eq!(fold(&neg, "magnitude", &[]), Some(big("99999999999999999999")));
        assert_eq!(fold(&n, "finite?", &[]), Some(Scalar::Bool(true)));
        assert_eq!(fold(&n, "infinite?", &[]), Some(Scalar::Nil));
        assert_eq!(
            fold(&n, "inspect", &[]),
            Some(Scalar::Str("99999999999999999999".into()))
        );
        assert_eq!(fold(&n, "bit_length", &[]), Some(i(67)));
        // `-(A)` counts `(A-1)`'s bits — same 67 here (a power-of-two
        // magnitude is where it shrinks).
        assert_eq!(fold(&neg, "bit_length", &[]), Some(i(67)));
        assert_eq!(fold(&n, "to_f", &[]), Some(Scalar::Float(1e20)));
        assert_eq!(
            fold(&n, "abs2", &[]),
            Some(big("9999999999999999999800000000000000000001"))
        );
        // `to_r`/`to_c`/`rationalize` pin a Rational / Complex constant the
        // port's `Scalar` cannot hold — `Unrepresentable` (untyped), never
        // the `for Rational` FP (rigor-rs#877).
        assert_eq!(fold_outcome(&n, "to_r", &[]), FoldOutcome::Unrepresentable);
        assert_eq!(fold_outcome(&n, "to_c", &[]), FoldOutcome::Unrepresentable);
        assert_eq!(fold_outcome(&i(5), "to_r", &[]), FoldOutcome::Unrepresentable);
        assert_eq!(fold_outcome(&i(5), "rationalize", &[]), FoldOutcome::Unrepresentable);
        // `Float#to_c` is NOT `INTEGER_UNARY`-folded on the oracle — the
        // `Complex` RBS answer stands.
        assert_ne!(
            fold_outcome(&Scalar::Float(1.5), "to_c", &[]),
            FoldOutcome::Unrepresentable
        );
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

    /// `foldable_constant_value?` — a NaN arithmetic RESULT declines; the
    /// reference never holds a `Constant[NaN]` (`NaN == NaN` is `false`,
    /// so it would corrupt type equality). `1e400 - 1e400` minted
    /// `Constant[NaN]` on master until rigor-rs#656's fold made the shape
    /// common; it now declines like `**` already did.
    #[test]
    fn float_arithmetic_declines_nan_results() {
        let f = Scalar::Float;
        assert_eq!(fold(&f(f64::INFINITY), "-", &[f(f64::INFINITY)]), None);
        assert_eq!(
            fold(&f(f64::INFINITY), "+", &[f(f64::NEG_INFINITY)]),
            None
        );
        assert_eq!(fold(&f(f64::INFINITY), "*", &[f(0.0)]), None);
        // Finite and infinite non-NaN results still fold.
        assert_eq!(
            fold(&f(f64::INFINITY), "+", &[f(f64::INFINITY)]),
            Some(f(f64::INFINITY))
        );
        assert_eq!(
            fold(&f(f64::INFINITY), "*", &[f(-1.0)]),
            Some(f(f64::NEG_INFINITY))
        );
        assert_eq!(fold(&f(1.5), "-", &[f(0.5)]), Some(f(1.0)));
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

    /// rigor-rs#837 — the chunk-powmod [`powmod_work`] estimate is bounded
    /// at [`POWMOD_WORK_LIMIT`] (~0.3s/fold measured); an estimate past it
    /// declines before iterating, so these rows answer instantly.
    #[test]
    fn pow_mod_declines_past_work_limit() {
        let i = Scalar::Int;
        let big = |d: &str| Scalar::BigInt(d.to_string());
        // The issue's pathological shape: 4000-digit exp (~13.4k est bits) x
        // 1261-digit mod (141 words) ≈ 265M work — declined.
        let huge_exp = big(&"9".repeat(4000));
        let huge_mod = big(&format!("1{}7", "0".repeat(1259)));
        assert_eq!(fold(&i(3), "pow", &[huge_exp, huge_mod]), None);
        // A row between the old (`1 << 28`) and new limits: 4000-digit exp
        // x 361-digit mod (41 words) ≈ 22M work — folded before, declines now.
        let wide_mod = big(&format!("9{}7", "9".repeat(359)));
        assert_eq!(fold(&i(3), "pow", &[big(&"9".repeat(4000)), wide_mod]), None);
        // Control: an RSA-2048-class fold (~2070 est bits x 69 words ≈ 9.9M)
        // still folds.
        let rsa_exp = big(&"9".repeat(617));
        let rsa_mod = big(&format!("9{}7", "9".repeat(616)));
        assert!(fold(&i(2), "pow", &[rsa_exp, rsa_mod]).is_some());
    }

    /// rigor-rs#850 — the estimate now charges the `ExpBits` chunk-shift pass
    /// (`exp_bits * shift_words`), and the u128 path gates a `PowExp::Big`
    /// exponent at [`POWMOD_U128_WORK_LIMIT`]: both ran uncounted before.
    #[test]
    fn pow_mod_gates_exp_shift_work() {
        let i = Scalar::Int;
        let big = |d: &str| Scalar::BigInt(d.to_string());
        // u128 path: a 60k-digit exponent (~6.7k chunks ≈ 1.34G est word-ops)
        // declines where it once ran ~2s; a 6k-digit one (~13.5M) folds in ms.
        assert_eq!(fold(&i(2), "pow", &[big(&"9".repeat(60_000)), i(97)]), None);
        assert!(fold(&i(2), "pow", &[big(&"9".repeat(6_000)), i(97)]).is_some());
        // Chunk path, a 3-word modulus: the modmul term (~30*2000*9 = 540k)
        // passed the estimate alone, but the shift term (~30*2000*2000 =
        // 120M) pushes an 18k-digit exponent past the limit.
        let narrow_mod = big("9999999999999999999");
        assert_eq!(
            fold(&i(2), "pow", &[big(&"9".repeat(18_000)), narrow_mod.clone()]),
            None
        );
        // Control: same modulus, a 1.8k-digit exponent still folds
        // (~30*200*(9+200) ≈ 1.25M work).
        assert!(
            fold(&i(2), "pow", &[big(&"9".repeat(1_800)), narrow_mod]).is_some()
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

