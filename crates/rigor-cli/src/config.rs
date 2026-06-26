//! `.rigor.yml` configuration loader (a safe, bounded subset of the reference's
//! schema). Config ONLY suppresses or scopes diagnostics — it never changes
//! analysis correctness. We implement two keys:
//!
//! - `disable:` — a list of rule tokens. Diagnostics whose `rule_id` matches
//!   (after the same token expansion as inline `# rigor:disable`) are dropped
//!   globally. The `internal-error` sentinel can never be disabled.
//! - `exclude:` — a list of path glob patterns. An analyzed file whose path
//!   matches any pattern is skipped entirely (no diagnostics for it).
//!
//! Any other key is ignored gracefully (the reference's full schema is large; an
//! unknown key must never error). An absent or unparseable `.rigor.yml` yields
//! [`Config::default`] — analyze normally, never crash.
//!
//! Discovery (HARNESS SAFETY): an explicit `--config <path>` wins; otherwise we
//! look for `.rigor.yml` in the CURRENT WORKING DIRECTORY only (not walking up,
//! not relative to each analyzed file), matching the reference's project-config
//! behavior. The differential harness runs from a directory with no `.rigor.yml`,
//! so config is inert there and parity is preserved.

use std::path::Path;

use glob::Pattern;
use rigor_rules::SuppressSet;
use serde::Deserialize;

/// The parsed `.rigor.yml`. Unknown keys are ignored (no `deny_unknown_fields`),
/// and every field defaults so a partial or empty file is valid.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Rule tokens to disable globally (e.g. `undefined-method`, `call`, `all`).
    pub disable: Vec<String>,
    /// Path glob patterns whose matching files are skipped entirely.
    pub exclude: Vec<String>,
    /// ADR-22 baseline path. `baseline: <path>` activates a baseline for
    /// `check`; `baseline: false` is the explicit-disable form. Absent / `null`
    /// means no baseline. Deserialized as an untyped value so both the string
    /// and `false` spellings are accepted, then coerced by [`Config::baseline_path`].
    #[serde(default)]
    pub baseline: serde_yaml::Value,
}

impl Config {
    /// Load the config. With `explicit = Some(path)` read exactly that file;
    /// otherwise auto-discover `.rigor.yml` in the current working directory.
    /// Returns [`Config::default`] on ANY problem (missing file, read error,
    /// malformed YAML), printing a brief `eprintln!` warning only for a file that
    /// exists but cannot be parsed. Never panics.
    #[must_use]
    pub fn load(explicit: Option<&Path>) -> Config {
        match explicit {
            Some(path) => {
                // An explicit path the user asked for: a read/parse failure is
                // worth a warning, but still degrades to default rather than
                // aborting the run.
                match std::fs::read_to_string(path) {
                    Ok(text) => Config::parse_or_warn(&text, &path.display().to_string()),
                    Err(e) => {
                        eprintln!("rigor: cannot read config {}: {e}", path.display());
                        Config::default()
                    }
                }
            }
            None => {
                // Auto-discovery: a missing `.rigor.yml` in cwd is the normal
                // case (no warning); only warn if it exists but is malformed.
                match std::fs::read_to_string(".rigor.yml") {
                    Ok(text) => Config::parse_or_warn(&text, ".rigor.yml"),
                    Err(_) => Config::default(),
                }
            }
        }
    }

