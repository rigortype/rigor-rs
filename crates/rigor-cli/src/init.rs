//! `rigor init` (§11) — scaffold a starter `.rigor.dist.yml` for a project.
//!
//! Mirrors the reference's `run_init` surface (`--force` to overwrite,
//! `--path PATH` for a non-default destination, default destination
//! `.rigor.dist.yml`, the same "Created … / Next steps:" stdout shape and the
//! same "already exists; use --force to overwrite it" refusal + exit 1).
//!
//! ## Parity note — template content
//!
//! The reference serialises its full `Configuration::DEFAULTS` (≈30 keys, most
//! of them forward-looking preview surface). rigor-rs's config loader
//! ([`crate::config::Config`]) HONORS only four keys — `disable:`, `exclude:`,
//! `plugins:`, `baseline:` — and ignores the rest. So rather than emit a
//! defaults dump that advertises keys rigor-rs silently drops, the template here
//! documents the four supported keys (with brief comments + inert defaults) and
//! is honest that it is rigor-rs's sound subset. The file round-trips through
//! `Config::load` (every key is one the loader recognises).

use std::path::Path;
use std::process::ExitCode;

/// Default destination — the committed project-default config (the reference's
/// `.rigor.dist.yml`; auto-discovery prefers a developer's `.rigor.yml` override
/// when both are present).
const DEFAULT_PATH: &str = ".rigor.dist.yml";

/// `rigor init [--force] [--path PATH]` — write a starter config. Exit 0 on
/// success, 1 if the destination exists and `--force` was not given, 64 on a
/// usage error.
pub fn cmd_init(args: &[String]) -> ExitCode {
    let mut force = false;
    let mut path = DEFAULT_PATH.to_string();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--force" => force = true,
            "--path" => match it.next() {
                Some(p) => path = p.clone(),
                None => {
                    eprintln!("rigor init: --path expects a path");
                    return ExitCode::from(64);
                }
            },
            // Accept `--path=PATH` (the reference's optparse spelling) too.
            other if other.starts_with("--path=") => {
                path = other["--path=".len()..].to_string();
            }
            other => {
                eprintln!("rigor init: unexpected argument `{other}`");
                return ExitCode::from(64);
            }
        }
    }

    if Path::new(&path).exists() && !force {
        // Match the reference's refusal message + exit 1.
        eprintln!("{path} already exists; use --force to overwrite it");
        return ExitCode::from(1);
    }

    if let Err(e) = std::fs::write(&path, INIT_TEMPLATE) {
        eprintln!("rigor init: cannot write {path}: {e}");
        return ExitCode::from(1);
    }

    println!("Created {path}");
    print_next_steps(&path);
    ExitCode::SUCCESS
}

/// The "Next steps:" block printed after a successful write (mirrors the
/// reference's `print_init_next_steps`, retargeted at rigor-rs's commands).
fn print_next_steps(path: &str) {
    println!();
    println!("Next steps:");
    println!("  1. Edit {path} — add the `plugins:` your project needs (run `rigor doctor` to see what's bundled).");
    println!("  2. Run `rigor doctor` to verify your RBS/plugin/rule setup.");
    println!("  3. Run `rigor check <file...>` to analyse your code.");
}

/// The starter `.rigor.dist.yml` body. Documents ONLY the keys rigor-rs's
/// loader honors today (disable / exclude / plugins / baseline); the header is
/// explicit that this is rigor-rs's standalone sound subset.
const INIT_TEMPLATE: &str = "\
# Rigor (rigor-rs) configuration — standalone, sound-subset port.
#
# rigor-rs HONORS the four keys below; any other key (the reference's larger
# preview schema) is ignored gracefully. Run `rigor doctor` for the active RBS
# source, the bundled plugins, and the implemented rule set.
#
# - disable:  rule ids / family tokens to silence project-wide. The shipped
#             rules are call.undefined-method, call.wrong-arity,
#             call.possible-nil-receiver, flow.dead-assignment,
#             def.override-visibility-reduced. A bare family token
#             (`call`, `flow`, `def`, …) wildcards every rule under that
#             prefix; legacy unprefixed names (`undefined-method`, …) still
#             resolve. In-source `# rigor:disable <rule>` at the end of a line
#             silences per-line; `# rigor:disable all` suppresses every rule.
# - exclude:  path glob patterns; a matching file is skipped entirely.
# - plugins:  config-gated plugins to load (opt-in). Bundled today:
#             activesupport-core-ext. Naming a plugin reopens core classes with
#             its RBS selectors. (Unknown ids are inert.)
# - baseline: path to a suppression baseline file, or `false` to disable.
#             Presence of the file alone never activates it — name it here (or
#             pass `--baseline PATH`).
disable: []
exclude: []
plugins: []
baseline: false
";

#[cfg(test)]
mod tests {
    use super::*;

    /// The template round-trips through the config loader — every key it writes
    /// is one rigor-rs actually recognises (no silently-dropped keys advertised).
    #[test]
    fn template_parses_as_config() {
        let cfg: crate::config::Config = serde_yaml::from_str(INIT_TEMPLATE).unwrap();
        assert!(cfg.disable.is_empty());
        assert!(cfg.exclude.is_empty());
        assert!(cfg.plugins.is_empty());
        assert_eq!(cfg.baseline_path(), None);
    }

    /// The template documents only the four honored keys (a defense against a
    /// future edit re-introducing an unsupported key as if it worked).
    #[test]
    fn template_keys_are_the_supported_subset() {
        // The active (non-comment) lines are exactly the four supported keys.
        let keys: Vec<&str> = INIT_TEMPLATE
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && l.contains(':'))
            .map(|l| l.split(':').next().unwrap().trim())
            .collect();
        assert_eq!(keys, vec!["disable", "exclude", "plugins", "baseline"]);
    }
}
