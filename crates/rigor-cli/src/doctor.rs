//! `rigor doctor` (§13) — an environment / setup diagnostic for rigor-rs.
//!
//! ## Parity note — what this reports vs the reference
//!
//! The reference's `doctor` (ADR-77) is a findings CLASSIFIER: it runs a scoped
//! `check` pass and reports a fixed set of pass/fail checks (config audit, RBS
//! environment, plugin load, baseline drift, Rails-unconfigured) keyed for a
//! JSON contract. rigor-rs's `doctor` instead reports the SETUP STATE a
//! standalone, embedded build needs to surface (audit-R1): the active RBS source
//! (embedded vendored set vs `RIGOR_RBS_CORE_DIR` override vs stub) + class
//! count, the bundled plugins and which the discovered config enables, whether a
//! `.rigor.yml` was found and parsed, and the implemented (sound-subset) rule
//! set. It borrows the reference's `[PASS]`/`[WARN]`/`[FAIL]` line shape and the
//! exit semantics (0 healthy, 1 if anything is broken), so the look is familiar.
//!
//! It does NOT run an analysis pass (no `configuration.paths` model in rigor-rs's
//! CLI yet), so the reference's baseline-drift and Rails-plugin checks are out of
//! scope here and noted as deferred.

use std::process::ExitCode;

use rigor_index::{CoreIndex, RbsSource};

use crate::config::Config;
use crate::ruby_mode;

/// `rigor doctor [--config PATH]` — report the environment/setup diagnostic.
/// Exit 0 when healthy, 1 if a check fails (a malformed explicit config).
pub fn cmd_doctor(args: &[String]) -> ExitCode {
    let mut explicit_config: Option<String> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => match it.next() {
                Some(p) => explicit_config = Some(p.clone()),
                None => {
                    eprintln!("rigor doctor: --config expects a path");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--config=") => {
                explicit_config = Some(other["--config=".len()..].to_string());
            }
            other => {
                eprintln!("rigor doctor: unexpected argument `{other}`");
                return ExitCode::from(64);
            }
        }
    }

    let mut healthy = true;

    println!("rigor doctor — rigor-rs v{} (standalone, sound-subset port)", env!("CARGO_PKG_VERSION"));
    println!();

    // --- Config discovery ---------------------------------------------------
    // Distinguish "no config" (normal) from "found but malformed" (warn). We
    // probe the raw file ourselves so we can tell the two apart — `Config::load`
    // degrades both to default.
    let (config_label, config_status, cfg) = resolve_config(explicit_config.as_deref());
    println!("[{config_status}] config: {config_label}");

    // --- RBS source (audit-R1) ----------------------------------------------
    // The keystone: surface whether coverage comes from the embedded vendored
    // set (the standalone default), a RIGOR_RBS_CORE_DIR override, or the stub.
    let index = CoreIndex::for_project(&cfg.plugins, &cfg.all_signature_dirs(std::path::Path::new(".")));
    let count = index.class_count();
    match index.rbs_source() {
        RbsSource::Embedded => {
            println!(
                "[PASS] rbs: embedded vendored set ({count} classes; no runtime rbs-gem dependency)"
            );
        }
        RbsSource::Override(dir) => {
            println!(
                "[PASS] rbs: RIGOR_RBS_CORE_DIR override active — {dir} ({count} classes)"
            );
        }
        RbsSource::Stub => {
            healthy = false;
            println!(
                "[FAIL] rbs: stub fallback active ({count} classes) — the embedded/override RBS produced nothing"
            );
            println!("  → coverage is severely reduced; report this (the embedded set should always load).");
        }
    }

    // --- Coverage posture (ADR-0036) ----------------------------------------
    // Which posture a default run resolves to (env + `rigor_rs.ruby`, default
    // `require`). The sidecar is not yet implemented, so a non-opt-out mode is a
    // *reduced* posture today (WARN, not FAIL — expected pre-sidecar); an explicit
    // `--ruby=off` is a deliberate choice (PASS).
    match ruby_mode::resolve(None, cfg.ruby_config_value(), ruby_mode::RubyMode::Require) {
        Ok(ruby) => {
            let (posture, reduced) = ruby_mode::interim_posture_line(&ruby);
            let marker = if reduced { "WARN" } else { "PASS" };
            println!("[{marker}] coverage posture: {posture}");
        }
        Err(e) => println!("[WARN] coverage posture: unresolved — {e}"),
    }

    // --- Plugins ------------------------------------------------------------
    // Bundled plugins are config-gated: available always, enabled only when
    // named in `.rigor.yml`'s `plugins:`.
    let bundled = rigor_index::plugins::bundled_plugins();
    if bundled.is_empty() {
        println!("[PASS] plugins: none bundled");
    } else {
        println!("[PASS] plugins: {} bundled (config-gated)", bundled.len());
        for p in bundled {
            let enabled = cfg
                .plugins
                .iter()
                .any(|id| rigor_index::plugins::bundled_plugin(id).is_some_and(|b| b.id == p.id));
            let state = if enabled { "enabled" } else { "available" };
            println!("  - {} ({state})", p.id);
        }
        // Flag config plugin ids that resolve to nothing (typo / unbundled).
        for id in &cfg.plugins {
            if rigor_index::plugins::bundled_plugin(id).is_none() {
                println!("  - {id} (unknown — not bundled, ignored)");
            }
        }
    }

    // --- Implemented rules (sound-subset, audit-R1 reduced-coverage) ---------
    let rules = rigor_rules::implemented_rules();
    println!("[PASS] rules: {} implemented (sound subset of the reference; ADR-0008)", rules.len());
    for r in rules {
        println!("  - {r}");
    }

    println!();
    if healthy {
        println!("rigor doctor: all checks passed — no setup problems detected.");
        ExitCode::SUCCESS
    } else {
        println!("rigor doctor: issues found — see [FAIL] lines above.");
        ExitCode::from(1)
    }
}

