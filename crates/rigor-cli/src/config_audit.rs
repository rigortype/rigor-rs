//! `.rigor.yml` config audit — a faithful port of the reference's `ConfigAudit`
//! (`lib/rigor/config_audit.rb`) and its `warn_unresolved_config` CLI hook
//! (`cli/check_command.rb`).
//!
//! It surfaces the class of mistake where a configured value silently resolves
//! to nothing — a typo'd or moved `signature_paths:` directory, an inert
//! `disable:` rule token, a missing explicit `rbs_collection.lockfile`. The
//! shared failure mode is that the loader filters the bad entry WITHOUT a word,
//! so the only symptom is downstream and confusing: missing signatures turn every
//! call into a high-confidence `call.undefined-method`, and an unrecognized
//! suppression token leaves the rule firing as if the `disable:` line were never
//! written. Each such entry is surfaced up front so the cause is visible.
//!
//! Every check mirrors the loader's own acceptance test, so a warning means the
//! loader really did load nothing, and it NEVER fires on a working setup
//! (matching the reference's FP-safe bar). Warnings are emitted to STDERR as
//! `rigor: <message>` regardless of `--format` (like the reference's
//! `@err.puts`). The reference additionally rides them into the `--format json`
//! payload under `config_warnings`; rigor-rs's JSON output is a bare diagnostics
//! *array* (an established shape divergence), so that machine-readable half is
//! deferred rather than forced into an incompatible shape.
//!
//! rigor-rs supports a subset of the reference's config schema, so three of the
//! reference's audit checks are ported (the ones whose keys exist here) and the
//! rest are structurally absent:
//! - `signature_paths:` resolving to nothing — ported.
//! - `disable:` inert built-in tokens — ported (the `severity_overrides:` twin
//!   has no rigor-rs key).
//! - explicit `rbs_collection.lockfile` that does not exist — ported.
//! - `libraries:` / `bundler.*` — no such rigor-rs keys, so nothing to audit.

use std::path::Path;

use crate::config::Config;

/// One config-level finding. `message` is the human-facing line printed to
/// stderr (byte-compatible with the reference's `Warning#message`).
pub struct ConfigWarning {
    /// The source-key discriminator (`"signature_path"` / `"disabled_rule"` /
    /// `"rbs_collection_lockfile"`), mirroring the reference's `Warning#kind`.
    /// Carried for a future structured (`--format json`) surface; unused by the
    /// stderr path today.
    #[allow(dead_code)]
    pub kind: &'static str,
    pub message: String,
}

/// Audit a loaded config. `project_root` is the directory the run's relative
/// paths resolve against (the CLI's cwd), used by the filesystem checks.
#[must_use]
pub fn warnings(cfg: &Config, project_root: &Path) -> Vec<ConfigWarning> {
    let mut out = Vec::new();
    unknown_key_warnings(cfg, &mut out);
    signature_path_warnings(cfg, project_root, &mut out);
    rule_token_warnings(cfg, &mut out);
    explicit_path_warnings(cfg, project_root, &mut out);
    out
}

