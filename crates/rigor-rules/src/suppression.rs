//! In-source suppression (`# rigor:disable` / `# rigor:disable-file`), the
//! `disable:` config set, and the rule-token tables both read.

use std::collections::{HashMap, HashSet};

use crate::{
    Diagnostic, CALL_ARGUMENT_TYPE_MISMATCH, CALL_POSSIBLE_NIL_RECEIVER, CALL_RAISE_NON_EXCEPTION,
    CALL_UNDEFINED_METHOD, CALL_UNRESOLVED_TOPLEVEL, CALL_WRONG_ARITY, DEF_IVAR_WRITE_MISMATCH,
    DEF_OVERRIDE_VISIBILITY_REDUCED, FLOW_ALWAYS_RAISES, FLOW_ALWAYS_TRUTHY_CONDITION,
    FLOW_DEAD_ASSIGNMENT, FLOW_DUPLICATE_HASH_KEY, FLOW_RETURN_IN_ENSURE,
    FLOW_SHADOWED_RESCUE_CLAUSE, FLOW_UNREACHABLE_BRANCH, STATIC_VALUE_USE_VOID, SUPPRESSION_EMPTY,
    SUPPRESSION_UNKNOWN_MARKER, SUPPRESSION_UNKNOWN_RULE,
};

// ---------------------------------------------------------------------------
// In-source diagnostic suppression (reference `filter_suppressed`)
// ---------------------------------------------------------------------------

/// The sentinel rule id of the synthetic internal-error diagnostic emitted on a
/// per-file panic (ADR-0016). Such diagnostics carry no real rule and MUST NEVER
/// be suppressed — they represent failures the user cannot silence away (matches
/// the reference's `rule == nil` guard in `filter_suppressed`).
pub(crate) const INTERNAL_ERROR_RULE: &str = "internal-error";

/// Family-wildcard tokens (`call`, `flow`, …). A token in this set expands to
/// every canonical rule whose id starts with `<token>.` (reference
/// `RULE_FAMILIES`, `check_rules/rule_ids.rb`). `assert`, `dump`, `effect` and
/// `global` match no rule rigor-rs emits yet; they are carried so the tokens
/// stay known vocabulary, as in the reference.
const RULE_FAMILIES: &[&str] =
    &["call", "flow", "assert", "dump", "def", "global", "suppression", "static", "effect"];

/// The canonical rule ids rigor-rs can actually emit. Family expansion is
/// checked against this set (`all` is the separate `SuppressSet::all` flag), so a
/// `call` family token
/// only ever expands to the `call.*` ids listed here (the reference expands
/// against its full `ALL_RULES`, but the extra ids it would add match no
/// rigor-rs diagnostic). A rule rigor-rs emits but this set omits escapes its
/// family token — `call.unresolved-toplevel` did until #250.
const IMPLEMENTED_RULES: &[&str] = &[
    CALL_UNDEFINED_METHOD,
    CALL_WRONG_ARITY,
    CALL_ARGUMENT_TYPE_MISMATCH,
    CALL_POSSIBLE_NIL_RECEIVER,
    CALL_UNRESOLVED_TOPLEVEL,
    FLOW_DEAD_ASSIGNMENT,
    DEF_OVERRIDE_VISIBILITY_REDUCED,
    FLOW_ALWAYS_RAISES,
    FLOW_UNREACHABLE_BRANCH,
    FLOW_ALWAYS_TRUTHY_CONDITION,
    FLOW_DUPLICATE_HASH_KEY,
    FLOW_RETURN_IN_ENSURE,
    CALL_RAISE_NON_EXCEPTION,
    FLOW_SHADOWED_RESCUE_CLAUSE,
    SUPPRESSION_UNKNOWN_RULE,
    SUPPRESSION_EMPTY,
    SUPPRESSION_UNKNOWN_MARKER,
    DEF_IVAR_WRITE_MISMATCH,
    STATIC_VALUE_USE_VOID,
];

