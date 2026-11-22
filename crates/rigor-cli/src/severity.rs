//! ADR-8 § "Severity profile" — a faithful port of the reference's
//! `Rigor::Configuration::SeverityProfile` (`lib/rigor/configuration/severity_profile.rb`).
//!
//! Three named profiles tune the severity of every built-in `Analysis::CheckRules`
//! rule for the run. Profiles are applied as a **final filter** on
//! `Diagnostic#severity`: rules emit with their authored severity, then the
//! runner re-stamps the severity from the active profile before adding the
//! diagnostic to the result.
//!
//! - `lenient`: only proven diagnostics stay `:error`; uncertain rules drop to
//!   `:warning`. Useful for incremental adoption on legacy code.
//! - `balanced` (**default**): the reference's current stance — most rules
//!   `:error`; `dump.type` `:info`; uncertain rules `:warning`.
//! - `strict`: every rule is `:error`. CI-friendly.
//!
//! The resolution order (reference `resolve` / `row_override`, #1985):
//!
//! 1. A user override from `.rigor.yml`'s `severity_overrides:` map. A
//!    plugin's row answers only to its qualified id (`plugin.<id>.<rule>`),
//!    then its `plugin.<id>` family — never its bare rule or a family
//!    wildcard. Any other row answers to its qualified id, then its bare
//!    rule id, then a family wildcard (the first `.`-separated segment), with
//!    `FAMILY_FALLBACK_EXEMPT` ids unreachable by the wildcard arm.
//! 2. A bleeding-edge override for the exact rule id (no family expansion) —
//!    the ADR-50 § WD2 overlay.
//! 3. The active profile's table entry for the rule.
//! 4. The diagnostic's own authored severity (the rule's default), when the
//!    rule appears in none of the above.
//!
//! This module intentionally has NO dependency on the diagnostic pipeline —
//! wiring `resolve` into the runner is a later step, so most of this module's
//! public surface (beyond what [`crate::config`]'s accessors already reach)
//! is exercised only by its own unit tests for now.
#![allow(dead_code)]

use std::fmt;

/// One of the three named severity profiles (reference `VALID_PROFILES`).
/// Unlike the reference (which raises on an unrecognized profile symbol),
/// [`Profile::from_str`] simply returns `None` for anything else; callers
/// degrade to [`Profile::default`] — see `Config::severity_profile`'s doc
/// comment for why rigor-rs never aborts a run over a config value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    Lenient,
    #[default]
    Balanced,
    Strict,
}

impl Profile {
    /// Parses the three profile names (`lenient` | `balanced` | `strict`).
    /// Any other string returns `None` — the reference's `DEFAULT_PROFILE`
    /// fallback is the caller's responsibility, matching how [`resolve`]
    /// falls back to [`Profile::Balanced`] for an unknown profile value.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Profile> {
        match s {
            "lenient" => Some(Profile::Lenient),
            "balanced" => Some(Profile::Balanced),
            "strict" => Some(Profile::Strict),
            _ => None,
        }
    }

    /// The profile's canonical name, as it appears in `.rigor.yml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Lenient => "lenient",
            Profile::Balanced => "balanced",
            Profile::Strict => "strict",
        }
    }

    /// This profile's severity table (one row per rule, alphabetical,
    /// verbatim from the pinned reference).
    #[must_use]
    fn table(self) -> &'static [(&'static str, ResolvedSeverity)] {
        match self {
            Profile::Lenient => LENIENT,
            Profile::Balanced => BALANCED,
            Profile::Strict => STRICT,
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A resolved diagnostic severity (reference `VALID_SEVERITIES`). `Off` means
/// "drop the diagnostic entirely" — it is a real resolution outcome, not an
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedSeverity {
    Error,
    Warning,
    Info,
    Off,
}

impl ResolvedSeverity {
    /// Parses `"error"` | `"warning"` | `"info"` | `"off"`. Any other string
    /// (including YAML's bare `off` misparsing as a boolean before it ever
    /// reaches this function — see `Config::severity_overrides`'s doc
    /// comment) returns `None`.
    #[must_use]
    pub fn from_str(s: &str) -> Option<ResolvedSeverity> {
        match s {
            "error" => Some(ResolvedSeverity::Error),
            "warning" => Some(ResolvedSeverity::Warning),
            "info" => Some(ResolvedSeverity::Info),
            "off" => Some(ResolvedSeverity::Off),
            _ => None,
        }
    }

    /// The severity's canonical name, as it appears in `.rigor.yml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ResolvedSeverity::Error => "error",
            ResolvedSeverity::Warning => "warning",
            ResolvedSeverity::Info => "info",
            ResolvedSeverity::Off => "off",
        }
    }
}

