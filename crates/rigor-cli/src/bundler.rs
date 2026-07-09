//! `Gemfile.lock`-gated bundled RBS overlays (ADR-72 port).
//!
//! When a gem is LOCKED in the project's `Gemfile.lock` but ships no RBS,
//! rigor-rs auto-applies the matching bundled RBS overlay so a Rails project
//! stops seeing a systematic `call.undefined-method` FP wall on the gem's
//! core-class extensions (`3.minutes`, `"x".squish`, `Object#blank?`) WITHOUT
//! the user having to name the plugin in `.rigor.yml`. A project that does NOT
//! lock the gem still sees the genuine diagnostic.
//!
//! This mirrors the reference `Environment::GEM_OVERLAY_PLUGIN_IDS` +
//! `gem_overlay_paths`: the gate is presence-in-`Gemfile.lock`, and the overlay
//! is the SAME vendored RBS the opt-in `plugins:` entry loads (rigor-rs's
//! `activesupport-core-ext` bundle). FP-SAFE by construction — it can only add
//! signatures for a gem the project actually depends on, so a real typo
//! (`5.minuets`) still fires. Gated on `bundler.auto_detect` (default `true`).

use std::collections::BTreeSet;
use std::path::Path;

/// A locked `Gemfile.lock` gem name → the bundled overlay plugin id it maps to
/// (reference `GEM_OVERLAY_PLUGIN_IDS`). Only gems whose overlay rigor-rs
/// actually bundles are listed.
const GEM_OVERLAY_PLUGIN_IDS: &[(&str, &str)] = &[("activesupport", "activesupport-core-ext")];

/// The overlay plugin ids auto-selected for a project rooted at `root`: for each
/// `Gemfile.lock`-locked gem with a bundled overlay, its plugin id. Empty when
/// there is no `Gemfile.lock` or no mapped gem is locked, so a non-Rails project
/// (and the config-less differential harness) is unaffected.
#[must_use]
pub fn auto_detected_overlays(root: &Path) -> Vec<String> {
    let locked = match std::fs::read_to_string(root.join("Gemfile.lock")) {
        Ok(text) => locked_gems(&text),
        Err(_) => return Vec::new(),
    };
    GEM_OVERLAY_PLUGIN_IDS
        .iter()
        .filter(|(gem, _)| locked.contains(*gem))
        .map(|(_, plugin)| (*plugin).to_string())
        .collect()
}

/// The gem names locked in a `Gemfile.lock` — the spec entries under the `GEM`
/// section's `specs:` block, which sit at EXACTLY four spaces of indent
/// (`    activesupport (7.1.3)`); their transitive dependencies sit deeper (six
/// spaces) and are skipped. A line-oriented parse (no bundler, no gem): the
/// `GEM` section runs until the first non-indented line, and within it the
/// four-space `name (version)` lines are the locked specs.
fn locked_gems(text: &str) -> BTreeSet<String> {
    let mut gems = BTreeSet::new();
    let mut in_gem_section = false;
    for line in text.lines() {
        if !line.starts_with(' ') && !line.is_empty() {
            // A section header (`GEM`, `PLATFORMS`, `DEPENDENCIES`, …).
            in_gem_section = line == "GEM";
            continue;
        }
        if !in_gem_section {
            continue;
        }
        // A spec line is indented exactly four spaces (deeper = a dependency).
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue; // six-space dependency line
        }
        // `name (version)` — take the token before the first space.
        if let Some(name) = rest.split(' ').next() {
            if !name.is_empty() {
                gems.insert(name.to_string());
            }
        }
    }
    gems
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = "\
GEM
  remote: https://rubygems.org/
  specs:
    activesupport (7.1.3)
      base64
      concurrent-ruby (~> 1.0, >= 1.0.2)
    base64 (0.2.0)
    concurrent-ruby (1.2.3)

PLATFORMS
  ruby

DEPENDENCIES
  activesupport

BUNDLED WITH
   2.5.6
";

    #[test]
    fn parses_locked_specs_not_dependencies() {
        let gems = locked_gems(LOCK);
        assert!(gems.contains("activesupport"));
        assert!(gems.contains("base64"));
        assert!(gems.contains("concurrent-ruby"));
        // The DEPENDENCIES section's `activesupport` is not double-counted; the
        // nested six-space deps under a spec are not mistaken for locked specs.
        assert_eq!(gems.len(), 3, "{gems:?}");
    }

    #[test]
    fn maps_activesupport_to_the_overlay() {
        let dir = std::env::temp_dir().join(format!("rigor_bundler_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Gemfile.lock"), LOCK).unwrap();
        assert_eq!(auto_detected_overlays(&dir), vec!["activesupport-core-ext".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_gemfile_lock_is_empty() {
        let dir = std::env::temp_dir().join(format!("rigor_bundler_none_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(auto_detected_overlays(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unmapped_gems_do_not_overlay() {
        // A lockfile without activesupport → no overlay.
        let gems = locked_gems("GEM\n  specs:\n    rails (7.1.3)\n\nDEPENDENCIES\n  rails\n");
        assert!(gems.contains("rails"));
        assert!(!gems.contains("activesupport"));
    }
}
