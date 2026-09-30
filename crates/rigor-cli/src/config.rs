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
//! unknown key must never error). An absent `.rigor.yml` yields
//! [`Config::default`] — analyze normally, never crash.
//!
//! Discovery (HARNESS SAFETY) follows the reference's
//! `Configuration::DISCOVERY_ORDER`: an explicit `--config <path>` wins;
//! otherwise `.rigor.yml` then `.rigor.dist.yml` in the CURRENT WORKING
//! DIRECTORY only (not walking up, not relative to each analyzed file); the
//! first present wins outright — the two are never implicitly merged (a
//! committed default is composed only via an explicit `includes:` list).
//! The differential harness runs from a directory with neither, so config is
//! inert there and parity is preserved.
//!
//! Path values (issue #158): the reference's `load_with_includes` resolves
//! every entry of `paths:`, `signature_paths:`, `pre_eval:`,
//! `plugins_io.allowed_paths:` and `includes:` with `File.expand_path`
//! against the directory of the FILE THAT DECLARES THEM — `~`/`~user`
//! expanded, `.`/`..` folded lexically (no symlink resolution), one resolver
//! per file, included files first and the including file's keys merged over
//! them. `Config::read` ports that whole pipeline: the `paths:` /
//! `signature_paths:` / `pre_eval:` fields carry the RESOLVED (absolute)
//! strings the reference's `Configuration` stores, while keys the file never
//! wrote keep their cwd-relative defaults (`["lib"]`, `["sig"]`).

use std::path::Path;

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
    #[serde(deserialize_with = "de_ruby_array")]
    pub disable: Vec<String>,
    /// Path glob patterns whose matching files are skipped entirely.
    #[serde(deserialize_with = "de_ruby_array")]
    pub exclude: Vec<String>,
    /// ADR-0040 — the default scan roots for a bare `rigor check` (no path
    /// args): `paths:` in `.rigor.yml`, defaulting to `["lib"]` (the reference's
    /// `Configuration` default). Each is expanded to its `**/*.rb` like an
    /// explicit directory arg. Ignored when the CLI is given explicit path args.
    #[serde(deserialize_with = "de_ruby_array")]
    pub paths: Vec<String>,
    /// Config-gated plugins to activate (ADR-25), as listed under `.rigor.yml`'s
    /// `plugins:`. Each entry is a plugin id — either the gem name
    /// (`rigor-activesupport-core-ext`) or the manifest id
    /// (`activesupport-core-ext`); `rigor_index::CoreIndex::with_plugins`
    /// normalises and resolves them, ignoring any that aren't bundled. The
    /// reference discovers plugins ONLY from this list (no Gemfile auto-detect),
    /// so the default (no-config) corpus run is unaffected.
    #[serde(deserialize_with = "de_ruby_array")]
    pub plugins: Vec<String>,
    /// ADR-17 — the `pre_eval:` monkey-patch files. Resolved at load like
    /// `paths:` (issue #158): each entry is `File.expand_path(entry,
    /// config_dir)` upstream, so a file-loaded config carries the ABSOLUTE
    /// spellings here. The port models only the slice-1 surface —
    /// `pre-eval.file-not-found` on an entry that is not a file on disk
    /// ([`crate::main`]'s run-level rows); the pre-pass scanner's suppression
    /// effect is not modelled.
    #[serde(deserialize_with = "de_ruby_array")]
    pub pre_eval: Vec<String>,
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
    #[serde(deserialize_with = "de_signature_paths")]
    pub signature_paths: Vec<String>,
    /// ADR-0034: `rbs collection` awareness. Mirrors the reference's
    /// `rbs_collection:` config block (`auto_detect` default `true`, optional
    /// `lockfile` override). Sub-values coerce like the reference's
    /// `hash.fetch(key)` reads (`auto_detect` is `== true`, `lockfile` is
    /// `to_s`-or-nil); a non-mapping block is a load error, like the
    /// reference's `Hash#merge` on it.
    #[serde(deserialize_with = "de_rbs_collection")]
    pub rbs_collection: RbsCollectionConfig,
    /// ADR-72: `Gemfile.lock`-gated bundled RBS overlays. `bundler.auto_detect`
    /// (default `true`) auto-applies a bundled overlay plugin for each locked gem
    /// that ships no RBS (currently `activesupport` → `activesupport-core-ext`),
    /// so a Rails project "just works" without naming the plugin in `plugins:`.
    #[serde(deserialize_with = "de_bundler")]
    pub bundler: BundlerConfig,
    /// ADR-50 WD2 — the `bleeding_edge:` selector: `false` (default) adopts
    /// nothing, `true` the whole overlay, a list of feature ids only those, and
    /// `{ all: true, except: [ids] }` everything but. Deserialized untyped (all
    /// four spellings) and coerced by [`Config::bleeding_edge_selector`].
    #[serde(default)]
    pub bleeding_edge: serde_yaml::Value,
    /// ADR-8 § "Severity profile" — `severity_profile:`, one of `lenient` |
    /// `balanced` | `strict`. Deserialized untyped so an absent key, a
    /// non-string, or an unrecognized name are all accepted at the parse
    /// layer and coerced by [`Config::severity_profile`].
    ///
    /// `#[allow(dead_code)]`: this is severity-resolution MACHINERY
    /// ([`crate::severity`]) landed ahead of the runner wiring that will
    /// read the accessor; not read directly (`Debug`'s derive doesn't count
    /// for dead-code analysis), only through [`Config::severity_profile`].
    #[serde(default)]
    #[allow(dead_code)]
    pub severity_profile: serde_yaml::Value,
    /// ADR-8 — `severity_overrides:`, a mapping of rule id or rule FAMILY (the
    /// first `.`-segment, e.g. `call`) to a severity string. Deserialized
    /// untyped (rather than `HashMap<String, String>`) so an invalid entry —
    /// wrong value type, unknown severity name, or YAML 1.1's bare `off`
    /// misparsing as `false` — degrades per-entry instead of failing the
    /// whole map; see [`Config::severity_overrides`]'s doc comment.
    ///
    /// `#[allow(dead_code)]`: see [`Self::severity_profile`]'s note — the
    /// runner wiring that consumes [`Config::severity_overrides`] lands later.
    #[serde(default)]
    #[allow(dead_code)]
    pub severity_overrides: serde_yaml::Value,
    /// ADR-0036: rigor-rs-SPECIFIC config, namespaced so it stays transparent to
    /// the pure-Ruby reference (which ignores unknown keys) — the same `.rigor.yml`
    /// feeds both. Reference-schema keys stay top-level; rigor-rs-only knobs live
    /// here. The reference NEVER reads this namespace (`RESERVED_NAMESPACES`),
    /// so the deserializer tolerates every shape — anything it cannot use is
    /// simply absent, never a load error.
    #[serde(deserialize_with = "de_rigor_rs")]
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
    /// `signature_paths:` merged to an explicit null — the reference's `nil`
    /// ("not configured"), so it is NOT an explicit declaration even though
    /// the key is in [`Self::present_keys`]. Set by [`Config::read`] from the
    /// MERGED document (an `includes:` chain may write it).
    #[serde(skip)]
    signature_paths_null: bool,
    /// `target_ruby:` as written (untyped: YAML reads `3.4` as a float). Read
    /// only by [`Config::target_ruby_supported`].
    #[serde(default)]
    target_ruby: serde_yaml::Value,
    /// The ABSOLUTE directory of the top-level config file actually read
    /// (`File.dirname(File.expand_path(path))` — the reference's `base_dir`).
    /// Path-bearing keys are resolved at LOAD time, so this is kept only for
    /// the conformance gates that still take a base ([`Self::config_base_dir`]);
    /// `None` for a `Config` that did not come from a file.
    #[serde(skip)]
    base_dir: Option<std::path::PathBuf>,
    /// Issue #129: the file's text is a config the reference provably loads
    /// and reads as the port does ([`crate::conformance_gate::config_text_ok`]).
    /// `false` for the default (no file). Never (de)serialized.
    #[serde(skip)]
    parity_text_ok: bool,
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
            pre_eval: Vec::new(),
            baseline: serde_yaml::Value::Null,
            bleeding_edge: serde_yaml::Value::Null,
            severity_profile: serde_yaml::Value::Null,
            severity_overrides: serde_yaml::Value::Null,
            signature_paths: default_signature_paths(),
            rbs_collection: RbsCollectionConfig::default(),
            bundler: BundlerConfig::default(),
            rigor_rs: RigorRsConfig::default(),
            present_keys: std::collections::BTreeSet::new(),
            signature_paths_null: false,
            target_ruby: serde_yaml::Value::Null,
            base_dir: None,
            parity_text_ok: false,
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

/// Ruby's `Array(value).map(&:to_s)` over a YAML value — how the reference's
/// `Configuration#initialize` reads EVERY list-valued key (`paths`, `exclude`,
/// `plugins`, `disable`, `signature_paths`, `pre_eval`, …): `nil` is `[]`,
/// a scalar is a one-element list, a sequence is itself, a mapping's pairs are
/// its `to_a` elements, and each element goes through `to_s` (`1` → `"1"`,
/// `true` → `"true"`, `nil` → `""`, `[1, 2]` → `"[1, 2]"`,
/// `{"a" => 1}` → `'{"a" => 1}'`). Never fails — the reference's `Array()`
/// accepts every YAML shape, so a key whose value is "wrongly" shaped loads
/// as the same inert tokens the reference stores.
fn ruby_array(value: &serde_yaml::Value) -> Result<Vec<String>, String> {
    Ok(ruby_array_raw(value)
        .iter()
        .map(ruby_to_s)
        .collect::<Vec<String>>())
}

/// `Array(value)` alone — the VALUE-level list coercion the loader's
/// `resolve_path_key!` / `includes:` handling uses before stringifying each
/// element. `nil` → `[]`, a sequence → its items, a mapping → its `to_a`
/// pair-arrays, any other scalar → a one-element list.
fn ruby_array_raw(value: &serde_yaml::Value) -> Vec<serde_yaml::Value> {
    use serde_yaml::Value;
    match value {
        Value::Null => Vec::new(),
        Value::Sequence(items) => items.clone(),
        Value::Mapping(map) => map
            .iter()
            .map(|(k, v)| Value::Sequence(vec![k.clone(), v.clone()]))
            .collect(),
        scalar => vec![scalar.clone()],
    }
}

/// Ruby `to_s` for a YAML value: scalars render plainly (`nil` → `""`), and a
/// collection renders in `inspect` form — `Array#to_s` / `Hash#to_s` inspect
/// their elements.
fn ruby_to_s(v: &serde_yaml::Value) -> String {
    use serde_yaml::Value;
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Sequence(_) | Value::Mapping(_) => ruby_inspect(v),
        Value::Tagged(t) => ruby_to_s(&t.value),
    }
}

/// Ruby `inspect` for a YAML value — how a collection element renders under
/// `to_s` (`["a", 1].to_s` ⇒ `'["a", 1]'`, `{"a" => 1}.to_s` ⇒
/// `'{"a" => 1}'`) and how the `include not found` error quotes its entry.
/// String quoting is Rust's `{:?}` — for the printable-ASCII config values
/// this surfaces it renders identically to `String#inspect`.
fn ruby_inspect(v: &serde_yaml::Value) -> String {
    use serde_yaml::Value;
    match v {
        Value::Null => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("{s:?}"),
        Value::Sequence(items) => {
            let inner: Vec<String> = items.iter().map(ruby_inspect).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Mapping(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{} => {}", ruby_inspect(k), ruby_inspect(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Value::Tagged(t) => ruby_inspect(&t.value),
    }
}

/// The shared serde adapter for every list-valued key: [`ruby_array`].
fn de_ruby_array<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(d)?;
    ruby_array(&value).map_err(serde::de::Error::custom)
}

/// `signature_paths:` is the one list key whose explicit `null` is NOT
/// `Array(nil)`: the reference keeps it `nil` (`sig_paths.nil? ? nil : …`),
/// i.e. "not configured, use the default discovery" — the same as an absent
/// key. So null yields the default here, and [`Config::explicit_signature_paths`]
/// treats it as undeclared.
fn de_signature_paths<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(d)?;
    if value.is_null() {
        return Ok(default_signature_paths());
    }
    ruby_array(&value).map_err(serde::de::Error::custom)
}