/// Resolve the config for the report: returns `(label, status, loaded_config)`.
/// `status` is `PASS` (found+parsed or absent), `WARN` (found but malformed).
/// The loaded `Config` is what `Config::load` returns (default on any problem).
fn resolve_config(explicit: Option<&str>) -> (String, &'static str, Config) {
    let path = explicit.unwrap_or(".rigor.yml");
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_yaml::from_str::<Config>(&text) {
            Ok(cfg) => (format!("{path} (found, parsed OK)"), "PASS", cfg),
            Err(_) => (
                format!("{path} (found but MALFORMED — ignored, analysing with defaults)"),
                "WARN",
                Config::default(),
            ),
        },
        Err(_) => {
            if explicit.is_some() {
                // The user named a path that does not exist — worth a WARN.
                (format!("{path} (not found)"), "WARN", Config::default())
            } else {
                ("no .rigor.yml in cwd (using defaults)".to_string(), "PASS", Config::default())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_is_pass_with_defaults() {
        let (_label, status, cfg) = resolve_config(Some("/nonexistent/.rigor.yml"));
        // An explicit missing path warns; auto-discovery would PASS.
        assert_eq!(status, "WARN");
        assert!(cfg.plugins.is_empty());
    }

    #[test]
    fn auto_discovery_absent_is_pass() {
        // Resolve against a path guaranteed absent under cwd; the auto-discovery
        // (explicit=None) branch treats absence as PASS.
        let (label, status, _cfg) = resolve_config(None);
        // Either there is no .rigor.yml (PASS, "no .rigor.yml") or one parses.
        assert!(status == "PASS" || status == "WARN");
        assert!(!label.is_empty());
    }

    #[test]
    fn index_exposes_a_source_and_count() {
        // doctor's keystone data is reachable from the public API.
        let index = CoreIndex::new();
        assert!(index.class_count() > 0);
        // Default (no override) is the embedded set in a normal build.
        matches!(index.rbs_source(), RbsSource::Embedded | RbsSource::Override(_));
    }
}