/// Top-level keys the loader does not own (reference `unknown_key_warnings`,
/// upstream 209f6fd9 / #166). The archetypal case is a typo — `excludee:` for
/// `exclude:` — which the loader drops in silence, so the exclusion never
/// applies and the run reports errors from the very files the user meant to
/// skip. The reserved `rigor_rs:` namespace is exempt by construction (it is a
/// [`Config::KNOWN_KEYS`] entry). Top level only, deliberately — nested
/// unknowns are the reference's schema tier's job (ADR-99).
fn unknown_key_warnings(cfg: &Config, out: &mut Vec<ConfigWarning>) {
    for key in cfg.unknown_keys() {
        let suggestion = spell_checker_correct(key, &Config::KNOWN_KEYS);
        let hint = suggestion
            .as_deref()
            .map(|s| format!(" Did you mean `{s}`?"))
            .unwrap_or_default();
        out.push(ConfigWarning {
            kind: "unknown_key",
            message: format!(
                "`{key}` is not a recognized configuration key; it has no effect.{hint}"
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// DidYouMean::SpellChecker — a verbatim port (Ruby 4.0 stdlib `did_you_mean`:
// spell_checker.rb + jaro_winkler.rb + levenshtein.rb), so the suggestion in
// the warning text is byte-identical to the reference's. The dictionary here
// is ~21 short config keys, so the quadratic loops are irrelevant.
// ---------------------------------------------------------------------------

/// `SpellChecker#correct(input).first` — the nearest dictionary word, or None.
fn spell_checker_correct(input: &str, dictionary: &[&'static str]) -> Option<String> {
    let normalized_input = spell_normalize(input);
    let threshold = if normalized_input.chars().count() > 3 { 0.834 } else { 0.77 };

    let mut words: Vec<&&str> = dictionary
        .iter()
        .filter(|w| jaro_winkler(&spell_normalize(w), &normalized_input) >= threshold)
        .collect();
    words.retain(|w| input != **w);
    // Ruby: `sort_by! { JaroWinkler.distance(word.to_s, normalized_input) }`
    // (the UN-normalized word on purpose) then `reverse!`. A stable sort keeps
    // dictionary order on ties, matching Ruby's merge sort.
    words.sort_by(|a, b| {
        jaro_winkler(a, &normalized_input)
            .partial_cmp(&jaro_winkler(b, &normalized_input))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    words.reverse();

    // Correct mistypes: Levenshtein within ceil(len * 0.25).
    let lev_threshold = (normalized_input.chars().count() as f64 * 0.25).ceil() as usize;
    let mistypes: Vec<&&&str> = words
        .iter()
        .filter(|w| levenshtein(&spell_normalize(w), &normalized_input) <= lev_threshold)
        .collect();
    if let Some(first) = mistypes.first() {
        return Some((***first).to_string());
    }

    // Correct misspells: Levenshtein strictly below the shorter length, first 1.
    words
        .iter()
        .find(|w| {
            let word = spell_normalize(w);
            let length = normalized_input.chars().count().min(word.chars().count());
            levenshtein(&word, &normalized_input) < length
        })
        .map(|w| (**w).to_string())
}

/// `SpellChecker#normalize`: downcase + strip `@`.
fn spell_normalize(s: &str) -> String {
    s.to_lowercase().replace('@', "")
}

/// `DidYouMean::Jaro.distance` — verbatim, including the bitmask matching and
/// the transposition scan. Codepoint-based like the Ruby original.
#[allow(clippy::cast_precision_loss)]
fn jaro(str1: &str, str2: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (str1.chars().collect(), str2.chars().collect());
    let (s1, s2) = if a.len() > b.len() { (b, a) } else { (a, b) };
    let (length1, length2) = (s1.len(), s2.len());

    let mut m = 0.0_f64;
    let mut t = 0.0_f64;
    let range = if length2 > 3 { length2 / 2 - 1 } else { 0 };
    let mut flags1: u128 = 0;
    let mut flags2: u128 = 0;

    for (i, &c1) in s1.iter().enumerate() {
        let last = i + range;
        let mut j = i.saturating_sub(range);
        while j <= last && j < length2 {
            if flags2 & (1 << j) == 0 && c1 == s2[j] {
                flags2 |= 1 << j;
                flags1 |= 1 << i;
                m += 1.0;
                break;
            }
            j += 1;
        }
    }

    let mut k = 0usize;
    for (i, &c1) in s1.iter().enumerate() {
        if flags1 & (1 << i) != 0 {
            let mut j = k;
            let mut index = k;
            while j < length2 {
                index = j;
                if flags2 & (1 << j) != 0 {
                    k = j + 1;
                    break;
                }
                j += 1;
            }
            if c1 != s2[index] {
                t += 1.0;
            }
        }
    }
    t = (t / 2.0).floor();

    if m == 0.0 {
        0.0
    } else {
        (m / length1 as f64 + m / length2 as f64 + (m - t) / m) / 3.0
    }
}

/// `DidYouMean::JaroWinkler.distance` — weight 0.1, threshold 0.7, prefix ≤ 4.
fn jaro_winkler(str1: &str, str2: &str) -> f64 {
    const WEIGHT: f64 = 0.1;
    const THRESHOLD: f64 = 0.7;
    let jaro_distance = jaro(str1, str2);
    if jaro_distance > THRESHOLD {
        let codepoints2: Vec<char> = str2.chars().collect();
        let mut prefix_bonus = 0usize;
        for char1 in str1.chars() {
            if Some(&char1) == codepoints2.get(prefix_bonus) && prefix_bonus < 4 {
                prefix_bonus += 1;
            } else {
                break;
            }
        }
        jaro_distance + (prefix_bonus as f64 * WEIGHT * (1.0 - jaro_distance))
    } else {
        jaro_distance
    }
}

/// `DidYouMean::Levenshtein.distance` — the Text-gem variant, verbatim.
fn levenshtein(str1: &str, str2: &str) -> usize {
    let s1: Vec<char> = str1.chars().collect();
    let s2: Vec<char> = str2.chars().collect();
    let (n, m) = (s1.len(), s2.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut d: Vec<usize> = (0..=m).collect();
    let mut x = 0usize;
    for (i, &char1) in s1.iter().enumerate() {
        let mut i_cost = i + 1;
        for j in 0..m {
            let cost = usize::from(char1 != s2[j]);
            x = (d[j + 1] + 1).min(i_cost + 1).min(d[j] + cost);
            d[j] = i_cost;
            i_cost = x;
        }
        d[m] = x;
    }
    x
}

/// Emit the audit to stderr as `rigor: <message>`, matching the reference's
/// `warn_unresolved_config`. Returns the warnings (for a future JSON surface).
pub fn emit(cfg: &Config, project_root: &Path) -> Vec<ConfigWarning> {
    let warnings = warnings(cfg, project_root);
    for w in &warnings {
        eprintln!("rigor: {}", w.message);
    }
    warnings
}

/// `signature_paths:` entries that resolve to nothing — but ONLY when the key was
/// explicitly configured (an implicit, auto-detected `sig/` is a normal setup,
/// never a misconfiguration). Mirrors `SignaturePathAudit`: a warning means the
/// loader's `directory?` + recursive `**/*.rbs` acceptance test would load zero
/// signatures from the entry.
fn signature_path_warnings(cfg: &Config, project_root: &Path, out: &mut Vec<ConfigWarning>) {
    let Some(paths) = cfg.explicit_signature_paths() else {
        return;
    };
    for path in paths {
        if let Some(message) = classify_signature_path(path, project_root) {
            out.push(ConfigWarning { kind: "signature_path", message });
        }
    }
}

/// Classify one `signature_paths:` entry, returning a warning message when it
/// resolves to nothing (`:missing` / `:not_directory` / `:empty`), or `None`
/// when it is a directory holding at least one `.rbs` (`:ok`). The wording and
/// `path.inspect`-style quoting match the reference's `Entry#message` exactly.
///
/// DELIBERATE DIVERGENCE: the message prints the RELATIVE configured string
/// (`"sigs_typo"`), whereas the reference prints the absolutized path its
/// `Configuration` stores (`"/abs/project/sigs_typo"`). rigor-rs resolves
/// signature dirs relative to the cwd and prints paths as given (its house style,
/// consistent with the diagnostic stream), and the absolute form is
/// environment-specific. The actionable message text is otherwise identical.
fn classify_signature_path(path: &str, project_root: &Path) -> Option<String> {
    let resolved = project_root.join(path);
    if !resolved.exists() {
        return Some(format!(
            "signature_paths: {path:?} does not exist (no signatures loaded from it)"
        ));
    }
    if !resolved.is_dir() {
        return Some(format!(
            "signature_paths: {path:?} is not a directory (no signatures loaded from it)"
        ));
    }
    if count_rbs_files(&resolved) == 0 {
        return Some(format!("signature_paths: {path:?} matched 0 signature files"));
    }
    None
}

/// Recursively count `.rbs` files under `dir` (the reference's
/// `Dir.glob("**/*.rbs").size`). A read error at any level contributes 0.
fn count_rbs_files(dir: &Path) -> usize {
    let mut count = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Follow the loader's own view: it globs `**/*.rbs`, which descends into
        // subdirectories and matches by extension (symlinks resolved by the OS).
        if path.is_dir() {
            count += count_rbs_files(&path);
        } else if path.extension().is_some_and(|e| e == "rbs") {
            count += 1;
        }
    }
    count
}

/// `disable:` tokens that name no rule under a built-in family — a likely typo
/// whose suppression has no effect. Restricted to inert built-in-family tokens so
/// a plugin / legacy-alias id is never mis-flagged (see
/// [`rigor_rules::is_inert_builtin_token`]).
fn rule_token_warnings(cfg: &Config, out: &mut Vec<ConfigWarning>) {
    for token in cfg.disable_tokens() {
        if rigor_rules::is_inert_builtin_token(token) {
            out.push(ConfigWarning {
                kind: "disabled_rule",
                message: format!(
                    "disable: {token:?} is not a recognized rule id; the suppression has no effect"
                ),
            });
        }
    }
}

/// Explicitly-configured `rbs_collection.lockfile` that does not exist. Only the
/// explicit form is audited — a `None` value means auto-detection, which finding
/// nothing is normal (`add_missing_file` in the reference).
fn explicit_path_warnings(cfg: &Config, project_root: &Path, out: &mut Vec<ConfigWarning>) {
    if let Some(path) = cfg.rbs_collection_lockfile() {
        if !project_root.join(path).is_file() {
            out.push(ConfigWarning {
                kind: "rbs_collection_lockfile",
                message: format!("rbs_collection.lockfile: {path:?} does not exist"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(cfg: &Config, root: &Path) -> Vec<String> {
        warnings(cfg, root).into_iter().map(|w| w.message).collect()
    }

    #[test]
    fn default_config_is_silent() {
        // The differential-harness case: no `.rigor.yml` ⇒ zero warnings.
        let cfg = Config::default();
        assert!(warnings(&cfg, Path::new(".")).is_empty());
    }

    #[test]
    fn inert_disable_token_warns() {
        let cfg: Config =
            serde_yaml::from_str("disable:\n  - call.undefiend-method\n  - call\n  - undefined-method\n")
                .unwrap();
        let msgs = messages(&cfg, Path::new("."));
        // Only the typo under a built-in family is flagged; the bare family
        // wildcard and the legacy alias are recognized and stay silent.
        assert_eq!(
            msgs,
            vec![
                "disable: \"call.undefiend-method\" is not a recognized rule id; the suppression has no effect"
                    .to_string()
            ]
        );
    }

    #[test]
    fn explicit_signature_path_missing_warns() {
        // Build via load so `present_keys` records `signature_paths` as explicit.
        let dir = std::env::temp_dir().join(format!("rigor_audit_sig_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join(".rigor.yml");
        std::fs::write(&cfg_path, "signature_paths:\n  - sigs_typo\n").unwrap();
        let cfg = Config::load(Some(&cfg_path));
        let msgs = messages(&cfg, &dir);
        assert_eq!(
            msgs,
            vec![
                "signature_paths: \"sigs_typo\" does not exist (no signatures loaded from it)"
                    .to_string()
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn default_signature_paths_never_warns_even_without_sig_dir() {
        // The implicit `["sig"]` default is NOT explicitly configured, so a
        // project without a `sig/` dir must stay silent (parity with the
        // reference's nil-when-unset gate).
        let cfg = Config::default();
        assert!(cfg.explicit_signature_paths().is_none());
        assert!(warnings(&cfg, Path::new("/nonexistent-xyz")).is_empty());
    }

    #[test]
    fn empty_signature_dir_warns() {
        let dir = std::env::temp_dir().join(format!("rigor_audit_empty_{}", std::process::id()));
        let sig = dir.join("sig");
        std::fs::create_dir_all(&sig).unwrap();
        std::fs::write(dir.join(".rigor.yml"), "signature_paths:\n  - sig\n").unwrap();
        let cfg = Config::load(Some(&dir.join(".rigor.yml")));
        let msgs = messages(&cfg, &dir);
        assert_eq!(
            msgs,
            vec!["signature_paths: \"sig\" matched 0 signature files".to_string()]
        );
        // With a `.rbs` present, it goes silent.
        std::fs::write(sig.join("x.rbs"), "class X\nend\n").unwrap();
        assert!(messages(&cfg, &dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_rbs_collection_lockfile_warns() {
        let dir = std::env::temp_dir().join(format!("rigor_audit_lock_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".rigor.yml"),
            "rbs_collection:\n  lockfile: rbs_collection.lock.yaml\n",
        )
        .unwrap();
        let cfg = Config::load(Some(&dir.join(".rigor.yml")));
        let msgs = messages(&cfg, &dir);
        assert_eq!(
            msgs,
            vec!["rbs_collection.lockfile: \"rbs_collection.lock.yaml\" does not exist".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod unknown_key_tests {
    use super::*;

    fn msgs(yaml: &str) -> Vec<String> {
        let cfg = Config::parse_or_warn(yaml, "test");
        warnings(&cfg, Path::new(".")).into_iter().map(|w| w.message).collect()
    }

    /// Byte-parity with the reference (probed live): the typo'd key warns with
    /// the did-you-mean hint; a no-suggestion unknown warns without one.
    #[test]
    fn unknown_key_warns_byte_exact() {
        let m = msgs("excludee:\n  - spec\n");
        assert_eq!(
            m,
            vec![
                "`excludee` is not a recognized configuration key; it has no effect. Did you mean `exclude`?"
                    .to_string()
            ]
        );
        let m = msgs("zzzz_nothing_close: 1\n");
        assert_eq!(
            m,
            vec!["`zzzz_nothing_close` is not a recognized configuration key; it has no effect."
                .to_string()]
        );
    }

    /// The reserved `rigor_rs:` namespace and reference-owned keys rigor-rs
    /// does not parse (`severity_overrides:`) are KNOWN — never warned.
    #[test]
    fn reserved_and_reference_owned_keys_stay_silent() {
        assert!(msgs("rigor_rs:\n  ruby: off\n").is_empty());
        assert!(msgs("severity_overrides:\n  call.undefined-method: warning\n").is_empty());
        assert!(msgs("libraries:\n  - csv\n").is_empty());
    }
}

#[cfg(test)]
mod spell_checker_parity_tests {
    use super::*;

    /// Byte-parity with Ruby's `DidYouMean::SpellChecker` over the config-key
    /// dictionary. The expected values are dumped from the REAL stdlib
    /// (`ruby -r did_you_mean -e '...'`, Ruby 4.0 / did_you_mean bundled) —
    /// regenerate with the one-liner in the batch note if the dictionary grows.
    #[test]
    fn suggestions_match_ruby_did_you_mean() {
        for (input, expected) in [
            ("excludee", Some("exclude")),
            ("exlude", Some("exclude")),
            ("pathes", Some("paths")),
            ("disabled", Some("disable")),
            ("plugin", Some("plugins")),
            ("include", Some("includes")),
            ("signature_path", Some("signature_paths")),
            ("severity_override", Some("severity_overrides")),
            ("basline", Some("baseline")),
            ("bundle", Some("bundler")),
            ("librarys", Some("libraries")),
            ("target_rubby", Some("target_ruby")),
            ("zzzz_nothing_close", None),
        ] {
            assert_eq!(
                spell_checker_correct(input, &Config::KNOWN_KEYS).as_deref(),
                expected,
                "input: {input}"
            );
        }
    }
}
