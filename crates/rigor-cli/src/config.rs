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
///
/// [`Default`] is hand-written (not derived) so `signature_paths` defaults to
/// `["sig"]` — the reference's default — for both an absent key (container
/// `#[serde(default)]` fills it from here) and a missing/malformed config file
/// (`Config::load` returns `Config::default()`).
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Rule tokens to disable globally (e.g. `undefined-method`, `call`, `all`).
    pub disable: Vec<String>,
    /// Path glob patterns whose matching files are skipped entirely.
    pub exclude: Vec<String>,
    /// ADR-0040 — the default scan roots for a bare `rigor check` (no path
    /// args): `paths:` in `.rigor.yml`, defaulting to `["lib"]` (the reference's
    /// `Configuration` default). Each is expanded to its `**/*.rb` like an
    /// explicit directory arg. Ignored when the CLI is given explicit path args.
    pub paths: Vec<String>,
    /// Config-gated plugins to activate (ADR-25), as listed under `.rigor.yml`'s
    /// `plugins:`. Each entry is a plugin id — either the gem name
    /// (`rigor-activesupport-core-ext`) or the manifest id
    /// (`activesupport-core-ext`); `rigor_index::CoreIndex::with_plugins`
    /// normalises and resolves them, ignoring any that aren't bundled. The
    /// reference discovers plugins ONLY from this list (no Gemfile auto-detect),
    /// so the default (no-config) corpus run is unaffected.
    pub plugins: Vec<String>,
    /// ADR-22 baseline path. `baseline: <path>` activates a baseline for
    /// `check`; `baseline: false` is the explicit-disable form. Absent / `null`
    /// means no baseline. Deserialized as an untyped value so both the string
    /// and `false` spellings are accepted, then coerced by [`Config::baseline_path`].
    #[serde(default)]
    pub baseline: serde_yaml::Value,
    /// ADR-0033: the project's own RBS signature directories, resolved relative
    /// to the process cwd (the project-root convention config discovery uses).
    /// Defaults to `["sig"]`, matching the reference. Each existing directory's
    /// `*.rbs` are ingested into the type environment on top of core + plugin
    /// RBS, so a project's hand-written types join the known-class surface the
    /// dispatch rules witness against. A named dir that doesn't exist is inert.
    pub signature_paths: Vec<String>,
    /// ADR-0034: `rbs collection` awareness. Mirrors the reference's
    /// `rbs_collection:` config block (`auto_detect` default `true`, optional
    /// `lockfile` override).
    pub rbs_collection: RbsCollectionConfig,
    /// ADR-72: `Gemfile.lock`-gated bundled RBS overlays. `bundler.auto_detect`
    /// (default `true`) auto-applies a bundled overlay plugin for each locked gem
    /// that ships no RBS (currently `activesupport` → `activesupport-core-ext`),
    /// so a Rails project "just works" without naming the plugin in `plugins:`.
    pub bundler: BundlerConfig,
    /// ADR-50 WD2 — the `bleeding_edge:` selector: `false` (default) adopts
    /// nothing, `true` the whole overlay, a list of feature ids only those, and
    /// `{ all: true, except: [ids] }` everything but. Deserialized untyped (all
    /// four spellings) and coerced by [`Config::bleeding_edge_selector`].
    #[serde(default)]
    pub bleeding_edge: serde_yaml::Value,
    /// ADR-0036: rigor-rs-SPECIFIC config, namespaced so it stays transparent to
    /// the pure-Ruby reference (which ignores unknown keys) — the same `.rigor.yml`
    /// feeds both. Reference-schema keys stay top-level; rigor-rs-only knobs live
    /// here.
    pub rigor_rs: RigorRsConfig,
    /// The set of top-level keys that were EXPLICITLY present in the parsed file
    /// (empty for `Config::default` and for direct `serde_yaml::from_str`). The
    /// config audit ([`crate::config_audit`]) uses it to distinguish an
    /// explicitly-configured `signature_paths:` (audited) from the implicit
    /// `["sig"]` default (not audited) — mirroring the reference, whose
    /// `Configuration#signature_paths` is `nil` when unset. Populated only by
    /// [`Config::load`]; never (de)serialized.
    #[serde(skip)]
    present_keys: std::collections::BTreeSet<String>,
}

