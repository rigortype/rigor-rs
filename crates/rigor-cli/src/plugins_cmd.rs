//! `rigor plugins` (§11) — report the bundled plugins rigor-rs ships and which
//! the discovered `.rigor.yml` activates.
//!
//! ## Parity note — what this reports vs the reference
//!
//! The reference's `plugins` (`PluginsCommand` + `PluginsRenderer`) is a plugin
//! **activation report** over a GEM-based loader: it reads `.rigor.yml`'s
//! `plugins:` entries, runs `Plugin::Loader.load` to instantiate each gem, and
//! prints per-plugin load status, manifest id/version/description,
//! `signature_paths:` with `.rbs` counts, and the full ADR-37 extension surface
//! (open/owns receivers, produces/consumes, node_rule / dynamic_return /
//! type_specifier protocols), plus `--format json`, `--capabilities`, and
//! `--strict`.
//!
//! rigor-rs ships ONLY native PURE-RBS bundled plugins (currently
//! `activesupport-core-ext`) — no gem loader, no gem-installed plugins, no
//! manifest-declared extension protocols. So the listing legitimately differs:
//! it reports the bundled-plugin catalogue (`rigor_index::plugins`, the same
//! source `doctor` uses) and, for each, whether the discovered config enables it.
//! It borrows the reference's `[OK]`/loaded-vs-available framing and exit-0
//! semantics. Gem-load status, signature-path inspection, the ADR-37 capability
//! catalogue, and `--format json` are out of scope for the standalone build and
//! noted as deferred.

use std::process::ExitCode;

use crate::config::Config;

/// `rigor plugins [list] [--config PATH]` — list the bundled plugins and their
/// config-gated activation state. Always exits 0 (read-only inspection,
/// matching the reference's non-`--strict` advisory exit).
pub fn cmd_plugins(args: &[String]) -> ExitCode {
    let mut explicit_config: Option<String> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            // The reference's grammar is the bare `rigor plugins`; accept a
            // leading `list` subcommand too since it reads naturally and some
            // users reach for it (it is a no-op selector — there is only one
            // view in the standalone build).
            "list" => {}
            "--config" => match it.next() {
                Some(p) => explicit_config = Some(p.clone()),
                None => {
                    eprintln!("rigor plugins: --config expects a path");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--config=") => {
                explicit_config = Some(other["--config=".len()..].to_string());
            }
            "-h" | "--help" | "help" => {
                println!("Usage: rigor plugins [list] [--config PATH]");
                println!();
                println!("List the bundled plugins rigor-rs ships and which `.rigor.yml`'s");
                println!("`plugins:` list enables. Read-only; always exits 0.");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("rigor plugins: unexpected argument `{other}`");
                return ExitCode::from(64);
            }
        }
    }

    let cfg = Config::load(explicit_config.as_deref().map(std::path::Path::new));
    let bundled = rigor_index::plugins::bundled_plugins();

    // Label the config truthfully: an explicit `--config` path, an
    // auto-discovered cwd `.rigor.yml`, or none (the normal case — defaults).
    // Probe the file ourselves so we never claim discovery that didn't happen
    // (`Config::load` degrades "absent" and "present" to the same value).
    let config_label = match &explicit_config {
        Some(path) => path.clone(),
        None if std::path::Path::new(".rigor.yml").is_file() => {
            ".rigor.yml (auto-discovered)".to_string()
        }
        None => "none (using defaults)".to_string(),
    };

    println!("Bundled plugin report");
    println!("  configuration: {config_label}");
    let enabled_count = bundled
        .iter()
        .filter(|p| plugin_enabled(&cfg, p.id))
        .count();
    println!(
        "  bundled: {}    enabled: {enabled_count}",
        bundled.len()
    );
    println!();

    if bundled.is_empty() {
        println!("  (no plugins bundled)");
    } else {
        for p in bundled {
            let enabled = plugin_enabled(&cfg, p.id);
            let state = if enabled { "enabled" } else { "available" };
            println!("  [OK] {}  ({state})", p.id);
            println!("        {}", plugin_description(p.id));
            // The vendored RBS bundle is the plugin's whole payload (no gem,
            // no analyzer code) — surface its file count as a sanity signal,
            // mirroring the reference's `signature_paths:` `.rbs` count.
            println!(
                "        signature bundle: {} .rbs file(s) (pure-RBS, embedded)",
                p.rbs.len()
            );
        }
    }

    // Flag config plugin ids that resolve to nothing (typo / unbundled) — the
    // reference reports these as load errors; here they are simply inert
    // (rigor-rs can't load a gem it doesn't bundle), matching `doctor`.
    for id in &cfg.plugins {
        if rigor_index::plugins::bundled_plugin(id).is_none() {
            println!("  [--] {id}  (unknown — not bundled, ignored)");
        }
    }

    println!();
    println!("rigor-rs ships only native, config-gated PURE-RBS bundled plugins");
    println!("(no gem-installed plugins), so this listing differs from the");
    println!("reference's gem-based activation report.");
    ExitCode::SUCCESS
}

/// Whether the discovered config's `plugins:` list enables `id` (gem-name and
/// manifest-id spellings both resolve, via the index's normalisation).
fn plugin_enabled(cfg: &Config, id: &str) -> bool {
    cfg.plugins
        .iter()
        .any(|p| rigor_index::plugins::bundled_plugin(p).is_some_and(|b| b.id == id))
}

/// A one-line description per bundled plugin id. The reference reads this from
/// the gem manifest; rigor-rs has no manifest, so the description lives here
/// alongside the bundle (a single, greppable source).
fn plugin_description(id: &str) -> &'static str {
    match id {
        "activesupport-core-ext" => {
            "ActiveSupport core extensions (String#squish, Integer#minutes, Time.current, …) as vendored RBS."
        }
        _ => "(no description available)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_plugin_has_a_description() {
        // The description table must cover every bundled id (no silent fallback
        // for a plugin we actually ship).
        for p in rigor_index::plugins::bundled_plugins() {
            assert_ne!(
                plugin_description(p.id),
                "(no description available)",
                "missing description for bundled plugin {}",
                p.id
            );
        }
    }

    #[test]
    fn enabled_tracks_config_plugins_list() {
        let mut cfg = Config::default();
        assert!(!plugin_enabled(&cfg, "activesupport-core-ext"));
        // Both the manifest id and the gem name enable it (index normalises).
        cfg.plugins = vec!["rigor-activesupport-core-ext".to_string()];
        assert!(plugin_enabled(&cfg, "activesupport-core-ext"));
        cfg.plugins = vec!["activesupport-core-ext".to_string()];
        assert!(plugin_enabled(&cfg, "activesupport-core-ext"));
    }

    #[test]
    fn unknown_config_plugin_does_not_enable_anything() {
        let mut cfg = Config::default();
        cfg.plugins = vec!["not-a-real-plugin".to_string()];
        assert!(!plugin_enabled(&cfg, "activesupport-core-ext"));
    }
}
