//! Baseline read/write/match (reference ADR-22, §7 of `docs/CURRENT_WORK.md`).
//!
//! A baseline records the *current* set of diagnostics so they are suppressed
//! on future runs, surfacing only NEW diagnostics. The on-disk file is the
//! reference's `.rigor-baseline.yml`, and this module is byte-compatible with
//! it so a baseline is interchangeable between the two tools.
//!
//! # On-disk format (reference `Rigor::Analysis::Baseline`)
//!
//! ```yaml
//! ---
//! version: 1
//! ignored:
//! - file: app/models/user.rb
//!   rule: call.undefined-method
//!   count: 3
//! - file: app/lib/sig.rb
//!   rule: call.undefined-method
//!   message: undefined\ method\ `merge'\ for\ Array
//!   count: 1
//! ```
//!
//! Two row shapes coexist in one file:
//! - **rule-ID row** — `(file, rule)` bucket; `message` absent (`None`).
//! - **message-pattern row** — `(file, rule, message_regex)` bucket; `message`
//!   present as a Ruby-`Regexp.escape`d source string.
//!
//! Field order on write is exactly `file`, `rule`, (`message`,) `count` so the
//! YAML is byte-identical to the reference's `YAML.dump`. An empty baseline
//! writes `ignored: []`.
//!
//! # Bucket semantics (reference WD4)
//!
//! Per `(file, rule [, message])` bucket, with `actual` = how many live
//! diagnostics land in the bucket and `count` = the recorded threshold:
//! - `actual <= count` → ALL diagnostics in the bucket are silenced.
//! - `actual >  count` → ALL of them surface (the bucket crossed its
//!   threshold — review focus shifts to "what's going on with this rule in
//!   this file", not "which N is new").
//!
//! # Matching precedence (reference `claim_bucket_for`)
//!
//! For a diagnostic, candidate buckets are those sharing its `(file, rule)`.
//! Message-pattern buckets are tried first (tighter match wins); a diagnostic
//! matching none of them falls through to the rule-ID bucket if one exists.
//!
//! # Filter-pipeline position (reference WD6)
//!
//! The baseline filter runs LAST among the suppression layers — after inline
//! `# rigor:disable` and config `disable:`. See `main.rs`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use regex::Regex;
use rigor_rules::Diagnostic;

/// The reference's default baseline file name.
pub const DEFAULT_BASELINE_PATH: &str = ".rigor-baseline.yml";

/// The schema version this module reads and writes.
pub const CURRENT_VERSION: u64 = 1;

/// How `baseline generate` keys rows: `Rule` (default, one bucket per
/// `(file, rule)`) or `Message` (one bucket per distinct message).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    Rule,
    Message,
}

/// A bucket's `file:`/`rule:` key as `YAML.safe_load` typed the scalar
/// (issue #326 — reference `parse_row` stores `row["file"]`/`row["rule"]`
/// RAW, whatever Psych resolved). `Str` is the only shape a diagnostic's
/// String key can equal; every other variant is a truthy non-String the
/// reference keeps but that never claims — `[5, rule]` never `==`
/// `["a.rb", rule]` upstream — and that displays/re-emits as its `to_s`.
#[derive(Clone, Debug)]
pub enum KeyScalar {
    /// A YAML String — the only diagnostic-matching key.
    Str(String),
    /// A YAML Integer — canonical decimal text (`0x10` stores `16`).
    Int(String),
    /// A YAML Float — `to_s` text (`1.5`, `Infinity`, `NaN`).
    Float(String),
    /// `true` — `false` never reaches a bucket (`row["k"] or raise` drops
    /// it on the `missing` error first).
    True,
    /// `:name`, stored without the colon.
    Symbol(String),
    /// A truthy non-String object the port does not fully type (a
    /// `!ruby/encoding` Encoding) — (`to_s`, class name for conversion
    /// errors), displays `to_s`, never claims.
    Opaque(String, &'static str),
}

impl KeyScalar {
    /// The String a diagnostic's key is compared against — `None` for every
    /// non-String variant (the inert-key rule).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            KeyScalar::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The scalar text `YAML.dump` would re-emit — `to_s` for most variants;
    /// floats take their `.inf`/`.nan` spellings, symbols their `:name`, an
    /// opaque object its `!ruby/object`-ish placeholder (never produced by
    /// `generate`; only a hand-edited inert row reaches here).
    fn yaml_emit(&self) -> String {
        match self {
            KeyScalar::Str(_) => unreachable!("Str keys emit through push_kv"),
            KeyScalar::Int(digits) => digits.clone(),
            KeyScalar::Float(text) => match text.as_str() {
                "Infinity" => ".inf".to_string(),
                "-Infinity" => "-.inf".to_string(),
                "NaN" => ".nan".to_string(),
                t => t.to_string(),
            },
            KeyScalar::True => "true".to_string(),
            KeyScalar::Symbol(name) => format!(":{name}"),
            KeyScalar::Opaque(text, _) => text.clone(),
        }
    }

    /// The `JSON.pretty_generate` encoding of the key — numbers and `true`
    /// bare, everything else a JSON string.
    pub fn json(&self) -> String {
        match self {
            KeyScalar::Str(s) => crate::json_string(s),
            KeyScalar::Int(digits) => digits.clone(),
            KeyScalar::Float(text) => text.clone(),
            KeyScalar::True => "true".to_string(),
            KeyScalar::Symbol(name) => crate::json_string(name),
            KeyScalar::Opaque(text, _) => crate::json_string(text),
        }
    }

    /// The text `TypeError: no implicit conversion of X into String`
    /// reports upstream when `File.fnmatch?`/`to_s`-implicit conversions
    /// meet a non-String key — the VALUE for nil/true/false
    /// (`of true into String`), the class elsewhere.
    pub fn class_name(&self) -> &str {
        match self {
            KeyScalar::Str(_) => "String",
            KeyScalar::Int(_) => "Integer",
            KeyScalar::Float(_) => "Float",
            KeyScalar::True => "true",
            KeyScalar::Symbol(_) => "Symbol",
            KeyScalar::Opaque(_, class) => class,
        }
    }

    /// A deterministic order for display sorts — the reference `sort_by`s
    /// the raw values, which crashes on a String↔Integer comparison; any
    /// port order already diverges from that crash, so sort by variant
    /// rank then `to_s`.
    fn rank(&self) -> u8 {
        match self {
            KeyScalar::Str(_) => 0,
            KeyScalar::Int(_) => 1,
            KeyScalar::Float(_) => 2,
            KeyScalar::True => 3,
            KeyScalar::Symbol(_) => 4,
            KeyScalar::Opaque(..) => 5,
        }
    }
}

impl Ord for KeyScalar {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.rank(), self.to_string()).cmp(&(other.rank(), other.to_string()))
    }
}

impl PartialOrd for KeyScalar {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for KeyScalar {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (KeyScalar::Str(a), KeyScalar::Str(b)) => a == b,
            (KeyScalar::Int(a), KeyScalar::Int(b)) => a == b,
            (KeyScalar::Float(a), KeyScalar::Float(b)) => a == b,
            (KeyScalar::True, KeyScalar::True) => true,
            (KeyScalar::Symbol(a), KeyScalar::Symbol(b)) => a == b,
            (KeyScalar::Opaque(a, _), KeyScalar::Opaque(b, _)) => a == b,
            _ => false,
        }
    }
}

impl Eq for KeyScalar {}

/// `key == "s"` — Ruby `5 == "5"` is `false`: only a `Str` key equals a
/// String.
impl PartialEq<str> for KeyScalar {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}

impl PartialEq<&str> for KeyScalar {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl PartialEq<String> for KeyScalar {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == Some(other.as_str())
    }
}

impl std::hash::Hash for KeyScalar {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.rank().hash(state);
        self.to_string().hash(state);
    }
}

impl std::fmt::Display for KeyScalar {
    /// Ruby `to_s`/`#{…}` of the value — what the reference's drift rows,
    /// `dump` text and sort keys print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyScalar::Str(s) | KeyScalar::Int(s) | KeyScalar::Float(s) | KeyScalar::Symbol(s) => {
                f.write_str(s)
            }
            KeyScalar::Opaque(s, _) => f.write_str(s),
            KeyScalar::True => f.write_str("true"),
        }
    }
}

/// Ruby `<=>` between two loaded scalar keys: `None` where the comparison
/// returns `nil` upstream (cross-class, `true`, an object — the
/// `sort_by`/`Array#<=>` caller then dies `comparison of Array with Array
/// failed`). Numerics compare across Integer/Float (`5 <=> 1.5` → -1);
/// `NaN` compares to nothing.
pub(crate) fn ruby_scalar_cmp(a: &KeyScalar, b: &KeyScalar) -> Option<std::cmp::Ordering> {
    use KeyScalar::*;
    let f64_of = |k: &KeyScalar| -> Option<f64> {
        match k {
            Int(d) => d.parse::<f64>().ok(),
            Float(t) => match t.as_str() {
                "Infinity" => Some(f64::INFINITY),
                "-Infinity" => Some(f64::NEG_INFINITY),
                "NaN" => Some(f64::NAN),
                t => t.parse::<f64>().ok(),
            },
            _ => None,
        }
    };
    match (a, b) {
        (Str(x), Str(y)) => Some(x.cmp(y)),
        (Symbol(x), Symbol(y)) => Some(x.cmp(y)),
        (Int(x), Int(y)) => Some(int_digits_cmp(x, y)),
        (Int(_) | Float(_), Int(_) | Float(_)) => {
            f64_of(a).and_then(|x| f64_of(b).and_then(|y| x.partial_cmp(&y)))
        }
        // `Object#<=>` falls back to `==` — `true <=> true` is 0 upstream,
        // and one Encoding compares equal to itself.
        (True, True) => Some(std::cmp::Ordering::Equal),
        (Opaque(x, _), Opaque(y, _)) if x == y => Some(std::cmp::Ordering::Equal),
        _ => None,
    }
}

/// `Array#<=>` on the `[file, rule]` pair — element-wise, first non-equal
/// wins, `nil` anywhere is `nil` overall (upstream then raises
/// `ArgumentError: comparison of Array with Array failed`).
pub(crate) fn ruby_pair_cmp(
    a: (&KeyScalar, &KeyScalar),
    b: (&KeyScalar, &KeyScalar),
) -> Option<std::cmp::Ordering> {
    match ruby_scalar_cmp(a.0, b.0) {
        None => None,
        Some(std::cmp::Ordering::Equal) => ruby_scalar_cmp(a.1, b.1),
        ord => ord,
    }
}

/// A sort that dies like upstream's `sort_by` when the comparator meets an
/// incomparable pair — `Err(())` maps to the
/// `ArgumentError: comparison of Array with Array failed` crash surface.
/// Insertion sort keeps the ordering deterministic and comparisons local;
/// for a totally-ordered set the result is the sorted order, and a set
/// Ruby's mergesort could order without hitting the pair is a documented
/// corner either way.
pub(crate) fn ruby_sort<T, F>(mut items: Vec<T>, mut cmp: F) -> Result<Vec<T>, ()>
where
    F: FnMut(&T, &T) -> Option<std::cmp::Ordering>,
{
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 {
            match cmp(&items[j - 1], &items[j]) {
                None => return Err(()),
                Some(std::cmp::Ordering::Greater) => {
                    items.swap(j - 1, j);
                    j -= 1;
                }
                _ => break,
            }
        }
    }
    Ok(items)
}

/// Numeric compare of two canonical decimal digit strings (possibly
/// `-`-led, arbitrary length).
fn int_digits_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let a_neg = a.starts_with('-');
    let b_neg = b.starts_with('-');
    if a_neg != b_neg {
        return if a_neg {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        };
    }
    let (x, y) = (a.trim_start_matches('-'), b.trim_start_matches('-'));
    let ord = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
    if a_neg {
        ord.reverse()
    } else {
        ord
    }
}

/// A single baseline bucket — the in-memory shape of one `ignored:` row.
#[derive(Clone, Debug)]
pub struct Bucket {
    /// Project-root-relative path, as stored on disk — a String upstream.
    /// Hand-edited rows can key on any truthy non-String scalar (`file: 5`);
    /// such a bucket can never claim a diagnostic (issue #326).
    pub file: KeyScalar,
    /// Qualified rule id (the reference's `qualified_rule`; for rigor-rs's
    /// all-`builtin` diagnostics this equals `Diagnostic::rule_id`).
    pub rule: KeyScalar,
    /// `Regexp.escape`d source string for message-pattern rows; `None` for
    /// rule-ID rows. Stored as the raw source so the file round-trips byte-for-byte.
    pub message: Option<String>,
    /// Compiled form of `message`, used by the matcher. Lazily compiled at load.
    /// Never serialized.
    pub message_regex: Option<Regex>,
    /// Recorded threshold (always a positive integer on disk).
    pub count: usize,
}

impl Bucket {
    fn rule_row(file: String, rule: String, count: usize) -> Self {
        Bucket {
            file: KeyScalar::Str(file),
            rule: KeyScalar::Str(rule),
            message: None,
            message_regex: None,
            count,
        }
    }

    fn message_row(file: KeyScalar, rule: KeyScalar, message: String, count: usize) -> Self {
        // The loader rejects an uncompilable pattern as LoadError before
        // reaching here (the reference's `Regexp.new` → LoadError contract);
        // `.ok()` here is only the defensive tail for generated rows, whose
        // `Regexp.escape`d source always compiles.
        let message_regex = Regex::new(&message).ok();
        Bucket {
            file,
            rule,
            message: Some(message),
            message_regex,
            count,
        }
    }

    fn rule_row_typed(file: KeyScalar, rule: KeyScalar, count: usize) -> Self {
        Bucket {
            file,
            rule,
            message: None,
            message_regex: None,
            count,
        }
    }
}

/// A parsed baseline: an ordered set of buckets plus a `(file, rule)` index.
#[derive(Debug, Default)]
pub struct Baseline {
    buckets: Vec<Bucket>,
}

/// A baseline load failure — two surfaces (issue #326):
///
/// - [`LoadError::Load`] is `Analysis::Baseline::LoadError` — RESCUED by the
///   CLI: `rigor: baseline load failed: … (continuing without baseline)` on
///   `check`, exit 64 on the `baseline` subcommands.
/// - [`LoadError::Fatal`] is an exception `safe_load`/`parse_row` raises
///   that nothing rescues — `Psych::DisallowedClass` on a Date/Time/`!ruby/*`
///   scalar, `Float()`'s `ArgumentError`, `Encoding.find`. The reference
///   dies with a backtrace, empty stdout, exit 1; the port mirrors the crash
///   surface (`rigor: <Class>: <message>`, exit 1) the way `config.rs`'s
///   `crash_failure` does for the same deaths in `.rigor.yml`.
#[derive(Debug)]
pub enum LoadError {
    /// `Analysis::Baseline::LoadError` — the rescued surface.
    Load(String),
    /// An uncaught exception upstream — fatal, exit 1.
    Fatal(String),
}

impl LoadError {
    /// The rescued `LoadError` surface.
    pub fn load(message: String) -> Self {
        LoadError::Load(message)
    }

    /// An uncaught-exception surface — `rigor: {self}`, exit 1.
    pub fn fatal(message: String) -> Self {
        LoadError::Fatal(message)
    }