/// The canonical rule ids rigor-rs can actually emit — the implemented coverage
/// scope, a SOUND SUBSET of the reference's catalogue (ADR-0008). Reported by
/// `rigor doctor` so users know which rules are live.
pub fn implemented_rules() -> &'static [&'static str] {
    IMPLEMENTED_RULES
}

/// The reference's FULL `ALL_RULES` canonical catalogue (35 ids,
/// `check_rules/rule_ids.rb`). Deliberately BROADER than [`IMPLEMENTED_RULES`]:
/// the config audit ([`is_inert_builtin_token`]) uses it to decide whether a
/// `disable:`/`severity_overrides:` token names a real rule, so it must never
/// flag an id the reference recognizes — even one rigor-rs does not yet emit.
const ALL_CANONICAL_RULES: &[&str] = &[
    "call.undefined-method",
    "call.self-undefined-method",
    "call.unresolved-toplevel",
    "call.wrong-arity",
    "call.argument-type-mismatch",
    "call.possible-nil-receiver",
    "call.raise-non-exception",
    "dump.type",
    "assert.type-mismatch",
    "flow.always-raises",
    "flow.unreachable-branch",
    "def.return-type-mismatch",
    "def.method-visibility-mismatch",
    "def.override-visibility-reduced",
    "def.override-return-widened",
    "def.override-param-narrowed",
    "def.ivar-write-mismatch",
    "flow.dead-assignment",
    "flow.always-truthy-condition",
    "flow.unreachable-clause",
    // v0.3.0 ids. `flow.duplicate-hash-key` / `flow.return-in-ensure` /
    // `call.raise-non-exception` / `flow.shadowed-rescue-clause` /
    // `suppression.unknown-rule` / `suppression.empty` /
    // `suppression.unknown-marker` are all implemented.
    "flow.duplicate-hash-key",
    "flow.return-in-ensure",
    "flow.shadowed-rescue-clause",
    "suppression.unknown-rule",
    "suppression.empty",
    "suppression.unknown-marker",
    "static.value-use.void",
    // ADR-103 effect rules: vocabulary only, rigor-rs emits none of them yet.
    "effect.envelope-exceeded",
    "effect.liskov-widened",
    "effect.unknown-label",
    "effect.annotations-unchecked",
    // Catalogue ids rigor-rs emits none of; without them a `# rigor:disable`
    // naming one fired a spurious `suppression.unknown-rule`.
    "rbs.contradicting-signature",
    "global.write-type-mismatch",
    "global.readonly-write",
    "call.deprecated-ruby2-keywords",
];

/// True when `token` looks like a built-in-family rule id but matches none — its
/// first `.`-segment is a built-in family (`call`/`flow`/`assert`/`dump`/`def`)
/// yet it is neither the bare family wildcard nor a known canonical id, so it is
/// a likely typo whose `disable:`/`severity_overrides:` entry has no effect.
///
/// A faithful port of `ConfigAudit#inert_builtin_token?`. A token whose family is
/// NOT built-in (a plugin / `rbs_extended.*` rule, or a bare legacy alias like
/// `undefined-method`) is deliberately never flagged — it may resolve at run
/// time, so under-warning is the FP-safe choice. Validated against the full
/// reference [`ALL_CANONICAL_RULES`], not the narrower [`IMPLEMENTED_RULES`].
#[must_use]
pub fn is_inert_builtin_token(token: &str) -> bool {
    let family = token.split('.').next().unwrap_or(token);
    if !RULE_FAMILIES.contains(&family) {
        return false;
    }
    if token == family {
        return false;
    }
    !ALL_CANONICAL_RULES.contains(&token)
}

