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
//! The resolution order (reference `resolve`):
//!
//! 1. A user override for the exact rule id, else for the rule's FAMILY (the
//!    first `.`-separated segment) — `overrides:` from `.rigor.yml`'s
//!    `severity_overrides:` map.
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
// duplicate). Do not edit or reorder; each has exactly 28 rows, alphabetical
// by rule id.

const LENIENT: &[(&str, ResolvedSeverity)] = &[
    ("assert.type-mismatch", ResolvedSeverity::Error),
    ("call.argument-type-mismatch", ResolvedSeverity::Warning),
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
    ("flow.always-raises", ResolvedSeverity::Warning),
    ("flow.always-truthy-condition", ResolvedSeverity::Info),
    ("flow.dead-assignment", ResolvedSeverity::Info),
    ("flow.duplicate-hash-key", ResolvedSeverity::Info),
    ("flow.return-in-ensure", ResolvedSeverity::Info),
    ("flow.shadowed-rescue-clause", ResolvedSeverity::Info),
    ("flow.unreachable-branch", ResolvedSeverity::Info),
    ("flow.unreachable-clause", ResolvedSeverity::Info),
    ("rbs_extended.unsatisfied-conformance", ResolvedSeverity::Warning),
    ("static.value-use.void", ResolvedSeverity::Off),
    ("suppression.empty", ResolvedSeverity::Warning),
    ("suppression.unknown-marker", ResolvedSeverity::Warning),
    ("suppression.unknown-rule", ResolvedSeverity::Warning),
];

const BALANCED: &[(&str, ResolvedSeverity)] = &[
    ("assert.type-mismatch", ResolvedSeverity::Error),
    ("call.argument-type-mismatch", ResolvedSeverity::Error),
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
    ("flow.always-raises", ResolvedSeverity::Error),
    ("flow.always-truthy-condition", ResolvedSeverity::Warning),
    ("flow.dead-assignment", ResolvedSeverity::Warning),
    ("flow.duplicate-hash-key", ResolvedSeverity::Warning),
    ("flow.return-in-ensure", ResolvedSeverity::Warning),
    ("flow.shadowed-rescue-clause", ResolvedSeverity::Warning),
    ("flow.unreachable-branch", ResolvedSeverity::Warning),
    ("flow.unreachable-clause", ResolvedSeverity::Info),
    ("rbs_extended.unsatisfied-conformance", ResolvedSeverity::Warning),
    ("static.value-use.void", ResolvedSeverity::Off),
    ("suppression.empty", ResolvedSeverity::Warning),
    ("suppression.unknown-marker", ResolvedSeverity::Warning),
    ("suppression.unknown-rule", ResolvedSeverity::Warning),
];

const STRICT: &[(&str, ResolvedSeverity)] = &[
    ("assert.type-mismatch", ResolvedSeverity::Error),
    ("call.argument-type-mismatch", ResolvedSeverity::Error),
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
    ("flow.always-raises", ResolvedSeverity::Error),
    ("flow.always-truthy-condition", ResolvedSeverity::Error),
    ("flow.dead-assignment", ResolvedSeverity::Error),
    ("flow.duplicate-hash-key", ResolvedSeverity::Error),
    ("flow.return-in-ensure", ResolvedSeverity::Error),
    ("flow.shadowed-rescue-clause", ResolvedSeverity::Error),
    ("flow.unreachable-branch", ResolvedSeverity::Error),
    ("flow.unreachable-clause", ResolvedSeverity::Warning),
    ("rbs_extended.unsatisfied-conformance", ResolvedSeverity::Error),
    ("static.value-use.void", ResolvedSeverity::Off),
    ("suppression.empty", ResolvedSeverity::Warning),
    ("suppression.unknown-marker", ResolvedSeverity::Warning),
    ("suppression.unknown-rule", ResolvedSeverity::Warning),
];

/// A rule's FAMILY: the first `.`-separated segment of its canonical id
/// (reference `family_override`'s `rule.split(".").first`). `None` only for
/// an empty rule id, which never occurs for a real diagnostic.
fn family(rule: &str) -> Option<&str> {
    rule.split('.').next().filter(|s| !s.is_empty())
}

/// Looks up `rule` (exact id first, then its family) in an ordered overrides
/// list. A `Vec` rather than a map so callers can preserve `.rigor.yml`
/// mapping-iteration order (see `Config::severity_overrides`'s doc comment);
/// linear scan is fine at the sizes a `severity_overrides:` map realistically
/// reaches.
fn lookup_override(rule: &str, overrides: &[(String, ResolvedSeverity)]) -> Option<ResolvedSeverity> {
    overrides
        .iter()
        .find(|(key, _)| key == rule)
        .or_else(|| {
            let fam = family(rule)?;
            overrides.iter().find(|(key, _)| key == fam)
        })
        .map(|(_, sev)| *sev)
}

