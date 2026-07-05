//! ADR-0036: the coverage-posture axis — whether a run uses the Ruby sidecar
//! (full fidelity) or the Ruby-free sound subset, and how strictly.
//!
//! This module owns the *policy surface* (mode grammar, layered resolution,
//! mutual-exclusion rules, the interim posture notice). The sidecar itself is
//! not yet implemented (ADR-0008), so today every mode runs the sound subset;
//! the value of shipping this now (ADR-0036 phase a) is to convert today's
//! *silent* subset into a *disclosed* coverage posture and freeze the vocabulary
//! before any production-ready announcement. The exit-69 hard-error teeth land
//! with the sidecar (phase b).

use std::fmt;

/// The requested coverage posture. `Path` is a specific ruby binary and implies
/// `Require` (naming a ruby means you want it used; an unusable one hard-errors
/// once the sidecar lands — it never falls back to the subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RubyMode {
    /// Full fidelity required; unavailability is a hard error (exit 69) once the
    /// sidecar exists. Default for one-shot commands.
    Require,
    /// Full fidelity if the sidecar is usable, else the sound subset — no error.
    /// Default for `rigor lsp`.
    Auto,
    /// The Ruby-free sound subset; no sidecar probe. The single explicit opt-out.
    Off,
    /// `Require` using this specific ruby binary.
    Path(String),
}

impl RubyMode {
    /// Whether this mode opts OUT of full fidelity deliberately (`off`). Used to
    /// decide whether the interim "sidecar pending" notice is shown: an explicit
    /// opt-out chose the subset, so it is not warned about.
    #[must_use]
    pub fn is_opt_out(&self) -> bool {
        matches!(self, RubyMode::Off)
    }
}

impl fmt::Display for RubyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RubyMode::Require => f.write_str("require"),
            RubyMode::Auto => f.write_str("auto"),
            RubyMode::Off => f.write_str("off"),
            RubyMode::Path(p) => write!(f, "{p}"),
        }
    }
}

/// Parse one `--ruby=` / `RIGOR_RUBY` / `rigor_rs.ruby` value. A reserved keyword
/// (`require`/`auto`/`off`) is that keyword; anything else is a ruby binary path
/// (use a path form like `./off` to name a ruby that collides with a keyword).
#[must_use]
pub fn parse_value(s: &str) -> RubyMode {
    match s {
        "require" => RubyMode::Require,
        "auto" => RubyMode::Auto,
        "off" => RubyMode::Off,
        other => RubyMode::Path(other.to_string()),
    }
}

/// Whether an env var reads as "set" for the boolean `RIGOR_NO_RUBY` — present,
/// non-empty, and not a `0`/`false` disable spelling.
fn env_flag_set(val: &str) -> bool {
    !val.is_empty() && val != "0" && !val.eq_ignore_ascii_case("false")
}

/// Resolve the effective [`RubyMode`] across layers (highest wins):
/// CLI > env (`RIGOR_RUBY` / `RIGOR_NO_RUBY`) > `.rigor.yml` (`rigor_rs.ruby`) >
/// `default_mode` (context: `Require` for one-shot commands, `Auto` for LSP).
///
/// `cli` is the already-resolved CLI-layer mode (the caller detects the
/// `--ruby` + `--no-ruby` same-layer conflict during arg parsing). Returns
/// `Err(message)` when the env layer sets both `RIGOR_RUBY` and `RIGOR_NO_RUBY`
/// (a same-layer conflict — fail loud, never silently pick one).
pub fn resolve(
    cli: Option<RubyMode>,
    config_value: Option<&str>,
    default_mode: RubyMode,
) -> Result<RubyMode, String> {
    if let Some(mode) = cli {
        return Ok(mode);
    }

    let env_ruby = std::env::var("RIGOR_RUBY").ok().filter(|s| !s.is_empty());
    let env_no_ruby = std::env::var("RIGOR_NO_RUBY")
        .ok()
        .filter(|s| env_flag_set(s));
    match (env_ruby, env_no_ruby) {
        (Some(_), Some(_)) => {
            return Err(
                "RIGOR_RUBY and RIGOR_NO_RUBY are both set — set at most one".to_string(),
            );
        }
        (Some(v), None) => return Ok(parse_value(&v)),
        (None, Some(_)) => return Ok(RubyMode::Off),
        (None, None) => {}
    }

    if let Some(v) = config_value {
        return Ok(parse_value(v));
    }
    Ok(default_mode)
}

/// The one-time stderr notice emitted (phase a) when a non-opt-out mode runs the
/// sound subset only because the sidecar is not yet implemented. Converts a
/// silent posture into a disclosed one.
pub const INTERIM_PENDING_NOTICE: &str =
    "rigor: full-fidelity Ruby sidecar not yet implemented — running the sound subset \
     (coverage posture: subset). Pass --ruby=off (or RIGOR_NO_RUBY=1) to silence this.";

/// A short human posture description for `rigor doctor`, plus whether it is a
/// *reduced* posture (deliberate opt-out is not reduced; a pending sidecar is).
#[must_use]
pub fn interim_posture_line(mode: &RubyMode) -> (String, bool) {
    if mode.is_opt_out() {
        (
            "sound subset (Ruby-free by request: --ruby=off)".to_string(),
            false,
        )
    } else {
        (
            format!(
                "sound subset — full fidelity pending the Ruby sidecar (requested: {mode}; ADR-0036)"
            ),
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keywords_and_path() {
        assert_eq!(parse_value("require"), RubyMode::Require);
        assert_eq!(parse_value("auto"), RubyMode::Auto);
        assert_eq!(parse_value("off"), RubyMode::Off);
        assert_eq!(parse_value("/opt/ruby"), RubyMode::Path("/opt/ruby".into()));
        // A keyword-colliding name is reachable via a path form.
        assert_eq!(parse_value("./off"), RubyMode::Path("./off".into()));
    }

    #[test]
    fn cli_layer_wins() {
        // Env/config are ignored when the CLI layer resolved a mode.
        let m = resolve(Some(RubyMode::Off), Some("require"), RubyMode::Require).unwrap();
        assert_eq!(m, RubyMode::Off);
    }

    #[test]
    fn config_used_when_no_cli_or_env() {
        // NB: relies on RIGOR_RUBY / RIGOR_NO_RUBY being unset in the test env.
        let m = resolve(None, Some("auto"), RubyMode::Require).unwrap();
        assert_eq!(m, RubyMode::Auto);
    }

    #[test]
    fn default_when_nothing_specified() {
        let m = resolve(None, None, RubyMode::Auto).unwrap();
        assert_eq!(m, RubyMode::Auto);
    }

    #[test]
    fn opt_out_is_not_a_reduced_posture() {
        let (_, reduced) = interim_posture_line(&RubyMode::Off);
        assert!(!reduced);
        let (_, reduced) = interim_posture_line(&RubyMode::Require);
        assert!(reduced);
    }
}