/// Maps a legacy short alias to its canonical id (reference `LEGACY_RULE_ALIASES`).
/// Only the three implemented ids can ever match a real diagnostic; the remaining
/// aliases are included for forward-compat (they expand to ids no rigor-rs
/// diagnostic carries, so they are inert).
pub fn legacy_alias(token: &str) -> Option<&'static str> {
    match token {
        "undefined-method" => Some(CALL_UNDEFINED_METHOD),
        "self-undefined-method" => Some("call.self-undefined-method"),
        "wrong-arity" => Some(CALL_WRONG_ARITY),
        "argument-type-mismatch" => Some("call.argument-type-mismatch"),
        "possible-nil-receiver" => Some(CALL_POSSIBLE_NIL_RECEIVER),
        "dump-type" => Some("dump.type"),
        "assert-type" => Some("assert.type-mismatch"),
        "always-raises" => Some("flow.always-raises"),
        "unreachable-branch" => Some("flow.unreachable-branch"),
        "method-visibility-mismatch" => Some("def.method-visibility-mismatch"),
        "ivar-write-mismatch" => Some("def.ivar-write-mismatch"),
        "dead-assignment" => Some("flow.dead-assignment"),
        "always-truthy-condition" => Some("flow.always-truthy-condition"),
        "unreachable-clause" => Some("flow.unreachable-clause"),
        "raise-non-exception" => Some("call.raise-non-exception"),
        "duplicate-hash-key" => Some(FLOW_DUPLICATE_HASH_KEY),
        "return-in-ensure" => Some(FLOW_RETURN_IN_ENSURE),
        "shadowed-rescue-clause" => Some("flow.shadowed-rescue-clause"),
        _ => None,
    }
}

/// The `pre-eval.*` ids (reference `PRE_EVAL_RULES`), the expansion of the
/// `pre-eval` family token.
const PRE_EVAL_RULES: &[&str] =
    &["pre-eval.parse-error", "pre-eval.file-not-found", "pre-eval.duplicate-declaration"];

/// Whether the `.rigor.yml` `disable:` list drops a run-level `pre-eval.*` row
/// (reference `DiagnosticAggregator#suppress_pre_eval`, #1892). The reference
/// runs these rows through `CheckRules.filter_suppressed` with
/// `disabled_rules:` and no comments (the row is positioned at `.rigor.yml`).
/// Since v0.4.4 (#1948) that filter tests `CheckRules.config_disabled?`, where
/// the `all` sentinel drops every rule-bearing row, so `disable: [all]` drops
/// the row too (at v0.4.3 it did not). Otherwise only the exact id and the
/// `pre-eval` family token match: the `pre-eval` rows sit outside every other
/// family expansion.
#[must_use]
pub fn config_disables_pre_eval<S: AsRef<str>>(disable: &[S], rule: &str) -> bool {
    disable.iter().any(|token| {
        let token = token.as_ref();
        token == "all" || token == rule || (token == "pre-eval" && PRE_EVAL_RULES.contains(&rule))
    })
}

/// Whether a suppression token list drops a plugin's own row (reference
/// v0.4.4 `CheckRules.plugin_row_disabled?`, #1953). Such a row answers only
/// to its qualified id (`plugin.<id>.<rule>`, `rule_id` here) or to the whole
/// plugin's `plugin.<id>` (`source_family`), compared as whole strings. A bare
/// rule and `all` never reach it, and a row whose family is not `plugin.`
/// (`plugin_loader`, `plugin_trust`, built-in) answers false. Family tokens
/// expand only to catalogue ids, so comparing the raw tokens is the same test
/// as comparing the reference's expanded set.
#[must_use]
pub fn config_disables_plugin_row<S: AsRef<str>>(
    disable: &[S],
    source_family: &str,
    rule_id: &str,
) -> bool {
    source_family.starts_with("plugin.")
        && disable.iter().any(|token| {
            let token = token.as_ref();
            token == source_family || token == rule_id
        })
}

/// Families of diagnostics the engine emits OUTSIDE the check-rule catalogue
/// (aggregator/reporter-level: `rbs_extended.*`, `dynamic.*`, `rbs.*`,
/// `pre-eval.*`), plus the `plugin.` prefix reserved for plugin-produced ids. A
/// suppression token whose first `.`-segment is one of these is treated as KNOWN
/// (under-warning is the FP-safe direction — these ids load dynamically / live in
/// the engine-heavy runner and cannot be enumerated here). Reference
/// `NON_CHECK_DIAGNOSTIC_FAMILIES`.
const NON_CHECK_DIAGNOSTIC_FAMILIES: &[&str] =
    &["rbs_extended", "dynamic", "rbs", "pre-eval", "plugin", "plugin_trust"];