    /// Whether this is the uncaught (fatal) surface.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        matches!(self, LoadError::Fatal(_))
    }

    /// The message text.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            LoadError::Load(m) | LoadError::Fatal(m) => m,
        }
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl Baseline {
    /// Number of buckets recorded.
    #[must_use]
    pub fn size(&self) -> usize {
        self.buckets.len()
    }

    /// Whether the baseline has no buckets (the filter is then a pass-through).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// The recorded buckets, in file order.
    #[must_use]
    pub fn buckets(&self) -> &[Bucket] {
        &self.buckets
    }

    /// Load a baseline from disk. Returns an empty baseline when the file does
    /// not exist (the reference's "no file yet" state under an explicit path).
    /// `Err(LoadError)` on a malformed file.
    pub fn load(path: &Path) -> Result<Baseline, LoadError> {
        if !path.exists() {
            return Ok(Baseline::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| {
            // `YAML.safe_load_file`'s `File.read` raises `Errno::*` (or an
            // encoding `ArgumentError` on bad UTF-8) — nothing rescues it
            // upstream, so this is the Fatal surface.
            let text = match e.kind() {
                std::io::ErrorKind::PermissionDenied => {
                    format!("Errno::EACCES: Permission denied - {}", path.display())
                }
                std::io::ErrorKind::InvalidData => {
                    "ArgumentError: invalid byte sequence in UTF-8".to_string()
                }
                _ => format!("{}: {e}", path.display()),
            };
            LoadError::fatal(text)
        })?;
        Baseline::parse(&text, &path.display().to_string())
    }

    /// Parse a baseline YAML document. Hand-rolled (the manifest / `.rigor.yml`
    /// loaders follow the same hand-rolled-parser precedent) so the read path
    /// stays in lock-step with the byte-exact writer and needs no serde shape.
    ///
    /// # Psych scalar dialect (probed against `YAML.dump` output)
    ///
    /// A row value may be a *folded* scalar — `baseline generate
    /// --match-mode=message` emits `Regexp.escape`d messages as plain scalars
    /// that libyaml wraps past column 80 at a breakable space, continuing on
    /// deeper-indented lines (the fold rejoins as a single space on load;
    /// each blank line rejoins as a `\n`). Quoted scalars may also span lines.
    /// Continuation handling below covers every shape Psych emits: folded
    ///
    /// # Typed scalars (issue #326)
    ///
    /// `safe_load_file(path, permitted_classes: [Symbol])` resolves a PLAIN
    /// scalar's text through `ScalarScanner#tokenize` *before* `parse_row`
    /// validates it — `count: 0x10` is the Integer 16, `message: false` a
    /// falsy no-message row, `file: 5` an inert Integer key, a `2024-01-01`
    /// date a `Psych::DisallowedClass` death (uncaught — [`LoadError::Fatal`]
    /// here). [`classify_scalar`] mirrors that typing; quoted scalars are
    /// Strings, and a `!tag` routes through `ToRuby#deserialize`'s arms.
    /// plain, multi-line `'…'`/`"…"` scalars, `|`/`>` block scalars, and a
    /// `!tag` prefix (`!!str '<<'`). A line deeper than the row's member-key
    /// column is scalar content, never a key.
    pub fn parse(text: &str, label: &str) -> Result<Baseline, LoadError> {
        // `version` keeps its loaded scalar until the end — the reference
        // validates the VALUE (`version == 1`), so a quoted `"1"` must
        // reject as `unsupported \`version: "1"\``, not parse, while `1.0`
        // and `0x1` pass (both `== 1` upstream).
        let mut version: Option<PsychScalar> = None;
        // `rows = raw["ignored"] || []` — the LOADED value's truthiness
        // decides (`ignored: false`/`no`/`off` are `[]` upstream, accepted);
        // the `is_a?(Array)` check runs AFTER `version`, so a truthy
        // non-array scalar defers to the end (issue #326 review).
        let mut ignored_scalar: Option<PsychScalar> = None;
        let mut ignored_empty_array = false;
        // Rows are collected, not validated, during the scan — upstream
        // `YAML.safe_load_file` types every scalar in the document before
        // `parse_loaded` validates anything, so a DisallowedClass/Float()
        // death ANYWHERE in the file beats every row error, and the version
        // and ignored-Array checks run before the first row error.
        let mut rows: Vec<RowAcc> = Vec::new();

        // Row accumulator: fields seen for the current `- file:` item.
        let mut cur: Option<RowAcc> = None;
        let mut in_ignored = false;
        // Column of the `-` that opened the current row and of its member
        // keys (`file:`), for `- item` vs continuation disambiguation.
        let mut item_col = 0usize;
        let mut member_col = 0usize;
        // A document that is a bare SEQUENCE (`- x` at column 0 before any
        // top-level `key:`) — its items still type (a `2024-01-01` anywhere
        // is a DisallowedClass death) and the `expected a Hash …, got
        // Array` error defers to after the scan, before `version:`
        // (upstream check order: `raw.is_a?(Hash)` → `version` → `ignored`
        // → rows).
        let mut top_level_seq = false;
        // Whether the last top-level `key:` left its value PENDING — a
        // `- item` that follows at column 0 belongs to that key's nested
        // sequence (`version:\n- 1` loads `[1]`, `gen:\n- x` loads too);
        // `Valued` means the key had an inline scalar (`ignored: ~\n- x`
        // is `Psych::SyntaxError` upstream).
        let mut top_ctx: Option<TopCtx> = None;
        // Scalars of a `version:` sequence — `[1]` renders the
        // `unsupported \`version: [1]\`` text upstream.
        let mut version_items: Vec<PsychScalar> = Vec::new();
        // Sequence items under a pending top-level key (`gen:` — typed for
        // DisallowedClass deaths, then discarded) or the doc's root
        // sequence (whose early `got Array` return discards them anyway).
        let mut cur_extra = false;
        // `---` opens doc 1; a SECOND `---` or an `...` line AT COLUMN 0
        // ends it — `safe_load` reads only the first document. Indented
        // markers are scalar text.
        let mut doc_open = false;

        for raw_line in text.lines() {
            let trimmed = raw_line.trim();
            let indent = raw_line.len() - raw_line.trim_start().len();
            if indent == 0 && (trimmed == "..." || (trimmed == "---" && doc_open)) {
                break;
            }
            // The document marker (column 0 only) and comment lines never
            // join a scalar — an indented `---`/`...` is scalar text.
            if (indent == 0 && trimmed == "---") || trimmed.starts_with('#') {
                doc_open |= indent == 0 && trimmed == "---";
                continue;
            }
            // Any other line is doc-1 content — a later `---` opens doc 2.
            doc_open = true;

            if trimmed.is_empty() {
                // A blank line inside a row is a pending fold break for the
                // scalar it follows (consumed only if a continuation comes).
                if let Some(acc) = cur.as_mut() {
                    acc.pending_breaks += 1;
                }
                continue;
            }

            // An UNCLOSED quoted scalar swallows every line until its closing
            // quote — including ones that look like keys or `- ` items (Psych
            // folds the break to one space; `\` at line end suppresses it).
            if let Some(acc) = cur.as_mut() {
                if acc.quote_open() {
                    acc.continue_scalar(trimmed);
                    continue;
                }
                if acc.head_open() {
                    acc.head_continue(trimmed);
                    continue;
                }
            }

            // A column-0 `- ` item: under `ignored:` it is a row; before
            // ANY top-level `key:` it opens the document's root sequence
            // (the deferred `got Array`); under a PENDING `key:` it is the
            // key's nested sequence; under a valued key it is
            // `Psych::SyntaxError` upstream.
            let is_dash = trimmed == "-" || trimmed.starts_with("- ");
            let mut pending_item = false;
            if !in_ignored && indent == 0 && is_dash {
                match top_ctx {
                    None => top_level_seq = true,
                    Some(TopCtx::Pending(_)) => pending_item = true,
                    Some(TopCtx::Valued) => {
                        return Err(LoadError::fatal(format!(
                            "Psych::SyntaxError: ({label}): did not find expected '-' indicator"
                        )));
                    }
                }
            }

            // Top-level keys (no indentation).
            if !raw_line.starts_with(' ') && !raw_line.starts_with('-') {
                // A non-`-` line at column 0 inside a sequence document
                // can't start a mapping — `Psych::SyntaxError` upstream.
                if top_level_seq {
                    return Err(LoadError::fatal(format!(
                        "Psych::SyntaxError: ({label}): did not find expected '-' indicator"
                    )));
                }
                if !trimmed.contains(':') {
                    // The doc's root scalar — upstream reports its loaded
                    // CLASS (`got Integer`/`NilClass`/`TrueClass`), and a
                    // DisallowedClass death fires here, before the shape
                    // check.
                    let (head, kind, tag) = scalar_head(trimmed);
                    let v = classify_scalar(
                        tag,
                        &unyaml_scalar(head),
                        matches!(kind, ScalarKind::Plain) && tag.is_none(),
                    )
                    .map_err(LoadError::fatal)?;
                    return Err(LoadError::load(format!(
                        "{label}: expected a Hash at top level, got {}",
                        v.class_name()
                    )));
                }
                // Close any open item before leaving its sequence —
                // flush_head types the deferred scalar head here (document
                // order); a `version:` sequence item feeds the `[1]`
                // unsupported-version inspect.
                if let Some(mut acc) = cur.take() {
                    let flushed = acc.flush_head()?;
                    if cur_extra {
                        if matches!(top_ctx, Some(TopCtx::Pending(TopKey::Version))) {
                            version_items.push(flushed.unwrap_or_else(|| acc.map_value()));
                        }
                    } else {
                        rows.push(acc);
                    }
                }
                cur_extra = false;
                in_ignored = false;
                if let Some(rest) = trimmed.strip_prefix("version:") {
                    // `version: 1 # schema` — the ` #…` is a comment. The
                    // scalar loads typed upstream: `1`/`1.0`/`0x1`/`!int 1`
                    // pass `== 1`, `"1"`/`1e0`/`true` don't, a Date dies.
                    // `version:` bare leaves the key PENDING — a `- item`
                    // sequence under it loads `[…]` (reported verbatim).
                    let value = rest.trim_start();
                    let (head, kind, tag) = scalar_head(value);
                    let decoded = unyaml_scalar(head);
                    version = Some(
                        classify_scalar(tag, &decoded, matches!(kind, ScalarKind::Plain))
                            .map_err(LoadError::fatal)?,
                    );
                    // A new `version:` line supersedes any sequence a
                    // previous pending `version:` collected (`version:`
                    // duplicated — the last key wins upstream).
                    version_items.clear();
                    top_ctx = if head.is_empty() {
                        Some(TopCtx::Pending(TopKey::Version))
                    } else {
                        Some(TopCtx::Valued)
                    };
                } else if let Some(rest) = trimmed.strip_prefix("ignored:") {
                    // `ignored:` bare or comment-only leaves the key's value
                    // unset — the block sequence below IS the value, so
                    // `in_ignored` stays open. Any PRESENT inline scalar —
                    // even a nil one (`~`/`null`/`Null`/`NULL`) — already
                    // consumed the value, and a following `- …` row is a
                    // Psych::SyntaxError upstream; closing `in_ignored` lands
                    // that row on the `expected a Hash …, got Array` error.
                    // `ignored: []` is an explicit empty array; a quoted or
                    // `!tag`d value is a String upstream — like any other
                    // non-nil inline it fails `rows.is_a?(Array)`.
                    let value = rest.trim_start();
                    let (inline, kind, tag) = scalar_head(value);
                    // A scalar Psych types as a `DisallowedClass` (or whose
                    // `Float()` conversion raises) kills the whole load
                    // upstream — `ignored: 2024-01-01` never reaches
                    // `rows.is_a?(Array)`.
                    let loaded = classify_scalar(
                        tag,
                        &unyaml_scalar(inline),
                        matches!(kind, ScalarKind::Plain) && tag.is_none(),
                    )
                    .map_err(LoadError::fatal)?;
                    let is_plain = matches!(kind, ScalarKind::Plain) && tag.is_none();
                    // A bare tag (`ignored: !!seq`) or anchor leaves the
                    // key pending the same way — the `- items` ARE the
                    // value upstream (an `ignored: !!seq` doc is a valid
                    // row sequence, probed).
                    in_ignored = inline.is_empty();
                    // `ignored:`'s `- items` are rows either way — the
                    // pending flag is irrelevant here (in_ignored routes
                    // them); a valued inline means the next `- ` is the
                    // upstream SyntaxError `did not find expected key`.
                    top_ctx = Some(TopCtx::Valued);
                    if is_plain && inline == "[]" {
                        // An explicit empty array — the `is_a?(Array)` check
                        // passes on its own; nothing to defer.
                        ignored_empty_array = true;
                    } else {
                        // The LOADED value's truthiness (`ignored: false` is
                        // `false || []` → `[]`, accepted) decides the
                        // deferred `is_a?(Array)` check after `version`.
                        ignored_scalar = Some(loaded);
                    }
                } else {
                    // Any other top-level key is ignored upstream — but its
                    // scalar still typed during `safe_load`, so a
                    // `generated_at: 2024-01-01` Date dies here, and a
                    // `gen:` pending key takes a `- item` sequence.
                    if let Some((_, value)) = trimmed.split_once(':') {
                        let (head, kind, tag) = scalar_head(value.trim_start());
                        classify_scalar(
                            tag,
                            &unyaml_scalar(head),
                            matches!(kind, ScalarKind::Plain) && tag.is_none(),
                        )
                        .map_err(LoadError::fatal)?;
                        top_ctx = if head.is_empty() {
                            Some(TopCtx::Pending(TopKey::Other))
                        } else {
                            Some(TopCtx::Valued)
                        };
                    }
                }
                continue;
            }

            // A deeper-indented `- item` under a pending top-level key is
            // the key's sequence too (`version:\n  - 1` → `[1]`,
            // `gen:\n  - 2024-01-01` → the DisallowedClass death) — probed.
            if !in_ignored
                && indent > 0
                && is_dash
                && matches!(top_ctx, Some(TopCtx::Pending(_)))
                && cur.is_none()
            {
                let v = seq_scalar_text(trimmed[1..].trim_start())?;
                if matches!(top_ctx, Some(TopCtx::Pending(TopKey::Version))) {
                    version_items.push(v);
                }
                continue;
            }

            if !in_ignored && !top_level_seq && !pending_item && cur.is_none() {
                continue;
            }

            // A new array item begins with `- ` (or bare `-`) at (or left
            // of) the row's dash column; deeper `- ` text is a folded
            // continuation. The first key (`file:`) rides on the same line
            // as the dash in the reference's emitter.
            if is_dash && (cur.is_none() || indent <= item_col) {
                if let Some(mut acc) = cur.take() {
                    let flushed = acc.flush_head()?;
                    if cur_extra {
                        if matches!(top_ctx, Some(TopCtx::Pending(TopKey::Version))) {
                            version_items.push(flushed.unwrap_or_else(|| acc.map_value()));
                        }
                    } else {
                        rows.push(acc);
                    }
                }
                cur_extra = pending_item;
                item_col = indent;
                let rest = trimmed.get(2..).unwrap_or("");
                member_col = indent + 2 + (rest.len() - rest.trim_start().len());
                let mut acc = RowAcc { idx: rows.len(), ..RowAcc::default() };
                acc.apply(rest.trim_start(), label, true)?;
                cur = Some(acc);
            } else if let Some(acc) = cur.as_mut() {
                if acc.takes_seq_item() && is_dash && indent >= member_col {
                    // A `- item` of a pending `key:`'s nested sequence (or
                    // of a bare `-` row's own sequence) — the elements
                    // type (`2024-01-01` dies DisallowedClass upstream).
                    acc.seq_item(trimmed[1..].trim_start(), label)?;
                } else if indent > member_col {
                    if acc.not_a_hash && acc.head_empty
                        && !is_dash
                        && key_split(trimmed).is_some()
                    {
                        // `-\n    file:` — a deeper-indented member still
                        // belongs to the bare-`-` row (the mapping's key
                        // column moves to the first member's indent).
                        member_col = indent;
                        acc.apply(trimmed, label, false)?;
                    } else {
                        // Folded / block-scalar continuation of the value.
                        acc.continue_scalar(raw_line);
                    }
                } else {
                    // A member key of the current item.
                    acc.apply(trimmed, label, false)?;
                }
            }
        }
        if let Some(mut acc) = cur.take() {
            let flushed = acc.flush_head()?;
            if cur_extra {
                if matches!(top_ctx, Some(TopCtx::Pending(TopKey::Version))) {
                    version_items.push(flushed.unwrap_or_else(|| acc.map_value()));
                }
            } else {
                rows.push(acc);
            }
        }

        // `unless raw.is_a?(Hash)` — a bare-sequence document types every
        // item first (deaths above), then reports Array, before `version:`.
        if top_level_seq {
            return Err(LoadError::load(format!(
                "{label}: expected a Hash at top level, got Array"
            )));
        }

        // A `version:` sequence loads `[…]` — `[1]` renders the upstream
        // `unsupported \`version: [1]\`` verbatim.
        if !version_items.is_empty() {
            let joined = version_items
                .iter()
                .map(PsychScalar::inspect)
                .collect::<Vec<_>>()
                .join(", ");
            version = Some(PsychScalar::Opaque { to_s: format!("[{joined}]"), class: "Array" });
        }

        // `unless version == CURRENT_VERSION` upstream — a missing `version:`
        // is `nil`, reported with the same message as a wrong one. The check
        // is Ruby `==`: Integer 1 and Float 1.0 both pass (probed: `1.0`,
        // `0x1`, `01`, `+1`, `!int 1`/`!float 1.0` accept; `1e0` is a String
        // — Psych's FLOAT arm needs a dot — and rejects as `"1e0"`).
        let version_ok = matches!(
            version,
            Some(PsychScalar::Int(ref i)) if i.value == CURRENT_VERSION as i128
        ) || matches!(version, Some(PsychScalar::Float(f)) if f == 1.0);
        if !version_ok {
            let shown = version
                .as_ref()
                .map_or_else(|| "nil".to_string(), PsychScalar::inspect);
            return Err(LoadError::load(format!(
                "{label}: unsupported `version: {shown}` (expected {CURRENT_VERSION})"
            )));
        }

        // `rows = raw["ignored"] || []`, then `rows.is_a?(Array)` — checked
        // after `version` upstream. `ignored: false`/`~`/`null` are `[]`
        // (falsy); `ignored: []` is an actual empty array; any truthy other
        // scalar fails.
        if ignored_scalar.as_ref().is_some_and(|v| !v.falsy()) && !ignored_empty_array {
            return Err(LoadError::load(format!(
                "{label}: `ignored:` must be an Array"
            )));
        }

        let mut buckets: Vec<Bucket> = Vec::new();
        for acc in rows {
            buckets.push(acc.into_bucket(label)?);
        }
        Ok(Baseline { buckets })
    }

    /// Build a baseline from a current run's diagnostics. Paths are stored
    /// project-root-relative; in `Message` mode each distinct message becomes
    /// its own bucket with a `Regexp.escape`d source.
    ///
    /// `paths`/`messages` are taken from the live diagnostics paired with their
    /// already-relativized path string (the caller relativizes against cwd, as
    /// the reference relativizes against `Dir.pwd`).
    #[must_use]
    pub fn from_diagnostics(entries: &[(String, &Diagnostic)], mode: MatchMode) -> Baseline {
        // Group preserving first-seen order of keys, like the reference's
        // `each_with_object({})`.
        let mut order: Vec<(String, String, Option<String>)> = Vec::new();
        let mut counts: BTreeMap<(String, String, Option<String>), usize> = BTreeMap::new();

        for (rel, diag) in entries {
            // `next if diag.qualified_rule.nil?` — a RULELESS diagnostic (a
            // parse error) is not baselinable: there is no rule to key a bucket
            // on, and an empty `rule:` row would silence every future parse
            // error in the file.
            let Some(rule) = diag.qualified_rule() else { continue };
            let rule = rule.to_string();
            let msg = match mode {
                MatchMode::Rule => None,
                MatchMode::Message => Some(regexp_escape(&diag.message)),
            };
            let key = (rel.clone(), rule, msg);
            if !counts.contains_key(&key) {
                order.push(key.clone());
            }
            *counts.entry(key).or_insert(0) += 1;
        }

        let buckets = order
            .into_iter()
            .map(|key| {
                let count = counts[&key];
                let (file, rule, msg) = key;
                match msg {
                    None => Bucket::rule_row(file, rule, count),
                    Some(m) => {
                        Bucket::message_row(KeyScalar::Str(file), KeyScalar::Str(rule), m, count)
                    }
                }
            })
            .collect();
        Baseline { buckets }
    }

    /// Serialize to the reference's exact YAML byte layout, including libyaml's
    /// plain-scalar wrapping: a scalar line past column 80 breaks at the next
    /// breakable space and continues on a `member column + 2`-indented line —
    /// the same bytes `Psych.dump` writes for a long `Regexp.escape`d message.
    #[must_use]
    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str(&format!("version: {CURRENT_VERSION}\n"));
        if self.buckets.is_empty() {
            out.push_str("ignored: []\n");
            return out;
        }
        out.push_str("ignored:\n");
        for b in &self.buckets {
            // Member keys sit at column 2 (`file` rides after `- `); Psych
            // indents a folded continuation two past that.
            push_key(&mut out, "- file: ", &b.file);
            push_key(&mut out, "  rule: ", &b.rule);
            if let Some(msg) = &b.message {
                push_kv(&mut out, "  message: ", 2, msg);
            }
            out.push_str(&format!("  count: {}\n", b.count));
        }
        out
    }

    /// Apply the baseline filter to a diagnostic stream. `entries` pairs each
    /// diagnostic with its project-root-relative path (the matcher key).
    ///
    /// Returns `(surfaced, silenced_count)`:
    /// - `surfaced` — the *indices* (into `entries`) that survive the filter:
    ///   new findings plus entire over-threshold buckets.
    /// - `silenced_count` — how many diagnostics the baseline suppressed.
    ///
    /// Diagnostics whose `(file, rule)` matches no bucket pass through as new.
    ///
    /// # Surfaced order (reference `group_diagnostics_for_filtering`)
    ///
    /// Under a non-empty baseline the reference BINS the stream by
    /// `(file, rule, message-source)` — or `(file, rule, :__none__)` when no
    /// bucket claims the row — in FIRST-APPEARANCE order (a plain Ruby Hash),
    /// and concatenates each surfacing bin's members in stream order. That
    /// regroups output: `a.rb:r1, b.rb:r2, a.rb:r1` prints as
    /// `a.rb:r1, a.rb:r1, b.rb:r2`. Diagnostics with no rule (or no path) are
    /// unkeyable and appended LAST (`surfaced + unkeyable`). An empty or
    /// absent baseline is a pass-through — input order is preserved.
    #[must_use]
    pub fn filter(&self, entries: &[(String, &Diagnostic)]) -> (Vec<usize>, usize) {
        if self.buckets.is_empty() {
            return ((0..entries.len()).collect(), 0);
        }

        let mut bins: Vec<Bin> = Vec::new();
        let mut slot_of: HashMap<BinKey, usize> = HashMap::new();
        let mut unkeyable = Vec::new();

        for (i, (rel, diag)) in entries.iter().enumerate() {
            // "Diagnostics that lacked a rule or a path bypass the baseline
            // entirely (the baseline can't address them)" — they are never
            // binned and therefore never silenced.
            let Some(rule) = diag.qualified_rule() else {
                unkeyable.push(i);
                continue;
            };
            let (key, bucket) = match self.claim_bucket(rel, diag) {
                Some(bi) => {
                    let b = &self.buckets[bi];
                    (
                        BinKey::Claimed(b.file.clone(), b.rule.clone(), b.message.clone()),
                        Some(bi),
                    )
                }
                None => (BinKey::NoBucket(rel.clone(), rule.to_string()), None),
            };
            let pos = match slot_of.get(&key) {
                Some(&p) => p,
                None => {
                    let p = bins.len();
                    bins.push(Bin { bucket, diagnostics: Vec::new() });
                    slot_of.insert(key, p);
                    p
                }
            };
            bins[pos].diagnostics.push(i);
        }

        let mut surfaced = Vec::new();
        let mut silenced = 0usize;
        for bin in bins {
            match bin.bucket {
                Some(bi) if bin.diagnostics.len() <= self.buckets[bi].count => {
                    silenced += bin.diagnostics.len();
                }
                _ => surfaced.extend(bin.diagnostics),
            }
        }
        surfaced.extend(unkeyable);
        (surfaced, silenced)
    }

    /// The bucket that claims `diag`: a message-pattern bucket whose regex
    /// matches the message wins; else the rule-ID bucket for `(file, rule)`.
    /// Returns the bucket's index in `self.buckets`, or `None` — including for
    /// a RULELESS diagnostic (a parse error), which no bucket can key on
    /// (reference: `next if diag.qualified_rule.nil?` in `audit` and
    /// `group_diagnostics_for_filtering`).
    fn claim_bucket(&self, rel: &str, diag: &Diagnostic) -> Option<usize> {
        let rule = diag.qualified_rule()?;
        let mut rule_fallback: Option<usize> = None;
        // Message-pattern buckets take precedence over the rule-ID bucket.
        for (i, b) in self.buckets.iter().enumerate() {
            // Only a String key can equal the diagnostic's — a typed
            // non-String key (`file: 5`) makes the bucket inert upstream
            // (`[5, rule]` never `==` `[path, rule]`).
            if b.file.as_str() != Some(rel) || b.rule.as_str() != Some(rule) {
                continue;
            }
            // Partition on the stored `message` like the reference partitions
            // on `message_regex` — a message row that never matches declines
            // rather than masquerading as the rule bucket.
            match (&b.message, &b.message_regex) {
                (Some(_), Some(re)) => {
                    if re.is_match(&diag.message) {
                        return Some(i);
                    }
                }
                (Some(_), None) => {}
                (None, _) => {
                    if rule_fallback.is_none() {
                        rule_fallback = Some(i);
                    }
                }
            }
        }
        rule_fallback
    }
}

/// A bucket's audited status against the current diagnostic stream
/// (reference `status_for`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftStatus {
    /// `actual > count` — the bucket exceeded its recorded threshold.
    Over,
    /// `actual == 0` — the bucket no longer matches any diagnostic.
    Cleared,
    /// `0 < actual < count` — the count can be tightened.
    Reducible,
    /// `actual == count` — the bucket exactly matches.
    Within,
}

/// One audited baseline bucket: the bucket, its live count, and status
/// (reference `DriftRow`). `delta = actual - count`.
#[derive(Debug)]
pub struct DriftRow<'a> {
    /// The audited bucket (borrowed from the baseline).
    pub bucket: &'a Bucket,
    /// How many current diagnostics this bucket claims.
    pub actual: usize,
    /// The bucket's drift status.
    pub status: DriftStatus,
    /// `actual - count` (may be negative).
    pub delta: i64,
}

impl Baseline {
    /// Audit the current diagnostic stream against every baseline bucket, in
    /// file order (reference `audit`). Each bucket claims diagnostics via the
    /// same message-pattern-before-rule-ID logic as `filter`; a bucket's
    /// `actual` is how many diagnostics it claims. Message-source-keyed counts
    /// are shared among buckets with the same `(file, rule, message-source)`
    /// key, matching the reference's `bucket_key`.
    #[must_use]
    pub fn audit<'a>(&'a self, entries: &[(String, &Diagnostic)]) -> Vec<DriftRow<'a>> {
        // Count diagnostics per claimed-bucket key.
        let mut counts: BTreeMap<(KeyScalar, KeyScalar, Option<String>), usize> = BTreeMap::new();
        for (rel, diag) in entries {
            if let Some(bi) = self.claim_bucket(rel, diag) {
                let b = &self.buckets[bi];
                let key = (b.file.clone(), b.rule.clone(), b.message.clone());
                *counts.entry(key).or_insert(0) += 1;
            }
        }

        self.buckets
            .iter()
            .map(|b| {
                let key = (b.file.clone(), b.rule.clone(), b.message.clone());
                let actual = counts.get(&key).copied().unwrap_or(0);
                let status = status_for(actual, b.count);
                let delta = actual as i64 - b.count as i64;
                DriftRow { bucket: b, actual, status, delta }
            })
            .collect()
    }

    /// A new baseline with the given buckets dropped, compared by FULL tuple
    /// `(file, rule, message-source, count)` — not by `(file, rule)` key
    /// (reference `without`, backed by Struct equality). Message is compared
    /// by its stored source string, not the compiled regex.
    #[must_use]
    pub fn without(&self, remove: &[&Bucket]) -> Baseline {
        let buckets = self
            .buckets
            .iter()
            .filter(|b| !remove.iter().any(|r| bucket_eq(b, r)))
            .cloned()
            .collect();
        Baseline { buckets }
    }
}

/// Full-tuple bucket equality: `(file, rule, message-source, count)`.
fn bucket_eq(a: &Bucket, b: &Bucket) -> bool {
    a.file == b.file && a.rule == b.rule && a.message == b.message && a.count == b.count
}

fn status_for(actual: usize, count: usize) -> DriftStatus {
    if actual == 0 {
        DriftStatus::Cleared
    } else if actual > count {
        DriftStatus::Over
    } else if actual == count {
        DriftStatus::Within
    } else {
        DriftStatus::Reducible
    }
}

/// One `(file, rule [, message-source])` bin of the reference's
/// `group_diagnostics_for_filtering`, in first-appearance order.
struct Bin {
    /// The claiming bucket (index into `self.buckets`) or `None` for the
    /// synthetic `(file, rule, :__none__)` bin that always surfaces.
    bucket: Option<usize>,
    /// Member diagnostics as `entries` indices, in stream order.
    diagnostics: Vec<usize>,
}

/// A bin's identity (reference `bins` Hash key): for a claimed diagnostic the
/// BUCKET's `(file, rule, message-source)` tuple — so two same-key rows share
/// one bin — and `NoBucket`'s `(file, rule)` for the unclaimed. `Claimed`
/// carries the TYPED keys so a `file: '5'` row and a `file: 5` row bin
/// separately, like the reference's `["5", rule]` vs `[5, rule]` hash keys.
#[derive(PartialEq, Eq, Hash)]
enum BinKey {
    Claimed(KeyScalar, KeyScalar, Option<String>),
    NoBucket(String, String),
}

/// Whether the last top-level `key:` left its value pending (a `- item`
/// sequence can follow) or already consumed it inline.
#[derive(Clone, Copy)]
enum TopCtx {
    Pending(TopKey),
    Valued,
}

/// Which pending top-level key a `- item` sequence attaches to — only
/// `version:` keeps its elements (for the `[1]` unsupported-version
/// inspect); other keys' elements type for deaths, then discard.
#[derive(Clone, Copy)]
enum TopKey {
    Version,
    Other,
}

/// Which row field the last `key:` line wrote — the field a folded or
/// block-scalar continuation line feeds.
#[derive(Clone, Copy)]
enum Field {
    File,
    Rule,
    Message,
    Count,
    Other,
}

/// The YAML scalar flavor of the last-written value — decides how
/// continuation lines join it.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ScalarKind {
    /// Unquoted; continuation folds the break to one space (a run of blank
    /// lines folds to that many `\n`).
    #[default]
    Plain,
    /// `"…"` — open until an unescaped `"` is seen; `\` at line end
    /// suppresses the fold space.
    DoubleQuoted,
    /// `'…'` — open until an unescaped `'` (`''` is an escaped quote).
    SingleQuoted,
    /// `|`/`>` block scalar (plus `-`/`+` chomping) — deeper-indented lines
    /// are literal content.
    Block { folded: bool, chomp: Chomp },
}

/// Block-scalar trailing-newline handling (`|` vs `|-` vs `|+`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Chomp {
    /// `|-`: strip trailing newlines.
    Strip,
    /// `|`: keep a single trailing newline.
    Clip,
    /// `|+`: keep all trailing newlines.
    Keep,
}

