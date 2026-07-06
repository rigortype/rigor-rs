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
    signature_path_warnings(cfg, project_root, &mut out);
    rule_token_warnings(cfg, &mut out);
    explicit_path_warnings(cfg, project_root, &mut out);
    out
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
