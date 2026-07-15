//! Fold-time evaluators for the receiverless `Kernel` conversion functions
//! (`format`/`sprintf`, `String()`, `Integer()`, `Float()`), ported from the
//! reference `KernelDispatch` (`kernel_dispatch.rb`).
//!
//! The reference reaches its precision by executing the *real* Ruby
//! (`Integer(*values)`, `String(v)`, `format(...)`) at fold time. rigor-rs is a
//! Ruby-free binary, so these are hand-ported evaluators. Their hard contract
//! is **byte-exact agreement with Ruby or nothing**: each returns `Some(result)`
//! only when the folded value is provably what Ruby would compute AND renders
//! byte-identically in a downstream `call.undefined-method` message; on any
//! doubt — an unsupported directive, an argument-count/-type mismatch, a case
//! Ruby would `raise`, a result Rust cannot spell like Ruby, or an output that
//! blows the size cap — they return `None` and the caller declines the fold
//! (silent: a coverage gap, never a false positive).
//!
//! ## Panic / OOM safety
//!
//! [`sprintf`] is a fold-time interpreter over attacker-shaped templates
//! (`%099999999d` demands gigabytes; a malformed directive must not panic). It
//! never indexes unchecked, parses every width/precision as a bounded `usize`
//! and declines BEFORE allocating when either exceeds [`STRING_FOLD_BYTE_LIMIT`],
//! and re-checks the running output length after every segment so an
//! accumulation of moderate widths cannot exceed the cap either.

use rigor_types::{ruby_float_to_s, Scalar};

/// A folded string result larger than this declines to the reference's
/// literal-string lift (reference `STRING_FOLD_BYTE_LIMIT`). Also the hard cap
/// on any single `sprintf` width/precision so a malformed template cannot
/// demand a giant allocation.
pub const STRING_FOLD_BYTE_LIMIT: usize = 4096;

// ---------------------------------------------------------------------------
// Kernel#String(v)
// ---------------------------------------------------------------------------

/// Ruby `Kernel#String(v)` / `v.to_s` for a value-pinned scalar. Every rigor-rs
/// `Scalar` kind is a literal-carrier whose `to_s` is a deterministic pure
/// function, so this is total over the scalar set (the reference restricts to
/// `STRING_SAFE_CLASSES`; rigor-rs's scalar set is a subset of it). Floats reuse
/// [`ruby_float_to_s`] so `String(3.0) == "3.0"`.
pub fn ruby_string_of(s: &Scalar) -> String {
    match s {
        Scalar::Int(n) => n.to_string(),
        Scalar::Str(v) => v.clone(),
        Scalar::Sym(v) => v.clone(),
        Scalar::Bool(b) => b.to_string(),
        Scalar::Nil => String::new(),
        Scalar::Float(f) => ruby_float_to_s(*f),
    }
}

// ---------------------------------------------------------------------------
// Kernel#Integer / Kernel#Float
// ---------------------------------------------------------------------------

/// Ruby `Kernel#Integer(arg)` / `Integer(str, base)`. `base` is `Some` only for
/// the two-arg form. Returns `Some(i)` only when Ruby parses `arg` to exactly
/// `i` (representable in `i64`); `None` for any case Ruby would `raise`, a value
/// outside `i64` (Ruby yields a Bignum rigor-rs cannot pin), or a shape we do
/// not model.
pub fn ruby_integer(arg: &Scalar, base: Option<i64>) -> Option<i64> {
    match arg {
        // `Integer(int)` — identity; `Integer(int, base)` raises (TypeError:
        // base only valid with a String) ⇒ decline.
        Scalar::Int(n) => match base {
            None => Some(*n),
            Some(_) => None,
        },
        // `Integer(float)` truncates toward zero; a base against a non-String
        // raises. Only fold a finite float whose truncation fits `i64`.
        Scalar::Float(f) => {
            if base.is_some() {
                return None;
            }
            if !f.is_finite() {
                return None;
            }
            let t = f.trunc();
            if t >= -(2f64.powi(63)) && t < 2f64.powi(63) {
                Some(t as i64)
            } else {
                None
            }
        }
        Scalar::Str(s) => parse_ruby_integer_str(s, base),
        // Integer(nil) raises TypeError; Integer(:sym)/Integer(true) raise.
        _ => None,
    }
}