/// One row field's accumulated value. `raw` holds the scalar SOURCE for
/// quoted scalars (quotes/escapes in place, decoded at flush) and the folded
/// text for plain ones; `lines` holds a `|`/`>` block scalar's content lines.
#[derive(Default)]
struct FieldVal {
    raw: String,
    kind: ScalarKind,
    /// The `!`-tag token on the scalar (`!foo`, `!!str`, `!ruby/regexp`) —
    /// `ToRuby#deserialize` routes tagged scalars through its `case o.tag`
    /// arms rather than the plain/quoted rule (issue #326).
    tag: Option<String>,
    /// A tag with NO scalar text (`file: !ruby/regexp`) still resolves —
    /// and can still die on the tag alone (`class_loader.regexp`). The
    /// resolved [`PsychScalar`] is stored here at `apply` time.
    resolved: Option<PsychScalar>,
    /// Elements of a nested `- item` sequence under a `key:` with no scalar
    /// (`meta:\n  - x` — upstream loads the Array; each element types, so a
    /// `2024-01-01` there is still a DisallowedClass death).
    seq: Vec<PsychScalar>,
    lines: Vec<String>,
    /// A block scalar's content indentation, set by its first content line;
    /// deeper lines keep their extra columns relative to it.
    block_indent: Option<usize>,
}

/// In-progress accumulator for one `ignored:` row during parsing.
///
/// `file`/`rule`/`message`/`count` hold *raw* text so a multi-line scalar can
/// keep accumulating; they are decoded at [`RowAcc::into_bucket`]. `count`
/// stays quoted/raw — `"5"` must fail the integer check, matching the
/// reference's `count.is_a?(Integer)` rejecting a YAML string.
#[derive(Default)]
struct RowAcc {
    /// `ignored[N]` position — the reference puts it in every row error.
    idx: usize,
    file: Option<FieldVal>,
    rule: Option<FieldVal>,
    message: Option<FieldVal>,
    count: Option<FieldVal>,
    /// Sink for continuations of unknown keys (`meta:` etc.).
    other: FieldVal,
    /// Which field the last `key:` line wrote — a deeper-indented line or an
    /// open quoted scalar's next line continues it.
    last: Option<Field>,
    /// Whether `last`'s quoted scalar lacks its closing quote — while open it
    /// swallows EVERY next line, at any indent (YAML flow-scalar semantics).
    quote_open: bool,
    /// Blank lines since the last value line — consumed as fold breaks by the
    /// next continuation (a run of n blanks folds to n `\n`s), else discarded.
    pending_breaks: usize,
    /// The row's `- ` head was a scalar/nested-seq, not a `key: value` — the
    /// row loads non-Hash upstream. `must be a Hash` defers to
    /// [`RowAcc::into_bucket`] so `version:`/`ignored:` checks and later
    /// DisallowedClass deaths still win (upstream check order).
    not_a_hash: bool,
    /// The head's accumulated folded text — a plain scalar continues on
    /// colon-less member lines (`- 5\n  more` is the String "5 more"
    /// upstream). Classified once at row close ([`RowAcc::flush_head`]):
    /// the JOINED text is what `tokenize` sees, so `2024-01-01 more` is a
    /// String, not a Date death.
    head_raw: String,
    /// The head's scalar shape — only Plain heads fold member continuations.
    head_kind: ScalarKind,
    head_tag: Option<String>,
    /// Bare `-` (or `- # comment`) — the element is the indented block: a
    /// `key:` member opens a mapping (a real row), `- ` members a nested
    /// sequence (the row is an Array), a colon-less member a scalar.
    head_empty: bool,
    /// `- - x` — the element is a nested sequence; `- ` members keep
    /// feeding it (`- - x\n  - y` → `[[x, y]]` upstream → `must be a
    /// Hash`), while `key:`/colon-less members are SyntaxError (a seq
    /// takes no mapping).
    head_nested: bool,
    /// A quoted head lacking its closing quote — swallows member lines
    /// like [`RowAcc::quote_open`] does for fields (`- 'x\n  y'` is one
    /// String element upstream → `must be a Hash`).
    head_quote_open: bool,
    /// Elements of a nested sequence under a bare `-` or `- -` head
    /// (`-\n  - x` / `- - x\n  - y` load `[x]`/`[x, y]` upstream) — typed
    /// (fatal-able), and rendered for a `version:` sequence's `[[1]]`
    /// inspect.
    row_seq: Vec<PsychScalar>,
    /// `key => typed-value` pairs of a mapping item — a `version:`
    /// sequence's map elements render `{\"a\" => 1}` upstream.
    map_items: Vec<(String, PsychScalar)>,
    /// Whether `head_raw`/`row_seq` already classified at row close.
    head_flushed: bool,
}

impl RowAcc {
    /// Whether the last scalar is an unclosed quoted one — such a scalar
    /// swallows the next line whole (keys, items, deeper content alike).
    fn quote_open(&self) -> bool {
        self.quote_open
    }

    fn field_mut(&mut self, f: Field) -> &mut FieldVal {
        match f {
            Field::File => self.file.get_or_insert_with(FieldVal::default),
            Field::Rule => self.rule.get_or_insert_with(FieldVal::default),
            Field::Message => self.message.get_or_insert_with(FieldVal::default),
            Field::Count => self.count.get_or_insert_with(FieldVal::default),
            Field::Other => &mut self.other,
        }
    }

    fn field_ref(&self, f: Field) -> Option<&FieldVal> {
        match f {
            Field::File => self.file.as_ref(),
            Field::Rule => self.rule.as_ref(),
            Field::Message => self.message.as_ref(),
            Field::Count => self.count.as_ref(),
            Field::Other => None,
        }
    }

    /// The field's Psych-loaded value under `safe_load_file(path,
    /// permitted_classes: [Symbol])` — `ScalarScanner#tokenize` on a plain
    /// scalar, the String itself for quoted/block scalars, the
    /// `deserialize` tag arms for tagged ones (issue #326). `nil` and
    /// `false` are the falsy values `row["k"] or raise` and
    /// `if row["message"]` drop on; a `DisallowedClass`/`Float()` death is
    /// the [`LoadError::Fatal`] surface, not a row error.
    fn typed(&self, f: Field) -> Result<PsychScalar, LoadError> {
        let Some(fv) = self.field_ref(f) else {
            return Ok(PsychScalar::Nil);
        };
        if !fv.seq.is_empty() {
            // `key:\n  - x\n  - y` loads an Array — `Array#to_s`/`#inspect`
            // are the element inspects joined (`["foo"]`, `[5]`).
            let joined = fv
                .seq
                .iter()
                .map(PsychScalar::inspect)
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(PsychScalar::Opaque { to_s: format!("[{joined}]"), class: "Array" });
        }
        if let Some(v) = &fv.resolved {
            return Ok(v.clone());
        }
        let decoded = self.decode(f).unwrap_or_default();
        classify_scalar(
            fv.tag.as_deref(),
            &decoded,
            matches!(fv.kind, ScalarKind::Plain),
        )
        .map_err(LoadError::fatal)
    }

    /// Apply a line inside the current `- ` item — the text after `- `
    /// (a row head, where the scalar/member distinction decides whether
    /// the row is a mapping at all) or a member line.
    fn apply(&mut self, fragment: &str, label: &str, row_head: bool) -> Result<(), LoadError> {
        if row_head {
            return self.row_head(fragment, label);
        }
        self.member_line(fragment, label)
    }

    /// The `- X` row head. `X` is a `key: value` only when [`key_split`]
    /// finds a member-shaped colon — `a:b`, `'a: b'`, `:sym`, `[a: 1]`,
    /// `http://x` and `- file: x` are scalars/nested sequences upstream,
    /// so the row is non-Hash (`must be a Hash`, deferred so `version:`/
    /// `ignored:` checks and later DisallowedClass deaths precede it).
    fn row_head(&mut self, fragment: &str, label: &str) -> Result<(), LoadError> {
        // `- - x` — the element is itself a sequence; its inner scalar
        // still types (`- - 2024-01-01` dies DisallowedClass upstream) and
        // later `- ` members keep feeding it (`- - x\n  - y` → `[x, y]`).
        if fragment == "-" || fragment.starts_with("- ") {
            self.not_a_hash = true;
            self.head_nested = true;
            let v = seq_scalar_text(fragment[1..].trim_start())?;
            self.row_seq.push(v);
            return Ok(());
        }
        if let Some((key, _)) = key_split(fragment) {
            // `'a: b'`/`[a: 1]`-shaped heads are scalars upstream — a
            // member-shaped key (`'a':`, `{a:`, `a:`) is a real field.
            if member_key(key).is_some() {
                return self.field_line(fragment, label);
            }
        }
        // A scalar (or empty) head — `must be a Hash` at `into_bucket`.
        // The head's classify waits for row close: a colon-less member
        // folds into it and the JOINED text is what `tokenize` sees
        // (`- 2024-01-01\n  more` is the String "2024-01-01 more"
        // upstream, not a Date death), and an open quoted head swallows
        // member lines until its close (`- 'x\n  y'`).
        self.not_a_hash = true;
        let (head, kind, tag) = scalar_head(fragment);
        self.head_empty = head.is_empty();
        self.head_raw = head.to_string();
        self.head_kind = kind;
        self.head_tag = tag.map(str::to_string);
        self.head_quote_open = match kind {
            ScalarKind::DoubleQuoted => !dq_closed(head),
            ScalarKind::SingleQuoted => !sq_closed(head),
            _ => false,
        };
        Ok(())
    }

    /// One member line inside the current `- ` item.
    fn member_line(&mut self, fragment: &str, label: &str) -> Result<(), LoadError> {
        if self.not_a_hash {
            if fragment == "-" || fragment.starts_with("- ") {
                // Under a bare `-` or `- -` head this is the row's nested
                // sequence (`-\n  - x` → `[x]`, `- - x\n  - y` → `[x, y]`
                // — both `must be a Hash`); under a scalar head it is
                // `did not find expected key` upstream (a scalar element
                // cannot carry children).
                if self.head_empty || self.head_nested {
                    let v = seq_scalar_text(fragment[1..].trim_start())?;
                    self.row_seq.push(v);
                    self.pending_breaks = 0;
                    return Ok(());
                }
                return Err(LoadError::fatal(format!(
                    "Psych::SyntaxError: ({label}): did not find expected key \
                     while parsing a block mapping"
                )));
            }
            // Any other member under a nested-seq (`- -`) head is `did
            // not find expected key` upstream — a sequence takes no
            // mapping.
            if self.head_nested {
                return Err(LoadError::fatal(format!(
                    "Psych::SyntaxError: ({label}): did not find expected key \
                     while parsing a block mapping"
                )));
            }
            if key_split(fragment).is_some() {
                // A `key:` under a BARE `-` opens a mapping — the element
                // is a real Hash (`-\n  file: a.rb` is a valid row);
                // under a scalar head `- 5\n  x: 1` is `mapping values
                // are not allowed` upstream.
                if self.head_empty {
                    self.not_a_hash = false;
                    self.head_flushed = true;
                    return self.field_line(fragment, label);
                }
                return Err(LoadError::fatal(format!(
                    "Psych::SyntaxError: ({label}): mapping values are not allowed in this context"
                )));
            }
            // A colon-less member folds into the head's plain scalar
            // (`- 5\n  more` → "5 more" upstream — still `must be a
            // Hash`, and `2024-01-01 more` is a String, no death). A
            // non-Plain head cannot take a continuation.
            if self.head_kind == ScalarKind::Plain {
                let pending = std::mem::take(&mut self.pending_breaks);
                if !self.head_raw.is_empty() {
                    self.head_raw.push_str(&if pending > 0 {
                        "\n".repeat(pending)
                    } else {
                        " ".to_string()
                    });
                }
                self.head_raw.push_str(fragment.trim());
                return Ok(());
            }
            return Err(LoadError::fatal(format!(
                "Psych::SyntaxError: ({label}): mapping values are not allowed in this context"
            )));
        }
        // A `- item` at the member column is a nested sequence element of a
        // PENDING `key:` (`meta:\n  - x` — upstream loads the Array under
        // the key and types each element).
        if fragment == "-" || fragment.starts_with("- ") {
            return self.seq_item(fragment[1..].trim_start(), label);
        }
        self.field_line(fragment, label)
    }

    /// A `key: value` member (or row head) inside a mapping row.
    fn field_line(&mut self, fragment: &str, label: &str) -> Result<(), LoadError> {
        let Some((key, value)) = key_split(fragment) else {
            // A bare `bogus` member line — `could not find expected ':'`.
            return Err(LoadError::fatal(format!(
                "Psych::SyntaxError: ({label}): could not find expected ':'"
            )));
        };
        // `'rule':` maps `rule` upstream — a quoted key resolves to its
        // inner text; an UNCLOSED quoted key is a syntax error upstream.
        let Some(key_name) = member_key(key.trim()) else {
            return Err(LoadError::fatal(format!(
                "Psych::SyntaxError: ({label}): did not find expected key \
                 while parsing a block mapping"
            )));
        };
        let (head, kind, tag) = scalar_head(value.trim_start());
        let field = match key_name.as_str() {
            "file" => Field::File,
            "rule" => Field::Rule,
            "message" => Field::Message,
            "count" => Field::Count,
            _ => Field::Other,
        };
        if matches!(field, Field::Other) {
            // `other` is a shared sink — a new unknown `key:` opens a fresh
            // pending field (`meta2:` after `meta:\n  - x` is a NEW key's
            // value upstream, not a continuation).
            self.other = FieldVal::default();
        }
        self.last = Some(field);
        self.pending_breaks = 0;
        // Eagerly type the scalar in DOCUMENT ORDER — `safe_load` typed it
        // before `parse_row` ran, so its DisallowedClass/`Float()` death
        // beats every validation error, including an unknown `meta:` field's
        // (upstream typed it too). The loaded value is kept for a
        // `version:`-sequence map element's `{"a" => 1}` inspect; a
        // multi-line plain/quoted scalar re-classifies its full text at
        // `typed()` (a `file: 2024-01-01` head plus a continuation line is
        // the documented corner where the eager death is one line early).
        let typed_v = if !head.is_empty() {
            Some(
                classify_scalar(
                    tag,
                    &unyaml_scalar(head),
                    matches!(kind, ScalarKind::Plain) && tag.is_none(),
                )
                .map_err(LoadError::fatal)?,
            )
        } else {
            None
        };
        self.map_items.push((format!("{key_name:?}"), typed_v.unwrap_or(PsychScalar::Nil)));
        // `key:` with an empty value leaves the field PENDING — a nested
        // `- item` sequence becomes the value. A bare TAG (`file: !str`,
        // `meta: !foo`) stays pending too (the tag attaches to whatever
        // value follows); its resolution is deferred to `typed()`.
        if head.is_empty() {
            if let Some(t) = tag {
                *self.field_mut(field) =
                    FieldVal { tag: Some(t.to_string()), ..FieldVal::default() };
            }
            self.quote_open = false;
            return Ok(());
        }
        self.quote_open = match kind {
            ScalarKind::DoubleQuoted => !dq_closed(head),
            ScalarKind::SingleQuoted => !sq_closed(head),
            _ => false,
        };
        *self.field_mut(field) = FieldVal {
            raw: head.to_string(),
            kind,
            tag: tag.map(str::to_string),
            ..FieldVal::default()
        };
        Ok(())
    }

    /// Whether the last `key:` left its value PENDING (`meta:` with no
    /// inline scalar) — a `- item` that follows belongs to that key's
    /// nested sequence. A valued field or a bare-scalar row cannot take one
    /// (`did not find expected key` / `mapping values are not allowed`
    /// upstream).
    fn last_is_pending(&self) -> bool {
        let Some(field) = self.last else { return false };
        let fv = match field {
            Field::Other => &self.other,
            // `last` is set by the `key:` line itself — an absent FieldVal
            // means that line wrote no value: pending.
            _ => match self.field_ref(field) {
                Some(fv) => fv,
                None => return true,
            },
        };
        // `seq` is NOT checked — a field collecting `- item`s stays
        // pending so the sequence keeps growing. A bare TAG (`meta: !foo`)
        // leaves the key pending too — the sequence IS the tagged value
        // (probed: `meta: !foo\n  - x` loads upstream).
        fv.raw.is_empty()
            && fv.resolved.is_none()
            && matches!(fv.kind, ScalarKind::Plain)
    }

    /// Whether the current `- ` item accepts a member `- item` line — a
    /// pending `key:`'s nested sequence, or the nested sequence under a
    /// bare `-` / `- -` row head.
    fn takes_seq_item(&self) -> bool {
        self.last_is_pending()
            || (self.not_a_hash && (self.head_empty || self.head_nested))
    }

    /// One `- item` — the pending `key:`'s nested-sequence element, or an
    /// element of a bare-`-` row's own sequence. Each element types (a
    /// `2024-01-01` there is the same DisallowedClass death as an inline
    /// scalar); under a valued field or scalar head the line is uncaught
    /// `Psych::SyntaxError` upstream.
    fn seq_item(&mut self, item: &str, label: &str) -> Result<(), LoadError> {
        if !self.takes_seq_item() {
            return Err(LoadError::fatal(format!(
                "Psych::SyntaxError: ({label}): did not find expected key \
                 while parsing a block mapping"
            )));
        }
        let v = seq_scalar_text(item)?;
        if self.not_a_hash {
            self.row_seq.push(v);
        } else {
            let field = self.last.expect("last_is_pending implies last");
            self.field_mut(field).seq.push(v);
        }
        self.pending_breaks = 0;
        Ok(())
    }

    /// Classify the deferred `- X` scalar head at row close — `safe_load`
    /// sees the FOLDED text, so `- 2024-01-01\n  more` is the String
    /// "2024-01-01 more" (no death) while a lone `- 2024-01-01` is the
    /// DisallowedClass death. Returns the loaded element — a `version:`
    /// sequence collects item values for its `[[1]]`/`{"a" => 1}`
    /// inspect (a nested seq renders `[…]`, a folded scalar its own
    /// inspect, a map item `{…}` via [`RowAcc::map_value`]).
    fn flush_head(&mut self) -> Result<Option<PsychScalar>, LoadError> {
        if !self.not_a_hash || self.head_flushed {
            return Ok(None);
        }
        self.head_flushed = true;
        if !self.row_seq.is_empty() {
            let joined = self
                .row_seq
                .iter()
                .map(PsychScalar::inspect)
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(Some(PsychScalar::Opaque {
                to_s: format!("[{joined}]"),
                class: "Array",
            }));
        }
        classify_scalar(
            self.head_tag.as_deref(),
            &unyaml_scalar(&self.head_raw),
            matches!(self.head_kind, ScalarKind::Plain) && self.head_tag.is_none(),
        )
        .map(Some)
        .map_err(LoadError::fatal)
    }

    /// The `{"a" => 1, …}` inspect of a mapping item — the element a
    /// `version:` sequence collected.
    fn map_value(&self) -> PsychScalar {
        let joined = self
            .map_items
            .iter()
            .map(|(k, v)| format!("{k} => {}", v.inspect()))
            .collect::<Vec<_>>()
            .join(", ");
        PsychScalar::Opaque { to_s: format!("{{{joined}}}"), class: "Hash" }
    }

    /// An open-quote head — like [`RowAcc::quote_open`], member lines at
    /// ANY indent fold into the scalar until its close.
    fn head_open(&self) -> bool {
        self.head_quote_open
    }

    /// Fold a line into the open quoted head — same rules as
    /// [`RowAcc::continue_scalar`] for quoted fields.
    fn head_continue(&mut self, text: &str) {
        let pending = std::mem::take(&mut self.pending_breaks);
        let text = text.trim();
        match self.head_kind {
            ScalarKind::DoubleQuoted => {
                let backslashes = self
                    .head_raw
                    .trim_end()
                    .chars()
                    .rev()
                    .take_while(|&c| c == '\\')
                    .count();
                if backslashes % 2 == 1 {
                    let t = self.head_raw.trim_end().len();
                    self.head_raw.truncate(t - 1);
                } else if pending > 0 {
                    self.head_raw.push_str(&"\n".repeat(pending));
                } else {
                    self.head_raw.push(' ');
                }
                self.head_raw.push_str(text);
                if dq_closed(&self.head_raw) {
                    self.head_quote_open = false;
                }
            }
            _ => {
                if pending > 0 {
                    self.head_raw.push_str(&"\n".repeat(pending));
                } else {
                    self.head_raw.push(' ');
                }
                self.head_raw.push_str(text);
                if self.head_kind == ScalarKind::SingleQuoted && sq_closed(&self.head_raw) {
                    self.head_quote_open = false;
                }
            }
        }
    }

    /// Feed a continuation line (deeper-indented than the member keys, or
    /// inside an open quoted scalar) into the last field's scalar.
    fn continue_scalar(&mut self, raw_line: &str) {
        let Some(field) = self.last else { return };
        let pending = std::mem::take(&mut self.pending_breaks);
        let fv = self.field_mut(field);
        match fv.kind {
            ScalarKind::Plain => {
                let sep = if pending > 0 { "\n".repeat(pending) } else { " ".to_string() };
                fv.raw.push_str(&sep);
                fv.raw.push_str(raw_line.trim());
            }
            ScalarKind::DoubleQuoted => {
                // Inside quotes a break folds to one space; a `\` at the end
                // of a double-quoted line escapes the break entirely — drop
                // that backslash along with the fold.
                let suppressed = fv.raw.trim_end().ends_with('\\')
                    && fv.raw.trim_end().chars().rev().take_while(|&c| c == '\\').count() % 2 == 1;
                if suppressed {
                    let t = fv.raw.trim_end().len();
                    fv.raw.truncate(t - 1);
                } else if pending > 0 {
                    fv.raw.push_str(&"\n".repeat(pending));
                } else {
                    fv.raw.push(' ');
                }
                fv.raw.push_str(raw_line.trim());
                if dq_closed(&fv.raw) {
                    self.quote_open = false;
                }
            }
            ScalarKind::SingleQuoted => {
                if pending > 0 {
                    fv.raw.push_str(&"\n".repeat(pending));
                } else {
                    fv.raw.push(' ');
                }
                fv.raw.push_str(raw_line.trim());
                if sq_closed(&fv.raw) {
                    self.quote_open = false;
                }
            }
            ScalarKind::Block { .. } => {
                // Blank lines inside a block scalar are literal empty lines.
                for _ in 0..pending {
                    fv.lines.push(String::new());
                }
                let indent = raw_line.len() - raw_line.trim_start().len();
                let block_indent = *fv.block_indent.get_or_insert(indent);
                fv.lines.push(raw_line.get(block_indent..).unwrap_or("").to_string());
            }
        }
    }

    /// Decode a field's accumulated text (folds and escapes applied). For
    /// `count` the caller separately checks the scalar was PLAIN — a quoted
    /// `"5"` is a YAML String upstream and fails `count.is_a?(Integer)` there.
    fn decode(&self, field: Field) -> Option<String> {
        let fv = self.field_ref(field)?;
        match fv.kind {
            ScalarKind::Block { folded, chomp } => Some(block_value(&fv.lines, folded, chomp)),
            _ => Some(unyaml_scalar(&fv.raw)),
        }
    }

