//! The `suppression.*` rules: diagnostics about the `# rigor:` suppression
//! markers themselves (an unknown rule or marker, an empty list), emitted
//! before `filter_suppressed` runs.

use crate::{
    known_suppression_token, match_directive, Diagnostic, Severity, SUPPRESSION_EMPTY,
    SUPPRESSION_UNKNOWN_MARKER, SUPPRESSION_UNKNOWN_RULE,
};

// ---------------------------------------------------------------------------
// suppression.unknown-rule / suppression.empty (v0.3.0)
// ---------------------------------------------------------------------------

/// Produce the `suppression.unknown-rule` / `suppression.empty` surveillance
/// diagnostics for a file's comments (reference `suppression_marker_diagnostics`).
/// MUST be emitted into the same diagnostic list BEFORE `filter_suppressed`, so a
/// marker can suppress its own complaint (`# rigor:disable suppression.unknown-rule`).
/// `comments` is the `(line, start_offset, text)` list from `comment_lines`; every
/// diagnostic anchors at the comment's `#` (`start_offset`), which the CLI resolves
/// to `(line, start_column+1)` exactly like the reference.
#[must_use]
pub fn suppression_marker_diagnostics(comments: &[(usize, usize, String)]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (_line, offset, text) in comments {
        if let Some(rules) = match_directive(text, "rigor:disable-file") {
            validate_suppression_tokens(rules, "rigor:disable-file", *offset, &mut out);
        } else if let Some(rules) = match_directive(text, "rigor:disable") {
            validate_suppression_tokens(rules, "rigor:disable", *offset, &mut out);
        } else {
            diagnose_bare_suppression_marker(text, *offset, &mut out);
        }
    }
    out
}

/// Validate a matched marker's rule tokens: an empty token list fires
/// `suppression.empty`; each token that is not a known identifier fires
/// `suppression.unknown-rule` (reference `validate_suppression_tokens`).
fn validate_suppression_tokens(raw: &str, marker: &str, offset: usize, out: &mut Vec<Diagnostic>) {
    let tokens: Vec<&str> = raw.split([' ', '\t', ',']).filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        out.push(empty_suppression_diagnostic(marker, offset));
        return;
    }
    for token in tokens {
        if !known_suppression_token(token) {
            out.push(unknown_suppression_rule_diagnostic(marker, token, offset));
        }
    }
}

/// A comment carrying a marker word but not the token-bearing suppression
/// grammar. First the BARE `# rigor:disable[-file]` form: an all-whitespace/comma
/// remainder is a genuinely empty marker (`suppression.empty`); anything else
/// (documentation prose) is left alone. Otherwise the out-of-grammar
/// `suppression.unknown-marker` fallback (`# rigor:disable-next-line`,
/// `# rigor:enable`, ...). Mirrors the reference's `diagnose_bare_suppression_marker`
/// (BARE_SUPPRESSION_MARKER) chaining into `diagnose_unknown_suppression_marker`
/// (UNKNOWN_SUPPRESSION_MARKER). The reference applies each pattern as a whole-
/// string search — BARE has priority — so bare is scanned across every `#` before
/// the unknown fallback is tried at all. The anchor is the comment start (offset),
/// matching the reference's `comment.location.start_column + 1`.
fn diagnose_bare_suppression_marker(text: &str, offset: usize, out: &mut Vec<Diagnostic>) {
    // Pass 1: BARE_SUPPRESSION_MARKER = /#\s*rigor:disable(-file)?(?![\w-])(?<rest>.*)/.
    // The `(-file)?` is greedy but backtracks under the `(?![\w-])` lookahead:
    // `disable-files` / `disable-file-x` fail BARE and drop to the unknown pass.
    for after_rigor in rigor_marker_tails(text) {
        let Some(after_disable) = after_rigor.strip_prefix("disable") else {
            continue;
        };
        if let Some((is_file, rest)) = match_bare_disable(after_disable) {
            if rest.chars().all(is_suppression_ws_or_comma) {
                let marker = if is_file { "rigor:disable-file" } else { "rigor:disable" };
                out.push(empty_suppression_diagnostic(marker, offset));
            }
            return;
        }
    }
    // Pass 2: UNKNOWN_SUPPRESSION_MARKER — an out-of-grammar `rigor:` marker word.
    for after_rigor in rigor_marker_tails(text) {
        if let Some((marker, rest)) = parse_unknown_marker(after_rigor) {
            // Fire only when the remainder is empty/commas or rule-list-shaped, so
            // prose mentioning the spelling (`<rule>`, backticks) stays a comment.
            if rest.chars().all(is_suppression_ws_or_comma) || is_rule_list_shaped(rest) {
                out.push(unknown_suppression_marker_diagnostic(marker, offset));
            }
            return;
        }
    }
}

/// True for an ASCII `[\w-]` byte (word char or hyphen), matching Ruby's default
/// ASCII `\w`. Non-ASCII lead bytes are not word chars, so `(?![\w-])` holds.
fn is_word_or_hyphen_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// True when `s` begins with a `[\w-]` char — the negated `(?![\w-])` lookahead
/// is the complement. An empty string (end-of-input) has no such char, so the
/// lookahead holds there.
fn starts_word_or_hyphen(s: &str) -> bool {
    s.bytes().next().is_some_and(is_word_or_hyphen_byte)
}

/// Byte length of the leading `[\w-]+` run (all ASCII).
fn word_or_hyphen_run_len(s: &str) -> usize {
    s.bytes().take_while(|b| is_word_or_hyphen_byte(*b)).count()
}