/// Ruby `Kernel#Float(arg)`. Returns `Some(f)` only for a numeric scalar or a
/// string Ruby's `Float()` accepts; `None` on any raise or an unmodeled shape.
pub fn ruby_float(arg: &Scalar) -> Option<f64> {
    match arg {
        Scalar::Int(n) => Some(*n as f64),
        Scalar::Float(f) => Some(*f),
        Scalar::Str(s) => parse_ruby_float_str(s),
        _ => None,
    }
}

/// Parse a string exactly as Ruby's `Integer(str, base)` does. Grammar:
/// optional surrounding ASCII whitespace, an optional sign, an optional radix
/// prefix (`0x`/`0b`/`0o`/`0d`/`0`), digits for the effective base with `_`
/// permitted strictly *between* digits. A base argument must be consistent with
/// any prefix. Anything else declines (Ruby raises `ArgumentError`).
fn parse_ruby_integer_str(input: &str, base: Option<i64>) -> Option<i64> {
    // Only ASCII is in play; a non-ASCII byte is never valid here.
    if !input.is_ascii() {
        return None;
    }
    let s = input.trim_matches(|c: char| c.is_ascii_whitespace());
    let mut rest = s;

    // Sign.
    let mut neg = false;
    if let Some(r) = rest.strip_prefix('+') {
        rest = r;
    } else if let Some(r) = rest.strip_prefix('-') {
        neg = true;
        rest = r;
    }

    // Radix prefix. `prefix_base` is the base implied by a `0x`/`0b`/`0o`/`0d`
    // prefix, or `Some(8)` for a bare leading `0` (Ruby's C-style octal), else
    // `None`.
    let (prefix_base, after_prefix): (Option<i64>, &str) =
        if let Some(r) = strip_ci_prefix(rest, "0x") {
            (Some(16), r)
        } else if let Some(r) = strip_ci_prefix(rest, "0b") {
            (Some(2), r)
        } else if let Some(r) = strip_ci_prefix(rest, "0o") {
            (Some(8), r)
        } else if let Some(r) = strip_ci_prefix(rest, "0d") {
            (Some(10), r)
        } else if rest.len() > 1 && rest.starts_with('0') {
            // Leading `0` (not the literal `"0"`): C-style octal, but the `0`
            // is NOT consumed as a prefix — it is a leading zero digit of an
            // octal number, so keep it in the digit stream under base 8.
            (Some(8), rest)
        } else {
            (None, rest)
        };

    // Reconcile with the explicit base argument.
    let effective_base: i64 = match (base, prefix_base) {
        (None, None) => 10,
        (None, Some(b)) => b,
        (Some(b), None) => b,
        (Some(b), Some(pb)) => {
            // A prefix with an explicit base is accepted only if they agree.
            // (Ruby also accepts base 0 = "infer from prefix", but rigor-rs
            // only ever passes a concrete literal base, so mismatch declines.)
            if b == pb {
                b
            } else {
                return None;
            }
        }
    };
    if !(2..=36).contains(&effective_base) {
        return None;
    }

    let digits = after_prefix;
    if digits.is_empty() {
        return None;
    }

    // Digit stream with `_` allowed strictly between two digits.
    let mut acc: i128 = 0;
    let mut prev_was_digit = false;
    let mut saw_digit = false;
    let bytes = digits.as_bytes();
    for (idx, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            // Not leading, not trailing, not doubled.
            if !prev_was_digit || idx + 1 >= bytes.len() {
                return None;
            }
            prev_was_digit = false;
            continue;
        }
        let d = (b as char).to_digit(36)? as i64;
        if d >= effective_base {
            return None;
        }
        acc = acc.checked_mul(effective_base as i128)?.checked_add(d as i128)?;
        if acc > (i64::MAX as i128) + 1 {
            // Beyond what a signed 64-bit result can hold either way; decline
            // (Ruby would produce a Bignum).
            return None;
        }
        prev_was_digit = true;
        saw_digit = true;
    }
    if !saw_digit {
        return None;
    }
    let signed: i128 = if neg { -acc } else { acc };
    if signed < i64::MIN as i128 || signed > i64::MAX as i128 {
        return None;
    }
    Some(signed as i64)
}