/// `bundler:` — the reference reads it as a Hash (`DEFAULTS.fetch.merge`) and
/// takes `auto_detect` strictly as `== true`; a non-mapping block crashes
/// upstream (`Hash#merge`), so it is a load error here.
fn de_bundler<'de, D>(d: D) -> Result<BundlerConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(d)?;
    match value {
        serde_yaml::Value::Mapping(map) => Ok(BundlerConfig {
            auto_detect: map.get("auto_detect") == Some(&serde_yaml::Value::Bool(true)),
        }),
        serde_yaml::Value::Null => Ok(BundlerConfig::default()),
        other => Err(serde::de::Error::custom(format!(
            "`bundler:` must be a mapping, got {}",
            describe_value(&other)
        ))),
    }
}

/// `rbs_collection:` — same `fetch`-then-coerce contract as `bundler:`:
/// `auto_detect` is `== true`, `lockfile` is `nil`-or-`to_s`.
fn de_rbs_collection<'de, D>(d: D) -> Result<RbsCollectionConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(d)?;
    match value {
        serde_yaml::Value::Mapping(map) => Ok(RbsCollectionConfig {
            auto_detect: map.get("auto_detect") == Some(&serde_yaml::Value::Bool(true)),
            lockfile: map
                .get("lockfile")
                .filter(|v| !v.is_null())
                .map(ruby_to_s),
        }),
        serde_yaml::Value::Null => Ok(RbsCollectionConfig::default()),
        other => Err(serde::de::Error::custom(format!(
            "`rbs_collection:` must be a mapping, got {}",
            describe_value(&other)
        ))),
    }
}

