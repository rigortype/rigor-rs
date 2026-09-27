//! Value-pinned Hash-literal keys (`HashKey`), precomputed while lowering so
//! `flow.duplicate-hash-key` stays source-free.

use crate::ruby_prism;

use super::{constant_string, span_of, Builder, Span};

/// The comparable identity of a value-pinned Hash-literal key, tagged by literal
/// KIND so the literal spaces stay separate (`:a` ≠ `"a"`, `1` ≠ `1.0`) — Ruby
/// `Hash#eql?` semantics. A faithful port of the reference's `literal_key`
/// (`duplicate_hash_key_collector.rb`): only these value-pinned forms participate
/// in the `flow.duplicate-hash-key` check; any other key form (interpolated
/// string/symbol, constant, call, local, `**splat`) never enters the seen set.
///
/// Float identity is carried as the `f64` bit pattern (`to_bits`), so `1.0` and
/// `1.00` collide (same bits) exactly as `1.0.eql?(1.00)` is true, while
/// `Int(1)` and `Float(1.0)` never compare (distinct variants).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HashKeyTag {
    Sym(String),
    Str(String),
    /// Parsed integer value (radix/underscore-normalised) — value identity, so
    /// `1` and `0x1` collide exactly as `1.eql?(0x1)` is true. `i128` covers every
    /// realistic literal key; a value beyond it is treated as non-pinned (skipped).
    Int(i128),
    /// `f64::to_bits` of the parsed float (Ruby `eql?`/`hash` identity).
    Float(u64),
    True,
    False,
    Nil,
}

/// A value-pinned key of a Hash literal (braced `HashNode` or bare-kwargs
/// `KeywordHashNode`), precomputed at lowering time so the `flow.duplicate-hash-key`
/// rule stays source-free. `tag` is the collision identity; `label` is the
/// message rendering (symbol → `:name`, string → Ruby `String#inspect`,
/// integer/float/`true`/`false`/`nil` → the verbatim source slice, so
/// `{ 1.0 => x, 1.00 => y }` renders `` `1.00' ``); `anchor` is the key node's
/// byte span (the diagnostic anchors at its start); `line` is the key's 1-based
/// start line (the message's "first set at line N"). Only value-pinned assoc keys
/// are recorded, in source order — splats and non-pinned keys are omitted (they
/// never participate), so the rule's seen-map dedup over this list reproduces the
/// reference's element walk exactly.
#[derive(Clone, Debug)]
pub struct HashKey {
    pub anchor: Span,
    pub line: u32,
    pub tag: HashKeyTag,
    pub label: String,
}

impl<'src> Builder<'src> {
    /// Build the value-pinned [`HashKey`] list for a Hash/keyword-hash literal's
    /// elements, in source order. A faithful port of the reference's `literal_key`
    /// and `key_label`: only a symbol, plain-string, integer, float, `true`,
    /// `false`, or `nil` key is recorded (non-value-pinned keys and `**`splats are
    /// skipped, never entering the seen set). Uses the borrowed Prism nodes so the
    /// verbatim source slice (integer/float labels) is available.
    pub(crate) fn hash_keys_of(&self, elements: &ruby_prism::NodeList<'_>) -> Vec<HashKey> {
        let mut keys = Vec::new();
        for el in elements.iter() {
            let Some(assoc) = el.as_assoc_node() else {
                continue; // `**splat` (AssocSplatNode) — inert, skipped.
            };
            let key = assoc.key();
            let loc = key.location();
            let anchor = span_of(&loc);
            let line = self.line_at(anchor.0);
            let raw = || constant_string(loc.as_slice());
            let (tag, label) = if let Some(sym) = key.as_symbol_node() {
                let name = constant_string(sym.unescaped());
                (HashKeyTag::Sym(name.clone()), format!(":{name}"))
            } else if let Some(s) = key.as_string_node() {
                let contents = constant_string(s.unescaped());
                (HashKeyTag::Str(contents.clone()), ruby_inspect_string(&contents))
            } else if key.as_integer_node().is_some() {
                match parse_ruby_integer(&raw()) {
                    Some(v) => (HashKeyTag::Int(v), raw()),
                    None => continue, // bignum beyond i128 — treat as non-pinned.
                }
            } else if let Some(f) = key.as_float_node() {
                (HashKeyTag::Float(f.value().to_bits()), raw())
            } else if key.as_true_node().is_some() {
                (HashKeyTag::True, raw())
            } else if key.as_false_node().is_some() {
                (HashKeyTag::False, raw())
            } else if key.as_nil_node().is_some() {
                (HashKeyTag::Nil, raw())
            } else {
                continue; // interpolated / constant / call / local — non-pinned.
            };
            keys.push(HashKey { anchor, line, tag, label });
        }
        keys
    }
}

/// Parse a Ruby integer literal's source slice to its `i128` value for
/// duplicate-key IDENTITY (Ruby compares hash keys by value: `1.eql?(0x1)`).
/// Handles an optional sign, `0x`/`0o`/`0b`/`0d` radix prefixes (and the bare
/// `0NNN` octal + `0NN` … actually Ruby's leading-zero octal), and `_` digit
/// separators. Returns `None` for a value beyond `i128` (rare bignum key) or an
/// unparsable form, so the caller treats it as non-value-pinned (FP-safe: a
/// missed witness, never a false one).
fn parse_ruby_integer(raw: &str) -> Option<i128> {
    let s = raw.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (radix, digits) = if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (16, r)
    } else if let Some(r) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        (8, r)
    } else if let Some(r) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        (2, r)
    } else if let Some(r) = s.strip_prefix("0d").or_else(|| s.strip_prefix("0D")) {
        (10, r)
    } else if s.len() > 1 && s.starts_with('0') {
        (8, &s[1..]) // Ruby leading-zero octal (`0755`).
    } else {
        (10, s)
    };
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }
    let mag = i128::from_str_radix(&cleaned, radix).ok()?;
    Some(if neg { -mag } else { mag })
}

/// Ruby `String#inspect` for the duplicate-hash-key label of a STRING key. Wraps
/// in double quotes and escapes the characters Ruby escapes in a double-quoted
/// literal. Covers the ASCII forms that appear as hash-key literals; a byte-exact
/// match of Ruby's full Unicode escaping is out of scope (string keys with
/// control/non-ASCII bytes duplicated in one literal do not occur in the probe
/// matrix or realistic corpora).
fn ruby_inspect_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\x0b' => out.push_str("\\v"),
            '\x1b' => out.push_str("\\e"),
            '#' => out.push('#'),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