/// Case-insensitive `strip_prefix` for the two-char radix markers.
fn strip_ci_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let pb = prefix.as_bytes();
    let sb = s.as_bytes();
    if sb.len() >= pb.len()
        && sb[0].eq_ignore_ascii_case(&pb[0])
        && sb[1].eq_ignore_ascii_case(&pb[1])
    {
        Some(&s[pb.len()..])
    } else {
        None
    }
}

/// Parse a string exactly as Ruby's `Float(str)` does. Ruby accepts optional
/// surrounding whitespace, a decimal float grammar with an optional exponent,
/// hexadecimal floats (`0x1p4`), and `_` between digits. rigor-rs folds the
/// cases it can prove Rust's `f64` parser matches byte-for-byte after
/// normalising `_`; hex-floats and any residual are declined.
fn parse_ruby_float_str(input: &str) -> Option<f64> {
    if !input.is_ascii() {
        return None;
    }
    let s = input.trim_matches(|c: char| c.is_ascii_whitespace());
    if s.is_empty() {
        return None;
    }
    // Hex-float (`0x1p4`) — Rust's `str::parse::<f64>` does NOT accept this
    // grammar, and reimplementing it byte-exactly is not worth the risk, so
    // decline (a gap, never an FP).
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    if unsigned.len() >= 2 && (unsigned.starts_with("0x") || unsigned.starts_with("0X")) {
        return None;
    }

    // Remove `_` but only where it sits strictly between two digits, else the
    // string is invalid (Ruby raises).
    let cleaned = strip_underscores_between_digits(s)?;

    // Ruby's `Float()` grammar is a strict decimal float. Rust's `f64` parser
    // is a superset that also accepts `inf`/`nan`/`infinity`; guard those out so
    // we never fold a token Ruby's `Float()` would reject.
    let lower = cleaned.to_ascii_lowercase();
    if lower.contains("inf") || lower.contains("nan") {
        return None;
    }
    // Require at least one decimal digit somewhere (rejects `"."`, `"e5"`, ...).
    if !cleaned.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    match cleaned.parse::<f64>() {
        Ok(f) if f.is_finite() => Some(f),
        _ => None,
    }
}