/// `rigor_rs:` — rigor-rs's reserved namespace. The reference NEVER reads it,
/// so any shape it cannot use is simply absent rather than a load error.
fn de_rigor_rs<'de, D>(d: D) -> Result<RigorRsConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(d)?;
    let serde_yaml::Value::Mapping(map) = value else {
        return Ok(RigorRsConfig::default());
    };
    Ok(RigorRsConfig {
        ruby: map
            .get("ruby")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

/// A short name for a YAML value's shape, for load-error text.
fn describe_value(v: &serde_yaml::Value) -> &'static str {
    match v {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "a boolean",
        serde_yaml::Value::Number(_) => "a number",
        serde_yaml::Value::String(_) => "a string",
        serde_yaml::Value::Sequence(_) => "a sequence",
        serde_yaml::Value::Mapping(_) => "a mapping",
        serde_yaml::Value::Tagged(_) => "a tagged value",
    }
}

/// The top-level mapping keys present in a `.rigor.yml` document. Used to record
/// which keys were explicitly configured (vs defaulted) — whatever the value's
/// shape: a scalar (`paths: other`) or null is as declared as a list. A
/// non-mapping / broken document yields an empty set — treated as "nothing
/// explicit", which is the FP-safe direction for the audit.
#[cfg(test)]
fn top_level_keys(text: &str) -> std::collections::BTreeSet<String> {
    match serde_yaml::from_str::<serde_yaml::Value>(text) {
        Ok(serde_yaml::Value::Mapping(map)) => map
            .keys()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect(),
        _ => std::collections::BTreeSet::new(),
    }
}

/// Whether the document writes `signature_paths:` with an explicit null value
/// (`signature_paths:` / `signature_paths: ~`) — the reference's `nil`, which
/// is "not configured" rather than an empty list.
#[cfg(test)]
fn signature_paths_is_null(text: &str) -> bool {
    match serde_yaml::from_str::<serde_yaml::Value>(text) {
        Ok(serde_yaml::Value::Mapping(map)) => {
            map.get("signature_paths").is_some_and(serde_yaml::Value::is_null)
        }
        _ => false,
    }
}

/// `Configuration::DISCOVERY_ORDER` — the cwd candidates `Config::load(None)`
/// probes in order; the first present wins outright (there is NO implicit
/// merge: a committed `.rigor.dist.yml` is composed only through an explicit
/// `includes:` list).
const DISCOVERY_ORDER: [&str; 2] = [".rigor.yml", ".rigor.dist.yml"];

/// A `.rigor.yml` load failure the reference dies on (`Configuration.load`
/// raising inside a CLI command): `code` is the exit status — `64`
/// (`EXIT_USAGE`, the `rescue ConfigurationError` surface — bad YAML, a
/// non-mapping document, an `includes:` miss or a cycle) or `1` (an
/// uncaught `Errno`/`ArgumentError` upstream — a file that exists but
/// cannot be read, an unknown `~user`). `message` is the `rigor:` line body.
#[derive(Debug)]
pub struct LoadFailure {
    pub message: String,
    pub code: u8,
}

impl LoadFailure {
    /// `rigor: <message>` on stderr — the reference dispatcher's
    /// `rescue ConfigurationError` line — and the exit status to propagate.
    pub fn report(&self) -> std::process::ExitCode {
        eprintln!("rigor: {}", self.message);
        std::process::ExitCode::from(self.code)
    }
}

/// What reading a `.rigor.yml` actually found — the three outcomes a caller
/// must distinguish.
///
/// The distinction matters for a LONG-LIVED reader (the LSP): **absent** is
/// the normal case — the defaults genuinely ARE the configuration — while
/// **fatal** is a config the user wrote but the reference would die on,
/// where adopting the defaults would silently drop their `disable:` list
/// mid-edit; the last good config keeps serving.
pub enum ConfigRead {
    /// Read, includes-resolved, and parsed.
    Parsed(Box<Config>),
    /// Nothing at that path (the reference's `File.exist?` gate). `.rigor.yml`
    /// is optional, so this is the normal case; the payload names the path
    /// for a caller that wants to report it.
    Absent(#[allow(dead_code)] String),
    /// The reference dies loading this config — see [`LoadFailure`].
    Fatal(LoadFailure),
}

impl Config {
    /// The path `Configuration.load(nil)` would read — the first
    /// `DISCOVERY_ORDER` candidate that exists — or `None` (defaults). A
    /// public mirror of `Configuration.discover` for labels and the LSP.
    #[must_use]
    pub fn discover() -> Option<std::path::PathBuf> {
        DISCOVERY_ORDER
            .iter()
            .map(Path::new)
            .find(|p| p.exists())
            .map(Path::to_path_buf)
    }

    /// Read the config at `path` WITHOUT deciding what to do about a failure —
    /// the shared loader behind [`Config::load`] (which reports fatal loads
    /// like the reference's dispatcher) and the LSP's reload (which keeps the
    /// last good config). Prints nothing itself, so the caller owns the
    /// channel and the wording.
    ///
    /// Two gates, in the reference's order: `File.exist?(path)` on the RAW
    /// spelling (a leading `~` is never expanded, `..` resolves through the
    /// OS — symlinks honoured), then `File.expand_path(path)` for the file
    /// actually read (`~`/`~user` expanded, `..` folded LEXICALLY — so
    /// `lnk/../x.yml` reads the lexical parent even when `lnk` points
    /// elsewhere, and a `~`-led path that only exists literally reads the
    /// `$HOME` file).
    #[must_use]
    pub fn read(path: &Path) -> ConfigRead {
        if !path.exists() {
            return ConfigRead::Absent(format!("no such file or directory — {}", path.display()));
        }
        let absolute = match expand_tilde_head(&path.to_string_lossy())
            .map(|s| crate::conformance_gate::expand_path(Path::new(&s)))
        {
            Ok(p) => p,
            Err(f) => return ConfigRead::Fatal(f),
        };
        let mut includes_seen = false;
        let merged = match load_with_includes(
            &absolute,
            &std::collections::BTreeSet::new(),
            &mut includes_seen,
        ) {
            Ok(m) => m,
            Err(f) => return ConfigRead::Fatal(f),
        };
        // `coerce_severity_overrides` raises ConfigurationError at load
        // (upstream initialize, configuration.rb:1037) — before the
        // unknown-key pass.
        if let Err(f) = validate_severity_overrides(&merged) {
            return ConfigRead::Fatal(f);
        }
        // Upstream keeps EVERY parsed key in `data` (objects, not just
        // strings) and renders `data.keys.map(&:to_s)` for unknown_keys —
        // `1: one` warns `` `1` `` and loads. serde_yaml would instead die
        // `invalid type: integer `1`, expected field identifier` inside
        // `from_value`. Coerce non-String keys to their `to_s` up front so
        // they take the same unknown-key path.
        let merged = stringify_top_level_keys(merged);
        let mut cfg: Config = match serde_yaml::from_value(serde_yaml::Value::Mapping(merged.clone()))
        {
            Ok(c) => c,
            // The per-key coercions accept every shape (Ruby's `Array()` /
            // `fetch`/`==` semantics), so a document that survived YAML
            // parsing reaches here only through a loader bug.
            Err(e) => {
                return ConfigRead::Fatal(LoadFailure {
                    message: format!("{}: invalid config: {e}", absolute.display()),
                    code: 64,
                })
            }
        };
        cfg.present_keys = merged
            .keys()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect();
        if includes_seen {
            // `includes:` is a load-time directive upstream — `delete`d before
            // merge so it never reaches `unknown_keys` (it IS a known key, so
            // recording it only feeds `declares_key`).
            cfg.present_keys.insert("includes".to_string());
        }
        cfg.signature_paths_null = merged
            .get("signature_paths")
            .is_some_and(serde_yaml::Value::is_null);
        cfg.base_dir = absolute.parent().map(Path::to_path_buf);
        cfg.parity_text_ok = crate::conformance_gate::config_text_ok(
            &std::fs::read_to_string(&absolute).unwrap_or_default(),
        );
        ConfigRead::Parsed(Box::new(cfg))
    }

    /// Load the config, exactly as `Configuration.load` does: `explicit`
    /// names the file (`File.exist?` first — a missing or `~`-led path uses
    /// the DEFAULTS, silently); `None` follows [`Self::discover`]'s
    /// `.rigor.yml` → `.rigor.dist.yml` order, defaults when neither exists.
    /// A file the reference dies on surfaces as [`LoadFailure`] instead of
    /// the defaults — the port's `rigor: <message>` + exit status mirror the
    /// dispatcher's `rescue ConfigurationError`.
    pub fn load(explicit: Option<&Path>) -> Result<Config, LoadFailure> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => match Config::discover() {
                Some(p) => p,
                None => return Ok(Config::default()),
            },
        };
        match Config::read(&path) {
            ConfigRead::Parsed(cfg) => Ok(*cfg),
            // A race — the file vanished between `exist?` and the read — lands
            // here; upstream's `File.exist?` gate already answered with the
            // defaults either way.
            ConfigRead::Absent(_) => Ok(Config::default()),
            ConfigRead::Fatal(f) => Err(f),
        }
    }

    /// Parse YAML text, warning and falling back to default on a parse error.
    /// Records the file's top-level keys ([`Config::present_keys`]) so the config
    /// audit can tell an explicitly-configured key from a defaulted one.
    ///
    /// **Test-only** since [`Config::read`] took the load path over: production
    /// reads a PATH (only there can absent be told from malformed) and owns its
    /// own warning channel — the LSP's is `window/showMessage`, not stderr.
    /// Inlined `includes:` are a load-pipeline concern [`Config::read`] owns;
    /// this helper parses a single document exactly as written.
    #[cfg(test)]
    pub(crate) fn parse_or_warn(text: &str, label: &str) -> Config {
        match serde_yaml::from_str::<Config>(text) {
            Ok(mut cfg) => {
                cfg.present_keys = top_level_keys(text);
                cfg.signature_paths_null = signature_paths_is_null(text);
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

    /// The coerced `severity_profile:` value (ADR-8, reference
    /// `Configuration#severity_profile` / `SeverityProfile::VALID_PROFILES`).
    /// A string naming one of the three profiles maps to it; anything else —
    /// absent/`null`, a non-string value, or an unrecognized name — degrades
    /// to [`crate::severity::Profile::default`] (`balanced`). The reference
    /// RAISES on an invalid value; rigor-rs's config layer never aborts a run
    /// on a bad `.rigor.yml` value (the same convention
    /// [`Self::bleeding_edge_selector`] documents) — a typo'd profile name
    /// silently runs `balanced` rather than crashing the CLI.
    ///
    /// `#[allow(dead_code)]`: not yet called from the diagnostic pipeline —
    /// wiring severity resolution into the runner is a later step (see the
    /// [`crate::severity`] module doc comment); exercised by this module's
    /// own unit tests today.
    #[must_use]
    #[allow(dead_code)]
    pub fn severity_profile(&self) -> crate::severity::Profile {
        self.severity_profile
            .as_str()
            .and_then(crate::severity::Profile::from_str)
            .unwrap_or_default()
    }

    /// The coerced `severity_overrides:` map (ADR-8, reference
    /// `Configuration#severity_overrides`), as an ordered list of (rule-or-family,
    /// severity) pairs ready for [`crate::severity::resolve`]'s `overrides`
    /// parameter.
    ///
    /// Each mapping entry's key is stringified (accepting the family
    /// shorthand, e.g. `call:`, as well as a full rule id) and its value must
    /// parse as one of `error` | `warning` | `info` | `off`
    /// ([`crate::severity::ResolvedSeverity::from_str`]). An entry that fails
    /// either check is DROPPED rather than failing the whole map or the run —
    /// same degrade-don't-abort convention as [`Self::severity_profile`].
    ///
    /// YAML TRAP (reference divergence, verified empirically against this
    /// crate's actual parser): the reference's Ruby YAML loader (Psych,
    /// YAML 1.1 core schema) folds a BARE `off` scalar to the boolean
    /// `false` (also `no`/`n`/`on`/`yes`), so the reference raises with a
    /// "quote it" hint unless the author writes `call: "off"`. `serde_yaml`
    /// 0.9 (the crate this loader actually uses, backed by `unsafe-libyaml`)
    /// implements YAML 1.2's Core Schema instead: ONLY `true`/`false`
    /// (case-insensitive) fold to booleans — `off`/`on`/`yes`/`no` parse as
    /// plain strings. So here `severity_overrides: { call: off }` parses
    /// `off` as the string `"off"` and resolves correctly WITHOUT quoting;
    /// quoting it also still works (a string either way). What DOES fold to
    /// [`serde_yaml::Value::Bool`] here — and is dropped like any other
    /// invalid entry, since `"true"`/`"false"` are not
    /// [`crate::severity::ResolvedSeverity`] spellings — is the literal word
    /// `true` or `false` as a value (e.g. a stray `call: false`). The
    /// reference's "quote it" hint has no analogue to port here since the
    /// trap it guards against doesn't reproduce; a future config-audit hint
    /// for a bare `true`/`false` value is out of scope for this port.
    ///
    /// Order: this preserves the source `.rigor.yml` mapping's iteration
    /// order (`serde_yaml::Mapping` is insertion-ordered), so an exact-id
    /// entry that happens to sort after its family entry still resolves
    /// correctly — [`crate::severity::resolve`]'s precedence is by exact-vs-family
    /// match, not by list position, so the order here is for determinism /
    /// display only, not correctness.
    ///
    /// `#[allow(dead_code)]`: see [`Self::severity_profile`]'s note.
    #[must_use]
    #[allow(dead_code)]
    pub fn severity_overrides(&self) -> Vec<(String, crate::severity::ResolvedSeverity)> {
        let serde_yaml::Value::Mapping(map) = &self.severity_overrides else {
            return Vec::new();
        };
        map.iter()
            .filter_map(|(k, v)| {
                let key = k.as_str()?.to_string();
                let sev = v.as_str().and_then(crate::severity::ResolvedSeverity::from_str)?;
                Some((key, sev))
            })
            .collect()
    }

    /// Whether `paths:` was EXPLICITLY declared in the loaded `.rigor.yml` (vs.
    /// left to the `["lib"]` default). `baseline drift`/`prune` use this to tell
    /// a real, user-declared analysis scope from the implicit default: with no
    /// positional roots AND no declared `paths:`, an audit against a non-empty
    /// baseline has no meaningful scope and would mislead (every out-of-default
    /// bucket falsely reads as cleared), so those commands refuse instead. Always
    /// `false` for `Config::default` / a direct `serde_yaml::from_str` (only
    /// [`Config::load`] populates `present_keys`).
    #[must_use]
    pub fn paths_explicitly_declared(&self) -> bool {
        self.present_keys.contains("paths")
    }

    /// Whether `key` was written at the top level of the loaded config file
    /// (issue #129: the `conforms-to` scan stands down when the file names a
    /// load-set input rigor-rs does not mirror, e.g. `libraries:`).
    #[must_use]
    pub fn declares_key(&self, key: &str) -> bool {
        self.present_keys.contains(key)
    }

    /// The `signature_paths:` entries when the key was EXPLICITLY configured, or
    /// `None` when it was left to the `["sig"]` default. The config audit only
    /// warns on explicit paths — an absent (auto-detected) `sig/` is a normal
    /// setup, not a misconfiguration — mirroring the reference, whose
    /// `Configuration#signature_paths` is `nil` when unset.
    #[must_use]
    pub fn explicit_signature_paths(&self) -> Option<&[String]> {
        (self.present_keys.contains("signature_paths") && !self.signature_paths_null)
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
    /// (ADR-0033), in the spelling the reference stores: a DECLARED entry was
    /// `File.expand_path`'d against its file's directory at load time (issue
    /// #158 — `--config conf/custom.yml`'s `sig` is `<cwd>/conf/sig`), while
    /// the `["sig"]` default was never in a file and stays cwd-relative. An
    /// entry naming a non-existent directory is inert — ingestion skips
    /// it — so the default costs nothing when a project ships no signatures.
    #[must_use]
    pub fn signature_dirs(&self) -> Vec<std::path::PathBuf> {
        self.signature_paths
            .iter()
            .map(std::path::PathBuf::from)
            .collect()
    }

    /// The `rbs collection` gem dirs discovered under `rbs_collection.lock.yaml`
    /// (ADR-0034) for a project rooted at `project_root`.
    #[must_use]
    pub fn collection_signature_dirs(&self, project_root: &Path) -> Vec<std::path::PathBuf> {
        crate::rbs_collection::discover(
            self.rbs_collection.lockfile.as_deref().map(Path::new),
            project_root,
            self.rbs_collection.auto_detect,
        )
    }

    /// Issue #129 / ADR-0044: whether the reference accepts this
    /// `target_ruby:`. It formats the value (`to_s`, so YAML's float `3.4`
    /// reads `"3.4"`), rejects a malformed one before the run (exit 64), and
    /// rejects one its Prism does not parse with a lone `configuration-error`
    /// row (exit 1): in both cases it emits no other row. The port does not
    /// reproduce either outcome; it accepts only the absent key and the
    /// versions every Prism the reference supports parses (3.3, 3.4, 4.0, with
    /// or without a patch level, and `latest`), and the `conforms-to` scan
    /// stands down otherwise.
    #[must_use]
    pub fn target_ruby_supported(&self) -> bool {
        self.target_ruby_value_supported()
    }

    /// Issue #129: whether the config text passed the environment-parity
    /// subset (see [`crate::conformance_gate::config_text_ok`]).
    #[must_use]
    pub fn parity_text_ok(&self) -> bool {
        self.parity_text_ok
    }

    /// The directory relative `signature_paths:` / `paths:` resolve against
    /// in the reference, when it is not the cwd.
    #[must_use]
    pub fn config_base_dir(&self) -> Option<&Path> {
        self.base_dir.as_deref()
    }

    fn target_ruby_value_supported(&self) -> bool {
        let text = match &self.target_ruby {
            serde_yaml::Value::Null => return !self.present_keys.contains("target_ruby"),
            serde_yaml::Value::String(s) => s.clone(),
            serde_yaml::Value::Number(n) => match n.as_f64() {
                Some(f) if n.is_f64() => format!("{f:?}"),
                _ => n.to_string(),
            },
            _ => return false,
        };
        if text == "latest" {
            return true;
        }
        let mut parts = text.split('.');
        let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
            return false;
        };
        let patch_ok = match (parts.next(), parts.next()) {
            (None, None) => true,
            (Some(p), None) => !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()),
            _ => false,
        };
        patch_ok && matches!((major, minor), ("3", "3") | ("3", "4") | ("4", "0"))
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
        dirs.extend(self.collection_signature_dirs(project_root));
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

}

// ---------------------------------------------------------------------------
// `load_with_includes` — the reference's per-file path resolution + merge
// (issue #158, `Configuration::load_with_includes` / `resolve_paths_in` /
// `merge_includes` / `deep_merge`, configuration.rb pin e59b7b89).
// ---------------------------------------------------------------------------

/// `Configuration::PATH_KEYS` — the top-level keys whose values are
/// file/directory paths resolved against the declaring file's directory.
/// `exclude:` is deliberately NOT one (its entries are glob patterns, not
/// paths); `baseline:` is not either — the reference never resolves it, so
/// `baseline: rel.yml` keeps its cwd-relative spelling.
const PATH_KEYS: [&str; 3] = ["paths", "signature_paths", "pre_eval"];

/// Read + parse `absolute` (already `File.expand_path`'d) as a YAML document,
/// or the [`LoadFailure`] the reference dies with: a read error is an uncaught
/// `Errno` upstream (exit 1); a `Psych::SyntaxError` is re-rendered upstream
/// as `<abs>:<line>:<col>: not valid YAML: <detail>` (exit 64); a document
/// that is not a mapping — an empty file parses as `nil` which IS a mapping
/// (`|| {}`) — is `config file must be a YAML mapping: <abs>` (exit 64).
fn read_yaml(absolute: &Path) -> Result<serde_yaml::Value, LoadFailure> {
    let bytes = std::fs::read(absolute).map_err(|e| LoadFailure {
        message: format!("cannot read config {}: {e}", absolute.display()),
        code: 1,
    })?;
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            let bytes = e.as_bytes();
            // A UTF-16 BOM makes the reference's `File.open(path, 'r:bom|utf-8')`
            // raise `ASCII incompatible encoding needs binmode` — an uncaught
            // ArgumentError (exit 1 + backtrace), not the SyntaxError surface.
            if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
                return Err(LoadFailure {
                    message: "ASCII incompatible encoding needs binmode".to_string(),
                    code: 1,
                });
            }
            // libyaml fails the whole read before the parser runs, so the mark
            // never advances: Psych reports these at 1:1 regardless of where
            // the bad byte sits (oracle: a `\xff` on line 3 prints `1:1`).
            return Err(LoadFailure {
                message: format!(
                    "{}:1:1: not valid YAML: {}",
                    absolute.display(),
                    utf8_error_detail(bytes, e.utf8_error().valid_up_to())
                ),
                code: 64,
            });
        }
    };
    let value = parse_yaml(&text).map_err(|e| {
        // Re-render as upstream's `#{absolute}:#{e.line}:#{e.column}: not
        // valid YAML: #{e.problem} #{e.context}` (exit 64). serde_yaml's
        // Display is `{problem} at line {pl} column {pc}, {context} at line
        // {cl} column {cc}` (context optional); Psych's `e.line`/`e.column`
        // name the CONTEXT position — the LAST `at line` — and the detail is
        // `problem` + ` ` + `context` with no position text at all.
        let (line, column, detail) = psych_render(&e.to_string());
        LoadFailure {
            message: format!("{}:{line}:{column}: not valid YAML: {detail}", absolute.display()),
            code: 64,
        }
    })?;
    match value {
        // `YAML.safe_load_file(...) || {}` — nil (empty document) reads as {}.
        serde_yaml::Value::Null => Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new())),
        serde_yaml::Value::Mapping(_) => Ok(value),
        _ => Err(LoadFailure {
            message: format!("config file must be a YAML mapping: {}", absolute.display()),
            code: 64,
        }),
    }
}