/// Bare (dot-less) diagnostic ids the engine emits outside the catalogue. A token
/// equal to one of these is KNOWN even without a family prefix. Reference
/// `NON_CHECK_DIAGNOSTIC_IDS`.
const NON_CHECK_DIAGNOSTIC_IDS: &[&str] = &[
    "configuration-error",
    "load-error",
    "pool-degraded",
    "runtime-error",
    "source-rbs-synthesis-failed",
    "source-rbs-annotation-not-honoured",
];

/// Reference v0.4.4 `ConfigAudit.effective_disable_token?`: `all`, or a token
/// `resolve_rule_token` expands to at least one id of the FULL catalogue (a
/// legacy alias, the `pre-eval` family, a family wildcard, a canonical id).
/// Every [`RULE_FAMILIES`] entry names at least one catalogue id.
#[must_use]
pub fn is_effective_disable_token(token: &str) -> bool {
    token == "all"
        || token == "pre-eval"
        || legacy_alias(token).is_some()
        || RULE_FAMILIES.contains(&token)
        || ALL_CANONICAL_RULES.contains(&token)
}

/// Reference `ConfigAudit.non_check_engine_token?`: a bare non-catalogue
/// engine id, or a token whose first `.`-segment is a non-check family.
#[must_use]
pub fn is_non_check_engine_token(token: &str) -> bool {
    NON_CHECK_DIAGNOSTIC_IDS.contains(&token)
        || token.split('.').next().is_some_and(|f| NON_CHECK_DIAGNOSTIC_FAMILIES.contains(&f))
}

/// True when a suppression token resolves to a diagnostic identifier some producer
/// can emit: the `all` wildcard, a canonical check-rule id (the FULL
/// [`ALL_CANONICAL_RULES`], not just the emitted subset), a legacy alias, a family
/// wildcard, a bare non-catalogue engine id, or a dotted id under a known
/// non-check family (`plugin.*` is always known). A faithful port of the
/// reference's `known_suppression_token?`. Used by `suppression.unknown-rule`.
#[must_use]
pub fn known_suppression_token(token: &str) -> bool {
    if token == "all" {
        return true;
    }
    if ALL_CANONICAL_RULES.contains(&token)
        || legacy_alias(token).is_some()
        || RULE_FAMILIES.contains(&token)
        || NON_CHECK_DIAGNOSTIC_IDS.contains(&token)
    {
        return true;
    }
    // A dotted id whose family is a known non-check family (`plugin.foo`, …).
    matches!(token.split_once('.'), Some((family, _)) if NON_CHECK_DIAGNOSTIC_FAMILIES.contains(&family))
}

/// A parsed suppression set: a flag for the `all` wildcard plus the explicit
/// canonical rule ids. Mirrors the reference's `Set` that may contain the
/// `"all"` sentinel alongside real ids.
///
/// This is the single source of truth for rule-token expansion (legacy aliases,
/// the `call`/`flow`/… family wildcards, canonical ids, and the `all` wildcard).
/// It backs BOTH in-source `# rigor:disable` suppression and the `.rigor.yml`
/// `disable:` config key, so the two stay in lockstep.
#[derive(Default, Clone)]
pub struct SuppressSet {
    all: bool,
    rules: HashSet<String>,
}

impl SuppressSet {
    /// Build a set from a list of user-supplied rule tokens (e.g. a config
    /// `disable:` list), expanding each through the same logic as inline
    /// `# rigor:disable` directives. The internal-error sentinel can never be
    /// matched here — even an explicit `internal-error`/`all` token leaves it
    /// reportable (enforced by [`SuppressSet::suppresses`]).
    #[must_use]
    pub fn from_tokens<S: AsRef<str>>(tokens: &[S]) -> Self {
        let mut set = Self::default();
        for token in tokens {
            set.absorb_token(token.as_ref());
        }
        set
    }