/// Return `input` with `_` removed, but only when every `_` sits strictly
/// between two ASCII digits; otherwise `None` (Ruby rejects a leading / trailing
/// / doubled / non-digit-adjacent underscore).
fn strip_underscores_between_digits(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    for (idx, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            let prev_digit = idx > 0 && bytes[idx - 1].is_ascii_digit();
            let next_digit = idx + 1 < bytes.len() && bytes[idx + 1].is_ascii_digit();
            if !prev_digit || !next_digit {
                return None;
            }
            continue;
        }
        out.push(b as char);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Kernel#format / Kernel#sprintf
// ---------------------------------------------------------------------------

/// Flags parsed from a single `%` directive.
#[derive(Default, Clone, Copy)]
struct Flags {
    minus: bool,
    zero: bool,
    plus: bool,
    space: bool,
    hash: bool,
}

/// Run Ruby `format`/`sprintf` over a constant template and value-pinned
/// argument scalars. Returns `Some(rendered)` only for the verified directive
/// subset with an exactly-matching argument count and an output within the size
/// cap; `None` on any unsupported/malformed directive, arg-count or arg-type
/// mismatch (both cases Ruby `raise`s), or an oversized result.
///
/// Supported conversions: `%%`, `d`/`i`/`u` (integer, also a finite float
/// truncated), `x`/`X`/`o`/`b`/`B` (non-negative integer radix), `c` (character
/// from a codepoint or single-char string), `s` (any scalar via `to_s`), `p`
/// (a scalar's inspect, restricted to shapes Rust spells identically). Flags
/// `-`, `0`, `+`, space, `#`; a decimal width and `.precision`. Float
/// conversions (`e`/`E`/`f`/`g`/`G`) and dynamic (`*`) / positional (`%1$s`) /
/// named (`%<n>s`, `%{n}`) forms are deliberately declined — Rust cannot
/// guarantee their byte-exact C-`printf` spelling here.
pub fn sprintf(template: &str, args: &[Scalar]) -> Option<String> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::new();
    let mut argi = 0usize;
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            if out.len() > STRING_FOLD_BYTE_LIMIT {
                return None;
            }
            continue;
        }
        // At a '%'. Consume it.
        i += 1;
        // A trailing '%' with nothing after is malformed.
        let next = *chars.get(i)?;
        if next == '%' {
            out.push('%');
            i += 1;
            continue;
        }

        // Flags.
        let mut flags = Flags::default();
        loop {
            match chars.get(i) {
                Some('-') => flags.minus = true,
                Some('0') => flags.zero = true,
                Some('+') => flags.plus = true,
                Some(' ') => flags.space = true,
                Some('#') => flags.hash = true,
                _ => break,
            }
            i += 1;
        }

        // Reject dynamic width `*`.
        if chars.get(i) == Some(&'*') {
            return None;
        }

        // Width (decimal). Cap it BEFORE it can drive an allocation.
        let mut wstr = String::new();
        while let Some(d) = chars.get(i) {
            if d.is_ascii_digit() {
                wstr.push(*d);
                i += 1;
            } else {
                break;
            }
        }
        // A `$` here would be a positional argument reference — decline.
        if chars.get(i) == Some(&'$') {
            return None;
        }
        let width: Option<usize> = if wstr.is_empty() {
            None
        } else {
            let w: usize = wstr.parse().ok()?;
            if w > STRING_FOLD_BYTE_LIMIT {
                return None;
            }
            Some(w)
        };

        // Precision.
        let mut precision: Option<usize> = None;
        if chars.get(i) == Some(&'.') {
            i += 1;
            if chars.get(i) == Some(&'*') {
                return None;
            }
            let mut pstr = String::new();
            while let Some(d) = chars.get(i) {
                if d.is_ascii_digit() {
                    pstr.push(*d);
                    i += 1;
                } else {
                    break;
                }
            }
            let p: usize = if pstr.is_empty() { 0 } else { pstr.parse().ok()? };
            if p > STRING_FOLD_BYTE_LIMIT {
                return None;
            }
            precision = Some(p);
        }

        // Conversion character.
        let conv = *chars.get(i)?;
        i += 1;

        let segment = match conv {
            'd' | 'i' | 'u' => {
                let arg = args.get(argi)?;
                argi += 1;
                fmt_integer(int_value(arg)?, 10, false, flags, width, precision)?
            }
            'x' => fmt_radix_arg(args, &mut argi, 16, false, "0x", flags, width, precision)?,
            'X' => fmt_radix_arg(args, &mut argi, 16, true, "0X", flags, width, precision)?,
            'o' => fmt_radix_arg(args, &mut argi, 8, false, "0", flags, width, precision)?,
            'b' => fmt_radix_arg(args, &mut argi, 2, false, "0b", flags, width, precision)?,
            'B' => fmt_radix_arg(args, &mut argi, 2, false, "0B", flags, width, precision)?,
            's' => {
                let arg = args.get(argi)?;
                argi += 1;
                fmt_string(&ruby_string_of(arg), flags, width, precision)?
            }
            'p' => {
                let arg = args.get(argi)?;
                argi += 1;
                fmt_string(&ruby_inspect(arg)?, flags, width, precision)?
            }
            'c' => {
                let arg = args.get(argi)?;
                argi += 1;
                // Precision is meaningless for %c; decline if given.
                if precision.is_some() {
                    return None;
                }
                fmt_string(&char_value(arg)?, flags, width, None)?
            }
            // Float conversions and anything unrecognised: decline.
            _ => return None,
        };

        out.push_str(&segment);
        if out.len() > STRING_FOLD_BYTE_LIMIT {
            return None;
        }
    }

    // Ruby raises on any unconsumed argument (too many) — decline.
    if argi != args.len() {
        return None;
    }
    if out.len() > STRING_FOLD_BYTE_LIMIT {
        return None;
    }
    Some(out)
}