impl fmt::Display for ResolvedSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// The three PROFILES tables, dumped VERBATIM from the pinned reference
// (`lib/rigor/configuration/severity_profile.rb`'s `PROFILES` constant, RC pin
// — see the reference file for the per-row ADR commentary this port does not
// duplicate). Do not edit or reorder; each has exactly 37 rows, alphabetical
// by rule id.

const LENIENT: &[(&str, ResolvedSeverity)] = &[
    ("assert.type-mismatch", ResolvedSeverity::Error),
    ("call.argument-type-mismatch", ResolvedSeverity::Warning),
    ("call.deprecated-ruby2-keywords", ResolvedSeverity::Info),
    ("call.possible-nil-receiver", ResolvedSeverity::Warning),
    ("call.raise-non-exception", ResolvedSeverity::Warning),
    ("call.self-undefined-method", ResolvedSeverity::Off),
    ("call.undefined-method", ResolvedSeverity::Error),
    ("call.unresolved-toplevel", ResolvedSeverity::Off),
    ("call.wrong-arity", ResolvedSeverity::Error),
    ("def.ivar-write-mismatch", ResolvedSeverity::Warning),
    ("def.method-visibility-mismatch", ResolvedSeverity::Warning),
    ("def.override-param-narrowed", ResolvedSeverity::Off),
    ("def.override-return-widened", ResolvedSeverity::Off),
    ("def.override-visibility-reduced", ResolvedSeverity::Off),
    ("def.return-type-mismatch", ResolvedSeverity::Warning),
    ("dump.type", ResolvedSeverity::Info),
    ("effect.annotations-unchecked", ResolvedSeverity::Info),
    ("effect.envelope-exceeded", ResolvedSeverity::Warning),
    ("effect.liskov-widened", ResolvedSeverity::Warning),
    ("effect.unknown-label", ResolvedSeverity::Info),
    ("flow.always-raises", ResolvedSeverity::Warning),
    ("flow.always-truthy-condition", ResolvedSeverity::Info),
    ("flow.dead-assignment", ResolvedSeverity::Info),
    ("flow.duplicate-hash-key", ResolvedSeverity::Info),
    ("flow.return-in-ensure", ResolvedSeverity::Info),
    ("flow.shadowed-rescue-clause", ResolvedSeverity::Info),
    ("flow.unreachable-branch", ResolvedSeverity::Info),
    ("flow.unreachable-clause", ResolvedSeverity::Info),
    ("global.readonly-write", ResolvedSeverity::Error),
    ("global.write-type-mismatch", ResolvedSeverity::Warning),
    ("plugin.api-surface", ResolvedSeverity::Off),
    ("rbs.contradicting-signature", ResolvedSeverity::Error),
    ("rbs_extended.unsatisfied-conformance", ResolvedSeverity::Warning),
    ("static.value-use.void", ResolvedSeverity::Off),
    ("suppression.empty", ResolvedSeverity::Warning),
    ("suppression.unknown-marker", ResolvedSeverity::Warning),
    ("suppression.unknown-rule", ResolvedSeverity::Warning),
];