    fn into_bucket(self, label: &str) -> Result<Bucket, LoadError> {
        let idx = self.idx;
        // `row.is_a?(Hash)` — a bare-scalar row (`- 5`, `- [1]`, `- nil`)
        // deferred its shape error here so `version:`/`ignored:` checks
        // and document-order DisallowedClass deaths still precede it.
        if self.not_a_hash {
            return Err(LoadError::load(format!("{label}: ignored[{idx}] must be a Hash")));
        }
        // Type all four fields before validating any — `safe_load` typed
        // the whole document before `parse_row`, so a `message: 2024-01-01`
        // death beats the `count: 0` error beside it (issue #326 review).
        let file_v = self.typed(Field::File)?;
        let rule_v = self.typed(Field::Rule)?;
        let count_v = self.typed(Field::Count)?;
        let message_v = self.typed(Field::Message)?;
        // `file = row["file"] or raise`, `rule = row["rule"] or raise` — the
        // TYPED value's truthiness decides (nil/false are missing; `file: 5`
        // and `file: true` are truthy non-String keys — an inert bucket).
        let file = match file_v {
            v if v.falsy() => {
                return Err(LoadError::load(format!(
                    "{label}: ignored[{idx}] missing `file:`"
                )))
            }
            v => v.into_key(),
        };
        let rule = match rule_v {
            v if v.falsy() => {
                return Err(LoadError::load(format!(
                    "{label}: ignored[{idx}] missing `rule:`"
                )))
            }
            v => v.into_key(),
        };
        // `count.is_a?(Integer) && count.positive?` upstream — every other
        // type reports its `inspect` (`got "5"`, `got 1.5`, `got true`,
        // `got :x`, `got nil`), an Integer its canonical decimal (`got -16`
        // for `-0x10`). A huge Integer counts as `usize::MAX` — the
        // reference's `expect`-free threshold semantics need no bignum.
        let count = match count_v {
            PsychScalar::Int(i) if i.value > 0 => usize::try_from(i.value).unwrap_or(usize::MAX),
            v => {
                return Err(LoadError::load(format!(
                    "{label}: ignored[{idx}] `count:` must be a positive Integer (got {})",
                    v.inspect()
                )))
            }
        };
        Ok(match message_v {
            // `if (message = row["message"])` — a falsy typed value (nil,
            // `false`/`no`/`off`) is the rule-ID row, not a `/false/` bucket.
            v if v.falsy() => Bucket::rule_row_typed(file, rule, count),
            v => {
                // `compile_message_regex(message.to_s)` — a non-String keys
                // its `to_s` (`message: 0x10` compiles `/16/`, not `/0x10/`).
                let m = v.to_s();
                // The reference compiles the row's Regexp at load and raises
                // LoadError on a bad pattern — dropping the whole baseline —
                // rather than degrading the row to a rule bucket (issue #162:
                // Ruby-only SYNTAX stays upstream-deferred; the failure mode
                // is the contract). The error text after the colon is the
                // engine's own — Onigmo's RegexpError has no Rust analogue —
                // so the reason is kept terse (`…: /source/` like RegexpError).
                if let Err(e) = Regex::new(&m) {
                    let full = e.to_string();
                    let reason = full
                        .rsplit_once("error:")
                        .map_or(full.as_str(), |(_, r)| r)
                        .trim()
                        .to_string();
                    return Err(LoadError::load(format!(
                        "{label}: ignored[{idx}] `message:` is not a valid Regexp: \
                         {reason}: /{m}/"
                    )));
                }
                Bucket::message_row(file, rule, m, count)
            }
        })
    }
}

/// A `|`/`>` block scalar's decoded content from its collected lines:
/// `>` folds joins between non-empty lines to a space (empty lines force
/// `\n`); chomping then settles trailing newlines.
fn block_value(lines: &[String], folded: bool, chomp: Chomp) -> String {
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            if folded && !l.is_empty() && !lines[i - 1].is_empty() {
                out.push(' ');
            } else {
                out.push('\n');
            }
        }
        out.push_str(l);
    }
    match chomp {
        Chomp::Strip => {
            while out.ends_with('\n') {
                out.pop();
            }
        }
        Chomp::Clip => {
            while out.ends_with('\n') {
                out.pop();
            }
            if !lines.is_empty() {
                out.push('\n');
            }
        }
        Chomp::Keep => {
            if !lines.is_empty() {
                out.push('\n');
            }
        }
    }
    out
}

/// Classify and split a `key:`-line's value head: returns the scalar's
/// initial raw text, its kind, and the `!`-tag token if one led the value
/// (`!!str '<<'` decodes as `'<<'` under `deserialize`'s `!str` arm;
/// `!foo 5` tokenizes `5`). A trailing ` #` comment is stripped.
/// Split a `key: value` fragment — the member colon is the first `:`
/// followed by a space or the end of the line and the key must be
/// non-empty (`a:b`, `http://x`, `:sym` have no member colon — scalars
/// upstream; `a:: b` keys on `a:`). The key text is trimmed on the right
/// only; the caller matches it or rejects quoted/flow heads.
fn key_split(fragment: &str) -> Option<(&str, &str)> {
    let bytes = fragment.as_bytes();
    // A `'`/`"`-led fragment is a quoted key — its member colon sits after
    // the CLOSING quote (`'a: b': c` keys on `a: b`; `''` escapes in sq,
    // a `\` in dq). No close → the fragment is a scalar, not a member.
    if matches!(bytes.first(), Some(b'\'' | b'"')) {
        let q = bytes[0];
        let mut i = 1;
        let close = loop {
            let p = bytes[i..].iter().position(|&b| b == q)?;
            let idx = i + p;
            if q == b'\'' && bytes.get(idx + 1) == Some(&b'\'') {
                i = idx + 2;
            } else if q == b'"'
                && bytes[..idx].iter().rev().take_while(|&&b| b == b'\\').count() % 2
                    == 1
            {
                i = idx + 1;
            } else {
                break idx;
            }
        };
        let rest = fragment[close + 1..].trim_start();
        let rest = rest.strip_prefix(':')?;
        if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
            return None;
        }
        return Some((&fragment[..close + 1], rest));
    }
    let i = (0..bytes.len()).find(|&j| {
        bytes[j] == b':' && matches!(bytes.get(j + 1), None | Some(b' ' | b'\t'))
    })?;
    let key = fragment[..i].trim_end();
    if key.is_empty() {
        return None;
    }
    Some((key, &fragment[i + 1..]))
}

/// A member-shaped key — plain text, a CLOSED quoted scalar (`'a'` → `a`,
/// so `'a': 1` maps `"a" => 1` upstream while `'a: b'` is a scalar head),
/// or a `{` flow head (the accidentally-correct `missing file:`).
/// `[`- and unclosed-quote heads are scalars upstream, not members.
fn member_key(key: &str) -> Option<String> {
    if key.starts_with('[') {
        return None;
    }
    for q in ['\'', '"'] {
        if let Some(rest) = key.strip_prefix(q) {
            return rest.strip_suffix(q).map(str::to_string);
        }
    }
    Some(key.to_string())
}

/// Split a flow collection's inner text on top-level commas — a comma
/// inside a quoted scalar or a nested `[…]`/`{…}` does not split
/// (`['x, y']` is one element upstream; `\` escapes in dq, `''` in sq).
fn flow_items(inner: &str) -> Vec<&str> {
    let bytes = inner.as_bytes();
    let mut items = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth = depth.saturating_sub(1),
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') { i += 1; } else { break; }
                    }
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        break;
                    }
                    i += 1;
                }
            }
            b',' if depth == 0 => {
                items.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    items.push(&inner[start..]);
    items
}

/// The `inspect`-joined `[…]` a flow sequence loads — each element
/// types (a `2024-01-01` inside still dies DisallowedClass). Elements
/// that are themselves `k: v` pairs load as one-pair maps (`[a: 1]` →
/// `[{"a" => 1}]` upstream).
fn classify_flow_seq(inner: &str) -> Result<PsychScalar, String> {
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(PsychScalar::Opaque { to_s: "[]".to_string(), class: "Array" });
    }
    let mut parts = Vec::new();
    for item in flow_items(inner) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let v = if (item.starts_with('[') && item.ends_with(']'))
            || (item.starts_with('{') && item.ends_with('}'))
        {
            classify_scalar(None, item, true)?
        } else if let Some((k, v)) = key_split(item) {
            // A `k: v` element — a one-pair map upstream.
            let key_inspect = format!("{k:?}");
            let val = flow_scalar(v.trim())?;
            PsychScalar::Opaque {
                to_s: format!("{{{key_inspect} => {}}}", val.inspect()),
                class: "Hash",
            }
        } else {
            flow_scalar(item)?
        };
        parts.push(v.inspect());
    }
    Ok(PsychScalar::Opaque { to_s: format!("[{}]", parts.join(", ")), class: "Array" })
}

/// Classify one flow element — the same `scalar_head` + `unyaml` + kind
/// the block parser applies, so a quoted element decodes its quotes
/// (`'a, b'` → the String `a, b`, whose inspect is `"a, b"`).
fn flow_scalar(item: &str) -> Result<PsychScalar, String> {
    let (head, kind, tag) = scalar_head(item);
    classify_scalar(
        tag,
        &unyaml_scalar(head),
        matches!(kind, ScalarKind::Plain) && tag.is_none(),
    )
}

/// The `inspect`-joined `{…}` a flow mapping loads — keys and values
/// type (`{a: 2024-01-01}` dies DisallowedClass upstream).
fn classify_flow_map(inner: &str) -> Result<PsychScalar, String> {
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(PsychScalar::Opaque { to_s: "{}".to_string(), class: "Hash" });
    }
    let mut parts = Vec::new();
    for item in flow_items(inner) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if let Some((k, v)) = key_split(item) {
            classify_scalar(None, k.trim(), true)?;
            let val = flow_scalar(v.trim())?;
            parts.push(format!("{:?} => {}", k.trim(), val.inspect()));
        } else {
            // A bare flow-map entry (`{a}`) — types as a scalar.
            let v = flow_scalar(item)?;
            parts.push(format!("{} => nil", v.inspect()));
        }
    }
    Ok(PsychScalar::Opaque { to_s: format!("{{{}}}", parts.join(", ")), class: "Hash" })
}

/// The [`PsychScalar`] one `- item`'s text resolves to — the same typing
/// `safe_load` performs on the sequence element, one level of `key:`/
/// nested-sequence unwrapping included (`- a: 2024-01-01` and
/// `- - 2024-01-01` die DisallowedClass upstream).
fn seq_scalar_text(item: &str) -> Result<PsychScalar, LoadError> {
    if let Some((_k, v)) = key_split(item) {
        return seq_scalar_text(v.trim_start());
    }
    if item == "-" || item.starts_with("- ") {
        return seq_scalar_text(item[1..].trim_start());
    }
    let (head, kind, tag) = scalar_head(item);
    classify_scalar(
        tag,
        &unyaml_scalar(head),
        matches!(kind, ScalarKind::Plain) && tag.is_none(),
    )
    .map_err(LoadError::fatal)
}