/// libyaml's UTF-8 reader wording (reader.c `utf8` check): the byte at
/// `start` either cannot begin a sequence, has a bad continuation byte, or
/// the file ends mid-sequence.
fn utf8_error_detail(bytes: &[u8], start: usize) -> &'static str {
    let need = match bytes[start] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return "invalid leading UTF-8 octet",
    };
    for j in 1..need {
        match bytes.get(start + j) {
            None => return "incomplete UTF-8 octet sequence",
            Some(&c) if !(0x80..=0xBF).contains(&c) => return "invalid trailing UTF-8 octet",
            _ => {}
        }
    }
    // Lead + continuations all present but still not valid UTF-8 (overlong,
    // surrogate, or > U+10FFFF) — libyaml blames the lead.
    "invalid leading UTF-8 octet"
}

/// Parse `text` as ONE YAML document the way Psych's `YAML.safe_load_file`
/// does: the FIRST document only (a `---` follower is never even scanned —
/// oracle: `paths: [src]\n---\nother: 1` loads doc 1 and runs), and a
/// repeated mapping key folds last-wins at EVERY level (oracle:
/// `disable: [call]` then `disable: []` parses to `[]`). serde_yaml's
/// `from_str` refuses both shapes ("more than one document",
/// "duplicate entry with key"), so the first `Deserializer` document goes
/// through [`DupOk`]'s pairwise collector instead; `<<` merge keys are
/// applied Psych-style inside [`DupOkVisitor::visit_map`] — NEVER through
/// `Value::apply_merge`, which errors on non-mapping merge values where
/// Psych keeps `<<` as a literal key.
fn parse_yaml(text: &str) -> Result<serde_yaml::Value, serde_yaml::Error> {
    let Some(doc) = serde_yaml::Deserializer::from_str(text).next() else {
        return Ok(serde_yaml::Value::Null);
    };
    Ok(DupOk::deserialize(doc)?.0)
}

/// A `serde_yaml::Value` deserialized with Psych's duplicate-key semantics —
/// last-wins at every level, instead of serde_yaml `Value`'s
/// `duplicate entry with key` rejection. The visitor collects each mapping's
/// `(key, value)` pairs itself, so nothing between the parser and the map can
/// reject a repeat.
struct DupOk(serde_yaml::Value);

impl<'de> Deserialize<'de> for DupOk {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        d.deserialize_any(DupOkVisitor)
    }
}

struct DupOkVisitor;

impl<'de> serde::de::Visitor<'de> for DupOkVisitor {
    type Value = DupOk;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<DupOk, E> {
        Ok(DupOk(serde_yaml::Value::Bool(v)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<DupOk, E> {
        Ok(DupOk(serde_yaml::Value::Number(serde_yaml::Number::from(v))))
    }
    fn visit_i128<E>(self, v: i128) -> Result<DupOk, E> {
        Ok(DupOk(serde_yaml::Value::Number(serde_yaml::Number::from(
            i64::try_from(v).unwrap_or(i64::MAX),
        ))))
    }
    fn visit_u64<E>(self, v: u64) -> Result<DupOk, E> {
        Ok(DupOk(serde_yaml::Value::Number(serde_yaml::Number::from(v))))
    }
    fn visit_u128<E>(self, v: u128) -> Result<DupOk, E> {
        Ok(DupOk(serde_yaml::Value::Number(serde_yaml::Number::from(
            u64::try_from(v).unwrap_or(u64::MAX),
        ))))
    }
    fn visit_f64<E>(self, v: f64) -> Result<DupOk, E> {
        let n = serde_yaml::Number::from(v);
        Ok(DupOk(serde_yaml::Value::Number(n)))
    }
    fn visit_str<E>(self, v: &str) -> Result<DupOk, E>
    where
        E: serde::de::Error,
    {
        Ok(DupOk(serde_yaml::Value::String(v.to_string())))
    }
    fn visit_char<E>(self, v: char) -> Result<DupOk, E> {
        Ok(DupOk(serde_yaml::Value::String(v.to_string())))
    }
    fn visit_none<E>(self) -> Result<DupOk, E> {
        Ok(DupOk(serde_yaml::Value::Null))
    }
    fn visit_unit<E>(self) -> Result<DupOk, E> {
        Ok(DupOk(serde_yaml::Value::Null))
    }
    fn visit_some<D>(self, d: D) -> Result<DupOk, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        DupOk::deserialize(d)
    }
    fn visit_seq<A>(self, mut seq: A) -> Result<DupOk, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(DupOk(v)) = seq.next_element::<DupOk>()? {
            items.push(v);
        }
        Ok(DupOk(serde_yaml::Value::Sequence(items)))
    }
    fn visit_map<A>(self, mut map: A) -> Result<DupOk, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut m = serde_yaml::Mapping::new();
        while let Some((DupOk(k), DupOk(v))) = map.next_entry::<DupOk, DupOk>()? {
            if k.as_str() == Some("<<") {
                // YAML merge key, Psych's `merge_key` semantics — each `<<`
                // pair is handled in document order and every `<<` is
                // honoured (a repeated `<<` is NOT last-wins like other dup
                // keys; oracle: two `<<:` entries both merge). A mapping
                // value merges its entries; a sequence merges each element
                // only when EVERY element is a mapping. Anything else keeps
                // `<<` as a literal key (oracle: `{<<: 5}` loads
                // `{"<<" => 5}` verbatim — no error path). Merged entries
                // only fill slots not already taken: an explicit key always
                // wins, and the earlier of two merges wins.
                match v {
                    serde_yaml::Value::Mapping(mm) => {
                        for (mk, mv) in mm {
                            if m.get(&mk).is_none() {
                                m.insert(mk, mv);
                            }
                        }
                    }
                    serde_yaml::Value::Sequence(seq)
                        if seq
                            .iter()
                            .all(|e| matches!(e, serde_yaml::Value::Mapping(_))) =>
                    {
                        for e in seq {
                            if let serde_yaml::Value::Mapping(mm) = e {
                                for (mk, mv) in mm {
                                    if m.get(&mk).is_none() {
                                        m.insert(mk, mv);
                                    }
                                }
                            }
                        }
                    }
                    other => {
                        m.insert(serde_yaml::Value::String("<<".to_string()), other);
                    }
                }
                continue;
            }
            // Psych last-wins: `Hash#[]=` overwrites, and serde_yaml's
            // `Mapping::insert` replaces the existing entry the same way.
            m.insert(k, v);
        }
        Ok(DupOk(serde_yaml::Value::Mapping(m)))
    }

    /// serde_yaml surfaces a non-standard tag (`!foo`, `!ruby/object:Foo`)
    /// through `visit_enum` — the "variant" is the tag text minus the `!`.
    /// Psych drops an unrecognized tag and loads the bare value; `!ruby/*`
    /// is `Psych::DisallowedClass` upstream (uncaught — fatal on both sides,
    /// so here it just keeps dying).
    fn visit_enum<A>(self, data: A) -> Result<DupOk, A::Error>
    where
        A: serde::de::EnumAccess<'de>,
    {
        use serde::de::VariantAccess;
        let (tag, variant) = data.variant::<String>()?;
        if tag.starts_with("ruby/") {
            return Err(<A::Error as serde::de::Error>::custom(format!(
                "Tried to load unspecified class: {tag}"
            )));
        }
        variant.newtype_variant::<DupOk>()
    }
}

/// `data.keys.map(&:to_s)` — coerce every non-String TOP-LEVEL key to its
/// Ruby `to_s` (`` `1` `` → `1`, `["a","b"]` → `["a", "b"]`, `nil` → `""`,
/// `{"x"=>1}` → `{"x" => 1}`) so it flows into the unknown-key warning like
/// the reference. A coerced key never shadows a literal String key —
/// upstream's `data["paths"]`-style fetches only see the String one.
fn stringify_top_level_keys(map: serde_yaml::Mapping) -> serde_yaml::Mapping {
    let mut out = serde_yaml::Mapping::new();
    for (k, v) in map {
        if matches!(k, serde_yaml::Value::String(_)) {
            out.insert(k, v);
        } else {
            let sk = serde_yaml::Value::String(ruby_to_s(&k));
            if !out.contains_key(&sk) {
                out.insert(sk, v);
            }
        }
    }
    out
}

/// `Configuration#coerce_severity_overrides` (configuration.rb:1037): the
/// merged `severity_overrides:` value must be a Hash, every VALUE must be a
/// String/Symbol (`a YAML boolean` message — bare `off` is already `false`
/// upstream, though serde_yaml's 1.2 parse keeps it a string), and the
/// severity must be in `VALID_SEVERITIES`. Errors are ConfigurationError —
/// `rigor: <msg>` + exit 64 — raised against the FIRST bad entry in document
/// order. Keys keep their parsed type for `k.inspect` (`severity_overrides[5]`,
/// `severity_overrides["<<"]`).
fn validate_severity_overrides(merged: &serde_yaml::Mapping) -> Result<(), LoadFailure> {
    let Some(v) = merged.get("severity_overrides") else {
        return Ok(());
    };
    let serde_yaml::Value::Mapping(map) = v else {
        return Err(LoadFailure {
            message: format!(
                "severity_overrides must be a Hash, got {}",
                ruby_inspect(v)
            ),
            code: 64,
        });
    };
    for (k, val) in map {
        let kins = ruby_inspect(k);
        if let serde_yaml::Value::String(s) = val {
            if crate::severity::ResolvedSeverity::from_str(s).is_none() {
                return Err(LoadFailure {
                    message: format!(
                        "severity_overrides[{kins}] must be one of [:error, :warning, :info, :off], got {}",
                        ruby_inspect(val)
                    ),
                    code: 64,
                });
            }
            continue;
        }
        let hint = if *val == serde_yaml::Value::Bool(false) {
            " — did you mean the string \"off\"?"
        } else {
            ""
        };
        return Err(LoadFailure {
            message: format!(
                "severity_overrides[{kins}] is {}, a YAML boolean{hint} \
                 Bare off/on/no/yes/true/false are parsed as booleans; quote the severity \
                 (e.g. \"off\").",
                ruby_inspect(val)
            ),
            code: 64,
        });
    }
    Ok(())
}

