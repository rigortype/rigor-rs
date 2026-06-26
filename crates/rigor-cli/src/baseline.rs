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

use std::collections::BTreeMap;
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

/// A single baseline bucket — the in-memory shape of one `ignored:` row.
#[derive(Clone, Debug)]
pub struct Bucket {
    /// Project-root-relative path, as stored on disk.
    pub file: String,
    /// Qualified rule id (the reference's `qualified_rule`; for rigor-rs's
    /// all-`builtin` diagnostics this equals `Diagnostic::rule_id`).
    pub rule: String,
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
        Bucket { file, rule, message: None, message_regex: None, count }
    }

    fn message_row(file: String, rule: String, message: String, count: usize) -> Self {
        // A failed compile degrades to a non-matching bucket rather than
        // aborting; the reference raises LoadError, but for `check` we prefer
        // graceful continuation (the load-error path is reported by the caller).
        let message_regex = Regex::new(&message).ok();
        Bucket { file, rule, message: Some(message), message_regex, count }
    }
}

/// A parsed baseline: an ordered set of buckets plus a `(file, rule)` index.
#[derive(Debug, Default)]
pub struct Baseline {
    buckets: Vec<Bucket>,
}

/// A baseline parse failure (malformed YAML or a structurally invalid row).
#[derive(Debug)]
pub struct LoadError(pub String);

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
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
        let text = std::fs::read_to_string(path)
            .map_err(|e| LoadError(format!("{}: {e}", path.display())))?;
        Baseline::parse(&text, &path.display().to_string())
    }

    /// Parse a baseline YAML document. Hand-rolled (the manifest / `.rigor.yml`
    /// loaders follow the same hand-rolled-parser precedent) so the read path
    /// stays in lock-step with the byte-exact writer and needs no serde shape.
    pub fn parse(text: &str, label: &str) -> Result<Baseline, LoadError> {
        let mut version: Option<u64> = None;
        let mut buckets: Vec<Bucket> = Vec::new();

        // Row accumulator: fields seen for the current `- file:` item.
        let mut cur: Option<RowAcc> = None;
        let mut in_ignored = false;
        let mut ignored_seen = false;

        for raw_line in text.lines() {
            let line = raw_line;
            let trimmed = line.trim();
            // Skip the document marker, blanks, and comments.
            if trimmed.is_empty() || trimmed == "---" || trimmed.starts_with('#') {
                continue;
            }

            // Top-level keys (no indentation).
            if !line.starts_with(' ') && !line.starts_with('-') {
                // Flush any pending row before leaving the array.
                if let Some(acc) = cur.take() {
                    buckets.push(acc.into_bucket(label)?);
                }
                in_ignored = false;
                if let Some(rest) = trimmed.strip_prefix("version:") {
                    version = Some(parse_u64(rest.trim(), label, "version")?);
                } else if let Some(rest) = trimmed.strip_prefix("ignored:") {
                    in_ignored = true;
                    ignored_seen = true;
                    // `ignored: []` — an explicit empty array on one line.
                    if rest.trim() == "[]" {
                        in_ignored = false;
                    }
                }
                continue;
            }

            if !in_ignored {
                continue;
            }

            // A new array item begins with `- `. The first key (`file:`) rides
            // on the same line as the dash in the reference's emitter.
            if let Some(rest) = trimmed.strip_prefix("- ") {
                if let Some(acc) = cur.take() {
                    buckets.push(acc.into_bucket(label)?);
                }
                let mut acc = RowAcc::default();
                acc.apply(rest, label)?;
                cur = Some(acc);
            } else if let Some(acc) = cur.as_mut() {
                // A continuation key of the current item.
                acc.apply(trimmed, label)?;
            }
        }
        if let Some(acc) = cur.take() {
            buckets.push(acc.into_bucket(label)?);
        }

        match version {
            Some(v) if v == CURRENT_VERSION => {}
            Some(v) => {
                return Err(LoadError(format!(
                    "{label}: unsupported `version: {v}` (expected {CURRENT_VERSION})"
                )))
            }
            None => {
                return Err(LoadError(format!(
                    "{label}: missing `version:` (expected {CURRENT_VERSION})"
                )))
            }
        }
        let _ = ignored_seen; // `ignored:` may be legitimately absent → empty.

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
            let rule = diag.rule_id.to_string();
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
                    Some(m) => Bucket::message_row(file, rule, m, count),
                }
            })
            .collect();
        Baseline { buckets }
    }

    /// Serialize to the reference's exact YAML byte layout.
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
            out.push_str(&format!("- file: {}\n", yaml_scalar(&b.file)));
            out.push_str(&format!("  rule: {}\n", yaml_scalar(&b.rule)));
            if let Some(msg) = &b.message {
                out.push_str(&format!("  message: {}\n", yaml_scalar(msg)));
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
    #[must_use]
    pub fn filter(&self, entries: &[(String, &Diagnostic)]) -> (Vec<usize>, usize) {
        if self.buckets.is_empty() {
            return ((0..entries.len()).collect(), 0);
        }

        // Bin each diagnostic into the bucket that claims it (message-pattern
        // first, rule-ID fallback). A diagnostic claimed by no bucket goes into
        // a synthetic "no-bucket" bin keyed by (file, rule) so it surfaces.
        let mut bins: BTreeMap<BinKeyOrd, Vec<usize>> = BTreeMap::new();

        for (i, (rel, diag)) in entries.iter().enumerate() {
            let key = match self.claim_bucket(rel, diag) {
                Some(bi) => BinKeyOrd::Bucket(bi),
                None => BinKeyOrd::NoBucket(rel.clone(), diag.rule_id.to_string()),
            };
            bins.entry(key).or_default().push(i);
        }

        let mut surfaced = Vec::new();
        let mut silenced = 0usize;
        for (key, idxs) in bins {
            match key {
                BinKeyOrd::Bucket(bi) if idxs.len() <= self.buckets[bi].count => {
                    silenced += idxs.len();
                }
                _ => surfaced.extend(idxs),
            }
        }
        surfaced.sort_unstable();
        (surfaced, silenced)
    }

    /// The bucket that claims `diag`: a message-pattern bucket whose regex
    /// matches the message wins; else the rule-ID bucket for `(file, rule)`.
    /// Returns the bucket's index in `self.buckets`, or `None`.
    fn claim_bucket(&self, rel: &str, diag: &Diagnostic) -> Option<usize> {
        let mut rule_fallback: Option<usize> = None;
        // Message-pattern buckets take precedence over the rule-ID bucket.
        for (i, b) in self.buckets.iter().enumerate() {
            if b.file != rel || b.rule != diag.rule_id {
                continue;
            }
            match &b.message_regex {
                Some(re) => {
                    if re.is_match(&diag.message) {
                        return Some(i);
                    }
                }
                None => {
                    if rule_fallback.is_none() {
                        rule_fallback = Some(i);
                    }
                }
            }
        }
        rule_fallback
    }
}