fn scalar_head(value: &str) -> (&str, ScalarKind, Option<&str>) {
    let mut v = value;
    // A `#` in value position is a comment — the scalar is empty.
    if v.starts_with('#') {
        return ("", ScalarKind::Plain, None);
    }
    // One `!`-tag or `&`-anchor token then the scalar (`!!str '<<'`,
    // `! 'x'`, `&a x`, `!t &a x` — either order). A tag/anchor with no
    // following scalar leaves the value PENDING (`meta: !foo\n  - x` loads
    // the sequence under the tag). `*name` aliases stay in `v` —
    // `safe_load`'s alias handling dies on them (probed: `file: *x` is
    // `Psych::BadAlias` upstream).
    let mut tag = None;
    loop {
        if let Some(rest) = v.strip_prefix('&') {
            match rest.find([' ', '\t']) {
                Some(i) => v = rest[i..].trim_start(),
                None => v = "",
            }
        } else if v.starts_with('!') {
            match v[1..].find([' ', '\t']) {
                Some(i) => {
                    tag = Some(&v[..1 + i]);
                    v = v[1 + i..].trim_start();
                }
                None => {
                    tag = Some(v);
                    v = "";
                }
            }
        } else {
            break;
        }
    }
    // `!<URI>` is the verbatim tag spelling — Psych reports the tag as the
    // bare URI (`!<!str>` → `!str`, `!<tag:yaml.org,2002:str>` →
    // `tag:yaml.org,2002:str`), so unwrap it before `deserialize` routes.
    let tag = tag.map(|t| {
        t.strip_prefix("!<")
            .and_then(|inner| inner.strip_suffix('>'))
            .unwrap_or(t)
    });
    let kind = match v.chars().next() {
        Some('"') => ScalarKind::DoubleQuoted,
        Some('\'') => ScalarKind::SingleQuoted,
        Some('|' | '>') => {
            let folded = v.starts_with('>');
            let chomp = match v.as_bytes().get(1) {
                Some(b'-') => Chomp::Strip,
                Some(b'+') => Chomp::Keep,
                _ => Chomp::Clip,
            };
            ScalarKind::Block { folded, chomp }
        }
        _ => ScalarKind::Plain,
    };
    let v = match kind {
        ScalarKind::Plain => {
            // ` #` opens a comment in a plain scalar — and a `#`-first
            // remainder left by a stripped tag/anchor is comment-only
            // (`!foo # c` → the value stays PENDING, probed).
            if v.starts_with('#') {
                ""
            } else {
                match v.find(" #") {
                    Some(i) => v[..i].trim_end(),
                    None => v.trim_end(),
                }
            }
        }
        ScalarKind::Block { .. } => v,
        ScalarKind::DoubleQuoted | ScalarKind::SingleQuoted => {
            // Keep the whole quoted span; a ` #…` after the close is comment.
            let quote = v.as_bytes()[0];
            let mut end = 1usize;
            let bytes = v.as_bytes();
            while end < bytes.len() {
                if bytes[end] == quote {
                    if quote == b'\'' && bytes.get(end + 1) == Some(&b'\'') {
                        end += 2;
                        continue;
                    }
                    break;
                }
                if quote == b'"' && bytes[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            if end < bytes.len() { &v[..=end] } else { v }
        }
    };
    (v, kind, tag)
}

/// Does a double-quoted scalar source hold its closing quote?
fn dq_closed(raw: &str) -> bool {
    if !raw.starts_with('"') {
        return false;
    }
    let bytes = raw.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'"' => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Does a single-quoted scalar source hold its closing quote (`''` escapes)?
fn sq_closed(raw: &str) -> bool {
    if !raw.starts_with('\'') {
        return false;
    }
    let bytes = raw.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

// ---------------------------------------------------------------------------
// Typed scalars — `ScalarScanner#tokenize` + `ToRuby#deserialize` (issue #326)
// ---------------------------------------------------------------------------

/// A scalar's loaded value under the baseline's
/// `YAML.safe_load_file(path, permitted_classes: [Symbol])` — what
/// `parse_row`'s `row["file"]`/`row["rule"]`/`row["count"]`/`row["message"]`
/// and `parse_loaded`'s `raw["version"]`/`raw["ignored"]` see.
#[derive(Clone, Debug)]
enum PsychScalar {
    /// `nil` — `~`/`null`/empty, falsy.
    Nil,
    /// `true`/`false` — `yes`/`no`/`on`/`off`/`true`/`false`, case-insensitive
    /// (`y`/`n` are Strings in Psych 5).
    Bool(bool),
    /// An Integer. `value` saturates at `i128` (the reference's Integer is
    /// unbounded — saturation only touches `== 1`/`positive?` and the
    /// `usize` cap); `digits` is the canonical `to_s`/`inspect` text
    /// (`-0x10` → `-16`).
    Int(PsychInt),
    /// A Float (`1.5`, `1.`, `.5`, `1:2.5`, `.inf`, `.nan`).
    Float(f64),
    /// `:name` — `permitted_classes: [Symbol]` admits every symbol.
    Symbol(String),
    /// A String — quoted scalars, and every plain form `tokenize` declines.
    Str(String),
    /// A truthy non-String the port does not fully type (`!ruby/encoding`'s
    /// Encoding). Displays its `to_s`; `class` is for `can't convert` texts.
    Opaque { to_s: String, class: &'static str },
}

/// A loaded Integer — saturated `value` plus canonical decimal `digits`.
#[derive(Clone, Debug)]
struct PsychInt {
    value: i128,
    digits: String,
}

impl PsychScalar {
    /// `row["k"] or raise` / `if row["message"]` — Ruby falsy is nil|false.
    fn falsy(&self) -> bool {
        matches!(self, PsychScalar::Nil | PsychScalar::Bool(false))
    }

    /// `raw.class` text — the `expected a Hash at top level, got …` class
    /// name and the `can't convert X into Float` subject.
    fn class_name(&self) -> &str {
        match self {
            PsychScalar::Nil => "NilClass",
            PsychScalar::Bool(true) => "TrueClass",
            PsychScalar::Bool(false) => "FalseClass",
            PsychScalar::Int(_) => "Integer",
            PsychScalar::Float(_) => "Float",
            PsychScalar::Symbol(_) => "Symbol",
            PsychScalar::Str(_) => "String",
            PsychScalar::Opaque { class, .. } => class,
        }
    }

    /// Ruby `to_s` — the `message:` Regexp source for non-String values
    /// (`0x10` → `16`, `true` → `true`, `:x` → `x`).
    fn to_s(&self) -> String {
        match self {
            PsychScalar::Nil => String::new(),
            PsychScalar::Bool(b) => b.to_string(),
            PsychScalar::Int(i) => i.digits.clone(),
            PsychScalar::Float(f) => ruby_float_to_s(*f),
            PsychScalar::Symbol(name) => name.clone(),
            PsychScalar::Str(s) => s.clone(),
            PsychScalar::Opaque { to_s, .. } => to_s.clone(),
        }
    }

    /// Ruby `inspect` — the `unsupported \`version: …\`` and
    /// ``got …`` error texts (`"5"`, `1.5`, `true`, `:x`, `nil`, `-16`).
    fn inspect(&self) -> String {
        match self {
            PsychScalar::Nil => "nil".to_string(),
            PsychScalar::Bool(b) => b.to_string(),
            PsychScalar::Int(i) => i.digits.clone(),
            PsychScalar::Float(f) => ruby_float_to_s(*f),
            PsychScalar::Symbol(name) => symbol_inspect(name),
            PsychScalar::Str(s) => format!("{s:?}"),
            PsychScalar::Opaque { to_s, .. } => to_s.clone(),
        }
    }

    /// The bucket key this value keys on — `Str` alone can match a
    /// diagnostic's String path/rule (`file: 5` keys an inert bucket).
    fn into_key(self) -> KeyScalar {
        match self {
            PsychScalar::Str(s) => KeyScalar::Str(s),
            PsychScalar::Int(i) => KeyScalar::Int(i.digits),
            PsychScalar::Float(f) => KeyScalar::Float(ruby_float_to_s(f)),
            PsychScalar::Bool(true) => KeyScalar::True,
            PsychScalar::Symbol(name) => KeyScalar::Symbol(name),
            // Nil/false are filtered by the caller's falsy check first;
            // unreachable keys stay inert rather than panic.
            PsychScalar::Nil => KeyScalar::Opaque("nil".to_string(), "NilClass"),
            PsychScalar::Bool(false) => KeyScalar::Opaque("false".to_string(), "FalseClass"),
            PsychScalar::Opaque { to_s, class } => KeyScalar::Opaque(to_s, class),
        }
    }
}

/// `ToRuby#deserialize`'s resolution of one scalar: a plain UNTAGGED scalar
/// goes to `ScalarScanner#tokenize`, a QUOTED or BLOCK untagged one is the
/// String, and a tagged one routes through the `case o.tag` arms. `Err` is
/// an UNCAUGHT exception — `Psych::DisallowedClass`, `Float()`'s
/// `ArgumentError`, `can't convert`'s `TypeError` — carried as the
/// `<Class>: <message>` text [`LoadError::fatal`] reports.
fn classify_scalar(tag: Option<&str>, text: &str, plain: bool) -> Result<PsychScalar, String> {
    // A plain `*alias` dies at parse level under `safe_load`'s
    // `aliases: false` (probed: `Psych::AliasesNotEnabled`) — before any
    // deserialization.
    if plain && tag.is_none() && text.starts_with('*') {
        return Err(
            "Psych::AliasesNotEnabled: Alias parsing was not enabled".to_string()
        );
    }
    // A `[…]`/`{…}` flow collection — upstream loads the seq/map, typing
    // every element (`meta: [2024-01-01]` dies DisallowedClass).
    if plain && tag.is_none() && text.starts_with('[') && text.ends_with(']') {
        return classify_flow_seq(&text[1..text.len() - 1]);
    }
    if plain && tag.is_none() && text.starts_with('{') && text.ends_with('}') {
        return classify_flow_map(&text[1..text.len() - 1]);
    }
    match tag {
        None if plain => tokenize_scalar(text),
        None => Ok(PsychScalar::Str(text.to_string())),
        Some(t) => classify_tagged(t, text),
    }
}

/// `deserialize`'s `case o.tag` (psych `to_ruby.rb`), under
/// `permitted_classes: [Symbol]`. The `else` arm tokenizes the scalar's own
/// text — so `!foo 5`, `!!int 5` and `!ruby/object:Foo 5` all load as the
/// Integer 5 (probed), while `!str`-family/`!float`/`!ruby/*`-resolver tags
/// take their named arms.
fn classify_tagged(tag: &str, text: &str) -> Result<PsychScalar, String> {
    let disallowed = |name: &str| {
        Err(format!(
            "Psych::DisallowedClass: Tried to load unspecified class: {name}"
        ))
    };
    match tag {
        // `o.value.unpack('m').first` — base64-decode to a String.
        "!binary" | "!!binary" | "tag:yaml.org,2002:binary" => {
            Ok(PsychScalar::Str(base64_decode(text)))
        }
        // `/^!(?:str|ruby\/string)(?::(.*))?$/` — nil/empty name → the
        // String itself; a name resolves through the restricted loader.
        "!str" | "!!str" | "tag:yaml.org,2002:str" | "!ruby/string" | "!str:" | "!ruby/string:" => {
            Ok(PsychScalar::Str(text.to_string()))
        }
        _ if tag.starts_with("!str:") || tag.starts_with("!ruby/string:") => {
            // `(?::(.*))?` — `$1` is the WHOLE tail, `!str:Foo::Bar` names
            // `Foo::Bar`. `resolve_class($1)`: `Symbol` is the one
            // permitted class — `Symbol.allocate` then dies
            // `allocator undefined` upstream; an empty name is `nil`.
            let name = tag
                .strip_prefix("!str:")
                .or_else(|| tag.strip_prefix("!ruby/string:"))
                .unwrap_or_default();
            if name.is_empty() {
                Ok(PsychScalar::Str(text.to_string()))
            } else if name == "Symbol" {
                Err("TypeError: allocator undefined for Symbol".to_string())
            } else {
                disallowed(name)
            }
        }
        // `Float(@ss.tokenize(o.value))` — tokenize first (its own deaths
        // fire first), then `Kernel#Float` on the typed value.
        "!float" | "!!float" | "tag:yaml.org,2002:float" => ruby_float_of(tokenize_scalar(text)?),
        "!ruby/regexp" => disallowed("Regexp"),
        "!ruby/range" => disallowed("Range"),
        "!ruby/object:BigDecimal" => disallowed("BigDecimal"),
        "!ruby/object:DateTime" => disallowed("DateTime"),
        "!ruby/object:Complex" => disallowed("Complex"),
        "!ruby/object:Rational" => disallowed("Rational"),
        // `resolve_class o.value` — an empty name resolves to nil (a
        // `file: !ruby/class` row hits `missing \`file:\`` upstream);
        // `Symbol` alone is permitted (and keys an inert Symbol-class
        // bucket); anything else is DisallowedClass.
        "!ruby/class" | "!ruby/module" => {
            if text.is_empty() {
                Ok(PsychScalar::Nil)
            } else if text == "Symbol" {
                Ok(PsychScalar::Opaque {
                    to_s: "Symbol".to_string(),
                    class: "Class",
                })
            } else {
                disallowed(text)
            }
        }
        // `/^!ruby\/sym(bol)?:?(.*)?$/` — `.*` swallows any tail.
        _ if tag.starts_with("!ruby/sym") => Ok(PsychScalar::Symbol(text.to_string())),
        // `Encoding.find` — case-insensitive over `name_list` + `aliases`,
        // loading to the CANONICAL name (`ascii` → `US-ASCII`, whose `to_s`
        // is the name, not `#<Encoding:…>`); unknown names die
        // `ArgumentError: unknown encoding name - foo`.
        "!ruby/encoding" => {
            let upper = text.to_uppercase();
            // `Encoding.find`'s pseudo-names: `internal` → the nil internal
            // encoding (a `missing \`file:\`` upstream — probed), the env
            // defaults `external`/`locale`/`filesystem` → UTF-8 here.
            if upper == "INTERNAL" {
                return Ok(PsychScalar::Nil);
            }
            if matches!(upper.as_str(), "EXTERNAL" | "LOCALE" | "FILESYSTEM") {
                return Ok(PsychScalar::Opaque {
                    to_s: "UTF-8".to_string(),
                    class: "Encoding",
                });
            }
            match ENCODING_TABLE
                .iter()
                .find(|(name, _)| *name == upper)
                .map(|(_, canonical)| *canonical)
            {
                Some(canonical) => Ok(PsychScalar::Opaque {
                    to_s: canonical.to_string(),
                    class: "Encoding",
                }),
                None => Err(format!(
                    "ArgumentError: unknown encoding name - {text}"
                )),
            }
        }
        // `!foo`, `!!int`/`!!bool`/`!!null`, `!omap`, `!map:K`,
        // `!ruby/object:K`, `!ruby/exception:K`… — every tag with no
        // scalar arm tokenizes the text (`file: !foo '5'` → Integer 5,
        // probed).
        _ => tokenize_scalar(text),
    }
}

/// `ScalarScanner#tokenize` under `permitted_classes: [Symbol]` — Psych
/// 5.3.1's arm order, sharing the `.rigor.yml` loader's regexes
/// (`crate::config::psych_error`). The Date/Time arms die
/// `Psych::DisallowedClass` upstream (the restricted loader has no Date);
/// the FLOAT arm's `Float()` can die `ArgumentError` (`count: .e+1`).
fn tokenize_scalar(s: &str) -> Result<PsychScalar, String> {
    use crate::config::psych_error as pe;
    if s.is_empty() {
        return Ok(PsychScalar::Nil);
    }
    // The ALPHA arm — word-only scalars ≤5 chars resolve `~`/`null`/the
    // YAML 1.1 bool words; everything else in the arm is a String, as is
    // any `\n`-containing scalar (a folded/tagged block's text). Ruby's
    // `string.length` counts chars, not bytes.
    if pe::alpha_match(s) || s.contains('\n') {
        if s.chars().count() > 5 {
            return Ok(PsychScalar::Str(s.to_string()));
        }
        let first = s.chars().next().unwrap_or('\0').to_ascii_lowercase();
        if !matches!(first, 'y' | 't' | 'o' | 'n' | 'f' | '~') {
            return Ok(PsychScalar::Str(s.to_string()));
        }
        if s == "~" || s.eq_ignore_ascii_case("null") {
            return Ok(PsychScalar::Nil);
        }
        let lower = s.to_ascii_lowercase();
        return Ok(match lower.as_str() {
            "yes" | "true" | "on" => PsychScalar::Bool(true),
            "no" | "false" | "off" => PsychScalar::Bool(false),
            _ => PsychScalar::Str(s.to_string()),
        });
    }
    if pe::time_match(s) {
        return Err("Psych::DisallowedClass: Tried to load unspecified class: Time".to_string());
    }
    if pe::date_match(s) {
        return Err("Psych::DisallowedClass: Tried to load unspecified class: Date".to_string());
    }
    if s.eq_ignore_ascii_case(".inf") || s.eq_ignore_ascii_case("+.inf") {
        return Ok(PsychScalar::Float(f64::INFINITY));
    }
    if s.eq_ignore_ascii_case("-.inf") {
        return Ok(PsychScalar::Float(f64::NEG_INFINITY));
    }
    if s.eq_ignore_ascii_case(".nan") {
        return Ok(PsychScalar::Float(f64::NAN));
    }
    // `/^:(['"])(.*)\1/` is NOT end-anchored — `(.*)\1` backtracks to the
    // LAST matching quote (`:'x'y` → `:x`); a `:'x` with no closing quote
    // falls through to the plain `/^:(.*)/` arm, name `'x`.
    if s.len() > 1 && s.starts_with(':') {
        let rest = &s[1..];
        let name = match rest.chars().next() {
            Some(q @ ('\'' | '"')) => match rest[1..].rfind(q) {
                Some(i) => rest[1..][..i].strip_prefix(':').unwrap_or(&rest[1..][..i]),
                None => rest.strip_prefix(':').unwrap_or(rest),
            },
            _ => rest.strip_prefix(':').unwrap_or(rest),
        };
        return Ok(PsychScalar::Symbol(name.to_string()));
    }
    if let Some(i) = sexagesimal_int(s) {
        return Ok(PsychScalar::Int(i));
    }
    if let Some(f) = sexagesimal_float(s) {
        return Ok(PsychScalar::Float(f));
    }
    if pe::psych_float_match(s) {
        if pe::bare_dot_match(s) {
            return Ok(PsychScalar::Str(s.to_string()));
        }
        // The arm's `Float(string.delete(',_').gsub(/\.([Ee]|$)/, '\\1'))` —
        // a `.` before `e`/`E` or the end is dropped (`1.`→`1`, `1.e+5`→
        // `1e+5`); a mantissa-empty form dies `ArgumentError` upstream.
        let normalized: String = s
            .char_indices()
            .filter(|(_, c)| !matches!(c, '_' | ','))
            .filter(|(i, c)| {
                !(*c == '.' && (s[i + 1..].starts_with(['e', 'E']) || i + 1 == s.len()))
            })
            .map(|(_, c)| c)
            .collect();
        return match normalized.parse::<f64>() {
            Ok(f) => Ok(PsychScalar::Float(f)),
            Err(_) => Err(format!(
                "ArgumentError: invalid value for Float(): \"{normalized}\""
            )),
        };
    }
    if let Some(i) = psych_int(s) {
        return Ok(PsychScalar::Int(i));
    }
    Ok(PsychScalar::Str(s.to_string()))
}

/// `ScalarScanner`'s INTEGER_LEGACY arm — base 2/8/10/16 with `,`/`_`
/// separators and a sign — then `parse_int`'s `Integer(delete(',_'))`.
/// Returns the value saturated to i128 and the canonical decimal text
/// (`0x10` → `16`, `-0x10` → `-16`); `None` when no arm matches
/// (`0o17`/`08`/`1__0`/`1,,0` are Strings upstream — probed).
fn psych_int(s: &str) -> Option<PsychInt> {
    let (neg, t) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let strip = |x: &str| -> String { x.chars().filter(|c| !matches!(c, '_' | ',')).collect() };
    let (digits, base) = if let Some(rest) = t.strip_prefix("0b") {
        // `0b[_,]*[0-1][0-1_,]*`
        let rest = rest.trim_start_matches(['_', ',']);
        if rest.is_empty()
            || !rest.starts_with(['0', '1'])
            || !rest.chars().all(|c| matches!(c, '0' | '1' | '_' | ','))
        {
            return None;
        }
        (strip(&t[2..]), 2)
    } else if let Some(rest) = t.strip_prefix("0x") {
        // `0x[_,]*[0-9a-fA-F][0-9a-fA-F_,]*`
        let rest = rest.trim_start_matches(['_', ',']);
        if rest.is_empty()
            || !rest.starts_with(|c: char| c.is_ascii_hexdigit())
            || !rest
                .chars()
                .all(|c| c.is_ascii_hexdigit() || matches!(c, '_' | ','))
        {
            return None;
        }
        (strip(&t[2..]), 16)
    } else if t == "0" {
        ("0".to_string(), 8)
    } else if t.starts_with('0') && !t.starts_with("0x") {
        // `0[_,]*[0-7][0-7_,]*` — leading-zero octal (`017` → 15, `0_7` → 7;
        // `08` fails → String).
        let rest = t[1..].trim_start_matches(['_', ',']);
        if rest.is_empty()
            || !rest.chars().all(|c| matches!(c, '0'..='7' | '_' | ','))
            || !rest.starts_with(|c: char| ('0'..='7').contains(&c))
        {
            return None;
        }
        (strip(t), 8)
    } else {
        // `0|[1-9](?:[0-9]|,[0-9]|_[0-9])*` — every separator must precede
        // a digit (`1__0`, `1,,0`, `1_`, `1,` fail → String).
        if !t.starts_with(|c: char| c.is_ascii_digit() && c != '0') {
            return None;
        }
        let chars: Vec<char> = t.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            match c {
                '0'..='9' => {}
                '_' | ',' if chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()) => {}
                _ => return None,
            }
        }
        (strip(t), 10)
    };
    let (magnitude, decimal) = digits_to_decimal(&digits, base);
    let value = if neg { -magnitude } else { magnitude };
    let digits = if neg && magnitude != 0 {
        format!("-{decimal}")
    } else {
        decimal
    };
    Some(PsychInt { value, digits })
}

/// The shared shape check for the two sexagesimal arms:
/// `^[-+]?[0-9][0-9_]*(:[0-5]?[0-9]){1,2}` — a `[0-9][0-9_]*` head segment
/// (with its own sign) plus 1–2 `:[0-5]?[0-9]` segments. Returns the
/// `:`-split segments (the last may carry the Float arm's `.[0-9_]*` tail).
fn sexa_segments(s: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let head = parts[0].strip_prefix(['+', '-']).unwrap_or(parts[0]);
    if head.is_empty()
        || !head.starts_with(|c: char| c.is_ascii_digit())
        || !head.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    for (i, p) in parts.iter().enumerate().skip(1) {
        // `[0-5]?[0-9]` — one or two digits, leading digit ≤ 5; the LAST
        // segment may carry the Float arm's `.[0-9_]*` fraction.
        let int_part = if i == parts.len() - 1 {
            p.split('.').next().unwrap_or(p)
        } else {
            p
        };
        if int_part.is_empty()
            || int_part.len() > 2
            || !int_part.chars().all(|c| c.is_ascii_digit())
            || (int_part.len() == 2 && int_part.as_bytes()[0] > b'5')
        {
            return None;
        }
    }
    Some(parts)
}

/// Ruby `String#to_i` — optional sign, then leading digits with `_`
/// separators BETWEEN digits (`"1_0"` → 10, `"1__0"` → 1 — to_i stops at
/// the doubled `_`), saturated.
fn str_to_i(s: &str) -> i128 {
    let (neg, b) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits = digit_run(b);
    let v = digits.parse::<i128>().unwrap_or(0).min(i128::MAX / 3601);
    if neg {
        -v
    } else {
        v
    }
}

/// The `to_i`/`to_f` digit run at the head of `s`: digits, with `_` kept
/// only when a digit follows (`1_0` → `10`, `1__0`/`1_` stop).
fn digit_run(b: &str) -> String {
    let chars: Vec<char> = b.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            c if c.is_ascii_digit() => {
                out.push(c);
                i += 1;
            }
            '_' if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() => i += 1,
            _ => break,
        }
    }
    out
}

/// Ruby `String#to_f` — optional sign, then the longest float prefix:
/// int digits with `_` separators, `.` + frac digits with `_` (`30.5_5` →
/// 30.55; a `.` with no frac digits ends the run before it — `"1."` → 1.0).
fn str_to_f(s: &str) -> f64 {
    let (neg, b) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let chars: Vec<char> = b.chars().collect();
    let mut pos = 0;
    while pos < chars.len() {
        match chars[pos] {
            c if c.is_ascii_digit() => pos += 1,
            '_' if pos > 0
                && chars[pos - 1].is_ascii_digit()
                && pos + 1 < chars.len()
                && chars[pos + 1].is_ascii_digit() =>
            {
                pos += 1
            }
            _ => break,
        }
    }
    if pos < chars.len() && chars[pos] == '.' {
        let mut p = pos + 1;
        let mut any = false;
        while p < chars.len() {
            match chars[p] {
                c if c.is_ascii_digit() => {
                    any = true;
                    p += 1;
                }
                '_' if any && p + 1 < chars.len() && chars[p + 1].is_ascii_digit() => p += 1,
                _ => break,
            }
        }
        if any {
            pos = p;
        }
    }
    let end = b.char_indices().nth(pos).map_or(b.len(), |(i, _)| i);
    let v: f64 = b[..end].replace('_', "").parse().unwrap_or(0.0);
    if neg {
        -v
    } else {
        v
    }
}

/// `^[-+]?[0-9][0-9_]*(:[0-5]?[0-9]){1,2}$` — base-60 sexagesimal Integer,
/// summed upstream as `n.to_i * 60**(e-2).abs` per segment (`1:30` → 5400,
/// `1:0` → 3600 — the sign rides the FIRST segment only).
fn sexagesimal_int(s: &str) -> Option<PsychInt> {
    // Reject a `.` anywhere — that's the Float arm's shape.
    if s.contains('.') {
        return None;
    }
    let parts = sexa_segments(s)?;
    let mut value: i128 = 0;
    for (e, p) in parts.iter().enumerate() {
        let weight: i128 = [3600, 60, 1][e.min(2)];
        value = value.saturating_add(str_to_i(p).saturating_mul(weight));
    }
    Some(PsychInt {
        value,
        digits: value.to_string(),
    })
}

/// `^[-+]?[0-9][0-9_]*(:[0-5]?[0-9]){1,2}\.[0-9_]*$` — the Float twin
/// (`n.to_f` per segment; `1:30.5` → 5430.0).
fn sexagesimal_float(s: &str) -> Option<f64> {
    // The `.` must sit in the LAST segment's tail (`\.[0-9_]*$`).
    let (head, frac) = s.rsplit_once('.')?;
    if !frac.chars().all(|c| c.is_ascii_digit() || c == '_') || head.contains('.') {
        return None;
    }
    let parts = sexa_segments(s)?;
    let mut value = 0f64;
    for (e, p) in parts.iter().enumerate() {
        let weight: f64 = [3600.0, 60.0, 1.0][e.min(2)];
        value += str_to_f(p) * weight;
    }
    Some(value)
}

/// `Float(tokenize(text))` for `!float` — the tagged conversion arm.
/// `Kernel#Float` accepts Integers/Floats directly and strict numeric
/// strings; everything else is `can't convert X into Float` (TypeError) or
/// `invalid value for Float(): "…"` (ArgumentError) — both uncaught.
fn ruby_float_of(v: PsychScalar) -> Result<PsychScalar, String> {
    match v {
        PsychScalar::Int(i) => Ok(PsychScalar::Float(i.value as f64)),
        PsychScalar::Float(f) => Ok(PsychScalar::Float(f)),
        PsychScalar::Str(s) => ruby_float(&s).map(PsychScalar::Float),
        PsychScalar::Nil => Err("TypeError: can't convert nil into Float".to_string()),
        PsychScalar::Bool(b) => Err(format!("TypeError: can't convert {b} into Float")),
        PsychScalar::Symbol(_) => Err("TypeError: can't convert Symbol into Float".to_string()),
        PsychScalar::Opaque { class, .. } => {
            Err(format!("TypeError: can't convert {class} into Float"))
        }
    }
}

/// `Kernel#Float(s)` — the strict grammar upstream applies to `!float`
/// text: `[+-]?\d+(_\d+)*(\.\d+(_\d+)*)?([eE][+-]?\d+(_\d+)*)?` inside
/// optional whitespace, or `0x` hex. NO comma separators, no
/// `Infinity`/`NaN` words, no bare `1.`/`.5` — each dies `ArgumentError`
/// (probed).
fn ruby_float(s: &str) -> Result<f64, String> {
    let t = s.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if let Some(hex) = body.strip_prefix("0x") {
        if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let v = i128::from_str_radix(hex, 16).unwrap_or(i128::MAX);
            return Ok(if neg { -(v as f64) } else { v as f64 });
        }
    }
    // Strict grammar check before parsing: `_` only between digits.
    let valid = {
        let chars: Vec<char> = body.chars().collect();
        let mut i = 0;
        let num_run = |i: &mut usize| {
            let start = *i;
            while *i < chars.len() {
                match chars[*i] {
                    c if c.is_ascii_digit() => *i += 1,
                    '_' if *i + 1 < chars.len() && chars[*i + 1].is_ascii_digit() => *i += 1,
                    _ => break,
                }
            }
            *i > start
        };
        let mut ok = num_run(&mut i);
        if ok && i < chars.len() && chars[i] == '.' {
            i += 1;
            ok = num_run(&mut i);
        }
        if ok
            && i < chars.len()
            && (chars[i] == 'e' || chars[i] == 'E')
        {
            i += 1;
            if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                i += 1;
            }
            ok = num_run(&mut i);
        }
        ok && i == chars.len()
    };
    if !valid {
        return Err(format!("ArgumentError: invalid value for Float(): \"{s}\""));
    }
    body.replace('_', "")
        .parse::<f64>()
        .map(|v| if neg { -v } else { v })
        .map_err(|_| format!("ArgumentError: invalid value for Float(): \"{s}\""))
}

/// Ruby `Float#to_s`/`inspect` — `1.5`, `3.0`, `Infinity`, `NaN`,
/// `1.0e+20`, `1.0e-05`. Rust's `{:?}` already emits the shortest
/// round-trip form; the fixes are a `.0` mantissa, a signed exponent, and
/// a 2-digit exponent pad.
fn ruby_float_to_s(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f == f64::INFINITY {
        return "Infinity".to_string();
    }
    if f == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    let s = format!("{f:?}");
    match s.find(['e', 'E']) {
        Some(ei) => {
            let (m, e) = s.split_at(ei);
            let m = if m.contains('.') {
                m.to_string()
            } else {
                format!("{m}.0")
            };
            let exp: i32 = e[1..].parse().unwrap_or(0);
            let sign = if exp < 0 { '-' } else { '+' };
            format!("{m}e{sign}{:02}", exp.abs())
        }
        None if s.contains('.') => s,
        None => format!("{s}.0"),
    }
}

/// Ruby `Symbol#inspect` — `:name` for identifier-ish names, `:"name"`
/// otherwise.
fn symbol_inspect(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '!' | '?' | '='));
    if plain {
        format!(":{name}")
    } else {
        format!(":{name:?}")
    }
}

/// `digits` (already `,`/`_`-stripped, no sign or `0x`/`0b` prefix) in
/// `base` → (value saturated to i128, canonical decimal text). Values past
/// i128 get the correct decimal digits via a widening multiply-add; the
/// saturated `value` still orders `== 1`/`positive?` right.
fn digits_to_decimal(digits: &str, base: u32) -> (i128, String) {
    // First pass — the i128 value, detecting overflow.
    let mut acc: i128 = 0;
    let mut overflow = false;
    for c in digits.chars() {
        let d = c.to_digit(base).unwrap_or(0) as i128;
        match acc
            .checked_mul(base as i128)
            .and_then(|a| a.checked_add(d))
        {
            Some(v) if !overflow => acc = v,
            _ => overflow = true,
        }
    }
    if !overflow {
        return (acc, acc.to_string());
    }
    // Second pass (rare): the canonical decimal text via widening
    // multiply-add — upstream's Integer is unbounded.
    let mut dec: Vec<u8> = Vec::new();
    for c in digits.chars() {
        let mut carry = c.to_digit(base).unwrap_or(0) as u128;
        for slot in dec.iter_mut() {
            let v = *slot as u128 * base as u128 + carry;
            *slot = (v % 10) as u8;
            carry = v / 10;
        }
        while carry > 0 {
            dec.insert(0, (carry % 10) as u8);
            carry /= 10;
        }
    }
    (
        i128::MAX,
        dec.into_iter().map(|d| (b'0' + d) as char).collect(),
    )
}