/// Split a serde_yaml error Display into Psych's `(line, column,
/// "problem context")` pieces: the header position is the LAST embedded
/// ` at line L column C` (the context anchor — where the construct started,
/// e.g. the `[` of an unterminated flow sequence), and the detail is
/// `problem` + ` ` + `context` — the position fragments removed and serde's
/// `", "` separator between them replaced by a space. A message with no
/// position text keeps `(0, 0)` and itself as the detail.
fn psych_render(detail: &str) -> (usize, usize, String) {
    // Each ` at line <n> column <m>` marker as (start, end, line, column).
    let mut marks: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut search = 0usize;
    while let Some(off) = detail[search..].find(" at line ") {
        let start = search + off;
        let tail = &detail[start + " at line ".len()..];
        let Some((n, r)) = tail.split_once(" column ") else {
            search = start + " at line ".len();
            continue;
        };
        let Ok(line) = n.trim().parse::<usize>() else {
            search = start + " at line ".len();
            continue;
        };
        let digits = r.bytes().take_while(|b| b.is_ascii_digit()).count();
        let Ok(column) = r[..digits].parse::<usize>() else {
            search = start + " at line ".len();
            continue;
        };
        let end = start + " at line ".len() + n.len() + " column ".len() + digits;
        marks.push((start, end, line, column));
        search = end;
    }
    let Some(&(_, _, line, column)) = marks.last() else {
        return (0, 0, detail.to_string());
    };
    // The header position is the LAST marker's (the context anchor when one
    // carries a position — Psych's `e.line`/`e.column` — else the problem's).
    // The detail is `problem` + ` ` + `context`: every marker dropped and each
    // text piece between them stripped of serde's `, ` separator. Covers all
    // three emitted shapes: `{p} at line L C` (no context), `{p} at line L C,
    // {ctx}` (context without a position), and `{p} at line L C, {ctx} at
    // line L2 C2` (positioned context); N>2 markers degrade the same way.
    let mut pieces: Vec<&str> = Vec::new();
    let mut cursor = 0usize;
    for &(start, end, _, _) in &marks {
        pieces.push(&detail[cursor..start]);
        cursor = end;
    }
    pieces.push(&detail[cursor..]);
    let problem = pieces[0];
    let mut rendered = problem.to_string();
    for piece in &pieces[1..] {
        let piece = piece.strip_prefix(", ").unwrap_or(piece);
        if piece.is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push(' ');
        }
        rendered.push_str(piece);
    }
    (line, column, rendered)
}

/// `Configuration::load_with_includes` — reads `absolute` plus every file its
/// `includes:` names (recursively, per-file path resolution), and returns the
/// merged mapping: included files first (in declaration order), the current
/// file's keys overriding. `visited` is the include CHAIN's absolute paths —
/// upstream's `visited + [absolute]` makes a FRESH set per level, so a diamond
/// (two siblings both including the same file) loads that file twice and is
/// NOT circular; only an ancestor re-entry is.
/// `includes_seen` records whether ANY file in the chain declared `includes:`
/// (the merged map has it deleted, so `Config::declares_key("includes")`
/// needs the record).
fn load_with_includes(
    absolute: &Path,
    visited: &std::collections::BTreeSet<std::path::PathBuf>,
    includes_seen: &mut bool,
) -> Result<serde_yaml::Mapping, LoadFailure> {
    if visited.contains(absolute) {
        return Err(LoadFailure {
            message: format!("circular include: {}", absolute.display()),
            code: 64,
        });
    }
    let serde_yaml::Value::Mapping(mut raw) = read_yaml(absolute)? else {
        unreachable!("read_yaml yields a mapping");
    };
    let base_dir = absolute.parent().unwrap_or_else(|| Path::new("/"));
    let includes = match raw.remove("includes") {
        Some(v) => {
            *includes_seen = true;
            ruby_array_raw(&v)
        }
        None => Vec::new(),
    };
    resolve_paths_in(&mut raw, base_dir)?;
    let mut next_visited = visited.clone();
    next_visited.insert(absolute.to_path_buf());
    merge_includes(raw, &includes, base_dir, &next_visited, includes_seen)
}

/// `Configuration::merge_includes`: each `includes:` entry is
/// `File.expand_path(inc.to_s, base_dir)` — `base_dir` is the INCLUDING file's
/// directory — then loaded recursively; a missing one is the load-time
/// `include not found: <inc> (referenced from <base_dir>)` error (exit 64).
/// Included files merge left-to-right (a later include wins over an earlier),
/// then the current file's own keys merge over the lot.
fn merge_includes(
    data: serde_yaml::Mapping,
    includes: &[serde_yaml::Value],
    base_dir: &Path,
    visited: &std::collections::BTreeSet<std::path::PathBuf>,
    includes_seen: &mut bool,
) -> Result<serde_yaml::Mapping, LoadFailure> {
    let mut accumulated = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    for inc in includes {
        let inc_path = expand_config_entry(&ruby_to_s(inc), base_dir)?;
        if !inc_path.exists() {
            return Err(LoadFailure {
                message: format!(
                    "include not found: {} (referenced from {})",
                    ruby_inspect(inc),
                    base_dir.display()
                ),
                code: 64,
            });
        }
        let sub = load_with_includes(&inc_path, visited, includes_seen)?;
        accumulated = deep_merge(
            &accumulated,
            &serde_yaml::Value::Mapping(sub),
        );
    }
    let serde_yaml::Value::Mapping(merged) = deep_merge(
        &accumulated,
        &serde_yaml::Value::Mapping(data),
    ) else {
        unreachable!("deep_merge of two mappings yields a mapping")
    };
    Ok(merged)
}

/// `Configuration::deep_merge` over YAML mappings: a key present in both as a
/// mapping merges recursively; anything else is right-wins. `dependencies`
/// merges deeply too but concatenates `source_inference` (`merge_value`'s
/// carve-out, ADR-10 § "config-conflict diagnostic").
fn deep_merge(left: &serde_yaml::Value, right: &serde_yaml::Value) -> serde_yaml::Value {
    use serde_yaml::Value;
    let (Value::Mapping(l), Value::Mapping(r)) = (left, right) else {
        return right.clone();
    };
    let mut merged = l.clone();
    for (k, v) in r {
        let entry = match (merged.get(k), v) {
            (Some(lv @ Value::Mapping(_)), Value::Mapping(_)) => {
                if k.as_str() == Some("dependencies") {
                    merge_dependencies_hash(lv, v)
                } else {
                    deep_merge(lv, v)
                }
            }
            _ => v.clone(),
        };
        merged.insert(k.clone(), entry);
    }
    Value::Mapping(merged)
}

/// `Configuration::merge_dependencies_hash` — deep-merge, then
/// `source_inference` is the CONCATENATION `left + right` (kept unless both
/// sides are empty, so an `includes:` chain sees every contributor's entries).
fn merge_dependencies_hash(
    left: &serde_yaml::Value,
    right: &serde_yaml::Value,
) -> serde_yaml::Value {
    let mut out = deep_merge(left, right);
    let lsi = left.get("source_inference").map(ruby_array_raw);
    let rsi = right.get("source_inference").map(ruby_array_raw);
    let mut both_empty = true;
    let mut joined = Vec::new();
    for v in [lsi, rsi].into_iter().flatten() {
        if !v.is_empty() {
            both_empty = false;
        }
        joined.extend(v);
    }
    if !both_empty {
        if let serde_yaml::Value::Mapping(m) = &mut out {
            m.insert(
                serde_yaml::Value::String("source_inference".to_string()),
                serde_yaml::Value::Sequence(joined),
            );
        }
    }
    out
}

/// `Configuration::resolve_paths_in`: each PATH_KEYS entry plus the nested
/// `plugins_io.allowed_paths:` is `Array(v).map { File.expand_path(p.to_s,
/// base_dir) }` — a present-but-null key stays null (the reference's "not
/// configured"). `cache.path:` is intentionally left as-is upstream.
fn resolve_paths_in(
    out: &mut serde_yaml::Mapping,
    base_dir: &Path,
) -> Result<(), LoadFailure> {
    for key in PATH_KEYS {
        let Some(v) = out.get(key) else { continue };
        if v.is_null() {
            continue;
        }
        let mut resolved = Vec::new();
        for entry in ruby_array_raw(v) {
            resolved.push(serde_yaml::Value::String(
                expand_config_entry(&ruby_to_s(&entry), base_dir)?
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        out.insert(
            serde_yaml::Value::String(key.to_string()),
            serde_yaml::Value::Sequence(resolved),
        );
    }
    // `resolve_plugins_io_paths!` — `plugins_io:` is a Hash upstream; a
    // non-mapping value is left untouched (the reference's `is_a?(Hash)`
    // gate), and so is an absent / null `allowed_paths`.
    if let Some(serde_yaml::Value::Mapping(plugins_io)) = out.get_mut("plugins_io") {
        if let Some(ap) = plugins_io.get("allowed_paths") {
            if !ap.is_null() {
                let mut resolved = Vec::new();
                for entry in ruby_array_raw(ap) {
                    resolved.push(serde_yaml::Value::String(
                        expand_config_entry(&ruby_to_s(&entry), base_dir)?
                            .to_string_lossy()
                            .into_owned(),
                    ));
                }
                plugins_io.insert(
                    serde_yaml::Value::String("allowed_paths".to_string()),
                    serde_yaml::Value::Sequence(resolved),
                );
            }
        }
    }
    Ok(())
}

/// Ruby `File.expand_path(entry, base_dir)`: `~`/`~user` expand at the head,
/// then `.`/`..` fold LEXICALLY against `base` (no symlink resolution — the
/// `lnk/../sig` case lands on the lexical parent, upstream-verified). `base`
/// is already absolute (the expanded config's directory).
fn expand_config_entry(entry: &str, base: &Path) -> Result<std::path::PathBuf, LoadFailure> {
    let expanded = expand_tilde_head(entry)?;
    let path = Path::new(&expanded);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    Ok(crate::conformance_gate::expand_path(&joined))
}

/// `File.expand_path`'s `~` handling at the head of `raw` only: `~` / `~/…`
/// names the current user's home (`ENV["HOME"]`, falling back to the
/// password database like `Dir.home`); `~user/…` names that user's home —
/// an unknown user is upstream's `ArgumentError` (uncaught → exit 1). A `~`
/// with no resolvable home stays literal — the path then names nothing on
/// disk, the safe direction.
fn expand_tilde_head(raw: &str) -> Result<String, LoadFailure> {
    let Some(rest) = raw.strip_prefix('~') else {
        return Ok(raw.to_string());
    };
    let (user, tail) = match rest.split_once('/') {
        Some((u, t)) => (u.to_string(), format!("/{t}")),
        None => (rest.to_string(), String::new()),
    };
    let home = if user.is_empty() {
        home_dir()
    } else {
        passwd_home(&user)
    };
    match home {
        Some(h) => Ok(format!("{}{tail}", h.trim_end_matches('/'))),
        None if user.is_empty() => Ok(raw.to_string()),
        None => Err(LoadFailure {
            message: format!("user {user} doesn't exist"),
            code: 1,
        }),
    }
}

/// `Dir.home` — `ENV["HOME"]`, else the current uid's passwd entry.
fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(current_user_home)
}

/// `/etc/passwd` lookup by login name (`getpwnam` equivalent enough for a
/// `~user` expansion — the reference's own resolution is the OS's).
fn passwd_home(user: &str) -> Option<String> {
    passwd_entry(|fields| fields.first() == Some(&user))
}

/// The current user's home from `/etc/passwd`, keyed by `id -u` (no libc dep
/// here); `None` when it cannot be told (the `~` then stays literal).
fn current_user_home() -> Option<String> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    passwd_entry(|fields| fields.get(2) == Some(&uid.as_str()))
}

/// Scan `/etc/passwd` for the first record `pred` accepts, returning its
/// home-directory field.
fn passwd_entry(pred: impl Fn(&[&str]) -> bool) -> Option<String> {
    let text = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 6 && pred(&fields) {
            return Some(fields[5].to_string());
        }
    }
    None
}