const BALANCED: &[(&str, ResolvedSeverity)] = &[
    ("assert.type-mismatch", ResolvedSeverity::Error),
    ("call.argument-type-mismatch", ResolvedSeverity::Error),
    ("call.deprecated-ruby2-keywords", ResolvedSeverity::Warning),
    ("call.possible-nil-receiver", ResolvedSeverity::Error),
    ("call.raise-non-exception", ResolvedSeverity::Error),
    ("call.self-undefined-method", ResolvedSeverity::Off),
    ("call.undefined-method", ResolvedSeverity::Error),
    ("call.unresolved-toplevel", ResolvedSeverity::Warning),
    ("call.wrong-arity", ResolvedSeverity::Error),
    ("def.ivar-write-mismatch", ResolvedSeverity::Warning),
    ("def.method-visibility-mismatch", ResolvedSeverity::Error),
    ("def.override-param-narrowed", ResolvedSeverity::Warning),
    ("def.override-return-widened", ResolvedSeverity::Warning),
    ("def.override-visibility-reduced", ResolvedSeverity::Warning),
    ("def.return-type-mismatch", ResolvedSeverity::Warning),
    ("dump.type", ResolvedSeverity::Info),
    ("effect.annotations-unchecked", ResolvedSeverity::Info),
    ("effect.envelope-exceeded", ResolvedSeverity::Warning),
    ("effect.liskov-widened", ResolvedSeverity::Warning),
    ("effect.unknown-label", ResolvedSeverity::Info),
    ("flow.always-raises", ResolvedSeverity::Error),
    ("flow.always-truthy-condition", ResolvedSeverity::Warning),
    ("flow.dead-assignment", ResolvedSeverity::Warning),
    ("flow.duplicate-hash-key", ResolvedSeverity::Warning),
    ("flow.return-in-ensure", ResolvedSeverity::Warning),
    ("flow.shadowed-rescue-clause", ResolvedSeverity::Warning),
    ("flow.unreachable-branch", ResolvedSeverity::Warning),
    ("flow.unreachable-clause", ResolvedSeverity::Info),
    ("global.readonly-write", ResolvedSeverity::Error),
    ("global.write-type-mismatch", ResolvedSeverity::Error),
    ("plugin.api-surface", ResolvedSeverity::Off),
    ("rbs.contradicting-signature", ResolvedSeverity::Error),
    ("rbs_extended.unsatisfied-conformance", ResolvedSeverity::Warning),
    ("static.value-use.void", ResolvedSeverity::Off),
    ("suppression.empty", ResolvedSeverity::Warning),
    ("suppression.unknown-marker", ResolvedSeverity::Warning),
    ("suppression.unknown-rule", ResolvedSeverity::Warning),
];

const STRICT: &[(&str, ResolvedSeverity)] = &[
    ("assert.type-mismatch", ResolvedSeverity::Error),
    ("call.argument-type-mismatch", ResolvedSeverity::Error),
    ("call.deprecated-ruby2-keywords", ResolvedSeverity::Error),
    ("call.possible-nil-receiver", ResolvedSeverity::Error),
    ("call.raise-non-exception", ResolvedSeverity::Error),
    ("call.self-undefined-method", ResolvedSeverity::Off),
    ("call.undefined-method", ResolvedSeverity::Error),
    ("call.unresolved-toplevel", ResolvedSeverity::Error),
    ("call.wrong-arity", ResolvedSeverity::Error),
    ("def.ivar-write-mismatch", ResolvedSeverity::Error),
    ("def.method-visibility-mismatch", ResolvedSeverity::Error),
    ("def.override-param-narrowed", ResolvedSeverity::Error),
    ("def.override-return-widened", ResolvedSeverity::Error),
    ("def.override-visibility-reduced", ResolvedSeverity::Error),
    ("def.return-type-mismatch", ResolvedSeverity::Error),
    ("dump.type", ResolvedSeverity::Error),
    ("effect.annotations-unchecked", ResolvedSeverity::Info),
    ("effect.envelope-exceeded", ResolvedSeverity::Error),
    ("effect.liskov-widened", ResolvedSeverity::Error),
    ("effect.unknown-label", ResolvedSeverity::Warning),
    ("flow.always-raises", ResolvedSeverity::Error),
    ("flow.always-truthy-condition", ResolvedSeverity::Error),
    ("flow.dead-assignment", ResolvedSeverity::Error),
    ("flow.duplicate-hash-key", ResolvedSeverity::Error),
    ("flow.return-in-ensure", ResolvedSeverity::Error),
    ("flow.shadowed-rescue-clause", ResolvedSeverity::Error),
    ("flow.unreachable-branch", ResolvedSeverity::Error),
    ("flow.unreachable-clause", ResolvedSeverity::Warning),
    ("global.readonly-write", ResolvedSeverity::Error),
    ("global.write-type-mismatch", ResolvedSeverity::Error),
    ("plugin.api-surface", ResolvedSeverity::Off),
    ("rbs.contradicting-signature", ResolvedSeverity::Error),
    ("rbs_extended.unsatisfied-conformance", ResolvedSeverity::Error),
    ("static.value-use.void", ResolvedSeverity::Off),
    ("suppression.empty", ResolvedSeverity::Warning),
    ("suppression.unknown-marker", ResolvedSeverity::Warning),
    ("suppression.unknown-rule", ResolvedSeverity::Warning),
];