    /// Whether this set matches `rule` (so the diagnostic should be dropped). The
    /// `internal-error` sentinel is NEVER matched, regardless of `all` or an
    /// explicit token — it represents a failure the user cannot silence (reference
    /// `rule == nil` guard).
    #[must_use]
    pub fn suppresses(&self, rule: &str) -> bool {
        if rule == INTERNAL_ERROR_RULE {
            return false;
        }
        self.all || self.rules.contains(rule)
    }

    fn is_empty(&self) -> bool {
        !self.all && self.rules.is_empty()
    }

    /// Expand one user token into this set (reference `expand_token` +
    /// `absorb_suppression_tokens`).
    fn absorb_token(&mut self, token: &str) {
        if token == "all" {
            self.all = true;
        } else if let Some(canonical) = legacy_alias(token) {
            self.rules.insert(canonical.to_string());
        } else if RULE_FAMILIES.contains(&token) {
            let prefix = format!("{token}.");
            for rule in IMPLEMENTED_RULES {
                if rule.starts_with(&prefix) {
                    self.rules.insert((*rule).to_string());
                }
            }
        } else {
            // Canonical id → itself; unknown token → passes through verbatim
            // (matches no real diagnostic ⇒ a no-op). Both paths just insert
            // the token, matching the reference's `expand_token` fallthrough.
            self.rules.insert(token.to_string());
        }
    }
}