/// Total ordering wrapper for bin keys (BTreeMap gives a deterministic walk).
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum BinKeyOrd {
    Bucket(usize),
    NoBucket(String, String),
}

/// In-progress accumulator for one `ignored:` row during parsing.
#[derive(Default)]
struct RowAcc {
    file: Option<String>,
    rule: Option<String>,
    message: Option<String>,
    count: Option<usize>,
}

impl RowAcc {
    /// Apply a single `key: value` fragment to the accumulator.
    fn apply(&mut self, fragment: &str, label: &str) -> Result<(), LoadError> {
        let (key, value) = fragment
            .split_once(':')
            .ok_or_else(|| LoadError(format!("{label}: malformed row entry {fragment:?}")))?;
        let value = value.trim();
        match key.trim() {
            "file" => self.file = Some(unyaml_scalar(value)),
            "rule" => self.rule = Some(unyaml_scalar(value)),
            "message" => self.message = Some(unyaml_scalar(value)),
            "count" => self.count = Some(parse_u64(value, label, "count")? as usize),
            _ => {} // unknown row key — ignore gracefully
        }
        Ok(())
    }

    fn into_bucket(self, label: &str) -> Result<Bucket, LoadError> {
        let file = self.file.ok_or_else(|| LoadError(format!("{label}: row missing `file:`")))?;
        let rule = self.rule.ok_or_else(|| LoadError(format!("{label}: row missing `rule:`")))?;
        let count = self
            .count
            .ok_or_else(|| LoadError(format!("{label}: row missing `count:`")))?;
        if count == 0 {
            return Err(LoadError(format!(
                "{label}: `count:` must be a positive Integer (got 0)"
            )));
        }
        Ok(match self.message {
            Some(m) => Bucket::message_row(file, rule, m, count),
            None => Bucket::rule_row(file, rule, count),
        })
    }
}

fn parse_u64(s: &str, label: &str, field: &str) -> Result<u64, LoadError> {
    s.parse::<u64>()
        .map_err(|_| LoadError(format!("{label}: `{field}:` must be an integer (got {s:?})")))
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

/// Render a scalar for the YAML writer. The reference's `Psych` emitter leaves
/// our values plain (file paths, rule ids, and `Regexp.escape`d messages never
/// start with an indicator char nor contain `": "` / trailing space after
/// escaping), so we emit them plain to stay byte-compatible. The few values
/// that WOULD need quoting (defensive: an empty string, or a leading/edge
/// indicator) are single-quoted Psych-style.
fn yaml_scalar(s: &str) -> String {
    if needs_quoting(s) {
        // Psych single-quote: double any embedded single quote.
        format!("'{}'", s.replace('\'', "''"))
    } else {
        s.to_string()
    }
}

/// Inverse of `yaml_scalar` for the reader: strip a surrounding quote pair and
/// unescape, else return the plain scalar as-is.
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
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
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

/// Whether a plain scalar would be misread by a YAML parser and so needs
/// quoting. Conservative: matches Psych's decision for the value shapes we emit.
fn needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // A `Regexp.escape`d message has every space backslash-escaped, so it never
    // contains a bare `": "` or a leading indicator. Plain paths/rules are
    // safe. Guard the indicator chars and the `: ` / trailing-space cases.
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
    // A bare `": "` (colon-space) inside would start an implicit mapping.
    if s.contains(": ") {
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
        assert!(err.0.contains("unsupported"));
    }

    #[test]
    fn missing_count_is_error() {
        let err = Baseline::parse("---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n", "t")
            .unwrap_err();
        assert!(err.0.contains("count"));
    }

    #[test]
    fn parses_empty_inline_ignored_array() {
        let b = Baseline::parse("---\nversion: 1\nignored: []\n", "t").unwrap();
        assert!(b.is_empty());
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
}