/// Rule ids a dot-free family key does not reach (reference
/// `FAMILY_FALLBACK_EXEMPT`, #2822). `plugin.api-surface` is the one built-in
/// id under `plugin.`, the prefix plugin-emitted rows (`plugin.<id>.<rule>`)
/// share, so a `plugin` key must not turn the bleeding-edge rule on;
/// `disable: [plugin]` does not reach it either. A key naming the id itself
/// still does, in either direction.
const FAMILY_FALLBACK_EXEMPT: &[&str] = &["plugin.api-surface"];

/// A rule's FAMILY: the first `.`-separated segment of its canonical id
/// (reference `family_override`'s `rule.split(".").first`). `None` only for
/// an empty rule id, which never occurs for a real diagnostic.
fn family(rule: &str) -> Option<&str> {
    rule.split('.').next().filter(|s| !s.is_empty())
}

/// The `severity_overrides:` entry a row answers to, or `None` (reference
/// `row_override`, #1985). A plugin's row (`plugin_family` is `Some`): its
/// qualified id, then its `plugin.<id>` — never its bare `rule` or a family
/// wildcard, because plugins share bare rules (`load-error`) and a bare key
/// would re-stamp them all. Any other row: its qualified id (the plugin
/// loader's `plugin_loader.load-error`), then its bare rule, then a family
/// wildcard. The qualified key is the more specific one, so it wins over the
/// bare key, as it does for a plugin's row.
///
/// A `Vec` rather than a map so callers can preserve `.rigor.yml`
/// mapping-iteration order (see `Config::severity_overrides`'s doc comment);
/// linear scan is fine at the sizes a `severity_overrides:` map realistically
/// reaches.
fn row_override(
    rule: &str,
    qualified_rule: &str,
    plugin_family: Option<&str>,
    overrides: &[(String, ResolvedSeverity)],
) -> Option<ResolvedSeverity> {
    if let Some(family_key) = plugin_family {
        return overrides
            .iter()
            .find(|(key, _)| key == qualified_rule)
            .or_else(|| overrides.iter().find(|(key, _)| key == family_key))
            .map(|(_, sev)| *sev);
    }
    overrides
        .iter()
        .find(|(key, _)| key == qualified_rule)
        .or_else(|| overrides.iter().find(|(key, _)| key == rule))
        .map(|(_, sev)| *sev)
        .or_else(|| family_override(rule, overrides))
}

/// The family-wildcard arm of `row_override` (reference `family_override`).
/// A family name has no `.` in it, so only a dot-free key can match one.
/// `resolve` runs for every diagnostic in a run's stream, and most
/// configurations carry no such key (usually no overrides at all), so the
/// family scan is skipped unless a dot-free key exists — the reference's own
/// no-alloc shortcut. `FAMILY_FALLBACK_EXEMPT` ids are unreachable by this
/// arm.
fn family_override(
    rule: &str,
    overrides: &[(String, ResolvedSeverity)],
) -> Option<ResolvedSeverity> {
    if !overrides.iter().any(|(key, _)| !key.contains('.')) {
        return None;
    }
    if FAMILY_FALLBACK_EXEMPT.contains(&rule) {
        return None;
    }
    let fam = family(rule)?;
    overrides.iter().find(|(key, _)| key == fam).map(|(_, sev)| *sev)
}