/// Drop the diagnostics suppressed by the file's inline `# rigor:disable` /
/// `# rigor:disable-file` comments, mirroring the reference's `filter_suppressed`
/// (honored regardless of any config file). Each input is `(line, diagnostic)`
/// where `line` is the diagnostic's 1-based source line; `comments` is the
/// `(line, text)` list from [`rigor_parse::comment_lines`].
///
/// A diagnostic is dropped iff its `rule_id` is in the file-suppression set (or
/// that set contains `all`), OR its `rule_id` is in its line's suppression set
/// (or that line's set contains `all`). The internal-error sentinel is never
/// dropped.
#[must_use]
pub fn filter_suppressed(
    diagnostics: Vec<(usize, Diagnostic)>,
    comments: &[(usize, usize, String)],
) -> Vec<(usize, Diagnostic)> {
    let (line_suppressions, file_suppressions) = parse_suppression_comments(comments);

    diagnostics
        .into_iter()
        .filter(|(line, diag)| {
            // Never suppress the internal-error sentinel (reference: `rule.nil?`).
            if diag.rule_id == INTERNAL_ERROR_RULE {
                return true;
            }
            if file_suppressions.suppresses(diag.rule_id) {
                return false;
            }
            if let Some(set) = line_suppressions.get(line) {
                if set.suppresses(diag.rule_id) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Parse the comment list into `(line_suppressions, file_suppressions)`.
/// File-level directives (`# rigor:disable-file ...`) apply to every line; the
/// `-file` form is checked FIRST so a `disable-file` comment is not also read as
/// a line-level `disable` (the reference's `(?!-file)` negative lookahead).
pub(crate) fn parse_suppression_comments(
    comments: &[(usize, usize, String)],
) -> (HashMap<usize, SuppressSet>, SuppressSet) {
    let mut line_suppressions: HashMap<usize, SuppressSet> = HashMap::new();
    let mut file_suppressions = SuppressSet::default();

    for (line, _offset, text) in comments {
        if let Some(rules) = match_directive(text, "rigor:disable-file") {
            absorb_tokens(rules, &mut file_suppressions);
        } else if let Some(rules) = match_directive(text, "rigor:disable") {
            absorb_tokens(rules, line_suppressions.entry(*line).or_default());
        }
    }

    (line_suppressions, file_suppressions)
}

/// Find `#` `<ws>*` `<keyword>` `<ws>+` in `text` and return the rule-token tail
/// (everything after the keyword's trailing whitespace). Hand-rolled equivalent
/// of the reference's `/#\s*<keyword>\s+(?<rules>[\w.,\s-]+)/` (the `regex` crate
/// is a cached dep, but the patterns are simple enough to scan directly and avoid
/// pulling it into this crate). Returns `None` when the directive is absent or
/// has no whitespace-separated tail.
///
/// For the `rigor:disable` keyword the caller has already tried `disable-file`
/// first, which is how the reference's `(?!-file)` lookahead is honored: a
/// `disable-file` comment matches the `-file` branch and never reaches here.
pub(crate) fn match_directive<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let hash = text.find('#')?;
    let mut rest = &text[hash + 1..];
    // `#\s*`
    rest = rest.trim_start_matches([' ', '\t']);
    let after_kw = rest.strip_prefix(keyword)?;
    // `\s+` — at least one whitespace must follow the keyword.
    let trimmed = after_kw.trim_start_matches([' ', '\t']);
    if trimmed.len() == after_kw.len() {
        return None;
    }
    Some(trimmed)
}

/// Split the rule-token tail on whitespace/commas and absorb each token,
/// matching the reference's `raw.split(/[\s,]+/)`. The reference's `[\w.,\s-]+`
/// capture stops at the first character outside that class; tokens here are split
/// on the same delimiters, and any token is absorbed verbatim, so a trailing
/// non-rule word is simply an unknown token (a no-op).
fn absorb_tokens(tail: &str, target: &mut SuppressSet) {
    for token in tail.split([' ', '\t', ',']) {
        if !token.is_empty() {
            target.absorb_token(token);
        }
    }
}

#[cfg(test)]
mod plugin_row_disable_tests {
    use super::config_disables_plugin_row;

    const FAMILY: &str = "plugin.rails-routes";
    const ROW: &str = "plugin.rails-routes.load-error";

    #[test]
    fn qualified_id_and_plugin_family_drop_the_row() {
        assert!(config_disables_plugin_row(&["plugin.rails-routes.load-error"], FAMILY, ROW));
        assert!(config_disables_plugin_row(&["call", "plugin.rails-routes"], FAMILY, ROW));
    }

    // Must-still-fire controls (oracle v0.4.4): `all`, a bare id, the bare
    // `plugin` prefix, another plugin, and a prefix of a dotted id.
    #[test]
    fn bare_all_and_other_plugins_leave_the_row() {
        assert!(!config_disables_plugin_row::<&str>(&[], FAMILY, ROW));
        assert!(!config_disables_plugin_row(&["all"], FAMILY, ROW));
        assert!(!config_disables_plugin_row(&["load-error"], FAMILY, ROW));
        assert!(!config_disables_plugin_row(&["plugin"], FAMILY, ROW));
        assert!(!config_disables_plugin_row(&["plugin.activerecord"], FAMILY, ROW));
        assert!(!config_disables_plugin_row(&["plugin.rails"], FAMILY, ROW));
        assert!(!config_disables_plugin_row(
            &["plugin_loader.load-error", "plugin_loader"],
            "plugin_loader",
            "plugin_loader.load-error"
        ));
    }
}

#[cfg(test)]
mod pre_eval_disable_tests {
    use super::config_disables_pre_eval;

    const ROW: &str = "pre-eval.file-not-found";

    #[test]
    fn exact_id_family_token_and_all_drop_the_row() {
        assert!(config_disables_pre_eval(&["pre-eval.file-not-found"], ROW));
        assert!(config_disables_pre_eval(&["call", "pre-eval"], ROW));
        // v0.4.4 (#1948): `all` now reaches the row.
        assert!(config_disables_pre_eval(&["all"], ROW));
    }

    // Must-still-fire controls (oracle v0.4.4): no `disable:`, a sibling id
    // and an unrelated family leave the row standing.
    #[test]
    fn unrelated_tokens_leave_the_row() {
        assert!(!config_disables_pre_eval::<&str>(&[], ROW));
        assert!(!config_disables_pre_eval(&["pre-eval.parse-error"], ROW));
        assert!(!config_disables_pre_eval(&["flow", "pre"], ROW));
    }
}