/// `Encoding.find` name→canonical table, generated from the pinned Ruby 4.0.5
/// (`Encoding.name_list` minus the internal encoding, + `Encoding.aliases`;
/// `find` is case-insensitive, so keys are uppercased).
const ENCODING_TABLE: &[(&str, &str)] = &[
    ("646", "US-ASCII"),
    ("ANSI_X3.4-1968", "US-ASCII"),
    ("ASCII", "US-ASCII"),
    ("ASCII-8BIT", "ASCII-8BIT"),
    ("BIG5", "Big5"),
    ("BIG5-HKSCS", "Big5-HKSCS"),
    ("BIG5-HKSCS:2008", "Big5-HKSCS"),
    ("BIG5-UAO", "Big5-UAO"),
    ("BINARY", "ASCII-8BIT"),
    ("CESU-8", "CESU-8"),
    ("CP1250", "Windows-1250"),
    ("CP1251", "Windows-1251"),
    ("CP1252", "Windows-1252"),
    ("CP1253", "Windows-1253"),
    ("CP1254", "Windows-1254"),
    ("CP1255", "Windows-1255"),
    ("CP1256", "Windows-1256"),
    ("CP1257", "Windows-1257"),
    ("CP1258", "Windows-1258"),
    ("CP437", "IBM437"),
    ("CP50220", "CP50220"),
    ("CP50221", "CP50221"),
    ("CP51932", "CP51932"),
    ("CP65000", "UTF-7"),
    ("CP65001", "UTF-8"),
    ("CP720", "IBM720"),
    ("CP737", "IBM737"),
    ("CP775", "IBM775"),
    ("CP850", "CP850"),
    ("CP852", "CP852"),
    ("CP855", "CP855"),
    ("CP857", "IBM857"),
    ("CP860", "IBM860"),
    ("CP861", "IBM861"),
    ("CP862", "IBM862"),
    ("CP863", "IBM863"),
    ("CP864", "IBM864"),
    ("CP865", "IBM865"),
    ("CP866", "IBM866"),
    ("CP869", "IBM869"),
    ("CP874", "Windows-874"),
    ("CP878", "KOI8-R"),
    ("CP932", "Windows-31J"),
    ("CP936", "GBK"),
    ("CP949", "CP949"),
    ("CP950", "CP950"),
    ("CP951", "CP951"),
    ("CSWINDOWS31J", "Windows-31J"),
    ("EBCDIC-CP-US", "IBM037"),
    ("EMACS-MULE", "Emacs-Mule"),
    ("EUC-CN", "GB2312"),
    ("EUC-JIS-2004", "EUC-JIS-2004"),
    ("EUC-JISX0213", "EUC-JIS-2004"),
    ("EUC-JP", "EUC-JP"),
    ("EUC-JP-MS", "eucJP-ms"),
    ("EUC-KR", "EUC-KR"),
    ("EUC-TW", "EUC-TW"),
    ("EUCCN", "GB2312"),
    ("EUCJP", "EUC-JP"),
    ("EUCJP-MS", "eucJP-ms"),
    ("EUCKR", "EUC-KR"),
    ("EUCTW", "EUC-TW"),
    ("EXTERNAL", "UTF-8"),
    ("FILESYSTEM", "UTF-8"),
    ("GB12345", "GB12345"),
    ("GB18030", "GB18030"),
    ("GB1988", "GB1988"),
    ("GB2312", "GB2312"),
    ("GBK", "GBK"),
    ("IBM037", "IBM037"),
    ("IBM437", "IBM437"),
    ("IBM720", "IBM720"),
    ("IBM737", "IBM737"),
    ("IBM775", "IBM775"),
    ("IBM850", "CP850"),
    ("IBM852", "IBM852"),
    ("IBM855", "IBM855"),
    ("IBM857", "IBM857"),
    ("IBM860", "IBM860"),
    ("IBM861", "IBM861"),
    ("IBM862", "IBM862"),
    ("IBM863", "IBM863"),
    ("IBM864", "IBM864"),
    ("IBM865", "IBM865"),
    ("IBM866", "IBM866"),
    ("IBM869", "IBM869"),
    ("ISO-2022-JP", "ISO-2022-JP"),
    ("ISO-2022-JP-2", "ISO-2022-JP-2"),
    ("ISO-2022-JP-KDDI", "ISO-2022-JP-KDDI"),
    ("ISO-8859-1", "ISO-8859-1"),
    ("ISO-8859-10", "ISO-8859-10"),
    ("ISO-8859-11", "ISO-8859-11"),
    ("ISO-8859-13", "ISO-8859-13"),
    ("ISO-8859-14", "ISO-8859-14"),
    ("ISO-8859-15", "ISO-8859-15"),
    ("ISO-8859-16", "ISO-8859-16"),
    ("ISO-8859-2", "ISO-8859-2"),
    ("ISO-8859-3", "ISO-8859-3"),
    ("ISO-8859-4", "ISO-8859-4"),
    ("ISO-8859-5", "ISO-8859-5"),
    ("ISO-8859-6", "ISO-8859-6"),
    ("ISO-8859-7", "ISO-8859-7"),
    ("ISO-8859-8", "ISO-8859-8"),
    ("ISO-8859-9", "ISO-8859-9"),
    ("ISO2022-JP", "ISO-2022-JP"),
    ("ISO2022-JP2", "ISO-2022-JP-2"),
    ("ISO8859-1", "ISO-8859-1"),
    ("ISO8859-10", "ISO-8859-10"),
    ("ISO8859-11", "ISO-8859-11"),
    ("ISO8859-13", "ISO-8859-13"),
    ("ISO8859-14", "ISO-8859-14"),
    ("ISO8859-15", "ISO-8859-15"),
    ("ISO8859-16", "ISO-8859-16"),
    ("ISO8859-2", "ISO-8859-2"),
    ("ISO8859-3", "ISO-8859-3"),
    ("ISO8859-4", "ISO-8859-4"),
    ("ISO8859-5", "ISO-8859-5"),
    ("ISO8859-6", "ISO-8859-6"),
    ("ISO8859-7", "ISO-8859-7"),
    ("ISO8859-8", "ISO-8859-8"),
    ("ISO8859-9", "ISO-8859-9"),
    ("KOI8-R", "KOI8-R"),
    ("KOI8-U", "KOI8-U"),
    ("LOCALE", "UTF-8"),
    ("MACCENTEURO", "macCentEuro"),
    ("MACCROATIAN", "macCroatian"),
    ("MACCYRILLIC", "macCyrillic"),
    ("MACGREEK", "macGreek"),
    ("MACICELAND", "macIceland"),
    ("MACJAPAN", "MacJapanese"),
    ("MACJAPANESE", "MacJapanese"),
    ("MACROMAN", "macRoman"),
    ("MACROMANIA", "macRomania"),
    ("MACTHAI", "macThai"),
    ("MACTURKISH", "macTurkish"),
    ("MACUKRAINE", "macUkraine"),
    ("PCK", "Windows-31J"),
    ("SHIFT_JIS", "Shift_JIS"),
    ("SJIS", "Windows-31J"),
    ("SJIS-DOCOMO", "SJIS-DoCoMo"),
    ("SJIS-KDDI", "SJIS-KDDI"),
    ("SJIS-SOFTBANK", "SJIS-SoftBank"),
    ("STATELESS-ISO-2022-JP", "stateless-ISO-2022-JP"),
    ("STATELESS-ISO-2022-JP-KDDI", "stateless-ISO-2022-JP-KDDI"),
    ("TIS-620", "TIS-620"),
    ("UCS-2BE", "UTF-16BE"),
    ("UCS-4BE", "UTF-32BE"),
    ("UCS-4LE", "UTF-32LE"),
    ("US-ASCII", "US-ASCII"),
    ("UTF-16", "UTF-16"),
    ("UTF-16BE", "UTF-16BE"),
    ("UTF-16LE", "UTF-16LE"),
    ("UTF-32", "UTF-32"),
    ("UTF-32BE", "UTF-32BE"),
    ("UTF-32LE", "UTF-32LE"),
    ("UTF-7", "UTF-7"),
    ("UTF-8", "UTF-8"),
    ("UTF-8-HFS", "UTF8-MAC"),
    ("UTF-8-MAC", "UTF8-MAC"),
    ("UTF8-DOCOMO", "UTF8-DoCoMo"),
    ("UTF8-KDDI", "UTF8-KDDI"),
    ("UTF8-MAC", "UTF8-MAC"),
    ("UTF8-SOFTBANK", "UTF8-SoftBank"),
    ("WINDOWS-1250", "Windows-1250"),
    ("WINDOWS-1251", "Windows-1251"),
    ("WINDOWS-1252", "Windows-1252"),
    ("WINDOWS-1253", "Windows-1253"),
    ("WINDOWS-1254", "Windows-1254"),
    ("WINDOWS-1255", "Windows-1255"),
    ("WINDOWS-1256", "Windows-1256"),
    ("WINDOWS-1257", "Windows-1257"),
    ("WINDOWS-1258", "Windows-1258"),
    ("WINDOWS-31J", "Windows-31J"),
    ("WINDOWS-874", "Windows-874"),
];