/// Resolves the effective severity for `rule` (reference
/// `SeverityProfile.resolve`).
///
/// Precedence, highest first:
///
/// 1. `overrides`: a match through [`row_override`] — `.rigor.yml`'s
///    `severity_overrides:`.
/// 2. `bleeding_edge_overrides`: an exact match on `rule` ONLY — no family
///    expansion (reference: the overlay "never carries family wildcards").
/// 3. `profile`'s table entry for `rule`.
/// 4. `authored` — the rule's own default severity, when `rule` appears in
///    none of the above.
///
/// `rule` is the row's SHORT rule id (`"call.undefined-method"`, or
/// `"load-error"` for a `plugin_loader.load-error` / `plugin.<id>.load-error`
/// row); `qualified_rule` is the `<source_family>.<rule>` spelling the row
/// displays — equal to `rule` for every built-in (`builtin`-family) row,
/// which is what all current non-plugin call sites pass.
/// `plugin_family` is `Some("plugin.<id>")` for a row a plugin emitted and
/// `None` for every other row (`plugin_loader` is a source family, not a
/// `plugin.` one); `Some` switches `overrides` matching to the plugin arm of
/// [`row_override`].
///
/// The reference takes `rule: nil` as a short-circuit for "return the
/// authored severity" — there is no such state here because a resolved
/// diagnostic always carries a rule id, so that branch has no port-side
/// equivalent to write.
#[must_use]
pub fn resolve(
    rule: &str,
    authored: ResolvedSeverity,
    profile: Profile,
    overrides: &[(String, ResolvedSeverity)],
    bleeding_edge_overrides: &[(&str, ResolvedSeverity)],
    qualified_rule: &str,
    plugin_family: Option<&str>,
) -> ResolvedSeverity {
    if let Some(sev) = row_override(rule, qualified_rule, plugin_family, overrides) {
        return sev;
    }

    if let Some((_, sev)) = bleeding_edge_overrides.iter().find(|(id, _)| *id == rule) {
        return *sev;
    }

    profile
        .table()
        .iter()
        .find(|(id, _)| *id == rule)
        .map_or(authored, |(_, sev)| *sev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_profile_table_has_37_rows() {
        assert_eq!(LENIENT.len(), 37);
        assert_eq!(BALANCED.len(), 37);
        assert_eq!(STRICT.len(), 37);
    }

    #[test]
    fn tables_are_alphabetical() {
        for table in [LENIENT, BALANCED, STRICT] {
            let ids: Vec<&str> = table.iter().map(|(id, _)| *id).collect();
            let mut sorted = ids.clone();
            sorted.sort_unstable();
            assert_eq!(ids, sorted);
        }
    }

    #[test]
    fn lenient_spot_checks() {
        let p = Profile::Lenient;
        assert_eq!(
            resolve("call.argument-type-mismatch", ResolvedSeverity::Error, p, &[], &[], "call.argument-type-mismatch", None),
            ResolvedSeverity::Warning
        );
        assert_eq!(
            resolve("call.self-undefined-method", ResolvedSeverity::Error, p, &[], &[], "call.self-undefined-method", None),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, p, &[], &[], "dump.type", None),
            ResolvedSeverity::Info
        );
        assert_eq!(
            resolve("static.value-use.void", ResolvedSeverity::Error, p, &[], &[], "static.value-use.void", None),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("suppression.empty", ResolvedSeverity::Error, p, &[], &[], "suppression.empty", None),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn balanced_spot_checks() {
        let p = Profile::Balanced;
        assert_eq!(
            resolve("call.argument-type-mismatch", ResolvedSeverity::Warning, p, &[], &[], "call.argument-type-mismatch", None),
            ResolvedSeverity::Error
        );
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, p, &[], &[], "dump.type", None),
            ResolvedSeverity::Info
        );
        assert_eq!(
            resolve("flow.unreachable-clause", ResolvedSeverity::Error, p, &[], &[], "flow.unreachable-clause", None),
            ResolvedSeverity::Info
        );
        assert_eq!(
            resolve("static.value-use.void", ResolvedSeverity::Error, p, &[], &[], "static.value-use.void", None),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("suppression.unknown-marker", ResolvedSeverity::Error, p, &[], &[], "suppression.unknown-marker", None),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn strict_spot_checks() {
        let p = Profile::Strict;
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Info, p, &[], &[], "dump.type", None),
            ResolvedSeverity::Error
        );
        assert_eq!(
            resolve("flow.unreachable-clause", ResolvedSeverity::Info, p, &[], &[], "flow.unreachable-clause", None),
            ResolvedSeverity::Warning
        );
        assert_eq!(
            resolve("call.self-undefined-method", ResolvedSeverity::Error, p, &[], &[], "call.self-undefined-method", None),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("static.value-use.void", ResolvedSeverity::Error, p, &[], &[], "static.value-use.void", None),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("suppression.unknown-rule", ResolvedSeverity::Info, p, &[], &[], "suppression.unknown-rule", None),
            ResolvedSeverity::Warning
        );
    }

    /// rigor-rs#818 — the nine rows resynced from the pinned reference (the
    /// same ids #255 found missing from `explain`'s catalogue). The spot
    /// checks cover the profile values that diverge from the authored
    /// severity, which is what makes a missing row a trap.
    #[test]
    fn resynced_rows_spot_checks() {
        // `global.write-type-mismatch` is authored :error but resolves like
        // `call.argument-type-mismatch` under lenient; `global.readonly-write`
        // is an error in every profile.
        assert_eq!(
            resolve("global.write-type-mismatch", ResolvedSeverity::Error, Profile::Lenient, &[], &[], "global.write-type-mismatch", None),
            ResolvedSeverity::Warning
        );
        assert_eq!(
            resolve("global.readonly-write", ResolvedSeverity::Error, Profile::Lenient, &[], &[], "global.readonly-write", None),
            ResolvedSeverity::Error
        );
        // `call.deprecated-ruby2-keywords`: info / warning / error.
        assert_eq!(
            resolve("call.deprecated-ruby2-keywords", ResolvedSeverity::Error, Profile::Lenient, &[], &[], "call.deprecated-ruby2-keywords", None),
            ResolvedSeverity::Info
        );
        assert_eq!(
            resolve("call.deprecated-ruby2-keywords", ResolvedSeverity::Error, Profile::Balanced, &[], &[], "call.deprecated-ruby2-keywords", None),
            ResolvedSeverity::Warning
        );
        // The effect rows: `unknown-label` stops at :warning even under
        // strict; `annotations-unchecked` stays :info in every profile.
        assert_eq!(
            resolve("effect.unknown-label", ResolvedSeverity::Error, Profile::Strict, &[], &[], "effect.unknown-label", None),
            ResolvedSeverity::Warning
        );
        assert_eq!(
            resolve("effect.annotations-unchecked", ResolvedSeverity::Error, Profile::Strict, &[], &[], "effect.annotations-unchecked", None),
            ResolvedSeverity::Info
        );
        assert_eq!(
            resolve("effect.envelope-exceeded", ResolvedSeverity::Error, Profile::Strict, &[], &[], "effect.envelope-exceeded", None),
            ResolvedSeverity::Error
        );
        // `plugin.api-surface` is :off and `rbs.contradicting-signature` is
        // :error in all three profiles.
        for p in [Profile::Lenient, Profile::Balanced, Profile::Strict] {
            assert_eq!(
                resolve("plugin.api-surface", ResolvedSeverity::Error, p, &[], &[], "plugin.api-surface", None),
                ResolvedSeverity::Off
            );
            assert_eq!(
                resolve("rbs.contradicting-signature", ResolvedSeverity::Error, p, &[], &[], "rbs.contradicting-signature", None),
                ResolvedSeverity::Error
            );
        }
    }

    #[test]
    fn balanced_is_the_default_profile() {
        assert_eq!(Profile::default(), Profile::Balanced);
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, Profile::default(), &[], &[], "dump.type", None),
            ResolvedSeverity::Info
        );
    }

    #[test]
    fn unknown_rule_falls_back_to_authored() {
        assert_eq!(
            resolve("unknown.rule", ResolvedSeverity::Warning, Profile::Balanced, &[], &[], "unknown.rule", None),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn per_rule_override_beats_profile() {
        let overrides = vec![("call.undefined-method".to_string(), ResolvedSeverity::Warning)];
        assert_eq!(
            resolve(
                "call.undefined-method",
                ResolvedSeverity::Error,
                Profile::Balanced,
                &overrides,
                &[]
            , "call.undefined-method", None),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn family_override_applies_only_to_its_family() {
        let overrides = vec![("call".to_string(), ResolvedSeverity::Off)];
        // Silences call.undefined-method (family "call")...
        assert_eq!(
            resolve(
                "call.undefined-method",
                ResolvedSeverity::Error,
                Profile::Balanced,
                &overrides,
                &[]
            , "call.undefined-method", None),
            ResolvedSeverity::Off
        );
        // ...but not flow.dead-assignment (family "flow").
        assert_eq!(
            resolve(
                "flow.dead-assignment",
                ResolvedSeverity::Warning,
                Profile::Balanced,
                &overrides,
                &[]
            , "flow.dead-assignment", None),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn per_rule_override_beats_family_override() {
        let overrides = vec![
            ("call".to_string(), ResolvedSeverity::Off),
            ("call.undefined-method".to_string(), ResolvedSeverity::Error),
        ];
        assert_eq!(
            resolve(
                "call.undefined-method",
                ResolvedSeverity::Error,
                Profile::Strict,
                &overrides,
                &[]
            , "call.undefined-method", None),
            ResolvedSeverity::Error
        );
    }

    #[test]
    fn user_override_beats_bleeding_edge_override() {
        let overrides = vec![("dump.type".to_string(), ResolvedSeverity::Warning)];
        let bleeding = [("dump.type", ResolvedSeverity::Error)];
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, Profile::Balanced, &overrides, &bleeding, "dump.type", None),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn family_override_beats_bleeding_edge_override() {
        let overrides = vec![("dump".to_string(), ResolvedSeverity::Off)];
        let bleeding = [("dump.type", ResolvedSeverity::Error)];
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, Profile::Balanced, &overrides, &bleeding, "dump.type", None),
            ResolvedSeverity::Off
        );
    }

    #[test]
    fn bleeding_edge_override_beats_profile() {
        let bleeding = [("dump.type", ResolvedSeverity::Error)];
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, Profile::Balanced, &[], &bleeding, "dump.type", None),
            ResolvedSeverity::Error
        );
    }

    #[test]
    fn bleeding_edge_override_is_exact_id_only_no_family_expansion() {
        // A bleeding-edge entry keyed by "dump" (a family, not a rule id) must
        // NOT match "dump.type" — unlike `overrides`, there is no family
        // lookup on this tier.
        let bleeding = [("dump", ResolvedSeverity::Error)];
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Warning, Profile::Balanced, &[], &bleeding, "dump.type", None),
            ResolvedSeverity::Info // falls through to the balanced table entry
        );
    }

    #[test]
    fn off_resolution_is_returned_as_off() {
        assert_eq!(
            resolve("call.self-undefined-method", ResolvedSeverity::Error, Profile::Strict, &[], &[], "call.self-undefined-method", None),
            ResolvedSeverity::Off
        );
        let overrides = vec![("flow".to_string(), ResolvedSeverity::Off)];
        assert_eq!(
            resolve("flow.dead-assignment", ResolvedSeverity::Error, Profile::Strict, &overrides, &[], "flow.dead-assignment", None),
            ResolvedSeverity::Off
        );
    }

    #[test]
    fn profile_str_roundtrip() {
        assert_eq!(Profile::from_str("lenient"), Some(Profile::Lenient));
        assert_eq!(Profile::from_str("balanced"), Some(Profile::Balanced));
        assert_eq!(Profile::from_str("strict"), Some(Profile::Strict));
        assert_eq!(Profile::from_str("nonsense"), None);
        assert_eq!(Profile::Lenient.as_str(), "lenient");
        assert_eq!(Profile::Balanced.as_str(), "balanced");
        assert_eq!(Profile::Strict.as_str(), "strict");
    }

    /// rigor-rs#825 — `FAMILY_FALLBACK_EXEMPT`: `plugin.api-surface` is the
    /// one built-in id under `plugin.`, so a dot-free `plugin` key (a
    /// plugin-family key upstream) must NOT re-stamp it; an exact key still
    /// does, in either direction.
    #[test]
    fn plugin_family_key_does_not_reach_plugin_api_surface() {
        let overrides = vec![("plugin".to_string(), ResolvedSeverity::Error)];
        assert_eq!(
            resolve("plugin.api-surface", ResolvedSeverity::Warning, Profile::Balanced, &overrides, &[], "plugin.api-surface", None),
            ResolvedSeverity::Off // falls through to the profile table entry
        );
        // The exact id still reaches it.
        let overrides = vec![("plugin.api-surface".to_string(), ResolvedSeverity::Error)];
        assert_eq!(
            resolve("plugin.api-surface", ResolvedSeverity::Warning, Profile::Balanced, &overrides, &[], "plugin.api-surface", None),
            ResolvedSeverity::Error
        );
    }

    /// rigor-rs#825 — `row_override`'s plugin arm (#1985): a `plugin.<id>`
    /// row answers to its qualified id, then its `plugin.<id>` family —
    /// never its bare rule or a family wildcard.
    #[test]
    fn plugin_row_answers_only_to_qualified_id_and_plugin_family() {
        let qualified = "plugin.activerecord.load-error";
        // Neither the bare rule nor a family wildcard reaches the row; its
        // `plugin.<id>` family does.
        let overrides = vec![
            ("load-error".to_string(), ResolvedSeverity::Off),
            ("plugin".to_string(), ResolvedSeverity::Off),
            ("plugin.activerecord".to_string(), ResolvedSeverity::Error),
        ];
        assert_eq!(
            resolve("load-error", ResolvedSeverity::Info, Profile::Balanced, &overrides, &[], qualified, Some("plugin.activerecord")),
            ResolvedSeverity::Error
        );
        // The qualified id wins over the `plugin.<id>` family.
        let overrides = vec![
            ("plugin.activerecord".to_string(), ResolvedSeverity::Error),
            (qualified.to_string(), ResolvedSeverity::Warning),
        ];
        assert_eq!(
            resolve("load-error", ResolvedSeverity::Info, Profile::Balanced, &overrides, &[], qualified, Some("plugin.activerecord")),
            ResolvedSeverity::Warning
        );
    }

    /// rigor-rs#825 — `plugin_loader` is a source family, not a `plugin.`
    /// one, so its row takes the non-plugin path: qualified id, then bare
    /// rule, then family wildcard. Oracle: `plugin_loader: "off"` leaves the
    /// row at :error (a miss under all three), while `load-error: "off"`
    /// drops it.
    #[test]
    fn plugin_loader_row_answers_qualified_then_bare_then_family() {
        let qualified = "plugin_loader.load-error";
        for overrides in [
            vec![("plugin_loader".to_string(), ResolvedSeverity::Off)],
            vec![("plugin_loader.load".to_string(), ResolvedSeverity::Off)],
        ] {
            assert_eq!(
                resolve("load-error", ResolvedSeverity::Error, Profile::Balanced, &overrides, &[], qualified, None),
                ResolvedSeverity::Error
            );
        }
        for overrides in [
            vec![("plugin_loader.load-error".to_string(), ResolvedSeverity::Off)],
            vec![("load-error".to_string(), ResolvedSeverity::Off)],
        ] {
            assert_eq!(
                resolve("load-error", ResolvedSeverity::Error, Profile::Balanced, &overrides, &[], qualified, None),
                ResolvedSeverity::Off
            );
        }
        // The qualified key wins over the bare one.
        let overrides = vec![
            ("load-error".to_string(), ResolvedSeverity::Off),
            (qualified.to_string(), ResolvedSeverity::Warning),
        ];
        assert_eq!(
            resolve("load-error", ResolvedSeverity::Error, Profile::Balanced, &overrides, &[], qualified, None),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn severity_str_roundtrip() {
        assert_eq!(ResolvedSeverity::from_str("error"), Some(ResolvedSeverity::Error));
        assert_eq!(ResolvedSeverity::from_str("warning"), Some(ResolvedSeverity::Warning));
        assert_eq!(ResolvedSeverity::from_str("info"), Some(ResolvedSeverity::Info));
        assert_eq!(ResolvedSeverity::from_str("off"), Some(ResolvedSeverity::Off));
        assert_eq!(ResolvedSeverity::from_str("nonsense"), None);
        assert_eq!(ResolvedSeverity::Error.as_str(), "error");
        assert_eq!(ResolvedSeverity::Warning.as_str(), "warning");
        assert_eq!(ResolvedSeverity::Info.as_str(), "info");
        assert_eq!(ResolvedSeverity::Off.as_str(), "off");
    }
}