/// ADR-0036: the `rigor_rs:` namespace for rigor-rs-specific config keys — those
/// with no equivalent in the pure-Ruby reference's schema (the Ruby-sidecar
/// coverage-posture mode is the first).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RigorRsConfig {
    /// The coverage-posture mode: `require` | `auto` | `off` | a ruby binary path
    /// (ADR-0036, same grammar as `--ruby`). `None` ⇒ the context default.
    pub ruby: Option<String>,
}

/// ADR-0034: the `rbs_collection:` config block. `auto_detect` (default `true`,
/// matching the reference) enables auto-discovery of `rbs_collection.lock.yaml`
/// at the project root; `lockfile` names an explicit lockfile path instead.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct RbsCollectionConfig {
    pub auto_detect: bool,
    pub lockfile: Option<String>,
}

impl Default for RbsCollectionConfig {
    fn default() -> Self {
        RbsCollectionConfig { auto_detect: true, lockfile: None }
    }
}

/// ADR-72: the `bundler:` config block. `auto_detect` (default `true`, matching
/// the reference) enables `Gemfile.lock`-gated auto-application of bundled RBS
/// overlays.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct BundlerConfig {
    pub auto_detect: bool,
}

/// ADR-50 WD2 — the resolved bleeding-edge adoption (config `bleeding_edge:`,
/// overridable by `--bleeding-edge[=LIST]` / `--no-bleeding-edge`). Feature ids
/// are contract vocabulary (kebab-case discipline names); an unknown id in a
/// `List` / `except` is simply absent from the overlay and contributes nothing
/// — symmetric with the reference (robust across versions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BleedingEdgeSelector {
    None,
    All { except: Vec<String> },
    List(Vec<String>),
}

impl BleedingEdgeSelector {
    /// Whether the selector adopts feature `id`.
    #[must_use]
    pub fn activates(&self, id: &str) -> bool {
        match self {
            BleedingEdgeSelector::None => false,
            BleedingEdgeSelector::All { except } => !except.iter().any(|e| e == id),
            BleedingEdgeSelector::List(ids) => ids.iter().any(|e| e == id),
        }
    }
}

impl Default for BundlerConfig {
    fn default() -> Self {
        BundlerConfig { auto_detect: true }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            disable: Vec::new(),
            exclude: Vec::new(),
            paths: default_paths(),
            plugins: Vec::new(),
            baseline: serde_yaml::Value::Null,
            bleeding_edge: serde_yaml::Value::Null,
            signature_paths: default_signature_paths(),
            rbs_collection: RbsCollectionConfig::default(),
            bundler: BundlerConfig::default(),
            rigor_rs: RigorRsConfig::default(),
            present_keys: std::collections::BTreeSet::new(),
        }
    }
}

/// The reference's default `signature_paths`. A standalone fn so it seeds both
/// the [`Default`] impl and serde's per-field container default.
fn default_signature_paths() -> Vec<String> {
    vec!["sig".to_string()]
}

/// The reference's default `paths` (`Configuration`'s `"paths" => ["lib"]`) — the
/// scan roots for a bare `rigor check` with no path args.
fn default_paths() -> Vec<String> {
    vec!["lib".to_string()]
}