/// Resolves the effective severity for `rule` (reference
/// `SeverityProfile.resolve`).
///
/// Precedence, highest first:
///
/// 1. `overrides`: an exact match on `rule`, else a match on `rule`'s family
///    (the first `.`-segment) — `.rigor.yml`'s `severity_overrides:`.
/// 2. `bleeding_edge_overrides`: an exact match on `rule` ONLY — no family
///    expansion (reference: the overlay "never carries family wildcards").
/// 3. `profile`'s table entry for `rule`.
/// 4. `authored` — the rule's own default severity, when `rule` appears in
///    none of the above.
///
/// `rule` is a canonical rule id (`"call.undefined-method"`); the reference
/// takes `rule: nil` as a short-circuit for "return the authored severity" —
/// there is no such state here because a resolved diagnostic always carries a
/// rule id, so that branch has no port-side equivalent to write.
#[must_use]
pub fn resolve(
    rule: &str,
    authored: ResolvedSeverity,
    profile: Profile,
    overrides: &[(String, ResolvedSeverity)],
    bleeding_edge_overrides: &[(&str, ResolvedSeverity)],
) -> ResolvedSeverity {
    if let Some(sev) = lookup_override(rule, overrides) {
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
    fn each_profile_table_has_28_rows() {
        assert_eq!(LENIENT.len(), 28);
        assert_eq!(BALANCED.len(), 28);
        assert_eq!(STRICT.len(), 28);
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
            resolve("call.argument-type-mismatch", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Warning
        );
        assert_eq!(
            resolve("call.self-undefined-method", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Info
        );
        assert_eq!(
            resolve("static.value-use.void", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("suppression.empty", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn balanced_spot_checks() {
        let p = Profile::Balanced;
        assert_eq!(
            resolve("call.argument-type-mismatch", ResolvedSeverity::Warning, p, &[], &[]),
            ResolvedSeverity::Error
        );
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Info
        );
        assert_eq!(
            resolve("flow.unreachable-clause", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Info
        );
        assert_eq!(
            resolve("static.value-use.void", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("suppression.unknown-marker", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn strict_spot_checks() {
        let p = Profile::Strict;
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Info, p, &[], &[]),
            ResolvedSeverity::Error
        );
        assert_eq!(
            resolve("flow.unreachable-clause", ResolvedSeverity::Info, p, &[], &[]),
            ResolvedSeverity::Warning
        );
        assert_eq!(
            resolve("call.self-undefined-method", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("static.value-use.void", ResolvedSeverity::Error, p, &[], &[]),
            ResolvedSeverity::Off
        );
        assert_eq!(
            resolve("suppression.unknown-rule", ResolvedSeverity::Info, p, &[], &[]),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn balanced_is_the_default_profile() {
        assert_eq!(Profile::default(), Profile::Balanced);
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, Profile::default(), &[], &[]),
            ResolvedSeverity::Info
        );
    }

    #[test]
    fn unknown_rule_falls_back_to_authored() {
        assert_eq!(
            resolve("unknown.rule", ResolvedSeverity::Warning, Profile::Balanced, &[], &[]),
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
            ),
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
            ),
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
            ),
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
            ),
            ResolvedSeverity::Error
        );
    }

    #[test]
    fn user_override_beats_bleeding_edge_override() {
        let overrides = vec![("dump.type".to_string(), ResolvedSeverity::Warning)];
        let bleeding = [("dump.type", ResolvedSeverity::Error)];
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, Profile::Balanced, &overrides, &bleeding),
            ResolvedSeverity::Warning
        );
    }

    #[test]
    fn family_override_beats_bleeding_edge_override() {
        let overrides = vec![("dump".to_string(), ResolvedSeverity::Off)];
        let bleeding = [("dump.type", ResolvedSeverity::Error)];
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, Profile::Balanced, &overrides, &bleeding),
            ResolvedSeverity::Off
        );
    }

    #[test]
    fn bleeding_edge_override_beats_profile() {
        let bleeding = [("dump.type", ResolvedSeverity::Error)];
        assert_eq!(
            resolve("dump.type", ResolvedSeverity::Error, Profile::Balanced, &[], &bleeding),
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
            resolve("dump.type", ResolvedSeverity::Warning, Profile::Balanced, &[], &bleeding),
            ResolvedSeverity::Info // falls through to the balanced table entry
        );
    }

    #[test]
    fn off_resolution_is_returned_as_off() {
        assert_eq!(
            resolve("call.self-undefined-method", ResolvedSeverity::Error, Profile::Strict, &[], &[]),
            ResolvedSeverity::Off
        );
        let overrides = vec![("flow".to_string(), ResolvedSeverity::Off)];
        assert_eq!(
            resolve("flow.dead-assignment", ResolvedSeverity::Error, Profile::Strict, &overrides, &[]),
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