/// `o.value.unpack('m').first` — a lenient base64 decoder (whitespace and
/// non-alphabet bytes are skipped, like Ruby's `unpack('m')`).
fn base64_decode(text: &str) -> String {
    let mut bits = 0u32;
    let mut nbits = 0u32;
    let mut out = Vec::new();
    for &b in text.as_bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        bits = (bits << 6) | v as u32;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Ruby's `Regexp.escape` — escape regex metacharacters and whitespace so the
/// stored source matches the literal message. The character map is taken
/// verbatim from Ruby (`onig_quote`): control whitespace becomes its escape,
/// metacharacters get a leading backslash. Must stay byte-identical so a
/// rigor-rs-generated message row equals a reference-generated one.
#[must_use]
pub fn regexp_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\x0b' => out.push_str("\\v"),
            '\x0c' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ' ' | '#' | '$' | '(' | ')' | '*' | '+' | '-' | '.' | '?' | '[' | '\\' | ']'
            | '^' | '{' | '|' | '}' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// `Psych::Visitors::YAMLTree#visit_String`'s scalar style for one value —
/// the reference's emit rules, in its check order (probed against
/// `YAML.dump` at the pin):
///
///   embedded `\n`             → `|`/`|-` block literal (folding can't carry
///                               a newline; Psych never gets here for
///                               `Regexp.escape`d messages, which have none)
///   `"<<"`                    → `!!str '<<'` (a bare `<<` is the merge key)
///   `y` `Y` `n` `N`           → double-quoted (YAML 1.1 booleans)
///   `^[^[:word:]][^"]*$`      → double-quoted — the `../…` and `-x` paths,
///                               a leading `.`/`#`/indicator char
///   else-if ScalarScanner would reparse the plain form as a non-String
///   (`: `, ` #`, number/bool/date/`:`-symbol shapes) → single-quoted
///   otherwise                 → plain (folded past column 80)
enum YamlStyle {
    Plain,
    Single,
    Double,
    TaggedSingle,
    Literal,
}

fn scalar_style(s: &str) -> YamlStyle {
    // `o.match?(/\n(?!\Z)/)` — a newline with content after it.
    if s.find('\n').is_some_and(|i| i + 1 < s.len()) {
        return YamlStyle::Literal;
    }
    if s == "<<" {
        return YamlStyle::TaggedSingle;
    }
    if matches!(s, "y" | "Y" | "n" | "N") {
        return YamlStyle::Double;
    }
    if let Some(first) = s.chars().next() {
        // Ruby `[:word:]` ≈ `[a-zA-Z0-9_]` plus Unicode alphanumerics.
        if !(first.is_alphanumeric() || first == '_' || s.contains('"')) {
            return YamlStyle::Double;
        }
    }
    if needs_single_quoting(s) {
        return YamlStyle::Single;
    }
    if s.contains('\t') || s.contains('\n') || s.chars().any(|c| c.is_control()) {
        // Tab / stray control chars can't ride a plain scalar; escape them.
        return YamlStyle::Double;
    }
    YamlStyle::Plain
}

/// Emit `prefix + key + '\n'` for a bucket's `file:`/`rule:` — the String
/// styling of [`push_kv`] for `Str` keys; a typed non-String key emits its
/// canonical scalar text (`file: 16` for `0x10`, `file: :x` for a symbol).
fn push_key(out: &mut String, prefix: &str, key: &KeyScalar) {
    match key {
        KeyScalar::Str(s) => push_kv(out, prefix, 2, s),
        other => {
            out.push_str(prefix);
            out.push_str(&other.yaml_emit());
            out.push('\n');
        }
    }
}

/// Emit `prefix + scalar + '\n'`, folding a PLAIN scalar libyaml-style: when
/// the line is already past column 80, a space is replaced by a break and the
/// scalar resumes at `member_col + 2`. Quoted and block styles never fold
/// (probed: Psych emits them on one line / as `|-` content lines).
fn push_kv(out: &mut String, prefix: &str, member_col: usize, value: &str) {
    out.push_str(prefix);
    match scalar_style(value) {
        YamlStyle::Literal => {
            // Chomping marker: `|+` keeps ≥2 trailing newlines, `|` keeps
            // one, `|-` strips. Content lines indent at member_col + 2.
            let header = if value.ends_with("\n\n") {
                "|+"
            } else if value.ends_with('\n') {
                "|"
            } else {
                "|-"
            };
            out.push_str(header);
            out.push('\n');
            let ind = " ".repeat(member_col + 2);
            let mut lines = value.split('\n').collect::<Vec<_>>();
            // `split` leaves a trailing "" for the terminal newline — the
            // line break itself, not content.
            if lines.last() == Some(&"") {
                lines.pop();
            }
            for line in lines {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(&ind);
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        YamlStyle::TaggedSingle => {
            out.push_str(&format!("!!str '{}'", value.replace('\'', "''")));
            out.push('\n');
        }
        YamlStyle::Single => {
            out.push_str(&format!("'{}'", value.replace('\'', "''")));
            out.push('\n');
        }
        YamlStyle::Double => {
            out.push_str(&double_quote(value));
            out.push('\n');
        }
        YamlStyle::Plain => {
            let mut col = prefix.chars().count();
            for c in value.chars() {
                if c == ' ' && col > 80 {
                    out.push('\n');
                    for _ in 0..member_col + 2 {
                        out.push(' ');
                    }
                    col = member_col + 2;
                } else {
                    out.push(c);
                    col += 1;
                }
            }
            out.push('\n');
        }
    }
}

/// A `"…"`-quoted YAML scalar with the escapes libyaml writes.
fn double_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The reader side of [`push_kv`]'s quoted styles: strip a surrounding quote
/// pair and unescape, else return the plain scalar as-is.
fn unyaml_scalar(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s[1..s.len() - 1].replace("''", "'");
    }
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        // Double-quoted: handle the common backslash escapes Psych would emit.
        let inner = &s[1..s.len() - 1];
        return unescape_double(inner);
    }
    s.to_string()
}

fn unescape_double(s: &str) -> String {
    // YAML 1.1 double-quoted escapes (Psych/libyaml's set, not just the common
    // five) — `\x`/`\u`/`\U` are hex codepoints; `\N`/`\_`/`\L`/`\P` are the
    // Unicode space/separator escapes; an unknown escape keeps its backslash
    // (Psych would SyntaxError — a non-matching bucket is the safe decline).
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('0') => out.push('\0'),
                Some('a') => out.push('\u{7}'),
                Some('b') => out.push('\u{8}'),
                Some('n') => out.push('\n'),
                Some('t' | '\t') => out.push('\t'),
                Some('v') => out.push('\u{b}'),
                Some('f') => out.push('\u{c}'),
                Some('r') => out.push('\r'),
                Some('e') => out.push('\u{1b}'),
                Some('"') => out.push('"'),
                Some('/') => out.push('/'),
                Some('\\') => out.push('\\'),
                Some(' ') => out.push(' '),
                Some('N') => out.push('\u{85}'),
                Some('_') => out.push('\u{a0}'),
                Some('L') => out.push('\u{2028}'),
                Some('P') => out.push('\u{2029}'),
                Some('x') => out.push(hex_escape(&mut chars, 2)),
                Some('u') => out.push(hex_escape(&mut chars, 4)),
                Some('U') => out.push(hex_escape(&mut chars, 8)),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Decode a `\xHH`/`\uHHHH`/`\UHHHHHHHH` codepoint escape; an invalid or
/// non-scalar-value escape decodes to U+FFFD (Psych would SyntaxError).
fn hex_escape(chars: &mut std::str::Chars<'_>, width: usize) -> char {
    let mut v = 0u32;
    for _ in 0..width {
        let Some(c) = chars.next() else { return '\u{fffd}' };
        let Some(d) = c.to_digit(16) else { return '\u{fffd}' };
        v = v * 16 + d;
    }
    char::from_u32(v).unwrap_or('\u{fffd}')
}

/// Whether a value must be single-quoted because its plain form would reparse
/// as a non-String — Psych's `not String === @ss.tokenize(o)` check, ported
/// for the value shapes a baseline can hold. (Leading non-word chars are the
/// caller's earlier double-quote branch; these fire only for word-started or
/// `"`-containing values.)
fn needs_single_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // `": "` inside would start an implicit mapping; ` #` opens a comment.
    if s.contains(": ") || s.contains(" #") {
        return true;
    }
    let first = s.chars().next().unwrap();
    if matches!(
        first,
        '!' | '&' | '*' | '?' | '|' | '>' | '%' | '@' | '`' | '"' | '\'' | '#' | ',' | '['
            | ']' | '{' | '}' | ' '
    ) {
        return true;
    }
    if s.ends_with(' ') || s.ends_with(':') {
        return true;
    }
    // ScalarScanner shapes that load as a non-String: YAML 1.1 bools/null,
    // integers/floats (with `_` separators and 0x/0o/0b/leading-0 forms),
    // `.inf`/`.nan`, timestamps, and `:`-led symbols.
    scans_as_nonstring(s)
}

/// `Psych::ScalarScanner#tokenize` returning non-String, approximated for the
/// values a baseline row can hold.
fn scans_as_nonstring(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "yes" | "no" | "true" | "false" | "on" | "off" | "null" | "~" | ".nan" | ".inf" | "-.inf"
            | "+.inf"
    ) {
        return true;
    }
    if s.starts_with(':') {
        return true;
    }
    let t = s.strip_prefix(['+', '-']).unwrap_or(s);
    let digits = |x: &str| {
        !x.is_empty()
            && x.chars().next().unwrap().is_ascii_digit()
            && x.chars().all(|c| c.is_ascii_digit() || c == '_')
    };
    if digits(t) {
        return true;
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        if !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit() || c == '_') {
            return true;
        }
    }
    if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        if !o.is_empty() && o.chars().all(|c| ('0'..='7').contains(&c) || c == '_') {
            return true;
        }
    }
    if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        if !b.is_empty() && b.chars().all(|c| matches!(c, '0' | '1' | '_')) {
            return true;
        }
    }
    // Floats: `1.0`, `1e3`, `1.2e-3`, `.5`, sexagesimal `1:2`… plus Psych's
    // special `/\A0[0-7]*[89]/` (invalid octal like `09` STILL gets quoted).
    if s.len() >= 2 && s.starts_with('0') && s[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    if s.parse::<f64>().is_ok() && s.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.' || c == '+' || c == '-') {
        return true;
    }
    // `YYYY-MM-DD` and timestamps.
    if s.len() >= 8
        && s.as_bytes()[0].is_ascii_digit()
        && s.as_bytes()[4] == b'-'
        && s[..4].chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_rules::Severity;

    fn diag(rule_id: &'static str, message: &str) -> Diagnostic {
        Diagnostic {
            rule_id,
            start_offset: 0,
            end_offset: 0,
            message: message.to_string(),
            severity: Severity::Error,
            source_family: "builtin",
            receiver_type: None,
            receiver_classes: None,
            method_name: None,
        }
    }

    #[test]
    fn regexp_escape_matches_ruby() {
        // Mirrors `Regexp.escape("undefined method `lenght' for \"hello\"")`.
        assert_eq!(
            regexp_escape("undefined method `lenght' for \"hello\""),
            "undefined\\ method\\ `lenght'\\ for\\ \"hello\""
        );
        assert_eq!(
            regexp_escape("wrong number of arguments (1 for 2)"),
            "wrong\\ number\\ of\\ arguments\\ \\(1\\ for\\ 2\\)"
        );
        assert_eq!(regexp_escape("a.b-c"), "a\\.b\\-c");
    }

    #[test]
    fn empty_baseline_yaml_byte_layout() {
        let b = Baseline::default();
        assert_eq!(b.to_yaml(), "---\nversion: 1\nignored: []\n");
    }

    #[test]
    fn rule_mode_yaml_byte_layout() {
        let d = diag("call.undefined-method", "undefined method `lenght' for \"hello\"");
        let b = Baseline::from_diagnostics(&[("sample.rb".to_string(), &d)], MatchMode::Rule);
        assert_eq!(
            b.to_yaml(),
            "---\nversion: 1\nignored:\n- file: sample.rb\n  rule: call.undefined-method\n  count: 1\n"
        );
    }

    #[test]
    fn message_mode_yaml_byte_layout() {
        let d = diag("call.undefined-method", "undefined method `lenght' for \"hello\"");
        let b = Baseline::from_diagnostics(&[("sample.rb".to_string(), &d)], MatchMode::Message);
        assert_eq!(
            b.to_yaml(),
            "---\nversion: 1\nignored:\n- file: sample.rb\n  rule: call.undefined-method\n  \
             message: undefined\\ method\\ `lenght'\\ for\\ \"hello\"\n  count: 1\n"
        );
    }

    #[test]
    fn round_trip_rule_mode() {
        let d = diag("call.undefined-method", "m");
        let b = Baseline::from_diagnostics(&[("a.rb".to_string(), &d)], MatchMode::Rule);
        let text = b.to_yaml();
        let parsed = Baseline::parse(&text, "t").unwrap();
        assert_eq!(parsed.size(), 1);
        assert_eq!(parsed.buckets()[0].file, "a.rb");
        assert_eq!(parsed.buckets()[0].rule, "call.undefined-method");
        assert!(parsed.buckets()[0].message.is_none());
        assert_eq!(parsed.buckets()[0].count, 1);
    }

    #[test]
    fn round_trip_message_mode() {
        let d = diag("call.undefined-method", "undefined method `x' for nil");
        let b = Baseline::from_diagnostics(&[("a.rb".to_string(), &d)], MatchMode::Message);
        let parsed = Baseline::parse(&b.to_yaml(), "t").unwrap();
        assert_eq!(parsed.size(), 1);
        let bucket = &parsed.buckets()[0];
        assert!(bucket.message.is_some());
        // The compiled regex matches the original literal message back.
        assert!(bucket.message_regex.as_ref().unwrap().is_match("undefined method `x' for nil"));
    }

    #[test]
    fn filter_hit_suppresses_within_threshold() {
        let d = diag("call.undefined-method", "m");
        let b = Baseline::from_diagnostics(&[("a.rb".to_string(), &d)], MatchMode::Rule);
        let entries = vec![("a.rb".to_string(), &d)];
        let (surfaced, silenced) = b.filter(&entries);
        assert!(surfaced.is_empty());
        assert_eq!(silenced, 1);
    }

    #[test]
    fn filter_miss_surfaces_new_diagnostic() {
        // Baseline has one rule on a.rb; a NEW diagnostic for a different rule
        // (and a different file) must surface.
        let recorded = diag("call.undefined-method", "m");
        let b = Baseline::from_diagnostics(&[("a.rb".to_string(), &recorded)], MatchMode::Rule);

        let new_rule = diag("call.wrong-arity", "boom");
        let new_file = diag("call.undefined-method", "m");
        let entries = vec![("a.rb".to_string(), &new_rule), ("b.rb".to_string(), &new_file)];
        let (surfaced, silenced) = b.filter(&entries);
        assert_eq!(surfaced, vec![0, 1]);
        assert_eq!(silenced, 0);
    }

    #[test]
    fn filter_over_threshold_surfaces_whole_bucket() {
        // Recorded count is 1; two live diagnostics in the same bucket → both
        // surface (over-threshold), none silenced.
        let d = diag("call.undefined-method", "m");
        let b = Baseline::from_diagnostics(&[("a.rb".to_string(), &d)], MatchMode::Rule);
        let d2 = diag("call.undefined-method", "m2");
        let entries = vec![("a.rb".to_string(), &d), ("a.rb".to_string(), &d2)];
        let (surfaced, silenced) = b.filter(&entries);
        assert_eq!(surfaced, vec![0, 1]);
        assert_eq!(silenced, 0);
    }

    #[test]
    fn message_bucket_precedence_over_rule_bucket() {
        // A baseline with both a message row (count 1) and would-be rule row:
        // construct by hand-parsing a two-row file. The message bucket claims
        // the matching diagnostic; a non-matching one falls to the rule bucket.
        let text = "---\nversion: 1\nignored:\n\
                    - file: a.rb\n  rule: r\n  message: foo\n  count: 1\n\
                    - file: a.rb\n  rule: r\n  count: 5\n";
        let b = Baseline::parse(text, "t").unwrap();
        assert_eq!(b.size(), 2);

        let matches_msg = diag("r", "foo bar");
        let other = diag("r", "zzz");
        let entries = vec![("a.rb".to_string(), &matches_msg), ("a.rb".to_string(), &other)];
        let (surfaced, silenced) = b.filter(&entries);
        // Both land within their buckets' thresholds → both silenced.
        assert!(surfaced.is_empty());
        assert_eq!(silenced, 2);
    }

    #[test]
    fn unsupported_version_is_error() {
        let err = Baseline::parse("---\nversion: 2\nignored: []\n", "t").unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[test]
    fn missing_count_is_error() {
        let err = Baseline::parse("---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n", "t")
            .unwrap_err();
        assert!(err.to_string().contains("count"));
    }

    #[test]
    fn parses_empty_inline_ignored_array() {
        let b = Baseline::parse("---\nversion: 1\nignored: []\n", "t").unwrap();
        assert!(b.is_empty());
    }

    #[test]
    fn audit_computes_all_four_statuses_and_deltas() {
        // Baseline: a.rb/r1 count 2 (→ within), b.rb/r2 count 3 (→ reducible),
        // c.rb/r3 count 1 (→ over), d.rb/r4 count 1 (→ cleared).
        let text = "---\nversion: 1\nignored:\n\
                    - file: a.rb\n  rule: r1\n  count: 2\n\
                    - file: b.rb\n  rule: r2\n  count: 3\n\
                    - file: c.rb\n  rule: r3\n  count: 1\n\
                    - file: d.rb\n  rule: r4\n  count: 1\n";
        let b = Baseline::parse(text, "t").unwrap();
        let d_a = diag("r1", "m");
        let d_b = diag("r2", "m");
        let d_c = diag("r3", "m");
        let entries = vec![
            ("a.rb".to_string(), &d_a),
            ("a.rb".to_string(), &d_a), // within: 2 == 2
            ("b.rb".to_string(), &d_b), // reducible: 1 < 3
            ("c.rb".to_string(), &d_c),
            ("c.rb".to_string(), &d_c), // over: 2 > 1
            // d.rb has no diagnostics → cleared
        ];
        let rows = b.audit(&entries);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].status, DriftStatus::Within);
        assert_eq!(rows[0].actual, 2);
        assert_eq!(rows[0].delta, 0);
        assert_eq!(rows[1].status, DriftStatus::Reducible);
        assert_eq!(rows[1].actual, 1);
        assert_eq!(rows[1].delta, -2);
        assert_eq!(rows[2].status, DriftStatus::Over);
        assert_eq!(rows[2].actual, 2);
        assert_eq!(rows[2].delta, 1);
        assert_eq!(rows[3].status, DriftStatus::Cleared);
        assert_eq!(rows[3].actual, 0);
        assert_eq!(rows[3].delta, -1);
    }

    #[test]
    fn audit_respects_message_bucket_before_rule_bucket() {
        // A message bucket (count 1) claims the matching diagnostic before the
        // rule bucket (count 5) for the same (file, rule).
        let text = "---\nversion: 1\nignored:\n\
                    - file: a.rb\n  rule: r\n  message: foo\n  count: 1\n\
                    - file: a.rb\n  rule: r\n  count: 5\n";
        let b = Baseline::parse(text, "t").unwrap();
        let matches_msg = diag("r", "foo bar");
        let other = diag("r", "zzz");
        let entries = vec![("a.rb".to_string(), &matches_msg), ("a.rb".to_string(), &other)];
        let rows = b.audit(&entries);
        // Message bucket: actual 1 == count 1 → within.
        assert_eq!(rows[0].status, DriftStatus::Within);
        assert_eq!(rows[0].actual, 1);
        // Rule bucket: actual 1 (the non-matching one) < count 5 → reducible.
        assert_eq!(rows[1].status, DriftStatus::Reducible);
        assert_eq!(rows[1].actual, 1);
    }

    #[test]
    fn without_removes_by_full_tuple() {
        // Two rows sharing (a.rb, r): one message row, one rule row. Removing
        // the message row keeps the rule row.
        let text = "---\nversion: 1\nignored:\n\
                    - file: a.rb\n  rule: r\n  message: foo\n  count: 1\n\
                    - file: a.rb\n  rule: r\n  count: 5\n";
        let b = Baseline::parse(text, "t").unwrap();
        let to_remove: Vec<&Bucket> = vec![&b.buckets()[0]];
        let pruned = b.without(&to_remove);
        assert_eq!(pruned.size(), 1);
        assert_eq!(pruned.buckets()[0].rule, "r");
        assert!(pruned.buckets()[0].message.is_none());
        assert_eq!(pruned.buckets()[0].count, 5);
    }

    #[test]
    fn without_full_tuple_distinguishes_count() {
        // A remove-candidate that differs only in count must NOT match.
        let text = "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n  count: 5\n";
        let b = Baseline::parse(text, "t").unwrap();
        let ghost = Bucket::rule_row("a.rb".to_string(), "r".to_string(), 3);
        let pruned = b.without(&[&ghost]);
        assert_eq!(pruned.size(), 1); // count 3 != count 5 → not removed
    }

    #[test]
    fn drift_default_filter_hides_zero_delta() {
        let rows = [
            (DriftStatus::Within, 0i64),
            (DriftStatus::Over, 1i64),
            (DriftStatus::Cleared, -1i64),
        ];
        let shown: Vec<_> = rows.iter().filter(|(_, d)| *d != 0).collect();
        assert_eq!(shown.len(), 2);
    }

    #[test]
    fn suppression_order_baseline_sees_only_survivors() {
        // Composition contract (reference WD6): the baseline runs LAST. In
        // `main.rs::analyze_files`, inline `# rigor:disable` and config
        // `disable:` have already dropped diagnostics before `filter` is
        // called — so the baseline only ever sees the survivors and silences
        // among THOSE. Here we model "config dropped one of two diagnostics
        // upstream": the baseline (count 1) silences the one survivor it sees;
        // a fresh, never-baselined survivor surfaces.
        let baselined = diag("call.undefined-method", "m");
        let b = Baseline::from_diagnostics(&[("a.rb".to_string(), &baselined)], MatchMode::Rule);

        // Upstream (inline + config) already removed everything except these
        // two survivors handed to the baseline filter:
        let survivor_known = diag("call.undefined-method", "m");
        let survivor_new = diag("call.wrong-arity", "new finding");
        let entries =
            vec![("a.rb".to_string(), &survivor_known), ("a.rb".to_string(), &survivor_new)];
        let (surfaced, silenced) = b.filter(&entries);
        // The known one is silenced by the baseline; the new one passes through.
        assert_eq!(silenced, 1);
        assert_eq!(surfaced, vec![1]);
    }

    // ---- Issue #162: baseline/output parity -----------------------------

    #[test]
    fn filter_regroups_by_first_seen_bin_under_nonempty_baseline() {
        // Reference `group_diagnostics_for_filtering`: bins keyed by
        // (bucket file, rule, message-source) — or (file, rule, :__none__)
        // when unclaimed — in FIRST-APPEARANCE order, concatenated. An
        // unrelated bucket still turns the regroup on; only an EMPTY
        // baseline is a pass-through.
        let b = Baseline::parse(
            "---\nversion: 1\nignored:\n- file: zzz.rb\n  rule: lint.unrelated\n  count: 1\n",
            "t",
        )
        .unwrap();
        let u1 = diag("call.undefined-method", "u1");
        let w_a = diag("call.wrong-arity", "w-a");
        let u2 = diag("call.undefined-method", "u2");
        let w_b = diag("call.wrong-arity", "w-b");
        let entries = vec![
            ("a.rb".to_string(), &u1),
            ("a.rb".to_string(), &w_a),
            ("a.rb".to_string(), &u2),
            ("b.rb".to_string(), &w_b),
        ];
        // Stream order 0,1,2,3 regroups to bins (a.rb,undefined)= {0,2},
        // (a.rb,wrong-arity)={1}, (b.rb,wrong-arity)={3} → 0,2,1,3.
        let (surfaced, silenced) = b.filter(&entries);
        assert_eq!(surfaced, vec![0, 2, 1, 3]);
        assert_eq!(silenced, 0);
    }

    #[test]
    fn filter_empty_baseline_is_input_order_passthrough() {
        let b = Baseline::default();
        let u = diag("call.undefined-method", "u");
        let w = diag("call.wrong-arity", "w");
        let entries = vec![
            ("a.rb".to_string(), &u),
            ("a.rb".to_string(), &w),
            ("a.rb".to_string(), &u),
        ];
        let (surfaced, silenced) = b.filter(&entries);
        assert_eq!(surfaced, vec![0, 1, 2]);
        assert_eq!(silenced, 0);
    }

    #[test]
    fn parses_reference_folded_plain_message_scalar() {
        // The exact shape `baseline generate --match-mode=message` writes for
        // a long `Regexp.escape`d message: the plain scalar folds past col 80,
        // continuing at member-indent + 2 (col 4) — a single space rejoins.
        let text = "---\nversion: 1\nignored:\n- file: lib/a.rb\n  rule: call.undefined-method\n  \
                    message: undefined\\ method\\ `an_extremely_long_method_name_that_definitely_does_not_exist'\\\n    for\\ \"x\"\n  count: 1\n";
        let b = Baseline::parse(text, "t").unwrap();
        let bucket = &b.buckets()[0];
        assert_eq!(
            bucket.message.as_deref().unwrap(),
            "undefined\\ method\\ `an_extremely_long_method_name_that_definitely_does_not_exist'\\ for\\ \"x\""
        );
        // And the decoded regex matches the literal message back.
        let d = diag(
            "call.undefined-method",
            "undefined method `an_extremely_long_method_name_that_definitely_does_not_exist' for \"x\"",
        );
        let entries = vec![("lib/a.rb".to_string(), &d)];
        let (surfaced, silenced) = b.filter(&entries);
        assert!(surfaced.is_empty() && silenced == 1);
    }

    #[test]
    fn to_yaml_folds_long_plain_scalars_libyaml_style() {
        // A >80-col line breaks at the next breakable space, continuing at
        // member column + 2 — the bytes Psych writes for the same message.
        let msg = format!("undefined method `{}' for \"x\"", "m".repeat(90));
        let d = diag("call.undefined-method", &msg);
        let b = Baseline::from_diagnostics(&[("lib/a.rb".to_string(), &d)], MatchMode::Message);
        let yaml = b.to_yaml();
        let cont = yaml.lines().find(|l| l.starts_with("    for\\")).unwrap();
        assert_eq!(cont, "    for\\ \"x\"");
        // Round-trips: the folded line decodes back to the escaped source.
        let parsed = Baseline::parse(&yaml, "t").unwrap();
        assert_eq!(parsed.buckets()[0].message, b.buckets()[0].message);
    }

    #[test]
    fn writer_double_quotes_leading_dot_paths_and_bare_bool_words() {
        // Psych's `^[^[:word:]][^"]*$` → `"../x"`; bare `y`/`n` → `"y"`/`"n"`.
        let d = diag("call.undefined-method", "m");
        let b = Baseline::from_diagnostics(
            &[("../sib/o.rb".to_string(), &d)],
            MatchMode::Rule,
        );
        assert!(b.to_yaml().contains("- file: \"../sib/o.rb\"\n"));
        let b2 = Baseline::from_diagnostics(&[("y".to_string(), &d)], MatchMode::Rule);
        assert!(b2.to_yaml().contains("- file: \"y\"\n"));
    }

    #[test]
    fn writer_single_quotes_scalars_that_would_misparse() {
        // `a # b` would open a comment plain; a plain `5` would load as an
        // Integer — Psych single-quotes both.
        let d = diag("call.undefined-method", "m");
        let b = Baseline::from_diagnostics(
            &[("a # b.rb".to_string(), &d), ("5".to_string(), &d)],
            MatchMode::Rule,
        );
        let yaml = b.to_yaml();
        assert!(yaml.contains("- file: 'a # b.rb'\n"));
        assert!(yaml.contains("- file: '5'\n"));
        // And both round-trip.
        let parsed = Baseline::parse(&yaml, "t").unwrap();
        assert_eq!(parsed.buckets()[0].file, "a # b.rb");
        assert_eq!(parsed.buckets()[1].file, "5");
    }

    #[test]
    fn writer_literal_block_for_embedded_newline() {
        // `o.match?(/\n(?!\Z)/)` → `|-` literal; content lines at col +2.
        let d = diag("call.undefined-method", "m");
        let b = Baseline::from_diagnostics(&[("a\nb.rb".to_string(), &d)], MatchMode::Rule);
        let yaml = b.to_yaml();
        assert!(yaml.contains("- file: |-\n    a\n    b.rb\n"), "{yaml}");
        let parsed = Baseline::parse(&yaml, "t").unwrap();
        assert_eq!(parsed.buckets()[0].file, "a\nb.rb");
    }

    #[test]
    fn parses_double_quoted_multiline_and_escape_variants() {
        // A `"`-scalar may span lines (fold → space, `\`-eol suppresses it)
        // and carry the YAML 1.1 escape set.
        let text = "---\nversion: 1\nignored:\n\
                    - file: \"lib/\\\n    spaced.rb\"\n  rule: r\n  count: 1\n\
                    - file: \"tab\\there.rb\"\n  rule: r\n  count: 1\n\
                    - file: \"uni\\x41.rb\"\n  rule: r\n  count: 1\n";
        let b = Baseline::parse(text, "t").unwrap();
        assert_eq!(b.buckets()[0].file, "lib/spaced.rb");
        assert_eq!(b.buckets()[1].file, "tab\there.rb");
        assert_eq!(b.buckets()[2].file, "uniA.rb");
    }

    #[test]
    fn parses_quoted_and_block_scalar_rows() {
        let text = "---\nversion: 1\nignored:\n\
                    - file: 'it''s.rb'\n  rule: r\n  count: 1\n\
                    - file: !!str '<<'\n  rule: r\n  count: 1\n\
                    - file: |\n    two\n    lines.rb\n  rule: r\n  count: 1\n\
                    - file: >-\n    folded\n    join.rb\n  rule: r\n  count: 1\n";
        let b = Baseline::parse(text, "t").unwrap();
        assert_eq!(b.buckets()[0].file, "it's.rb");
        assert_eq!(b.buckets()[1].file, "<<");
        assert_eq!(b.buckets()[2].file, "two\nlines.rb\n");
        assert_eq!(b.buckets()[3].file, "folded join.rb");
    }

    #[test]
    fn invalid_message_regex_is_a_load_error_like_the_reference() {
        // `Regexp.new` failure upstream → LoadError → the whole baseline is
        // dropped with a stderr note (issue #162's probed contract; Ruby-only
        // syntax lands here too — the safe, loud side).
        let text = "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n  message: \"foo(\"\n  count: 1\n";
        let err = Baseline::parse(text, "bl.yml").unwrap_err();
        assert_eq!(
            err.to_string(),
            "bl.yml: ignored[0] `message:` is not a valid Regexp: unclosed group: /foo(/"
        );
    }

    #[test]
    fn row_errors_carry_the_ignored_index_like_the_reference() {
        let e = Baseline::parse("---\nversion: 1\nignored:\n- file: a.rb\n  count: 1\n", "t")
            .unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] missing `rule:`");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- a scalar\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] must be a Hash");
        let e = Baseline::parse(
            "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n  count: \"5\"\n",
            "t",
        )
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "t: ignored[0] `count:` must be a positive Integer (got \"5\")"
        );
        let e = Baseline::parse(
            "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n  count: -1\n",
            "t",
        )
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "t: ignored[0] `count:` must be a positive Integer (got -1)"
        );
        let e = Baseline::parse("---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n", "t")
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "t: ignored[0] `count:` must be a positive Integer (got nil)"
        );
    }

    #[test]
    fn version_validation_matches_the_reference_value_check() {
        // `version == 1` upstream: missing → `nil`, quoted `"1"` → a String,
        // wrong number → the value — all under one `unsupported` message.
        let e = Baseline::parse("---\nignored: []\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: nil` (expected 1)");
        let e = Baseline::parse("---\nversion: \"1\"\nignored: []\n", "t").unwrap_err();
        assert_eq!(
            e.to_string(),
            "t: unsupported `version: \"1\"` (expected 1)"
        );
        let e = Baseline::parse("---\nversion: 2\nignored: []\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: 2` (expected 1)");
        // Top-level non-Hash shapes report their class.
        let e = Baseline::parse("---\n- a\n- b\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: expected a Hash at top level, got Array");
        let e = Baseline::parse("---\njust a scalar\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: expected a Hash at top level, got String");
    }

    #[test]
    fn yaml_null_scalars_read_as_absent_not_literal_strings() {
        // `message: ~`/`message:` is nil upstream → a rule-ID row. Only PLAIN
        // scalars nil out — `'~'` is the string "~", `|` content is literal.
        let text = "---\nversion: 1\nignored:\n\
                    - file: a.rb\n  rule: r\n  message: ~\n  count: 1\n\
                    - file: b.rb\n  rule: r\n  message: '~'\n  count: 1\n";
        let b = Baseline::parse(text, "t").unwrap();
        assert!(b.buckets()[0].message.is_none());
        assert_eq!(b.buckets()[1].message.as_deref(), Some("~"));
        // And a null `file`/`rule` is a `missing` error upstream.
        let e = Baseline::parse(
            "---\nversion: 1\nignored:\n- file: ~\n  rule: r\n  count: 1\n",
            "t",
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] missing `file:`");
    }

    #[test]
    fn top_level_keys_accept_trailing_hash_comments_and_null_forms() {
        // Review-found blocker: `version: 1 # schema` must strip ` #…` —
        // upstream Psych reads the scalar, not the raw line tail. Same for
        // `ignored:` inline values, and `null`/`Null`/`NULL`/`~` all read nil.
        let b = Baseline::parse(
            "---\nversion: 1 # schema version\nignored: # commented\n\
             - file: a.rb\n  rule: r\n  count: 2\n",
            "t",
        )
        .unwrap();
        assert_eq!(b.size(), 1);

        for nil_form in ["[] # c", "Null", "NULL", "~ # trailing"] {
            let text = format!("---\nversion: 1\nignored: {nil_form}\n");
            let b = Baseline::parse(&text, "t").unwrap();
            assert!(b.is_empty(), "{nil_form:?}");
        }
        // A quoted `[]` is a String upstream — not an Array.
        let e = Baseline::parse("---\nversion: 1\nignored: '[]'\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: `ignored:` must be an Array");
        // A comment mid-value ends a plain scalar there.
        let e = Baseline::parse("---\nversion: 2 # nope\nignored: []\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: 2` (expected 1)");
    }

    #[test]
    fn present_inline_scalar_closes_ignored_so_rows_are_rejected() {
        // Round-2 review blocker: a PRESENT inline scalar (`~`, `null`,
        // `[]`, quoted) already consumed `ignored:`'s value — a following
        // block sequence is a Psych::SyntaxError upstream. The port degrades
        // gracefully (LoadError → baseline dropped, everything surfaces) but
        // must NEVER parse the rows. A bare `ignored:` or comment-only line
        // leaves the value unset, and the block sequence below IS the value.
        for present in ["~", "null", "Null", "NULL", "~ # c", "[]", "'[]'"] {
            let text = format!(
                "---\nversion: 1\nignored: {present}\n- file: a.rb\n  rule: r\n  count: 5\n"
            );
            // `'[]'`/`5`-shaped strings are not Arrays — error text
            // differs but the outcome must be a LoadError (never
            // silently accepted rows).
            assert!(Baseline::parse(&text, "t").is_err(), "ignored: {present} + rows");
        }
        // A bare TAG (`ignored: !foo`) leaves the key pending like an
        // absent value — the block sequence IS the tagged value upstream
        // (the rows are valid Hashes there, probed).
        for absent in ["", " # comment", " !foo"] {
            let text =
                format!("---\nversion: 1\nignored:{absent}\n- file: a.rb\n  rule: r\n  count: 5\n");
            assert_eq!(
                Baseline::parse(&text, "t").unwrap().size(),
                1,
                "ignored:{absent}"
            );
        }
        // And a present nil scalar ALONE (no rows) is the reference's `|| []`.
        for nil in ["~", "null", "Null", "NULL", "~ # c"] {
            let text = format!("---\nversion: 1\nignored: {nil}\n");
            assert!(
                Baseline::parse(&text, "t").unwrap().is_empty(),
                "ignored: {nil}"
            );
        }
    }

    // ---- Issue #326: typed scalars (Psych::ScalarScanner) ------------------

    fn baseline_with_row(row: &str) -> Result<Baseline, LoadError> {
        Baseline::parse(&format!("---\nversion: 1\nignored:\n{row}\n"), "t")
    }

    #[test]
    fn count_accepts_every_psych_integer_form() {
        // INTEGER_LEGACY + the sexagesimal arm — every form probed accepted
        // upstream (a `count: 1` diagnostic silences the same row).
        for (form, want) in [
            ("1_000", 1000),
            ("1,000", 1000),
            ("0x10", 16),
            ("+3", 3),
            ("017", 15), // leading-zero octal
            ("0_7", 7),
            ("0b11", 3),
            ("0b_1", 1),
            ("0x_1", 1),
            ("0x1_", 1),
            ("017_", 15),
            ("1:30", 5400), // sexagesimal — first segment weighs 60²
            ("2147483648", 2147483648),
            ("99999999999999999999999", usize::MAX), // bignum → capped
            ("!!int 5", 5),                          // tagged → tokenize
        ] {
            let b = baseline_with_row(&format!("- file: a.rb\n  rule: r\n  count: {form}"))
                .unwrap_or_else(|e| panic!("{form}: {e}"));
            assert_eq!(b.buckets()[0].count, want, "count: {form}");
        }
    }

    #[test]
    fn count_rejects_non_integer_and_nonpositive_with_typed_inspect() {
        // `count.is_a?(Integer) && count.positive?` — the `got` text is the
        // TYPED inspect: `0o17`/`08` are Strings upstream (Psych has no 0o
        // arm), `1.5` a Float, `true` a bool, `:x` a symbol, `-0x10` the
        // Integer -16.
        for (form, got) in [
            ("0o17", "\"0o17\""),
            ("08", "\"08\""),
            ("1__0", "\"1__0\""),
            ("1,,0", "\"1,,0\""),
            ("'5'", "\"5\""),
            ("1.5", "1.5"),
            ("3.0", "3.0"),
            (".inf", "Infinity"),
            ("true", "true"),
            (":x", ":x"),
            ("-3", "-3"),
            ("-0x10", "-16"),
            ("0", "0"),
            ("00", "0"),
            ("~", "nil"),
        ] {
            let e = baseline_with_row(&format!("- file: a.rb\n  rule: r\n  count: {form}"))
                .unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("t: ignored[0] `count:` must be a positive Integer (got {got})"),
                "count: {form}"
            );
        }
    }

    #[test]
    fn falsy_typed_message_is_a_rule_bucket() {
        // `if row["message"]` upstream — `false`/`no`/`off`/`~`/`null` load
        // as falsy, so the row is a rule bucket, not a `/false/` pattern.
        for form in ["false", "no", "off", "~", "null", "Null", "NULL"] {
            let b = baseline_with_row(&format!(
                "- file: a.rb\n  rule: r\n  message: {form}\n  count: 1"
            ))
            .unwrap_or_else(|e| panic!("{form}: {e}"));
            assert!(b.buckets()[0].message.is_none(), "message: {form}");
        }
        // `y`/`n` are Strings in Psych 5 — `message: n` keeps the `/n/` bucket.
        let b = baseline_with_row("- file: a.rb\n  rule: r\n  message: n\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].message.as_deref(), Some("n"));
    }

    #[test]
    fn typed_truthy_message_compiles_its_to_s() {
        // `Regexp.new(message.to_s)` — `0x10` compiles `/16/`, `1.5` `/1.5/`,
        // `true` `/true/`, `:x` `/x/`.
        for (form, source) in [
            ("0x10", "16"),
            ("1.5", "1.5"),
            ("true", "true"),
            (":x", "x"),
        ] {
            let b = baseline_with_row(&format!(
                "- file: a.rb\n  rule: r\n  message: {form}\n  count: 1"
            ))
            .unwrap_or_else(|e| panic!("{form}: {e}"));
            assert_eq!(
                b.buckets()[0].message.as_deref(),
                Some(source),
                "message: {form}"
            );
        }
    }

    #[test]
    fn falsy_typed_file_and_rule_are_missing_errors() {
        for (field, form) in [
            ("file", "false"),
            ("file", "~"),
            ("file", "no"),
            ("rule", "false"),
            ("rule", "off"),
        ] {
            let row = if field == "file" {
                format!("- file: {form}\n  rule: r\n  count: 1")
            } else {
                format!("- file: a.rb\n  rule: {form}\n  count: 1")
            };
            let e = baseline_with_row(&row).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("t: ignored[0] missing `{field}:`"),
                "{field}: {form}"
            );
        }
    }

    #[test]
    fn typed_nonstring_keys_make_inert_buckets() {
        // `file: 5`/`file: true`/`rule: 5`/`file: :s`/`file: 0x10` — truthy
        // non-String keys the reference keeps but that never claim
        // (`[5, rule]` never `==` `[path, rule]`).
        let b = baseline_with_row("- file: 5\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::Int("5".to_string()));
        let b = baseline_with_row("- file: true\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::True);
        let b = baseline_with_row("- file: 0x10\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::Int("16".to_string()));
        let b = baseline_with_row("- file: :s\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::Symbol("s".to_string()));
        let b = baseline_with_row("- file: a.rb\n  rule: 5\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].rule, KeyScalar::Int("5".to_string()));

        // And the inert bucket silences nothing while `file: '5'` is a real
        // String key (`5 == "5"` is false upstream).
        let d = diag("r", "m");
        let b = baseline_with_row("- file: 5\n  rule: r\n  count: 1").unwrap();
        let entries = vec![("5".to_string(), &d)];
        let (surfaced, silenced) = b.filter(&entries);
        assert_eq!(surfaced, vec![0]);
        assert_eq!(silenced, 0);
        let b = baseline_with_row("- file: '5'\n  rule: r\n  count: 1").unwrap();
        let (surfaced, silenced) = b.filter(&entries);
        assert!(surfaced.is_empty());
        assert_eq!(silenced, 1);
    }

    #[test]
    fn version_accepts_numeric_one_forms() {
        // `version == 1` upstream: Integer 1 in every base AND Float 1.0 —
        // `1e0` is a String (Psych's FLOAT arm needs a dot) and rejects.
        for form in [
            "1",
            "1.0",
            "1.",
            "0x1",
            "0b1",
            "01",
            "+1",
            "!!int 1",
            "!float 1.0",
        ] {
            Baseline::parse(&format!("---\nversion: {form}\nignored: []\n"), "t")
                .unwrap_or_else(|e| panic!("version: {form}: {e}"));
        }
        for (form, shown) in [
            ("1e0", "\"1e0\""),
            ("\"1\"", "\"1\""),
            ("'1'", "\"1\""),
            ("2", "2"),
            ("1_0", "10"),
            (".5", "0.5"),
            ("true", "true"),
            (".nan", "NaN"),
            (":s", ":s"),
            ("1:0", "3600"),
            ("!!str 1", "\"1\""),
        ] {
            let e =
                Baseline::parse(&format!("---\nversion: {form}\nignored: []\n"), "t").unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("t: unsupported `version: {shown}` (expected 1)"),
                "version: {form}"
            );
        }
    }

    #[test]
    fn tagged_scalars_route_through_deserialize_arms() {
        // `!str` forces String; `!foo`/`!!int`/`!ruby/object:K` tokenize;
        // `!ruby/sym` symbolizes; `!ruby/*` resolver arms die DisallowedClass.
        let b = baseline_with_row("- file: !!str 5\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::Str("5".to_string()));
        let b = baseline_with_row("- file: !foo '5'\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::Int("5".to_string()));
        let b = baseline_with_row("- file: !ruby/object:Foo 5\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::Int("5".to_string()));
        let b = baseline_with_row("- file: !ruby/sym x\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::Symbol("x".to_string()));
    }

    #[test]
    fn kernel_float_grammar_is_strict() {
        // `!float` is `Float(tokenize(value))` upstream — only scalars that
        // tokenize to a STRING reach `Kernel#Float`, whose grammar rejects
        // `,` separators, `Infinity`/`NaN` words and `1_` (`0x` hex and
        // `_`-separated decimals pass). A non-numeric scalar dies ArgumentError
        // or `can't convert` TypeError — both Fatal.
        for (form, klass) in [
            ("!float Infinity", "ArgumentError"),
            ("!float nan", "ArgumentError"),
            ("!float abc", "ArgumentError"),
            ("!float 'abc'", "ArgumentError"),
            ("!float '1,5e3'", "ArgumentError"),
            ("!float '1__0'", "ArgumentError"),
            ("!float '1_'", "ArgumentError"),
            // `1.e5` is a String upstream — the FLOAT arm wants a SIGNED
            // exponent (`1.E+5` is a Float) — so `Float("1.e5")` dies.
            ("!float 1.e5", "ArgumentError"),
            ("!float :x", "TypeError"),
            ("!float ~", "TypeError"),
            ("!float true", "TypeError"),
        ] {
            let e = baseline_with_row(&format!("- file: a.rb\n  rule: r\n  count: {form}"))
                .unwrap_err();
            assert!(e.is_fatal(), "count: {form}: {e}");
            assert!(e.to_string().contains(klass), "count: {form}: {e}");
        }
        // Scalar forms tokenize resolves itself never reach `Kernel#Float` —
        // `1.`/`.5` are Float-arm values upstream, `0x10`/`1_0` are
        // Integers, each `Float(v)` → a Float → `got N.N`.
        for (form, got) in [
            ("!float '1.'", "got 1.0"),
            ("!float '.5'", "got 0.5"),
            ("!float 0x10", "got 16.0"),
            ("!float 1_0", "got 10.0"),
        ] {
            let e = baseline_with_row(&format!("- file: a.rb\n  rule: r\n  count: {form}"))
                .unwrap_err();
            assert!(!e.is_fatal(), "count: {form}: {e}");
            assert!(e.to_string().contains(got), "count: {form}: {e}");
        }
    }

    #[test]
    fn disallowed_class_scalars_are_fatal_not_load_errors() {
        // Date/Time/`!ruby/*` resolver deaths are UNCAUGHT upstream (the run
        // dies, empty stdout, exit 1) — the Fatal surface, not a LoadError.
        for row in [
            "- file: 2024-01-01\n  rule: r\n  count: 1",
            "- file: 2024-01-01T10:00:00Z\n  rule: r\n  count: 1",
            "- file: !ruby/regexp x\n  rule: r\n  count: 1",
            "- file: !ruby/range 1..5\n  rule: r\n  count: 1",
            "- file: !ruby/class Foo\n  rule: r\n  count: 1",
            "- file: a.rb\n  rule: r\n  count: 2024-01-01",
            "- file: a.rb\n  rule: r\n  count: .e+1", // `Float()` ArgumentError
        ] {
            let e = baseline_with_row(row).unwrap_err();
            assert!(e.is_fatal(), "{row}: {e}");
        }
        // `version:` and `ignored:` lines die the same death.
        let e = Baseline::parse("---\nversion: 2024-01-01\nignored: []\n", "t").unwrap_err();
        assert!(e.is_fatal());
        assert!(e.to_string().contains("Date"));
        let e = Baseline::parse("---\nversion: !float abc\nignored: []\n", "t").unwrap_err();
        assert!(e.is_fatal());
        assert!(e.to_string().contains("invalid value for Float()"));
    }

    #[test]
    fn fatal_scalars_anywhere_in_the_document_die_at_load() {
        // `safe_load_file` types the WHOLE document before `parse_row` — a
        // Date in an unknown top-level key, an unknown `meta:` row field, or
        // a bare `- ` row scalar is a Fatal, never an ignored key or a
        // `must be a Hash` (issue #326 review).
        let e = Baseline::parse(
            "---\nversion: 1\ngenerated_at: 2024-01-01\nignored: []\n",
            "t",
        )
        .unwrap_err();
        assert!(e.is_fatal());
        assert!(e.to_string().contains("Date"), "{e}");
        let e = baseline_with_row("- file: a.rb\n  rule: r\n  meta: 2024-01-01\n  count: 1")
            .unwrap_err();
        assert!(e.is_fatal(), "{e}");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 2024-01-01\n", "t").unwrap_err();
        assert!(e.is_fatal(), "{e}");
        // …and the Fatal beats row validation errors (`count: 0` beside a
        // Date'd `message:`), and `version:` beats every row error
        // (upstream checks version before rows).
        let e = baseline_with_row("- file: a.rb\n  rule: r\n  count: 0\n  message: 2024-01-01")
            .unwrap_err();
        assert!(e.is_fatal(), "{e}");
        let e = Baseline::parse(
            "---\nversion: 2\nignored:\n- file: a.rb\n  rule: r\n  count: nope\n",
            "t",
        )
        .unwrap_err();
        assert!(!e.is_fatal());
        assert_eq!(e.to_string(), "t: unsupported `version: 2` (expected 1)");
    }

    #[test]
    fn ignored_truthiness_and_top_level_class_names() {
        // `rows = raw["ignored"] || []` — a FALSY typed scalar is `[]`
        // upstream and accepted; a truthy non-Array fails the check, run
        // AFTER the version check.
        for form in ["false", "no", "off", "~", "null"] {
            let b = Baseline::parse(&format!("---\nversion: 1\nignored: {form}\n"), "t")
                .unwrap_or_else(|e| panic!("ignored: {form}: {e}"));
            assert!(b.is_empty(), "ignored: {form}");
        }
        for form in ["5", "!!str", "1.5", "true", ":x"] {
            let e = Baseline::parse(&format!("---\nversion: 1\nignored: {form}\n"), "t")
                .unwrap_err();
            assert_eq!(e.to_string(), "t: `ignored:` must be an Array", "ignored: {form}");
        }
        // The top-level `expected a Hash` reports the loaded class (a
        // `:x` doc reads as a `key:`-shaped line here — a shape corner, not
        // the class report).
        for (doc, class) in [("5", "Integer"), ("~", "NilClass"), ("true", "TrueClass"), ("1.5", "Float")] {
            let e = Baseline::parse(&format!("{doc}\n"), "t").unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("t: expected a Hash at top level, got {class}"),
                "doc: {doc}"
            );
        }
    }

    #[test]
    fn nested_sequences_and_row_shape_ordering() {
        // `meta:` (or any `key:`) with `- item` lines loads an Array
        // upstream — a valid row, no error (round-2 review N1 regression).
        let b = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  meta:\n  - foo")
            .unwrap();
        assert_eq!(b.size(), 1);
        // …and the elements TYPE: a Date under `meta:` is DisallowedClass.
        let e = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  meta:\n  - 2024-01-01")
            .unwrap_err();
        assert!(e.is_fatal(), "{e}");
        assert!(e.to_string().contains("Date"), "{e}");
        // `- item` under a VALUED field is `did not find expected key` —
        // a SyntaxError, i.e. Fatal, not a row error.
        let e = baseline_with_row("- file: a.rb\n  - foo\n  rule: r\n  count: 1").unwrap_err();
        assert!(e.is_fatal(), "{e}");
        assert!(e.to_string().contains("SyntaxError"), "{e}");
        // A nested seq under `count:` loads `[5]` — `got [5]`.
        let e = baseline_with_row("- file: a.rb\n  rule: r\n  count:\n  - 5").unwrap_err();
        assert!(!e.is_fatal(), "{e}");
        assert!(e.to_string().contains("got [5]"), "{e}");

        // `must be a Hash` defers to parse_row: `version:` wins over it,
        // and a later row's DisallowedClass death beats it (N2).
        let e = Baseline::parse("---\nversion: 2\nignored:\n- 5\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: 2` (expected 1)");
        let e = Baseline::parse(
            "---\nversion: 1\nignored:\n- 5\n- file: a.rb\n  rule: r\n  meta: 2024-01-01\n",
            "t",
        )
        .unwrap_err();
        assert!(e.is_fatal(), "{e}");
        assert!(e.to_string().contains("Date"), "{e}");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 5\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] must be a Hash");
        // Member lines under a bare-scalar row are `mapping values are not
        // allowed` upstream — SyntaxError.
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 5\n  rule: r\n", "t").unwrap_err();
        assert!(e.is_fatal(), "{e}");
        assert!(e.to_string().contains("SyntaxError"), "{e}");

        // A top-level sequence types its items before `got Array` (N3).
        let e = Baseline::parse("---\n- 2024-01-01\n", "t").unwrap_err();
        assert!(e.is_fatal(), "{e}");
        assert!(e.to_string().contains("Date"), "{e}");
        let e = Baseline::parse("---\n- x\n- 2024-01-01\n", "t").unwrap_err();
        assert!(e.is_fatal(), "{e}");
        let e = Baseline::parse("---\n- 5\n- abc\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: expected a Hash at top level, got Array");
        let e = Baseline::parse("---\n- file: 2024-01-01\n", "t").unwrap_err();
        assert!(e.is_fatal(), "{e}");

        // `...` and a second `---` end doc 1 — upstream reads only it.
        let b = Baseline::parse("---\nversion: 1\nignored: []\n...\nversion: 2\n", "t").unwrap();
        assert!(b.is_empty());
        let b = Baseline::parse("---\nversion: 1\nignored: []\n---\nversion: 2\n", "t").unwrap();
        assert!(b.is_empty());
    }

    #[test]
    fn pending_top_level_keys_take_sequences() {
        // `version:` pending + `- 1` → `[1]` (upstream `unsupported
        // version: [1]` — the sequence IS the value).
        let e = Baseline::parse("---\nversion:\n- 1\nignored: []\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: [1]` (expected 1)");
        // Deeper-indent items are the key's seq too, and their scalars
        // type (`gen:\n  - 2024-01-01` → DisallowedClass).
        let e = Baseline::parse(
            "---\nversion: 1\ngen:\n  - 2024-01-01\nignored: []\n",
            "t",
        )
        .unwrap_err();
        assert!(e.is_fatal(), "{e}");
        // `gen:` pending + `- x` — meta ignored, baseline loads.
        let b = Baseline::parse(
            "---\ngen:\n- x\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n  count: 1\n",
            "t",
        )
        .unwrap();
        assert_eq!(b.size(), 1);
        // `- ` under a VALUED top-level key is `Psych::SyntaxError`
        // upstream (`ignored: ~` already consumed the value).
        let e = Baseline::parse(
            "---\nversion: 1\nignored: ~\n- file: a.rb\n  rule: r\n  count: 1\n",
            "t",
        )
        .unwrap_err();
        assert!(e.is_fatal(), "{e}");
        assert!(e.to_string().contains("SyntaxError"), "{e}");
    }

    #[test]
    fn bare_dash_rows_take_the_indented_block() {
        // `-\n  - x` — the element is a nested sequence → `must be a
        // Hash` (LoadError), not a SyntaxError (round-3 R2).
        let e = Baseline::parse("---\nversion: 1\nignored:\n-\n  - x\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] must be a Hash");
        // `-\n  file: a.rb` — the element is a real mapping → valid row.
        let b = Baseline::parse(
            "---\nversion: 1\nignored:\n-\n  file: a.rb\n  rule: r\n  count: 1\n",
            "t",
        )
        .unwrap();
        assert_eq!(b.size(), 1);
        // A colon-less member folds into a scalar head: `- 5\n  more` is
        // "5 more" upstream — `must be a Hash`, and `- 2024-01-01\n  more`
        // is the String "2024-01-01 more" — NO DisallowedClass death (R3).
        for head in ["5", "2024-01-01"] {
            let e = Baseline::parse(
                &format!("---\nversion: 1\nignored:\n- {head}\n  more\n"),
                "t",
            )
            .unwrap_err();
            assert_eq!(e.to_string(), "t: ignored[0] must be a Hash", "head {head}");
        }
        // …but a lone `- 2024-01-01` still dies.
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 2024-01-01\n", "t").unwrap_err();
        assert!(e.is_fatal(), "{e}");
    }

    #[test]
    fn tagged_and_anchored_pending_keys_take_sequences() {
        // `meta: !foo` / `meta: &a` leave the key PENDING — the `- item`
        // sequence is the tagged/anchored value (round-3 P1).
        for key in ["meta: !foo", "meta: &a"] {
            let b = baseline_with_row(&format!("- file: a.rb\n  rule: r\n  count: 1\n  {key}\n  - x"))
                .unwrap_or_else(|e| panic!("{key}: {e}"));
            assert_eq!(b.size(), 1, "{key}");
        }
        // `file: *x` — `safe_load`'s alias handling dies.
        let e = baseline_with_row("- file: *x\n  rule: r\n  count: 1").unwrap_err();
        assert!(e.is_fatal(), "{e}");
        assert!(e.to_string().contains("AliasesNotEnabled"), "{e}");
        // Elements nested one level inside seq items still type.
        let e = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  meta:\n  - a: 2024-01-01")
            .unwrap_err();
        assert!(e.is_fatal(), "{e}");
        let e = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  meta:\n  - - 2024-01-01")
            .unwrap_err();
        assert!(e.is_fatal(), "{e}");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- - 2024-01-01\n", "t").unwrap_err();
        assert!(e.is_fatal(), "{e}");
    }

    #[test]
    fn scalar_row_heads_need_a_member_colon() {
        // `a:b`, `'a: b'`, `:sym`, `[a: 1]`, `http://x`, `- - file: x`
        // are scalars/nested sequences upstream → `must be a Hash`,
        // never a `missing file:`.
        for head in ["a:b", "'a: b'", "\"file: a.rb\"", ":sym", "[a: 1]", "http://x", "- file: a.rb"] {
            let e = Baseline::parse(
                &format!("---\nversion: 1\nignored:\n- {head}\n"),
                "t",
            )
            .unwrap_err();
            assert_eq!(e.to_string(), "t: ignored[0] must be a Hash", "head {head}");
        }
        // `- {a: 1}` — upstream a Hash → `missing file:` (the split is
        // accidentally correct here).
        let e = Baseline::parse("---\nversion: 1\nignored:\n- {a: 1}\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] missing `file:`");
        // An indented `...`/`---` is scalar text, not a doc marker (R4).
        let b = baseline_with_row("- file: a.rb\n  rule: r\n  message: zork\n    ...\n  count: 1")
            .unwrap();
        assert_eq!(b.size(), 1);
    }

    #[test]
    fn review4_head_shapes_and_flow_collections() {
        // N1 — a multi-line QUOTED row head folds into one scalar.
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 'x\n  y'\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] must be a Hash");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- \"x\n  y\"\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] must be a Hash");
        // N2 — a `- -` head takes more `- ` members; a `key:` member under
        // it is the upstream SyntaxError.
        let e = Baseline::parse("---\nversion: 1\nignored:\n- - 5\n  - 6\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] must be a Hash");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- - 2024-01-01\n  - x\n", "t").unwrap_err();
        assert!(e.is_fatal() && e.to_string().contains("Date"), "{e}");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- - 5\n  x: 1\n", "t").unwrap_err();
        assert!(e.is_fatal() && e.to_string().contains("SyntaxError"), "{e}");
        // N3 — a key ENDING in `:` (`a:: b` → key `a:`).
        let b = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  a:: b").unwrap();
        assert_eq!(b.size(), 1);
        let e = Baseline::parse(
            "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n  count: 1\n  meta:: 2024-01-01\n",
            "t",
        )
        .unwrap_err();
        assert!(e.is_fatal() && e.to_string().contains("Date"), "{e}");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- a:: b\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] missing `file:`");
        // N4 — a CLOSED quoted key is a member (`'a': 1` → {"a" => 1}, a
        // quoted `file:` a real row); `'a: b'` stays a scalar head.
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 'a': 1\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] missing `file:`");
        let b = baseline_with_row("- 'file': a.rb\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.size(), 1);
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 'a: b'\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] must be a Hash");
        // N5 — a `version:` sequence's map/seq elements inspect like Ruby.
        let e = Baseline::parse("---\nversion:\n- a: 1\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: [{\"a\" => 1}]` (expected 1)");
        let e = Baseline::parse("---\nversion:\n- - 1\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: [[1]]` (expected 1)");
        let e = Baseline::parse("---\nversion: [1]\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: [1]` (expected 1)");
        // N5's flow typing — elements die inside the collection.
        let e = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  meta: [2024-01-01]")
            .unwrap_err();
        assert!(e.is_fatal() && e.to_string().contains("Date"), "{e}");
        let e = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  meta: {a: 2024-01-01}")
            .unwrap_err();
        assert!(e.is_fatal() && e.to_string().contains("Date"), "{e}");
        // N6 — a bare `!!seq` tag leaves `ignored:` pending for the rows.
        let b = Baseline::parse(
            "---\nversion: 1\nignored: !!seq\n- file: a.rb\n  rule: r\n  count: 1\n",
            "t",
        )
        .unwrap();
        assert_eq!(b.size(), 1);
        // A deeper-indented member still belongs to a bare-`-` row.
        let b = Baseline::parse(
            "---\nversion: 1\nignored:\n-\n    file: a.rb\n    rule: r\n    count: 1\n",
            "t",
        )
        .unwrap();
        assert_eq!(b.size(), 1);
    }

    #[test]
    fn review5_flow_items_and_quoted_key_colons() {
        // R1 — flow commas inside quotes/nested brackets do not split.
        for meta in ["['x,2024-01-01,y']", "[\"x,2024-01-01,y\"]"] {
            let b = baseline_with_row(&format!(
                "- file: a.rb\n  rule: r\n  count: 1\n  meta: {meta}"
            ))
            .unwrap_or_else(|e| panic!("meta: {meta}: {e}"));
            assert_eq!(b.size(), 1, "meta: {meta}");
        }
        // …and the quoted element's inspect decodes its quotes.
        let e = Baseline::parse("---\nversion: ['a, b']\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: [\"a, b\"]` (expected 1)");
        let e = Baseline::parse("---\nversion: {a: 'x,y'}\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: {\"a\" => \"x,y\"}` (expected 1)");
        let e = Baseline::parse("---\nversion: [[1, 2], [3]]\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: unsupported `version: [[1, 2], [3]]` (expected 1)");
        // A `{…}`-shaped seq element types recursively, not as `k: v`.
        let e = baseline_with_row(
            "- file: a.rb\n  rule: r\n  count: 1\n  meta: [x, {a: [1, 2024-01-01]}]",
        )
        .unwrap_err();
        assert!(e.is_fatal() && e.to_string().contains("Date"), "{e}");
        // Control: a top-level comma still separates, a date inside dies.
        let e = baseline_with_row(
            "- file: a.rb\n  rule: r\n  count: 1\n  meta: [\"x\", 2024-01-01]",
        )
        .unwrap_err();
        assert!(e.is_fatal() && e.to_string().contains("Date"), "{e}");
        // R2 — a quoted key's member colon sits after the closing quote.
        let b = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  'a: b': c").unwrap();
        assert_eq!(b.size(), 1);
        let b = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1\n  \"a: b\": c").unwrap();
        assert_eq!(b.size(), 1);
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 'a: b': 1\n", "t").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] missing `file:`");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- 'x: y': 1\n  file: a.rb\n  rule: r\n", "t")
            .unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] `count:` must be a positive Integer (got nil)");
        // `ignored: !tag # comment` stays pending for the block sequence.
        let b = Baseline::parse(
            "---\nversion: 1\nignored: !!seq # c\n- file: a.rb\n  rule: r\n  count: 1\n",
            "t",
        )
        .unwrap();
        assert_eq!(b.size(), 1);
    }

    #[test]
    fn sexagesimal_underscores_and_symbol_quote_edges() {
        // `n.to_i` accepts `_` separators: `1_0:30` is 10·3600+30·60.
        let b = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1_0:30").unwrap();
        assert_eq!(b.buckets()[0].count, 37800);
        let b = baseline_with_row("- file: a.rb\n  rule: r\n  count: 1:2:3").unwrap();
        assert_eq!(b.buckets()[0].count, 3723);
        // `/^:(['"])(.*)\1/` is not end-anchored — `:'x'y` is `:x`.
        let b = baseline_with_row("- file: :'x'y\n  rule: r\n  count: 1").unwrap();
        assert_eq!(b.buckets()[0].file, KeyScalar::Symbol("x".to_string()));
        // `!ruby/class` on empty resolves nil → `missing \`file:\``.
        let e = baseline_with_row("- file: !ruby/class\n  rule: r\n  count: 1").unwrap_err();
        assert_eq!(e.to_string(), "t: ignored[0] missing `file:`");
        // `!str:Foo::Bar` names the WHOLE tail, not the last `::` segment.
        let e = baseline_with_row("- file: !str:Foo::Bar x\n  rule: r\n  count: 1").unwrap_err();
        assert!(e.is_fatal());
        assert!(e.to_string().contains("Foo::Bar"), "{e}");
        // `!ruby/encoding` canonicalises (`ascii` → `US-ASCII`, to_s is the
        // name, inert key); unknown names die ArgumentError.
        let b = baseline_with_row("- file: !ruby/encoding ascii\n  rule: r\n  count: 1").unwrap();
        assert_eq!(
            b.buckets()[0].file,
            KeyScalar::Opaque("US-ASCII".to_string(), "Encoding")
        );
        let e = baseline_with_row("- file: !ruby/encoding no-such-enc\n  rule: r\n  count: 1")
            .unwrap_err();
        assert!(e.is_fatal());
        assert!(e.to_string().contains("unknown encoding name"), "{e}");
    }
}