/// The top-level mapping keys present in a `.rigor.yml` document. Used to record
/// which keys were explicitly configured (vs defaulted). A non-mapping / broken
/// document yields an empty set — treated as "nothing explicit", which is the
/// FP-safe direction for the audit.
fn top_level_keys(text: &str) -> std::collections::BTreeSet<String> {
    match serde_yaml::from_str::<serde_yaml::Value>(text) {
        Ok(serde_yaml::Value::Mapping(map)) => map
            .keys()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect(),
        _ => std::collections::BTreeSet::new(),
    }
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
    /// Records the file's top-level keys ([`Config::present_keys`]) so the config
    /// audit can tell an explicitly-configured key from a defaulted one.
    pub(crate) fn parse_or_warn(text: &str, label: &str) -> Config {
        match serde_yaml::from_str::<Config>(text) {
            Ok(mut cfg) => {
                cfg.present_keys = top_level_keys(text);
                cfg
            }
            Err(e) => {
                eprintln!("rigor: ignoring malformed config {label}: {e}");
                Config::default()
            }
        }
    }

    /// The reference's full `Configuration::KNOWN_KEYS` — every top-level key a
    /// conforming `.rigor.yml` may carry (its `DEFAULTS` keys + `includes` + the
    /// reserved namespaces), dumped verbatim from the pinned reference. This is
    /// deliberately the REFERENCE's superset, not rigor-rs's parsed subset: a
    /// key the reference owns but rigor-rs does not parse (`severity_overrides`,
    /// `libraries`, …) is a REAL key that must never be warned about. Doubles as
    /// the did-you-mean dictionary. `rigor_rs` is the reserved namespace (ADR-99
    /// / ADR-0036) — known by construction, so the reserved-namespace exemption
    /// is inherent.
    pub const KNOWN_KEYS: [&'static str; 21] = [
        "target_ruby",
        "paths",
        "exclude",
        "plugins",
        "disable",
        "libraries",
        "signature_paths",
        "pre_eval",
        "baseline",
        "fold_platform_specific_paths",
        "cache",
        "plugins_io",
        "severity_profile",
        "severity_overrides",
        "bleeding_edge",
        "dependencies",
        "parallel",
        "bundler",
        "rbs_collection",
        "includes",
        "rigor_rs",
    ];

    /// Top-level keys the loaded file carried that no implementation owns —
    /// not a [`Self::KNOWN_KEYS`] entry (which includes the reserved
    /// namespaces). The reference records these on `Configuration#unknown_keys`
    /// at load time and `ConfigAudit` turns each into a warning; the archetypal
    /// case is a typo (`excludee:` for `exclude:`) that the loader drops in
    /// silence. Top level only, deliberately (nested unknowns are the schema
    /// tier's job — ADR-99). Empty for every conforming config.
    #[must_use]
    pub fn unknown_keys(&self) -> Vec<&str> {
        self.present_keys
            .iter()
            .filter(|k| !Self::KNOWN_KEYS.contains(&k.as_str()))
            .map(String::as_str)
            .collect()
    }

    /// The coerced `bleeding_edge:` selector (reference
    /// `Configuration#coerce_bleeding_edge`): an unrecognized shape degrades to
    /// `None` rather than erroring (the reference raises; config here never
    /// aborts a run — the audit surface owns misconfiguration complaints).
    #[must_use]
    pub fn bleeding_edge_selector(&self) -> BleedingEdgeSelector {
        match &self.bleeding_edge {
            serde_yaml::Value::Bool(true) => BleedingEdgeSelector::All { except: Vec::new() },
            serde_yaml::Value::Sequence(ids) => BleedingEdgeSelector::List(
                ids.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            ),
            serde_yaml::Value::Mapping(m) => {
                let all = m
                    .get(serde_yaml::Value::String("all".into()))
                    .and_then(serde_yaml::Value::as_bool)
                    .unwrap_or(false);
                if all {
                    let except = m
                        .get(serde_yaml::Value::String("except".into()))
                        .and_then(|v| v.as_sequence())
                        .map(|seq| {
                            seq.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
                        })
                        .unwrap_or_default();
                    BleedingEdgeSelector::All { except }
                } else {
                    BleedingEdgeSelector::None
                }
            }
            _ => BleedingEdgeSelector::None,
        }
    }

    /// The `signature_paths:` entries when the key was EXPLICITLY configured, or
    /// `None` when it was left to the `["sig"]` default. The config audit only
    /// warns on explicit paths — an absent (auto-detected) `sig/` is a normal
    /// setup, not a misconfiguration — mirroring the reference, whose
    /// `Configuration#signature_paths` is `nil` when unset.
    #[must_use]
    pub fn explicit_signature_paths(&self) -> Option<&[String]> {
        self.present_keys
            .contains("signature_paths")
            .then_some(self.signature_paths.as_slice())
    }

    /// The explicitly-configured `rbs_collection.lockfile` path, if any. `None`
    /// means auto-detection (finding nothing is normal, so it is not audited).
    #[must_use]
    pub fn rbs_collection_lockfile(&self) -> Option<&str> {
        self.rbs_collection.lockfile.as_deref()
    }

    /// The `disable:` tokens, for the config audit's inert-rule-token check.
    #[must_use]
    pub fn disable_tokens(&self) -> &[String] {
        &self.disable
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

    /// The project's own RBS signature directories from `signature_paths:`
    /// (ADR-0033), as paths resolved relative to the process cwd. An entry naming
    /// a non-existent directory is inert — ingestion skips it — so the default
    /// `["sig"]` costs nothing when a project ships no signatures.
    #[must_use]
    pub fn signature_dirs(&self) -> Vec<std::path::PathBuf> {
        self.signature_paths
            .iter()
            .map(std::path::PathBuf::from)
            .collect()
    }

    /// Every RBS signature directory to ingest for a project rooted at
    /// `project_root`: the `signature_paths:` dirs (ADR-0033) followed by the
    /// `rbs collection` gem dirs discovered under `rbs_collection.lock.yaml`
    /// (ADR-0034). Both tiers flow through the same authoritative ingestion path,
    /// so their classes are witnessed alike. Pass the process cwd (`"."`) as
    /// `project_root` — the same base config discovery uses.
    #[must_use]
    pub fn all_signature_dirs(&self, project_root: &Path) -> Vec<std::path::PathBuf> {
        let mut dirs = self.signature_dirs();
        dirs.extend(crate::rbs_collection::discover(
            self.rbs_collection.lockfile.as_deref().map(Path::new),
            project_root,
            self.rbs_collection.auto_detect,
        ));
        dirs
    }

    /// The effective plugin id set for a project rooted at `project_root`: the
    /// explicit `plugins:` list, plus (when `bundler.auto_detect`, ADR-72) a
    /// bundled overlay for each `Gemfile.lock`-locked gem that ships no RBS,
    /// de-duplicated (an explicit entry is never double-added). With no
    /// `Gemfile.lock` this equals `plugins:`, so the config-less differential
    /// harness is unaffected.
    #[must_use]
    pub fn effective_plugins(&self, project_root: &Path) -> Vec<String> {
        let mut plugins = self.plugins.clone();
        if self.bundler.auto_detect {
            for overlay in crate::bundler::auto_detected_overlays(project_root) {
                if !plugins.iter().any(|p| p == &overlay) {
                    plugins.push(overlay);
                }
            }
        }
        plugins
    }

    /// The `.rigor.yml` `rigor_rs.ruby` value (ADR-0036), if set — the config
    /// layer of the coverage-posture axis. `None` falls through to the context
    /// default during [`crate::ruby_mode::resolve`].
    #[must_use]
    pub fn ruby_config_value(&self) -> Option<&str> {
        self.rigor_rs.ruby.as_deref()
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
    fn paths_defaults_to_lib_and_parses() {
        // ADR-0040: `paths:` is the bare-`check` scan roots; default `["lib"]`.
        let empty: Config = serde_yaml::from_str("disable: []\n").unwrap();
        assert_eq!(empty.paths, vec!["lib"], "absent paths: ⇒ [\"lib\"]");
        assert_eq!(Config::default().paths, vec!["lib"]);
        let cfg: Config = serde_yaml::from_str("paths:\n  - app\n  - lib\n").unwrap();
        assert_eq!(cfg.paths, vec!["app", "lib"]);
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
    fn parses_plugins_list() {
        // ADR-25: `plugins:` is a typed list of plugin ids. Both the gem-name
        // and manifest-id spellings are accepted at the config layer (the
        // index's `with_plugins` normalises gem-name ↔ manifest-id).
        let yaml = "plugins:\n  - rigor-activesupport-core-ext\n  - activesupport-core-ext\n";
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            cfg.plugins,
            vec!["rigor-activesupport-core-ext", "activesupport-core-ext"]
        );
    }

    #[test]
    fn absent_plugins_is_empty() {
        // No `plugins:` key ⇒ empty list ⇒ the default no-config path (gating).
        let cfg: Config = serde_yaml::from_str("disable: []\n").unwrap();
        assert!(cfg.plugins.is_empty());
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
    fn signature_paths_defaults_to_sig() {
        // ADR-0033: an absent key defaults to ["sig"] (reference default), via
        // both the container `#[serde(default)]` and `Config::default()`.
        let present: Config = serde_yaml::from_str("disable: []\n").unwrap();
        assert_eq!(present.signature_paths, vec!["sig".to_string()]);
        assert_eq!(Config::default().signature_paths, vec!["sig".to_string()]);
        assert_eq!(
            Config::default().signature_dirs(),
            vec![std::path::PathBuf::from("sig")]
        );
    }

    #[test]
    fn signature_paths_explicit_list() {
        let cfg: Config =
            serde_yaml::from_str("signature_paths:\n  - sig\n  - vendor/rbs\n").unwrap();
        assert_eq!(cfg.signature_paths, vec!["sig", "vendor/rbs"]);
        assert_eq!(
            cfg.signature_dirs(),
            vec![
                std::path::PathBuf::from("sig"),
                std::path::PathBuf::from("vendor/rbs")
            ]
        );
        // An explicit empty list disables project-sig ingestion entirely.
        let none: Config = serde_yaml::from_str("signature_paths: []\n").unwrap();
        assert!(none.signature_paths.is_empty());
        assert!(none.signature_dirs().is_empty());
    }

    #[test]
    fn rigor_rs_ruby_namespaced_config() {
        // ADR-0036: the coverage-posture mode lives under the `rigor_rs:` group.
        let cfg: Config = serde_yaml::from_str("rigor_rs:\n  ruby: auto\n").unwrap();
        assert_eq!(cfg.ruby_config_value(), Some("auto"));
        // A path value round-trips verbatim.
        let p: Config = serde_yaml::from_str("rigor_rs:\n  ruby: /opt/ruby/bin/ruby\n").unwrap();
        assert_eq!(p.ruby_config_value(), Some("/opt/ruby/bin/ruby"));
        // Absent group ⇒ None (context default applies).
        assert_eq!(Config::default().ruby_config_value(), None);
        // A stray top-level `ruby:` is NOT the rigor_rs one (must be namespaced).
        let top: Config = serde_yaml::from_str("ruby: auto\n").unwrap();
        assert_eq!(top.ruby_config_value(), None);
    }

    #[test]
    fn effective_plugins_auto_detects_and_dedups() {
        let dir = std::env::temp_dir().join(format!("rigor_eff_plugins_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Gemfile.lock"),
            "GEM\n  specs:\n    activesupport (7.1.3)\n\nDEPENDENCIES\n  activesupport\n",
        )
        .unwrap();
        // Default (auto_detect on): the overlay is auto-added.
        let cfg = Config::default();
        assert_eq!(cfg.effective_plugins(&dir), vec!["activesupport-core-ext".to_string()]);
        // An explicit entry is not double-added.
        let explicit = Config { plugins: vec!["activesupport-core-ext".into()], ..Default::default() };
        assert_eq!(explicit.effective_plugins(&dir), vec!["activesupport-core-ext".to_string()]);
        // auto_detect off → only the explicit list.
        let off = Config { bundler: BundlerConfig { auto_detect: false }, ..Default::default() };
        assert!(off.effective_plugins(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
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

#[cfg(test)]
mod bleeding_edge_selector_tests {
    use super::*;

    fn sel(yaml: &str) -> BleedingEdgeSelector {
        Config::parse_or_warn(yaml, "test").bleeding_edge_selector()
    }

    /// The reference's `coerce_bleeding_edge` shapes: false/absent → None,
    /// true → All, list → List, `{all: true, except: [...]}` → All-with-except,
    /// `{all: false}` / garbage → None (degrade, never abort).
    #[test]
    fn selector_coercion_matches_reference_shapes() {
        assert_eq!(sel(""), BleedingEdgeSelector::None);
        assert_eq!(sel("bleeding_edge: false\n"), BleedingEdgeSelector::None);
        assert_eq!(
            sel("bleeding_edge: true\n"),
            BleedingEdgeSelector::All { except: Vec::new() }
        );
        assert_eq!(
            sel("bleeding_edge:\n  - use-of-void-value\n"),
            BleedingEdgeSelector::List(vec!["use-of-void-value".into()])
        );
        assert_eq!(
            sel("bleeding_edge:\n  all: true\n  except:\n    - use-of-void-value\n"),
            BleedingEdgeSelector::All { except: vec!["use-of-void-value".into()] }
        );
        assert_eq!(sel("bleeding_edge:\n  all: false\n"), BleedingEdgeSelector::None);
        assert_eq!(sel("bleeding_edge: 42\n"), BleedingEdgeSelector::None);
    }

    /// `activates`: None never; All unless excepted; List by membership —
    /// unknown ids inert in both list positions.
    #[test]
    fn selector_activation() {
        let id = "use-of-void-value";
        assert!(!BleedingEdgeSelector::None.activates(id));
        assert!(BleedingEdgeSelector::All { except: vec![] }.activates(id));
        assert!(!BleedingEdgeSelector::All { except: vec![id.into()] }.activates(id));
        assert!(BleedingEdgeSelector::List(vec![id.into()]).activates(id));
        assert!(!BleedingEdgeSelector::List(vec!["nope".into()]).activates(id));
    }
}