/// The integer value of a `%d`/`%i`/`%u` argument: an `Int` directly, or a
/// finite `Float` truncated toward zero (matching Ruby's `%d` coercion). Any
/// other scalar (Ruby would raise or coerce via `to_int`/`Integer()`, which we
/// do not model) declines.
fn int_value(s: &Scalar) -> Option<i128> {
    match s {
        Scalar::Int(n) => Some(*n as i128),
        Scalar::Float(f) => {
            if !f.is_finite() {
                return None;
            }
            let t = f.trunc();
            if t >= -(2f64.powi(63)) && t < 2f64.powi(63) {
                Some(t as i128)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Fetch and format a radix directive's (`x`/`X`/`o`/`b`) argument. Restricted
/// to a NON-NEGATIVE integer: Ruby renders a negative value under `%x` with its
/// infinite two's-complement `..f` notation, which is not worth reproducing, so
/// we decline it.
// too_many_arguments: threads the parsed directive spec (radix, case, prefix,
// flags, width, precision) plus the arg cursor; bundling it would only shuffle
// the same fields into a struct.
#[allow(clippy::too_many_arguments)]
fn fmt_radix_arg(
    args: &[Scalar],
    argi: &mut usize,
    radix: u32,
    upper: bool,
    alt_prefix: &str,
    flags: Flags,
    width: Option<usize>,
    precision: Option<usize>,
) -> Option<String> {
    let arg = args.get(*argi)?;
    *argi += 1;
    let v = match arg {
        Scalar::Int(n) if *n >= 0 => *n as i128,
        _ => return None,
    };
    let mut digits = to_radix(v as u128, radix, upper);
    // Precision = minimum digit count (zero-padded); `%.0` of 0 is empty.
    if let Some(p) = precision {
        if v == 0 && p == 0 {
            digits = String::new();
        } else if digits.len() < p {
            digits = format!("{}{}", "0".repeat(p - digits.len()), digits);
        }
    }
    let prefix = if flags.hash && v != 0 { alt_prefix } else { "" };
    let body = format!("{prefix}{digits}");
    Some(pad(body, "", flags, width, precision.is_some()))
}

/// Format an integer for `%d`/`%i`/`%u` (base 10, signed).
fn fmt_integer(
    value: i128,
    radix: u32,
    upper: bool,
    flags: Flags,
    width: Option<usize>,
    precision: Option<usize>,
) -> Option<String> {
    let neg = value < 0;
    let mag = value.unsigned_abs();
    let mut digits = to_radix(mag, radix, upper);
    if let Some(p) = precision {
        if value == 0 && p == 0 {
            digits = String::new();
        } else if digits.len() < p {
            digits = format!("{}{}", "0".repeat(p - digits.len()), digits);
        }
    }
    let sign = if neg {
        "-"
    } else if flags.plus {
        "+"
    } else if flags.space {
        " "
    } else {
        ""
    };
    Some(pad(digits, sign, flags, width, precision.is_some()))
}

/// Pad `body` (already carrying its own radix prefix, if any) with `sign`
/// prepended, to `width` per the flags. `precision_given` suppresses the `0`
/// flag (C semantics: an explicit precision on an integer ignores `0`).
fn pad(
    body: String,
    sign: &str,
    flags: Flags,
    width: Option<usize>,
    precision_given: bool,
) -> String {
    let content_len = sign.len() + body.len();
    let Some(w) = width else {
        return format!("{sign}{body}");
    };
    if content_len >= w {
        return format!("{sign}{body}");
    }
    let padding = w - content_len;
    if flags.minus {
        // Left-justified: spaces on the right (0 flag ignored with `-`).
        format!("{sign}{body}{}", " ".repeat(padding))
    } else if flags.zero && !precision_given {
        // Zero-padded between sign and body.
        format!("{sign}{}{body}", "0".repeat(padding))
    } else {
        // Right-justified with spaces.
        format!("{}{sign}{body}", " ".repeat(padding))
    }
}

/// Format a string for `%s`/`%p`/`%c`: precision truncates to that many
/// characters, then width pads with spaces (the `0` flag never zero-pads a
/// string in Ruby/C).
fn fmt_string(
    s: &str,
    flags: Flags,
    width: Option<usize>,
    precision: Option<usize>,
) -> Option<String> {
    let truncated: String = match precision {
        Some(p) => s.chars().take(p).collect(),
        None => s.to_string(),
    };
    let len = truncated.chars().count();
    let Some(w) = width else {
        return Some(truncated);
    };
    if len >= w {
        return Some(truncated);
    }
    let padding = w - len;
    if flags.minus {
        Some(format!("{truncated}{}", " ".repeat(padding)))
    } else {
        Some(format!("{}{truncated}", " ".repeat(padding)))
    }
}

/// A scalar's `inspect` for `%p`, restricted to shapes Rust spells identically
/// to Ruby. Strings only fold when every char is ASCII and needs no escape
/// beyond `"`/`\` (where Rust `Debug` and Ruby `inspect` agree); anything richer
/// declines so we never emit a divergent escape sequence.
fn ruby_inspect(s: &Scalar) -> Option<String> {
    match s {
        Scalar::Int(n) => Some(n.to_string()),
        Scalar::Float(f) if f.is_finite() => Some(ruby_float_to_s(*f)),
        Scalar::Bool(b) => Some(b.to_string()),
        Scalar::Nil => Some("nil".to_string()),
        Scalar::Sym(v) if is_simple_symbol(v) => Some(format!(":{v}")),
        Scalar::Str(v) if is_simple_string(v) => Some(format!("{v:?}")),
        _ => None,
    }
}

/// A string whose Rust `Debug` (`{:?}`) spelling equals Ruby's `String#inspect`:
/// printable ASCII, with no control characters or `#` (Ruby escapes `#{`/`#@`
/// contexts differently and interpolation-adjacent `#` is a divergence risk).
fn is_simple_string(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_graphic() && c != '#' || c == ' ')
}

/// A symbol whose inspect is exactly `:name` (a bare identifier).
fn is_simple_symbol(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The `%c` character value: a codepoint (`Int`) or the first character of a
/// single-char `String`. Declines an out-of-range codepoint or a multi-char
/// string (Ruby takes the first char of a longer string, but the safe subset
/// keeps to length 1).
fn char_value(s: &Scalar) -> Option<String> {
    match s {
        Scalar::Int(n) => {
            let cp = u32::try_from(*n).ok()?;
            char::from_u32(cp).map(|c| c.to_string())
        }
        Scalar::Str(v) => {
            let mut it = v.chars();
            let first = it.next()?;
            if it.next().is_none() {
                Some(first.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Render `mag` in `radix` (2/8/16), lowercase or uppercase hex digits.
fn to_radix(mag: u128, radix: u32, upper: bool) -> String {
    if mag == 0 {
        return "0".to_string();
    }
    let digits = b"0123456789abcdef";
    let mut buf = Vec::new();
    let mut n = mag;
    let r = radix as u128;
    while n > 0 {
        let d = (n % r) as usize;
        let ch = digits[d];
        buf.push(if upper { ch.to_ascii_uppercase() } else { ch });
        n /= r;
    }
    buf.reverse();
    // SAFETY: buf is ASCII digits only.
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
// approx_constant: several fixtures use `3.14` as an ordinary oracle-verified
// float literal, NOT as an approximation of π.
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;

    fn s(x: &str) -> Scalar {
        Scalar::Str(x.to_string())
    }

    // ---- sprintf: the f01-family success matrix (oracle-verified) ----
    #[test]
    fn sprintf_integer_and_string_basics() {
        assert_eq!(sprintf("%d", &[Scalar::Int(1)]).as_deref(), Some("1"));
        assert_eq!(sprintf("%d", &[Scalar::Int(42)]).as_deref(), Some("42"));
        assert_eq!(sprintf("hello", &[]).as_deref(), Some("hello"));
        assert_eq!(sprintf("%s", &[s("x")]).as_deref(), Some("x"));
        assert_eq!(sprintf("%s", &[Scalar::Int(42)]).as_deref(), Some("42"));
        assert_eq!(sprintf("%s", &[Scalar::Nil]).as_deref(), Some(""));
        assert_eq!(sprintf("%s", &[Scalar::Sym("sym".into())]).as_deref(), Some("sym"));
        assert_eq!(sprintf("%s", &[Scalar::Bool(true)]).as_deref(), Some("true"));
        assert_eq!(
            sprintf("%d %s", &[Scalar::Int(1), s("a")]).as_deref(),
            Some("1 a")
        );
    }

    #[test]
    fn sprintf_flags_width_precision() {
        assert_eq!(sprintf("%05d", &[Scalar::Int(42)]).as_deref(), Some("00042"));
        assert_eq!(sprintf("%-5d|", &[Scalar::Int(42)]).as_deref(), Some("42   |"));
        assert_eq!(sprintf("%+d", &[Scalar::Int(42)]).as_deref(), Some("+42"));
        assert_eq!(sprintf("% d", &[Scalar::Int(42)]).as_deref(), Some(" 42"));
        assert_eq!(sprintf("%x", &[Scalar::Int(255)]).as_deref(), Some("ff"));
        assert_eq!(sprintf("%X", &[Scalar::Int(255)]).as_deref(), Some("FF"));
        assert_eq!(sprintf("%o", &[Scalar::Int(8)]).as_deref(), Some("10"));
        assert_eq!(sprintf("%b", &[Scalar::Int(5)]).as_deref(), Some("101"));
        assert_eq!(sprintf("%#x", &[Scalar::Int(255)]).as_deref(), Some("0xff"));
        assert_eq!(sprintf("%c", &[Scalar::Int(65)]).as_deref(), Some("A"));
        assert_eq!(sprintf("%.3d", &[Scalar::Int(42)]).as_deref(), Some("042"));
        assert_eq!(sprintf("%5s", &[s("hi")]).as_deref(), Some("   hi"));
        assert_eq!(sprintf("%-5s|", &[s("hi")]).as_deref(), Some("hi   |"));
        assert_eq!(sprintf("%.3s", &[s("hello")]).as_deref(), Some("hel"));
        assert_eq!(sprintf("%d", &[Scalar::Float(3.9)]).as_deref(), Some("3"));
    }

    #[test]
    fn sprintf_percent_and_p() {
        assert_eq!(sprintf("%%", &[]).as_deref(), Some("%"));
        assert_eq!(sprintf("100%%", &[]).as_deref(), Some("100%"));
        assert_eq!(sprintf("%p", &[s("x")]).as_deref(), Some("\"x\""));
    }

    // ---- sprintf: decline cases (must be None → silent) ----
    #[test]
    fn sprintf_declines() {
        // arg-count mismatch (too few / too many)
        assert_eq!(sprintf("%d", &[]), None);
        assert_eq!(sprintf("%d", &[Scalar::Int(1), Scalar::Int(2)]), None);
        // arg-type mismatch
        assert_eq!(sprintf("%d", &[s("x")]), None);
        // unknown directive
        assert_eq!(sprintf("%z", &[Scalar::Int(1)]), None);
        // float conversions declined
        assert_eq!(sprintf("%f", &[Scalar::Float(3.14)]), None);
        assert_eq!(sprintf("%e", &[Scalar::Float(1000.0)]), None);
        assert_eq!(sprintf("%g", &[Scalar::Float(0.0001)]), None);
        // dynamic / positional / named forms
        assert_eq!(sprintf("%*d", &[Scalar::Int(5), Scalar::Int(1)]), None);
        assert_eq!(sprintf("%1$s", &[s("a")]), None);
        // trailing bare percent
        assert_eq!(sprintf("abc%", &[]), None);
        // negative radix declines (two's-complement notation)
        assert_eq!(sprintf("%x", &[Scalar::Int(-1)]), None);
    }

    // ---- OOM / panic safety: huge widths decline before allocating ----
    #[test]
    fn sprintf_huge_width_declines() {
        assert_eq!(sprintf("%099999999d", &[Scalar::Int(1)]), None);
        assert_eq!(sprintf("%.99999999d", &[Scalar::Int(1)]), None);
        assert_eq!(sprintf("%9999d", &[Scalar::Int(1)]), None); // over the 4096 cap
        // A width just under the cap folds and is bounded.
        let r = sprintf("%4000d", &[Scalar::Int(1)]).unwrap();
        assert_eq!(r.len(), 4000);
    }

    #[test]
    fn sprintf_never_panics_fuzz() {
        // A battery of malformed / adversarial templates must each return
        // cleanly (Some or None), never panic.
        let templates = [
            "%", "%%%", "%-", "%+", "% ", "%#", "%0", "%.", "%.5", "%-0+ #d",
            "%99999999999999999999d", "%.99999999999999999999d", "%*.*d", "%<x>s",
            "%{x}", "%$", "%1$", "%c", "%s%s%s", "abc", "", "%d%", "%09d",
            "%\u{1F600}d", "\u{1F600}%d", "%p", "%b", "%o", "%x%X%o%b",
        ];
        let args = [
            Scalar::Int(1),
            Scalar::Int(-5),
            Scalar::Float(2.5),
            Scalar::Str("hi".into()),
            Scalar::Nil,
        ];
        for t in templates {
            // Single-arg and multi-arg; just assert it returns without panicking.
            let _ = sprintf(t, &[]);
            let _ = sprintf(t, &args[..1]);
            let _ = sprintf(t, &args);
        }
    }

    // ---- Integer() ----
    #[test]
    fn integer_accepts() {
        assert_eq!(ruby_integer(&s("42"), None), Some(42));
        assert_eq!(ruby_integer(&s("-7"), None), Some(-7));
        assert_eq!(ruby_integer(&s("+42"), None), Some(42));
        assert_eq!(ruby_integer(&s("0x1A"), None), Some(26));
        assert_eq!(ruby_integer(&s("1_000"), None), Some(1000));
        assert_eq!(ruby_integer(&s(" 42 "), None), Some(42));
        assert_eq!(ruby_integer(&s("42"), Some(16)), Some(66));
        assert_eq!(ruby_integer(&s("0b101"), None), Some(5));
        assert_eq!(ruby_integer(&s("0o17"), None), Some(15));
        assert_eq!(ruby_integer(&s("017"), None), Some(15));
        assert_eq!(ruby_integer(&s("  0x1A  "), None), Some(26));
        assert_eq!(ruby_integer(&Scalar::Float(3.9), None), Some(3));
        assert_eq!(ruby_integer(&Scalar::Float(-3.9), None), Some(-3));
        assert_eq!(ruby_integer(&Scalar::Int(5), None), Some(5));
    }

    #[test]
    fn integer_declines() {
        assert_eq!(ruby_integer(&s("abc"), None), None);
        assert_eq!(ruby_integer(&s(""), None), None);
        assert_eq!(ruby_integer(&s("1.5"), None), None);
        assert_eq!(ruby_integer(&s("1__0"), None), None); // doubled underscore
        assert_eq!(ruby_integer(&s("_10"), None), None); // leading underscore
        assert_eq!(ruby_integer(&s("10_"), None), None); // trailing underscore
        assert_eq!(ruby_integer(&s("0x1G"), None), None); // bad hex digit
        assert_eq!(ruby_integer(&Scalar::Int(5), Some(16)), None); // base + int raises
        assert_eq!(ruby_integer(&Scalar::Nil, None), None);
        assert_eq!(ruby_integer(&Scalar::Float(f64::NAN), None), None);
        assert_eq!(ruby_integer(&Scalar::Float(1e30), None), None); // overflow i64
        // prefix vs explicit base mismatch
        assert_eq!(ruby_integer(&s("0x1A"), Some(10)), None);
    }

    // ---- Float() ----
    #[test]
    fn float_accepts() {
        assert_eq!(ruby_float(&s("1e3")), Some(1000.0));
        assert_eq!(ruby_float(&s("3.14")), Some(3.14));
        assert_eq!(ruby_float(&s("42")), Some(42.0));
        assert_eq!(ruby_float(&Scalar::Int(42)), Some(42.0));
        assert_eq!(ruby_float(&s("  1.5  ")), Some(1.5));
        assert_eq!(ruby_float(&s("1_000.5")), Some(1000.5));
    }

    #[test]
    fn float_declines() {
        assert_eq!(ruby_float(&s("abc")), None);
        assert_eq!(ruby_float(&s("0x1p4")), None); // hex float declined
        assert_eq!(ruby_float(&s("")), None);
        assert_eq!(ruby_float(&s("inf")), None);
        assert_eq!(ruby_float(&s("nan")), None);
        assert_eq!(ruby_float(&s(".")), None);
        assert_eq!(ruby_float(&Scalar::Nil), None);
    }

    // ---- String() ----
    #[test]
    fn string_of_scalars() {
        assert_eq!(ruby_string_of(&Scalar::Int(42)), "42");
        assert_eq!(ruby_string_of(&Scalar::Nil), "");
        assert_eq!(ruby_string_of(&Scalar::Sym("sym".into())), "sym");
        assert_eq!(ruby_string_of(&Scalar::Bool(true)), "true");
        assert_eq!(ruby_string_of(&Scalar::Bool(false)), "false");
        assert_eq!(ruby_string_of(&Scalar::Float(3.0)), "3.0");
        assert_eq!(ruby_string_of(&Scalar::Float(3.14)), "3.14");
        assert_eq!(ruby_string_of(&s("hi")), "hi");
    }
}
