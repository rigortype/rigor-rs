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
        // The loader rejects an uncompilable pattern as LoadError before
        // reaching here (the reference's `Regexp.new` → LoadError contract);
        // `.ok()` here is only the defensive tail for generated rows, whose
        // `Regexp.escape`d source always compiles.
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
    ///
    /// # Psych scalar dialect (probed against `YAML.dump` output)
    ///
    /// A row value may be a *folded* scalar — `baseline generate
    /// --match-mode=message` emits `Regexp.escape`d messages as plain scalars
    /// that libyaml wraps past column 80 at a breakable space, continuing on
    /// deeper-indented lines (the fold rejoins as a single space on load;
    /// each blank line rejoins as a `\n`). Quoted scalars may also span lines.
    /// Continuation handling below covers every shape Psych emits: folded
    /// plain, multi-line `'…'`/`"…"` scalars, `|`/`>` block scalars, and a
    /// `!tag` prefix (`!!str '<<'`). A line deeper than the row's member-key
    /// column is scalar content, never a key.
    pub fn parse(text: &str, label: &str) -> Result<Baseline, LoadError> {
        // `version` keeps its raw scalar text until the end — the reference
        // validates the LOADED value (`version == 1`), so a quoted `"1"` must
        // reject as `unsupported \`version: "1"\``, not parse.
        let mut version: Option<(String, bool)> = None;
        let mut buckets: Vec<Bucket> = Vec::new();

        // Row accumulator: fields seen for the current `- file:` item.
        let mut cur: Option<RowAcc> = None;
        let mut in_ignored = false;
        // Column of the `-` that opened the current row and of its member
        // keys (`file:`), for `- item` vs continuation disambiguation.
        let mut item_col = 0usize;
        let mut member_col = 0usize;

        for raw_line in text.lines() {
            let trimmed = raw_line.trim();
            // The document marker and comment lines never join a scalar.
            if trimmed == "---" || trimmed.starts_with('#') {
                continue;
            }
            let indent = raw_line.len() - raw_line.trim_start().len();
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
            }

            // A document that is a bare sequence (`- x` at column 0 outside
            // `ignored:`) loads as an Array upstream, a bare scalar as a
            // String — both fail `raw.is_a?(Hash)` there.
            if !in_ignored && (trimmed == "-" || trimmed.starts_with("- ")) {
                return Err(LoadError(format!("{label}: expected a Hash at top level, got Array")));
            }

            // Top-level keys (no indentation).
            if !raw_line.starts_with(' ') && !raw_line.starts_with('-') {
                if !trimmed.contains(':') {
                    return Err(LoadError(format!(
                        "{label}: expected a Hash at top level, got String"
                    )));
                }
                // Flush any pending row before leaving the array.
                if let Some(acc) = cur.take() {
                    buckets.push(acc.into_bucket(label)?);
                }
                in_ignored = false;
                if let Some(rest) = trimmed.strip_prefix("version:") {
                    let raw = rest.trim();
                    version = Some((
                        unyaml_scalar(raw),
                        raw.starts_with('"') || raw.starts_with('\''),
                    ));
                } else if let Some(rest) = trimmed.strip_prefix("ignored:") {
                    in_ignored = true;
                    // `ignored: []` — an explicit empty array on one line;
                    // `ignored:` with `null`/`~` is the reference's `|| []`.
                    // Anything else inline fails `rows.is_a?(Array)` upstream.
                    let inline = rest.trim();
                    if inline == "[]" {
                        in_ignored = false;
                    } else if !inline.is_empty() && inline != "~" && inline != "null" {
                        return Err(LoadError(format!("{label}: `ignored:` must be an Array")));
                    }
                }
                continue;
            }

            if !in_ignored {
                continue;
            }

            // A new array item begins with `- ` at (or left of) the row's
            // dash column; deeper `- ` text is a folded continuation. The
            // first key (`file:`) rides on the same line as the dash in the
            // reference's emitter.
            if trimmed.starts_with("- ") && (cur.is_none() || indent <= item_col) {
                if let Some(acc) = cur.take() {
                    buckets.push(acc.into_bucket(label)?);
                }
                item_col = indent;
                let rest = &trimmed[2..];
                member_col = indent + 2 + (rest.len() - rest.trim_start().len());
                let mut acc = RowAcc { idx: buckets.len(), ..RowAcc::default() };
                acc.apply(rest.trim_start(), label, true)?;
                cur = Some(acc);
            } else if let Some(acc) = cur.as_mut() {
                if indent > member_col {
                    // Folded / block-scalar continuation of the last value.
                    acc.continue_scalar(raw_line);
                } else {
                    // A member key of the current item.
                    acc.apply(trimmed, label, false)?;
                }
            }
        }
        if let Some(acc) = cur.take() {
            buckets.push(acc.into_bucket(label)?);
        }

        // `unless version == CURRENT_VERSION` upstream — a missing `version:`
        // is `nil`, reported with the same message as a wrong one.
        match version {
            Some((v, quoted)) if !quoted && v.parse::<u64>() == Ok(CURRENT_VERSION) => {}
            other => {
                let shown = match other {
                    Some((v, quoted)) => ruby_inspect(&v, quoted),
                    None => "nil".to_string(),
                };
                return Err(LoadError(format!(
                    "{label}: unsupported `version: {shown}` (expected {CURRENT_VERSION})"
                )));
            }
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
                    Some(m) => Bucket::message_row(file, rule, m, count),
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
            push_kv(&mut out, "- file: ", 2, &b.file);
            push_kv(&mut out, "  rule: ", 2, &b.rule);
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
            if b.file != rel || b.rule != rule {
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
        let mut counts: BTreeMap<(String, String, Option<String>), usize> = BTreeMap::new();
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
/// one bin — and `NoBucket`'s `(file, rule)` for the unclaimed.
#[derive(PartialEq, Eq, Hash)]
enum BinKey {
    Claimed(String, String, Option<String>),
    NoBucket(String, String),
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
#[derive(Clone, Copy, Default)]
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
#[derive(Clone, Copy)]
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

    /// Apply a `key: value` fragment — the text after `- ` (a row head, where
    /// a missing colon means the row isn't a mapping at all) or a member line
    /// (where it is malformed YAML the reference's Psych parse would have
    /// already rejected; report it as a row error either way).
    fn apply(&mut self, fragment: &str, label: &str, row_head: bool) -> Result<(), LoadError> {
        let (key, value) = fragment.split_once(':').ok_or_else(|| {
            if row_head {
                LoadError(format!("{label}: ignored[{}] must be a Hash", self.idx))
            } else {
                LoadError(format!("{label}: malformed row entry {fragment:?}"))
            }
        })?;
        let (head, kind) = scalar_head(value.trim_start());
        let field = match key.trim() {
            "file" => Field::File,
            "rule" => Field::Rule,
            "message" => Field::Message,
            "count" => Field::Count,
            _ => Field::Other,
        };
        self.last = Some(field);
        self.pending_breaks = 0;
        // `key:` with an empty value is YAML nil upstream — leave the field
        // unset (`file`/`rule` then hit `or raise`; a nil `message` reads as
        // a rule-ID row).
        if head.is_empty() {
            self.quote_open = false;
            return Ok(());
        }
        self.quote_open = match kind {
            ScalarKind::DoubleQuoted => !dq_closed(head),
            ScalarKind::SingleQuoted => !sq_closed(head),
            _ => false,
        };
        *self.field_mut(field) = FieldVal { raw: head.to_string(), kind, ..FieldVal::default() };
        Ok(())
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
        let file = self.decode(Field::File).ok_or_else(|| {
            LoadError(format!("{label}: ignored[{idx}] missing `file:`"))
        })?;
        let rule = self.decode(Field::Rule).ok_or_else(|| {
            LoadError(format!("{label}: ignored[{idx}] missing `rule:`"))
        })?;
        // `count.is_a?(Integer) && count.positive?` upstream: a quoted or
        // non-numeric scalar is a YAML String (not Integer), a missing/nil
        // value inspects as `nil`, and non-positive integers report their
        // value. One message covers all three failure shapes.
        let count_fv = self.field_ref(Field::Count);
        let count_decoded = self.decode(Field::Count).unwrap_or_default();
        let count_ok = count_fv.is_some_and(|fv| matches!(fv.kind, ScalarKind::Plain))
            && count_decoded
                .parse::<i64>()
                .is_ok_and(|n| n > 0);
        if !count_ok {
            let shown = ruby_inspect(&count_decoded, count_fv.is_some_and(|fv| fv.was_quoted()));
            return Err(LoadError(format!(
                "{label}: ignored[{idx}] `count:` must be a positive Integer (got {shown})"
            )));
        }
        let count = count_decoded.parse::<usize>().expect("positive i64 fits usize");
        Ok(match self.decode(Field::Message) {
            Some(m) => {
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
                    return Err(LoadError(format!(
                        "{label}: ignored[{idx}] `message:` is not a valid Regexp: \
                         {reason}: /{m}/"
                    )));
                }
                Bucket::message_row(file, rule, m, count)
            }
            None => Bucket::rule_row(file, rule, count),
        })
    }
}

impl FieldVal {
    /// Whether the scalar arrived quoted (`'…'`/`"…"`) — quoted scalars load
    /// as Strings regardless of content (`"1"` is not Integer 1 upstream).
    fn was_quoted(&self) -> bool {
        matches!(self.kind, ScalarKind::DoubleQuoted | ScalarKind::SingleQuoted)
    }
}

/// Ruby `inspect` of the YAML value a scalar loads as, for reference-shaped
/// error text (`got nil`, `got "5"`, `got 0`). Quoted scalars are Strings;
/// plain `null`/`~`/empty is nil; YAML 1.1 bool words inspect as `true`/
/// `false`; numerics print their digits (exotic bases keep their spelling —
/// an approximation only visible in an error path).
fn ruby_inspect(decoded: &str, quoted: bool) -> String {
    if quoted {
        return format!("{decoded:?}");
    }
    if decoded.is_empty() || decoded.eq_ignore_ascii_case("null") || decoded == "~" {
        return "nil".to_string();
    }
    match decoded.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => return "true".to_string(),
        "false" | "no" | "off" => return "false".to_string(),
        _ => {}
    }
    if decoded.parse::<i64>().is_ok() || decoded.parse::<f64>().is_ok() {
        return decoded.to_string();
    }
    format!("{decoded:?}")
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
/// initial raw text and its kind. Strips a `!tag` prefix (`!!str '<<'`
/// decodes as `'<<'`) and a trailing ` #` comment.
fn scalar_head(value: &str) -> (&str, ScalarKind) {
    let mut v = value;
    // One `!`-tag token then the scalar (`!!str '<<'`, `! 'x'`).
    if v.starts_with('!') {
        match v[1..].find(|c: char| c == ' ' || c == '\t') {
            Some(i) => v = v[1 + i..].trim_start(),
            None => v = "", // a bare tag with no scalar → nil
        }
    }
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
            // ` #` opens a comment in a plain scalar.
            match v.find(" #") {
                Some(i) => v[..i].trim_end(),
                None => v.trim_end(),
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
    (v, kind)
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
        if !(first.is_alphanumeric() || first == '_') && !s.contains('"') {
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
            err.0,
            "bl.yml: ignored[0] `message:` is not a valid Regexp: unclosed group: /foo(/"
        );
    }

    #[test]
    fn row_errors_carry_the_ignored_index_like_the_reference() {
        let e = Baseline::parse("---\nversion: 1\nignored:\n- file: a.rb\n  count: 1\n", "t")
            .unwrap_err();
        assert_eq!(e.0, "t: ignored[0] missing `rule:`");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- a scalar\n", "t").unwrap_err();
        assert_eq!(e.0, "t: ignored[0] must be a Hash");
        let e = Baseline::parse(
            "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n  count: \"5\"\n",
            "t",
        )
        .unwrap_err();
        assert_eq!(
            e.0,
            "t: ignored[0] `count:` must be a positive Integer (got \"5\")"
        );
        let e = Baseline::parse(
            "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n  count: -1\n",
            "t",
        )
        .unwrap_err();
        assert_eq!(e.0, "t: ignored[0] `count:` must be a positive Integer (got -1)");
        let e = Baseline::parse("---\nversion: 1\nignored:\n- file: a.rb\n  rule: r\n", "t")
            .unwrap_err();
        assert_eq!(
            e.0,
            "t: ignored[0] `count:` must be a positive Integer (got nil)"
        );
    }

    #[test]
    fn version_validation_matches_the_reference_value_check() {
        // `version == 1` upstream: missing → `nil`, quoted `"1"` → a String,
        // wrong number → the value — all under one `unsupported` message.
        let e = Baseline::parse("---\nignored: []\n", "t").unwrap_err();
        assert_eq!(e.0, "t: unsupported `version: nil` (expected 1)");
        let e = Baseline::parse("---\nversion: \"1\"\nignored: []\n", "t").unwrap_err();
        assert_eq!(e.0, "t: unsupported `version: \"1\"` (expected 1)");
        let e = Baseline::parse("---\nversion: 2\nignored: []\n", "t").unwrap_err();
        assert_eq!(e.0, "t: unsupported `version: 2` (expected 1)");
        // Top-level non-Hash shapes report their class.
        let e = Baseline::parse("---\n- a\n- b\n", "t").unwrap_err();
        assert_eq!(e.0, "t: expected a Hash at top level, got Array");
        let e = Baseline::parse("---\njust a scalar\n", "t").unwrap_err();
        assert_eq!(e.0, "t: expected a Hash at top level, got String");
    }
}