    /// Parse YAML text, warning and falling back to default on a parse error.
    fn parse_or_warn(text: &str, label: &str) -> Config {
        match serde_yaml::from_str::<Config>(text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("rigor: ignoring malformed config {label}: {e}");
                Config::default()
            }
        }
    }

    /// The expanded `disable:` matcher, reusing the SAME rule-token expansion as
    /// inline `# rigor:disable` (single source of truth in `rigor-rules`). The
    /// `internal-error` sentinel is never matched by it.
    #[must_use]
    pub fn disable_matcher(&self) -> SuppressSet {
        SuppressSet::from_tokens(&self.disable)
    }

    /// The effective baseline path from `.rigor.yml`'s `baseline:` key, or
    /// `None` when absent / `null` / `false` (ADR-22 WD2: presence of the file
    /// on disk alone never activates it — config or `--baseline` must name it).
    #[must_use]
    pub fn baseline_path(&self) -> Option<String> {
        match &self.baseline {
            serde_yaml::Value::String(s) => Some(s.clone()),
            _ => None, // null / false / absent / non-string → no baseline
        }
    }

    /// Whether `path` (as given on the command line) matches any `exclude:`
    /// pattern. Invalid glob patterns are skipped (they match nothing) so a typo
    /// in config can never crash the run.
    #[must_use]
    pub fn is_excluded(&self, path: &str) -> bool {
        self.exclude.iter().any(|pat| match Pattern::new(pat) {
            Ok(p) => p.matches(path),
            Err(_) => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_disable_and_exclude() {
        let yaml = "disable:\n  - undefined-method\n  - call.wrong-arity\nexclude:\n  - \"vendor/**\"\n  - \"db/schema.rb\"\n";
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.disable, vec!["undefined-method", "call.wrong-arity"]);
        assert_eq!(cfg.exclude, vec!["vendor/**", "db/schema.rb"]);
        // The expanded matcher drops the aliased + canonical rules.
        let m = cfg.disable_matcher();
        assert!(m.suppresses("call.undefined-method"));
        assert!(m.suppresses("call.wrong-arity"));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // A key outside our subset must not error (reference schema is large).
        let yaml = "disable:\n  - undefined-method\nseverity_overrides:\n  foo: bar\nplugins:\n  - whatever\n";
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.disable, vec!["undefined-method"]);
        assert!(cfg.exclude.is_empty());
    }

    #[test]
    fn empty_document_is_default() {
        // An empty / whitespace-only file deserializes to all-defaults.
        let cfg: Config = serde_yaml::from_str("").unwrap_or_default();
        assert!(cfg.disable.is_empty());
        assert!(cfg.exclude.is_empty());
    }

    #[test]
    fn malformed_yaml_yields_default_without_panic() {
        // `Config::load` is file-based; exercise the parse path directly: a
        // non-mapping / broken document must degrade to default, not panic.
        let cfg = Config::parse_or_warn("disable: [unterminated\n", "test");
        assert!(cfg.disable.is_empty());
        assert!(cfg.exclude.is_empty());
    }

    #[test]
    fn exclude_glob_matching() {
        let cfg = Config {
            disable: vec![],
            exclude: vec!["vendor/**".into(), "*.rb".into()],
            ..Default::default()
        };
        assert!(cfg.is_excluded("vendor/x/y.rb"));
        assert!(cfg.is_excluded("a.rb"));
        // `*.rb` matches a bare filename; non-matches stay false.
        assert!(!cfg.is_excluded("vendor")); // no trailing segment
        assert!(!cfg.is_excluded("src/lib.txt"));
    }

    #[test]
    fn invalid_glob_pattern_is_inert() {
        // A malformed pattern must never panic; it simply matches nothing.
        let cfg = Config { disable: vec![], exclude: vec!["[".into()], ..Default::default() };
        assert!(!cfg.is_excluded("anything.rb"));
    }

    #[test]
    fn disable_never_suppresses_internal_error() {
        let cfg = Config { disable: vec!["all".into()], exclude: vec![], ..Default::default() };
        assert!(!cfg.disable_matcher().suppresses("internal-error"));
    }

    #[test]
    fn baseline_path_coercion() {
        // String → Some(path); false / null / absent → None (ADR-22 WD2).
        let s: Config = serde_yaml::from_str("baseline: .rigor-baseline.yml\n").unwrap();
        assert_eq!(s.baseline_path().as_deref(), Some(".rigor-baseline.yml"));
        let f: Config = serde_yaml::from_str("baseline: false\n").unwrap();
        assert_eq!(f.baseline_path(), None);
        let n: Config = serde_yaml::from_str("disable: []\n").unwrap();
        assert_eq!(n.baseline_path(), None);
    }
}