/// Whether `path` matches any of the `exclude:` `patterns`, matched with the
/// reference's `File.fnmatch?` and NO flags (`reject_excluded`) — every `*`
/// spans `/`, consecutive `*`s collapse (so `a/**/b` never matches `a/b`),
/// `[…]` is a character class, `\` escapes, and the leading-period rule
/// applies at path position 0.
///
/// The single matcher authority: `check`'s expansion filter
/// (`expand_check_paths_excluding`) and the LSP's per-buffer gate both reach
/// the same `dir.c` port — a second implementation of the glob rule is
/// exactly the drift the retired `glob::Pattern` stage-1 gate was (a
/// `glob::Pattern` `a/**/b` matches `a/b`; `File.fnmatch?` does not).
#[must_use]
pub fn matches_exclude(patterns: &[String], path: &str) -> bool {
    let path: Vec<char> = path.chars().collect();
    patterns.iter().any(|pat| {
        let pat: Vec<char> = pat.chars().collect();
        crate::conformance_gate::fnmatch_chars(&pat, &path)
    })
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

    /// Issue #199 — `ruby_array` is Ruby's `Array(x).map(&:to_s)`.
    #[test]
    fn ruby_array_semantics() {
        let v = |s: &str| serde_yaml::from_str::<serde_yaml::Value>(s).unwrap();
        assert_eq!(ruby_array(&v("other")).unwrap(), vec!["other"], "scalar wraps");
        assert_eq!(ruby_array(&v("[a, b]")).unwrap(), vec!["a", "b"], "list is itself");
        assert_eq!(ruby_array(&v("~")).unwrap(), Vec::<String>::new(), "Array(nil) == []");
        assert_eq!(ruby_array(&v("1")).unwrap(), vec!["1"], "Integer#to_s");
        assert_eq!(ruby_array(&v("true")).unwrap(), vec!["true"], "true.to_s");
        assert_eq!(ruby_array(&v("1.5")).unwrap(), vec!["1.5"], "Float#to_s");
        assert_eq!(ruby_array(&v("[1, false, ~, x]")).unwrap(), vec!["1", "false", "", "x"]);
        // `Array()` accepts even collections: a mapping becomes its `to_a`
        // pairs and each element renders through `to_s` — `{"a" => 1}` →
        // `[["a", 1]]` → `'["a", 1]'` (Hash#to_a → Array#to_s). Never an
        // error: a wrongly-shaped key loads as the same inert tokens the
        // reference stores.
        assert_eq!(ruby_array(&v("{a: 1}")).unwrap(), vec![r#"["a", 1]"#]);
        assert_eq!(ruby_array(&v("[[a]]")).unwrap(), vec![r#"["a"]"#]);
        assert_eq!(ruby_array(&v("[{a: 1}]")).unwrap(), vec![r#"{"a" => 1}"#]);
    }

    /// Issue #199 — every list key accepts a scalar through the ONE shared
    /// adapter, and a scalar key never discards the rest of the document.
    #[test]
    fn scalar_list_keys_parse_and_keep_other_keys() {
        let yaml = "paths: other\nexclude: vendor/x.rb\ndisable: call.undefined-method\n\
                    plugins: rigor-activesupport-core-ext\nsignature_paths: types\nbaseline: b.yml\n";
        let cfg = Config::parse_or_warn(yaml, "test");
        assert_eq!(cfg.paths, vec!["other"]);
        assert_eq!(cfg.exclude, vec!["vendor/x.rb"]);
        assert_eq!(cfg.disable, vec!["call.undefined-method"]);
        assert_eq!(cfg.plugins, vec!["rigor-activesupport-core-ext"]);
        assert_eq!(cfg.signature_paths, vec!["types"]);
        assert_eq!(cfg.baseline_path().as_deref(), Some("b.yml"));
        // Mixed: one scalar key alongside a list key.
        let cfg = Config::parse_or_warn("paths: other\ndisable: [call.undefined-method]\n", "t");
        assert_eq!(cfg.paths, vec!["other"]);
        assert_eq!(cfg.disable, vec!["call.undefined-method"]);
        // Non-string scalars go through `to_s`.
        let cfg = Config::parse_or_warn("paths: 1\ndisable: true\n", "t");
        assert_eq!(cfg.paths, vec!["1"]);
        assert_eq!(cfg.disable, vec!["true"]);
    }

    /// Issue #199 — explicit null: `Array(nil) == []` for every list key
    /// except `signature_paths`, which the reference keeps `nil` (default
    /// discovery, not an explicit declaration).
    #[test]
    fn null_list_keys_follow_the_reference() {
        let yaml = "paths: ~\nexclude:\ndisable: ~\nplugins: ~\nsignature_paths: ~\n";
        let cfg = Config::parse_or_warn(yaml, "test");
        assert!(cfg.paths.is_empty());
        assert!(cfg.exclude.is_empty());
        assert!(cfg.disable.is_empty());
        assert!(cfg.plugins.is_empty());
        assert_eq!(cfg.signature_paths, vec!["sig"], "null => default discovery");
        assert!(cfg.explicit_signature_paths().is_none(), "null is not a declaration");
        // `paths: ~` is still DECLARED (the key is in the file).
        assert!(cfg.paths_explicitly_declared());
    }

    /// Issue #199 — a scalar-valued key is recorded as declared exactly as a
    /// list would be (drives the upstream #684 discovery widening and the
    /// config audit's explicit-`signature_paths` check).
    #[test]
    fn scalar_keys_are_declared() {
        let cfg = Config::parse_or_warn("paths: other\nsignature_paths: sig\n", "test");
        assert!(cfg.paths_explicitly_declared());
        assert!(cfg.declares_key("paths"));
        assert_eq!(cfg.explicit_signature_paths(), Some(&["sig".to_string()][..]));
        // The loader path records the same.
        let dir = std::env::temp_dir()
            .join(format!("rigor_cfg_scalar_{}_{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".rigor.yml");
        std::fs::write(&path, "paths: other\n").unwrap();
        match Config::read(&path) {
            ConfigRead::Parsed(cfg) => {
                // Resolved at load against the file's directory (#158).
                assert_eq!(cfg.paths, vec![dir.join("other").display().to_string()]);
                assert!(cfg.paths_explicitly_declared());
            }
            _ => panic!("a scalar `paths:` must parse, not fall back as malformed"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
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
    fn read_separates_absent_from_malformed() {
        // The distinction `load` collapses and the LSP reload depends on: a MISSING
        // config means "the defaults are the configuration" (adopt them), a BROKEN
        // one means "I don't know what the configuration is" (keep the last good).
        // Collapsing them is what would drop a user's `disable:` list on the save
        // of a half-written file.
        let dir = std::env::temp_dir()
            .join(format!("rigor_cfg_read_{}_{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".rigor.yml");

        assert!(matches!(Config::read(&path), ConfigRead::Absent(_)), "no file");

        std::fs::write(&path, "disable: [unterminated\n").unwrap();
        // Broken YAML is what the reference DIES on (`rescue
        // ConfigurationError` → `rigor: <file>:<line>:<col>: not valid YAML:
        // <detail>` + exit 64) — `Fatal`, not the degraded default.
        assert!(
            matches!(
                Config::read(&path),
                ConfigRead::Fatal(ref f) if f.code == 64 && f.message.contains("not valid YAML")
            ),
            "broken YAML is a fatal load upstream"
        );

        std::fs::write(&path, "disable:\n  - call.undefined-method\n").unwrap();
        match Config::read(&path) {
            ConfigRead::Parsed(cfg) => {
                assert_eq!(cfg.disable, vec!["call.undefined-method".to_string()]);
                // `present_keys` must survive the new loader — the config audit
                // tells an explicitly-set key from a defaulted one through it.
                assert_eq!(cfg.unknown_keys(), Vec::<&str>::new());
                assert!(cfg.present_keys.contains("disable"));
            }
            _ => panic!("valid YAML must parse"),
        }

        // A DIRECTORY at the config path is neither absent nor malformed: it is
        // there but cannot be read — upstream the `Errno::EISDIR` escapes
        // `rescue ConfigurationError` and dies (exit 1) — and the LSP treats it
        // like broken (keep the last good config) rather than like a delete.
        let as_dir = dir.join("dir.yml");
        std::fs::create_dir_all(&as_dir).unwrap();
        assert!(matches!(
            Config::read(&as_dir),
            ConfigRead::Fatal(ref f) if f.code == 1
        ));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exclude_glob_matching() {
        let cfg = Config {
            disable: vec![],
            exclude: vec!["vendor/**".into(), "*.rb".into()],
            ..Default::default()
        };
        let m = |path: &str| matches_exclude(&cfg.exclude, path);
        assert!(m("vendor/x/y.rb"));
        assert!(m("a.rb"));
        // `*.rb` matches a bare filename; non-matches stay false.
        assert!(!m("vendor")); // no trailing segment
        assert!(!m("src/lib.txt"));
        // `File.fnmatch?` (no flags), not `glob::Pattern`: consecutive `*`s
        // collapse, so `a/**/b` never matches `a/b` (issue #201).
        let cfg = Config {
            exclude: vec!["a/**/b".into()],
            ..Default::default()
        };
        let m = |path: &str| matches_exclude(&cfg.exclude, path);
        assert!(!m("a/b"));
        assert!(m("a/x/b"));
    }

    #[test]
    fn unterminated_class_is_inert() {
        // An unterminated `[` must never panic; it simply matches nothing.
        let cfg = Config { disable: vec![], exclude: vec!["[".into()], ..Default::default() };
        assert!(!matches_exclude(&cfg.exclude, "anything.rb"));
        assert!(!matches_exclude(&cfg.exclude, "["));
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

    /// Issue #129 (PR #150 review, family 6) + #158: the reference resolves a
    /// relative `signature_paths:` entry at LOAD time against the directory of
    /// the config file that named it (`Configuration.resolve_paths_in` —
    /// `File.expand_path`, so `..` folds lexically and the result is
    /// absolute); oracle-measured with `--config conf/custom.yml` →
    /// `conf/sig` — i.e. the absolute `<dir>/conf/sig`.
    #[test]
    fn signature_paths_resolve_against_the_config_dir() {
        let dir = std::env::temp_dir().join(format!("rigor_cfg_base_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        let yml = "signature_paths:\n  - sig\n  - ../shared\n  - /abs/sig\n";
        std::fs::write(dir.join("conf/custom.yml"), yml).unwrap();
        std::fs::write(dir.join(".rigor.yml"), yml).unwrap();
        let ConfigRead::Parsed(cfg) = Config::read(&dir.join("conf/custom.yml")) else {
            panic!("config did not parse");
        };
        assert_eq!(
            cfg.signature_dirs(),
            vec![
                dir.join("conf/sig"),
                dir.join("shared"), // `..` folded lexically at load
                std::path::PathBuf::from("/abs/sig"),
            ]
        );
        // `config_base_dir` is the EXPANDED file's directory (absolute) —
        // `File.dirname(File.expand_path(path))` upstream.
        assert_eq!(
            cfg.config_base_dir(),
            Some(dir.join("conf").as_path())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Issue #129 (family 7): the `conforms-to` scan runs only on a
    /// `target_ruby:` the reference accepts (oracle: `"3.2"` is a lone
    /// `configuration-error`, exit 1; `"x"` exits 64 before the run).
    #[test]
    fn target_ruby_supported_matches_the_reference_floor() {
        let ok = |yml: &str| serde_yaml::from_str::<Config>(yml).unwrap().target_ruby_supported();
        assert!(Config::default().target_ruby_supported());
        for accepted in [
            "target_ruby: \"3.3\"\n",
            "target_ruby: \"3.4.1\"\n",
            "target_ruby: 3.4\n",
            "target_ruby: 4.0\n",
            "target_ruby: \"4.0.2\"\n",
            "target_ruby: latest\n",
        ] {
            assert!(ok(accepted), "{accepted}");
        }
        for rejected in [
            "target_ruby: \"3.2\"\n",
            "target_ruby: \"x\"\n",
            "target_ruby: 3\n",
            "target_ruby: 3.10\n",
            "target_ruby: \"4.2\"\n",
            "target_ruby: \"3.4.\"\n",
            "target_ruby: [3.4]\n",
        ] {
            assert!(!ok(rejected), "{rejected}");
        }
        // Present but null: the reference formats `nil` as "" and rejects it.
        let mut null: Config = serde_yaml::from_str("target_ruby:\n").unwrap();
        null.present_keys = top_level_keys("target_ruby:\n");
        assert!(!null.target_ruby_supported());
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

#[cfg(test)]
mod severity_config_tests {
    use super::*;
    use crate::severity::{Profile, ResolvedSeverity};

    fn cfg(yaml: &str) -> Config {
        Config::parse_or_warn(yaml, "test")
    }

    /// `severity_profile:` parses all three names; anything else (absent, a
    /// non-string, an unknown name) degrades to `balanced` — never aborts.
    #[test]
    fn severity_profile_parses_known_names() {
        assert_eq!(cfg("severity_profile: lenient\n").severity_profile(), Profile::Lenient);
        assert_eq!(cfg("severity_profile: balanced\n").severity_profile(), Profile::Balanced);
        assert_eq!(cfg("severity_profile: strict\n").severity_profile(), Profile::Strict);
    }

    #[test]
    fn severity_profile_degrades_to_balanced() {
        // Absent key.
        assert_eq!(Config::default().severity_profile(), Profile::Balanced);
        assert_eq!(cfg("disable: []\n").severity_profile(), Profile::Balanced);
        // Wrong type (an integer, not a string).
        assert_eq!(cfg("severity_profile: 42\n").severity_profile(), Profile::Balanced);
        // Unknown name.
        assert_eq!(cfg("severity_profile: unknown\n").severity_profile(), Profile::Balanced);
    }

    /// `severity_overrides:` accepts a rule id, a family key, and both a
    /// QUOTED and a bare `off` (see the YAML-trap divergence note on
    /// [`Config::severity_overrides`] — this crate's YAML parser does not
    /// fold `off` to a boolean the way the reference's Psych does); drops a
    /// literal `false`/`true` value, an integer value, and an unknown
    /// severity name.
    #[test]
    fn severity_overrides_parses_valid_entries_and_drops_invalid_ones() {
        let yaml = "severity_overrides:\n  \
                    call.undefined-method: warning\n  \
                    call: \"off\"\n  \
                    dump.type: off\n  \
                    def.method-visibility-mismatch: false\n  \
                    def.ivar-write-mismatch: 42\n  \
                    flow.dead-assignment: not-a-severity\n";
        let overrides = cfg(yaml).severity_overrides();
        assert!(overrides.contains(&("call.undefined-method".to_string(), ResolvedSeverity::Warning)));
        assert!(overrides.contains(&("call".to_string(), ResolvedSeverity::Off)));
        // A BARE (unquoted) `off` is not a YAML 1.2 boolean literal in this
        // crate — it parses as the plain string "off", so it resolves to the
        // Off severity same as the quoted spelling above.
        assert!(overrides.contains(&("dump.type".to_string(), ResolvedSeverity::Off)));
        // A literal `false` DOES fold to a real YAML boolean here (unlike
        // `off`) — not a string, so it is dropped like any other non-string
        // value; same for an integer and an unrecognized severity name.
        assert!(!overrides.iter().any(|(k, _)| k == "def.method-visibility-mismatch"));
        assert!(!overrides.iter().any(|(k, _)| k == "def.ivar-write-mismatch"));
        assert!(!overrides.iter().any(|(k, _)| k == "flow.dead-assignment"));
        assert_eq!(overrides.len(), 3);
    }

    #[test]
    fn severity_overrides_absent_is_empty() {
        assert!(Config::default().severity_overrides().is_empty());
        assert!(cfg("disable: []\n").severity_overrides().is_empty());
    }

    #[test]
    fn severity_overrides_literal_boolean_value_is_dropped() {
        // Isolated repro of the actual (not the assumed) YAML edge case: a
        // literal `false`/`true` value folds to a real boolean in this
        // crate's parser and must not be mistaken for a severity string.
        let overrides = cfg("severity_overrides:\n  call: false\n").severity_overrides();
        assert!(overrides.is_empty());
        let overrides = cfg("severity_overrides:\n  call: true\n").severity_overrides();
        assert!(overrides.is_empty());
    }

    #[test]
    fn severity_overrides_bare_off_is_not_a_yaml_boolean_here() {
        // Contrast case for the divergence note: the reference's Ruby Psych
        // loader (YAML 1.1) folds bare `off` to boolean `false`; this crate's
        // parser (YAML 1.2 Core Schema) does not, so it resolves normally.
        let overrides = cfg("severity_overrides:\n  call: off\n").severity_overrides();
        assert_eq!(overrides, vec![("call".to_string(), ResolvedSeverity::Off)]);
    }

    // ------------------------------------------------------------------
    // Issue #158 — `Configuration::load_with_includes` parity: per-file
    // path resolution, include merge order, cycles, `~`, `..` folding.
    // ------------------------------------------------------------------

    /// A fresh temp dir per test (parallel cargo tests share no cwd here —
    /// every path is absolute).
    fn cfg_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rigor_cfg158_{}_{}_{}",
            std::process::id(),
            tag,
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn parsed(path: &Path) -> Config {
        match Config::read(path) {
            ConfigRead::Parsed(cfg) => *cfg,
            ConfigRead::Fatal(f) => panic!("fatal load: {} (exit {})", f.message, f.code),
            ConfigRead::Absent(m) => panic!("config absent: {m}"),
        }
    }

    /// `paths:` / `signature_paths:` / `pre_eval:` resolve against the FILE's
    /// directory at load; `..` folds lexically; `exclude:` and `baseline:`
    /// are NOT path keys upstream and keep their spelling.
    #[test]
    fn path_keys_resolve_against_the_file_dir() {
        let dir = cfg_dir("keys");
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(
            dir.join("conf/x.yml"),
            "paths: [src, ../shared]\n\
             signature_paths: [sig]\n\
             pre_eval: [boot/patch.rb]\n\
             exclude: [vendor/**]\n\
             baseline: base.yml\n",
        )
        .unwrap();
        let cfg = parsed(&dir.join("conf/x.yml"));
        assert_eq!(
            cfg.paths,
            vec![
                dir.join("conf/src").display().to_string(),
                dir.join("shared").display().to_string(), // `..` folded
            ]
        );
        assert_eq!(cfg.signature_paths, vec![dir.join("conf/sig").display().to_string()]);
        assert_eq!(
            cfg.pre_eval,
            vec![dir.join("conf/boot/patch.rb").display().to_string()]
        );
        // Not path keys — verbatim.
        assert_eq!(cfg.exclude, vec!["vendor/**"]);
        assert_eq!(cfg.baseline_path().as_deref(), Some("base.yml"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `~` expands at the head of a path entry (`File.expand_path`), `~user`
    /// names that user's home, and an unknown `~user` is the uncaught
    /// `ArgumentError` upstream — a fatal load (exit 1), not a silent miss.
    #[test]
    fn tilde_expands_at_the_head() {
        let dir = cfg_dir("tilde");
        let home = home_dir().expect("HOME must resolve for this test");
        std::fs::write(
            dir.join(".rigor.yml"),
            "paths: [~/proj, ~/../proj2, literal/~x]\n",
        )
        .unwrap();
        let cfg = parsed(&dir.join(".rigor.yml"));
        assert_eq!(cfg.paths[0], format!("{home}/proj"));
        // `~` expands THEN `..` folds lexically — `~/..` is the home's parent.
        let home_parent = Path::new(&home)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "/".to_string());
        assert_eq!(cfg.paths[1], format!("{home_parent}/proj2"));
        // `~` NOT at the head stays literal and resolves like any relative path.
        assert_eq!(cfg.paths[2], dir.join("literal/~x").display().to_string());

        std::fs::write(dir.join("bad.yml"), "paths: [~nonexistent_user_158/x]\n").unwrap();
        match Config::read(&dir.join("bad.yml")) {
            ConfigRead::Fatal(f) => {
                assert_eq!(f.code, 1);
                assert!(f.message.contains("doesn't exist"), "{}", f.message);
            }
            _ => panic!("~nouser must die like the reference"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `includes:` merge order (included first, current overrides), each file
    /// resolving its own relative paths against ITS directory, a missing
    /// include as the exit-64 error, and the `visited + [absolute]` chain
    /// semantics: a diamond is NOT circular, a self-inclusion is.
    #[test]
    fn includes_merge_and_resolve_per_file() {
        let dir = cfg_dir("inc");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(
            dir.join("sub/base.yml"),
            "paths: [bsrc]\ndisable: [call.undefined-method]\nsignature_paths: [bsig]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".rigor.yml"),
            "includes: [sub/base.yml]\npaths: [main]\ndisable: [flow.dead-assignment]\n",
        )
        .unwrap();
        let cfg = parsed(&dir.join(".rigor.yml"));
        // `paths:` from the current file WINS outright (deep_merge is
        // right-wins on sequences); `signature_paths:` survives from the
        // include — resolved against SUB's directory.
        assert_eq!(cfg.paths, vec![dir.join("main").display().to_string()]);
        assert_eq!(
            cfg.signature_paths,
            vec![dir.join("sub/bsig").display().to_string()]
        );
        assert_eq!(cfg.disable, vec!["flow.dead-assignment"]);
        assert!(cfg.declares_key("includes"));

        // Diamond: a and b both include shared — loaded twice, not circular.
        std::fs::write(dir.join("shared.yml"), "disable: [call]\n").unwrap();
        std::fs::write(dir.join("a.yml"), "includes: [shared.yml]\n").unwrap();
        std::fs::write(dir.join("b.yml"), "includes: [shared.yml]\n").unwrap();
        std::fs::write(
            dir.join("diamond.yml"),
            "includes: [a.yml, b.yml]\n",
        )
        .unwrap();
        let cfg = parsed(&dir.join("diamond.yml"));
        assert_eq!(cfg.disable, vec!["call"]);

        // Self-inclusion IS circular.
        std::fs::write(dir.join("cycle.yml"), "includes: [cycle.yml]\n").unwrap();
        match Config::read(&dir.join("cycle.yml")) {
            ConfigRead::Fatal(f) => {
                assert_eq!(f.code, 64);
                assert!(f.message.contains("circular include"), "{}", f.message);
            }
            _ => panic!("a self-include must die upstream"),
        }

        // Missing include — `include not found: "miss.yml" (referenced from …)`.
        std::fs::write(dir.join("miss.yml"), "includes: [nope.yml]\n").unwrap();
        match Config::read(&dir.join("miss.yml")) {
            ConfigRead::Fatal(f) => {
                assert_eq!(f.code, 64);
                assert!(
                    f.message.contains("include not found")
                        && f.message.contains("referenced from"),
                    "{}",
                    f.message
                );
            }
            _ => panic!("a missing include must die upstream"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `DISCOVERY_ORDER` is `.rigor.yml` before `.rigor.dist.yml` — the first
    /// existing wins outright (no implicit merge).
    #[test]
    fn discovery_order_is_local_then_dist() {
        assert_eq!(DISCOVERY_ORDER, [".rigor.yml", ".rigor.dist.yml"]);
    }

    /// Issue #158 review fix — the `not valid YAML` renderer must handle
    /// serde_yaml's THREE position shapes, not only the two-marker one:
    /// `{p} at line L C` (`a: b: c` → `1:5`), `{p} at line L C, {ctx}`
    /// (`paths: [src,` → `2:1`), and `{p} at line L C, {ctx} at line L2 C2`
    /// (`paths: [src` → context's `1:8`). Single-marker strings used to
    /// slice-invert and panic (begin > end). Positions/wording are
    /// oracle-verified.
    #[test]
    fn yaml_error_renders_like_psych() {
        let dir = cfg_dir("psych");
        for (yaml, want) in [
            (
                "a: b: c\n",
                "1:5: not valid YAML: mapping values are not allowed in this context",
            ),
            (
                "paths:\n\t- src\n",
                "2:1: not valid YAML: found character that cannot start any token \
                 while scanning for the next token",
            ),
            (
                "paths: [src,\n",
                "2:1: not valid YAML: did not find expected node content \
                 while parsing a flow node",
            ),
            (
                "paths: [:foo]\n",
                "1:9: not valid YAML: did not find expected node content \
                 while parsing a flow node",
            ),
            (
                "paths: [src\n",
                "1:8: not valid YAML: did not find expected ',' or ']' \
                 while parsing a flow sequence",
            ),
        ] {
            std::fs::write(dir.join("b.yml"), yaml).unwrap();
            match Config::read(&dir.join("b.yml")) {
                ConfigRead::Fatal(f) => {
                    assert_eq!(f.code, 64, "{yaml:?}");
                    assert!(
                        f.message.ends_with(want),
                        "{yaml:?} → {:?} (want …{want:?})",
                        f.message
                    );
                }
                _ => panic!("{yaml:?} must be a fatal parse error"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Psych's `safe_load_file` folds a repeated mapping key LAST-WINS at
    /// every level — serde_yaml's `Value` rejects it (`duplicate entry with
    /// key`), so the load goes through the pairwise [`DupOk`] collector.
    #[test]
    fn duplicate_keys_last_wins_like_psych() {
        let dir = cfg_dir("dup");
        std::fs::write(
            dir.join("d.yml"),
            "disable: [call.undefined-method]\ndisable: []\nplugins_io:\n  allowed_paths: [a]\n  allowed_paths: [b]\n",
        )
        .unwrap();
        let cfg = parsed(&dir.join("d.yml"));
        assert!(cfg.disable.is_empty(), "last `disable:` wins");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `safe_load_file` reads the FIRST document only — a `---` follower is
    /// never scanned (serde_yaml's `from_str` errors "more than one
    /// document").
    #[test]
    fn multi_document_reads_doc_one_like_psych() {
        let dir = cfg_dir("multidoc");
        std::fs::write(
            dir.join("m.yml"),
            "paths: [src]\n---\ndisable: [call.undefined-method]\n",
        )
        .unwrap();
        let cfg = parsed(&dir.join("m.yml"));
        // Doc 1's `paths:` is in force (resolved); doc 2's `disable:` never
        // read.
        assert_eq!(cfg.paths, vec![dir.join("src").display().to_string()]);
        assert!(cfg.disable.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// EVERY `<<` merge key applies — a repeated `<<` is not last-wins like
    /// other duplicate keys (oracle: both `<<:` entries merge; both
    /// suppressions land). serde_yaml's `Value::apply_merge` would keep only
    /// the last `<<` — and silently drop the suppression.
    #[test]
    fn duplicate_merge_keys_all_apply_like_psych() {
        let dir = cfg_dir("dupmerge");
        std::fs::write(
            dir.join("m.yml"),
            "severity_overrides:\n  <<: {call.unresolved-toplevel: \"off\"}\n  <<: {call.undefined-method: \"off\"}\n",
        )
        .unwrap();
        let cfg = parsed(&dir.join("m.yml"));
        let overrides = cfg.severity_overrides();
        assert_eq!(
            overrides,
            vec![
                (
                    "call.unresolved-toplevel".to_string(),
                    crate::severity::ResolvedSeverity::Off
                ),
                (
                    "call.undefined-method".to_string(),
                    crate::severity::ResolvedSeverity::Off
                ),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `<<` whose value can't merge — a scalar, or a sequence containing a
    /// non-mapping — stays a LITERAL `<<` key (oracle: `mystery: {<<: [a,b]}`
    /// loads `{"<<" => ["a","b"]}` and warns about `mystery`, not a YAML
    /// error; `severity_overrides: {<<: 5}` reaches the coercion as
    /// `severity_overrides["<<"] is 5, a YAML boolean …`).
    #[test]
    fn non_mergeable_merge_key_stays_literal() {
        let dir = cfg_dir("litmerge");
        std::fs::write(dir.join("a.yml"), "mystery: {<<: [a, b]}\npaths: [src]\n").unwrap();
        let cfg = parsed(&dir.join("a.yml"));
        assert!(cfg.present_keys.contains("mystery"));
        assert_eq!(cfg.paths, vec![dir.join("src").display().to_string()]);

        // `<<` inside severity_overrides stays literal → hits the coercion's
        // non-String-value branch, oracle wording.
        std::fs::write(dir.join("b.yml"), "severity_overrides: {<<: 5}\n").unwrap();
        match Config::read(&dir.join("b.yml")) {
            ConfigRead::Fatal(f) => {
                assert_eq!(f.code, 64);
                assert_eq!(
                    f.message,
                    "severity_overrides[\"<<\"] is 5, a YAML boolean \
                     Bare off/on/no/yes/true/false are parsed as booleans; \
                     quote the severity (e.g. \"off\")."
                );
            }
            _ => panic!("literal `<<` must reach coerce_severity_overrides"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A sequence-of-mappings `<<` merges EACH element (earlier wins on
    /// overlap — `or_insert` order), interleaved literal keys stay put.
    #[test]
    fn merge_sequence_and_interleave_like_psych() {
        let dir = cfg_dir("seqmerge");
        std::fs::write(
            dir.join("s.yml"),
            "severity_overrides:\n  <<: [{call.unresolved-toplevel: \"off\"}, {call.unresolved-toplevel: \"info\"}]\n",
        )
        .unwrap();
        let cfg = parsed(&dir.join("s.yml"));
        // Earlier element wins — "off", not the second element's "info".
        assert_eq!(
            cfg.severity_overrides(),
            vec![(
                "call.unresolved-toplevel".to_string(),
                crate::severity::ResolvedSeverity::Off
            )]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Non-String TOP-LEVEL keys ride `data.keys.map(&:to_s)` upstream: `1`
    /// warns `` `1` ``, `[a,b]` warns `` `["a", "b"]` ``, `~` warns `` `` ``,
    /// `{x: 1}` warns `` `{"x" => 1}` `` — never `invalid type: … field
    /// identifier`.
    #[test]
    fn non_string_top_level_keys_warn_not_die() {
        let dir = cfg_dir("nonstrkey");
        for (yaml, want_key) in [
            ("1: one\n", "1"),
            ("? [a, b]\n: v\n", "[\"a\", \"b\"]"),
            ("~: v\n", ""),
            ("? {x: 1}\n: v\n", "{\"x\" => 1}"),
        ] {
            std::fs::write(dir.join("k.yml"), yaml).unwrap();
            let cfg = parsed(&dir.join("k.yml"));
            assert!(
                cfg.unknown_keys().contains(&want_key),
                "{yaml:?} → unknown keys {:?} missing {want_key:?}",
                cfg.unknown_keys()
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An unrecognized tag (`!foo`) is DROPPED upstream — the bare value
    /// loads, the key warns unknown. `!ruby/*` is `Psych::DisallowedClass`
    /// (uncaught) upstream — still fatal here. `!!str` resolves natively.
    #[test]
    fn unknown_tags_drop_like_psych() {
        let dir = cfg_dir("tags");
        for yaml in ["x: !foo 1\n", "x: !foo [a, b]\n", "x: !foo {k: 1}\n", "x: !!str 5\n"] {
            std::fs::write(dir.join("t.yml"), yaml).unwrap();
            let cfg = parsed(&dir.join("t.yml"));
            assert!(cfg.unknown_keys().contains(&"x"), "{yaml:?} must load");
        }
        std::fs::write(dir.join("r.yml"), "x: !ruby/object:Foo {}\n").unwrap();
        match Config::read(&dir.join("r.yml")) {
            ConfigRead::Fatal(f) => assert_eq!(f.code, 64),
            _ => panic!("!ruby/object must stay fatal"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Invalid UTF-8 is libyaml's reader error, re-rendered upstream as
    /// `<abs>:1:1: not valid YAML: <detail>` (exit 64) — the mark never
    /// advances so position is always 1:1. A UTF-16 BOM is a different
    /// upstream surface (uncaught ArgumentError → exit 1).
    #[test]
    fn invalid_utf8_renders_like_psych() {
        let dir = cfg_dir("utf8");
        for (bytes, want) in [
            (&b"a: ok\nb: ok2\nc: \xff bad\n"[..], "invalid leading UTF-8 octet"),
            (&b"paths: [src]\n\xe2\x82"[..], "incomplete UTF-8 octet sequence"),
            (&b"x: \xe2\x28y\n"[..], "invalid trailing UTF-8 octet"),
        ] {
            std::fs::write(dir.join("u.yml"), bytes).unwrap();
            match Config::read(&dir.join("u.yml")) {
                ConfigRead::Fatal(f) => {
                    assert_eq!(f.code, 64);
                    assert!(
                        f.message.ends_with(&format!(":1:1: not valid YAML: {want}")),
                        "{bytes:?} → {:?}",
                        f.message
                    );
                }
                _ => panic!("{bytes:?} must be a fatal parse error"),
            }
        }
        // UTF-16 BOM → the `r:bom|utf-8` ArgumentError surface, exit 1.
        std::fs::write(dir.join("u16.yml"), b"\xff\xfe a\x00:\x00 \x001\x00\n\x00").unwrap();
        match Config::read(&dir.join("u16.yml")) {
            ConfigRead::Fatal(f) => assert_eq!(f.code, 1, "{:?}", f.message),
            _ => panic!("UTF-16 BOM must be fatal"),
        }
        // A valid UTF-8 BOM still loads.
        std::fs::write(dir.join("bom.yml"), b"\xef\xbb\xbfpaths: [src]\n").unwrap();
        let cfg = parsed(&dir.join("bom.yml"));
        assert_eq!(cfg.paths, vec![dir.join("src").display().to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `coerce_severity_overrides` upstream: non-Hash → `must be a Hash`;
    /// non-String value → `a YAML boolean` (hint only for `false`); bad
    /// severity name → `must be one of`.
    #[test]
    fn severity_overrides_validation_matches_reference() {
        let dir = cfg_dir("sevval");
        for (yaml, want) in [
            ("severity_overrides:\n", "severity_overrides must be a Hash, got nil"),
            ("severity_overrides: [a, b]\n", "severity_overrides must be a Hash, got [\"a\", \"b\"]"),
            (
                "severity_overrides:\n  k: v\n",
                "severity_overrides[\"k\"] must be one of [:error, :warning, :info, :off], got \"v\"",
            ),
            (
                "severity_overrides:\n  call: false\n",
                "severity_overrides[\"call\"] is false, a YAML boolean — did you mean the string \"off\"? \
                 Bare off/on/no/yes/true/false are parsed as booleans; quote the severity (e.g. \"off\").",
            ),
        ] {
            std::fs::write(dir.join("v.yml"), yaml).unwrap();
            match Config::read(&dir.join("v.yml")) {
                ConfigRead::Fatal(f) => {
                    assert_eq!(f.code, 64, "{yaml:?}");
                    assert_eq!(f.message, want, "{yaml:?}");
                }
                _ => panic!("{yaml:?} must be fatal"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A non-mapping document and a broken YAML file are the reference's
    /// `rescue ConfigurationError` surface — fatal (exit 64), not defaults.
    #[test]
    fn fatal_loads_match_the_reference_exit() {
        let dir = cfg_dir("fatal");
        std::fs::write(dir.join("seq.yml"), "- just\n- a\n- list\n").unwrap();
        match Config::read(&dir.join("seq.yml")) {
            ConfigRead::Fatal(f) => {
                assert_eq!(f.code, 64);
                assert!(f.message.contains("must be a YAML mapping"), "{}", f.message);
            }
            _ => panic!("a non-mapping document is a ConfigurationError upstream"),
        }
        // An empty file is `nil || {}` upstream — parses to defaults.
        std::fs::write(dir.join("empty.yml"), "").unwrap();
        let cfg = parsed(&dir.join("empty.yml"));
        assert_eq!(cfg.paths, vec!["lib"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