/// `[\s,]` — Ruby `\s` (`[ \t\r\n\f\v]`) plus comma.
fn is_suppression_ws_or_comma(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n' | '\u{0c}' | '\u{0b}' | ',')
}

/// Match the BARE `disable(-file)?(?![\w-])` marker against the text after
/// `rigor:disable`, replicating the regex's `(-file)?` backtracking. Returns
/// `(is_file, rest)` on a match, `None` when the marker word runs into a `[\w-]`
/// char (`disable-files`, `disablexyz`) and so is not the bare grammar.
fn match_bare_disable(after_disable: &str) -> Option<(bool, &str)> {
    // Greedy `-file` first, valid only if `(?![\w-])` holds right after it.
    if let Some(after_file) = after_disable.strip_prefix("-file") {
        if !starts_word_or_hyphen(after_file) {
            return Some((true, after_file));
        }
    }
    // Backtrack: no `-file`, then `(?![\w-])` right after `disable`.
    (!starts_word_or_hyphen(after_disable)).then_some((false, after_disable))
}

/// Match `UNKNOWN_SUPPRESSION_MARKER`'s `marker` group against the text after
/// `rigor:` — `disable-<suffix>` (suffix other than `file`) or `enable[-<suffix>]`,
/// each terminated by `(?![\w-])`. Returns `(marker, rest)`.
fn parse_unknown_marker(after_rigor: &str) -> Option<(&str, &str)> {
    // Alt A: `disable-(?!file(?![\w-]))[\w-]+`.
    if let Some(rem) = after_rigor.strip_prefix("disable-") {
        // Negative lookahead: reject a bare `disable-file` (handled by BARE).
        if let Some(after_file) = rem.strip_prefix("file") {
            if !starts_word_or_hyphen(after_file) {
                return None;
            }
        }
        let suffix = word_or_hyphen_run_len(rem);
        if suffix == 0 {
            return None; // `disable-` with no `[\w-]` suffix.
        }
        let end = "disable-".len() + suffix;
        return Some((&after_rigor[..end], &after_rigor[end..]));
    }
    // Alt B: `enable(?:-[\w-]+)?(?![\w-])`.
    if let Some(rem) = after_rigor.strip_prefix("enable") {
        let end = if let Some(after_dash) = rem.strip_prefix('-') {
            let suffix = word_or_hyphen_run_len(after_dash);
            if suffix == 0 {
                return None; // `enable-` then non-`[\w-]`: `(?![\w-])` sees `-`.
            }
            "enable".len() + 1 + suffix
        } else if starts_word_or_hyphen(rem) {
            return None; // `enablexyz` — `(?![\w-])` fails.
        } else {
            "enable".len()
        };
        return Some((&after_rigor[..end], &after_rigor[end..]));
    }
    None
}

/// `\A\s+[\w.,\s-]+\z` — a leading run of whitespace followed by a rule-list-
/// shaped remainder (word / `.` / `,` / whitespace / `-`). Reduces to: the first
/// char is whitespace and every char is in the class, with length ≥ 2.
fn is_rule_list_shaped(rest: &str) -> bool {
    let is_class = |c: char| {
        c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ',' | '-') || is_suppression_ws_or_comma(c)
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some(c) if is_suppression_ws_or_comma(c) => {}
        _ => return false,
    }
    let mut count = 1;
    for c in chars {
        if !is_class(c) {
            return false;
        }
        count += 1;
    }
    count >= 2
}

/// Iterate the tail after each `#\s*rigor:` occurrence in a comment, replicating
/// the `#\s*rigor:` prefix shared by the bare / unknown marker patterns.
fn rigor_marker_tails(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    (0..bytes.len()).filter_map(move |i| {
        if bytes[i] != b'#' {
            return None;
        }
        let mut j = i + 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        text[j..].strip_prefix("rigor:")
    })
}

fn unknown_suppression_rule_diagnostic(marker: &str, token: &str, offset: usize) -> Diagnostic {
    Diagnostic {
        rule_id: SUPPRESSION_UNKNOWN_RULE,
        start_offset: offset,
        end_offset: offset,
        message: format!(
            "unknown rule `{token}` in `# {marker}` — the token matches no known rule, \
             alias, or family, so this suppression has no effect. Likely a typo; \
             `rigor explain <rule>` lists the canonical ids."
        ),
        severity: Severity::Warning,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    }
}

fn unknown_suppression_marker_diagnostic(marker: &str, offset: usize) -> Diagnostic {
    Diagnostic {
        rule_id: SUPPRESSION_UNKNOWN_MARKER,
        start_offset: offset,
        end_offset: offset,
        message: format!(
            "unrecognised suppression marker `rigor:{marker}` — Rigor's markers are \
             `# rigor:disable <rules>` (suppresses on its own line) and \
             `# rigor:disable-file <rules>`, so this comment suppresses nothing."
        ),
        severity: Severity::Warning,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    }
}

fn empty_suppression_diagnostic(marker: &str, offset: usize) -> Diagnostic {
    Diagnostic {
        rule_id: SUPPRESSION_EMPTY,
        start_offset: offset,
        end_offset: offset,
        message: format!(
            "`# {marker}` lists no rules, so this suppression has no effect. Name the \
             rules to suppress (`# {marker} call.undefined-method`) or use `# {marker} all`."
        ),
        severity: Severity::Warning,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    }
}
