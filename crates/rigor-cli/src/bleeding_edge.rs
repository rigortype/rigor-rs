//! ADR-50 § WD2 — the bleeding-edge overlay (a faithful port of the reference's
//! `Rigor::BleedingEdge` + the `show-bleedingedge` command).
//!
//! A Rigor-maintained set of the NEXT MAJOR's queued changes — severity-map
//! promotions and new-discipline rule enablements — a user can adopt early via
//! `bleeding_edge:` in `.rigor.yml` or `--bleeding-edge[=LIST]` on `check`. It
//! is versioned with the tool, NOT a user-supplied file (the inspectable
//! counterpart to PHPStan's `bleedingEdge` include). Feature ids are
//! kebab-case DISCIPLINE names (contract vocabulary): a discipline may grow to
//! cover more rules without its id going stale, and a feature graduates to
//! default-on at a major by being removed from [`FEATURES`].
//!
//! rigor-rs consumes ONE feature in the engine today (`use-of-void-value`
//! gates the `static.value-use.void` collector); the registry carries the full
//! overlay verbatim so `show-bleedingedge` and the selector semantics match
//! the reference byte-for-byte (`reject-unparseable-signatures` promotes rules
//! rigor-rs does not emit — its adoption is inert here, exactly as an unknown
//! rule id is inert there).

use std::process::ExitCode;

use crate::config::{BleedingEdgeSelector, Config};
use crate::severity::ResolvedSeverity;

/// One queued change (reference `BleedingEdge::Feature`).
pub struct Feature {
    /// The stable feature id (contract vocabulary).
    pub id: &'static str,
    /// A one-line description of what it changes.
    pub summary: &'static str,
    /// Canonical rule id → the severity the feature imposes (rendered by
    /// `show-bleedingedge`; the engine gate reads only [`Feature::id`]).
    pub severity_overrides: &'static [(&'static str, &'static str)],
}

/// The overlay, verbatim from the pinned reference (`bleeding_edge.rb`).
pub const FEATURES: &[Feature] = &[
    Feature {
        id: "reject-unparseable-signatures",
        summary: "A broken `signature_paths:` RBS set fails the run instead of degrading it \
                  silently. An unparseable `.rbs` is otherwise skipped with a warning, and a \
                  duplicate-declaration conflict (a file that parses fine but collides on resolve \
                  — typically against Rigor's own bundled RBS) collapses the whole env with a \
                  warning; either way the run gets quieter rather than cleaner. This treats both \
                  as a build error, the way a broken source file already is.",
        severity_overrides: &[
            ("rbs.coverage.quarantined-signature", "error"),
            ("rbs.coverage.environment-build-failed", "error"),
        ],
    },
    Feature {
        id: "use-of-void-value",
        summary: "Using a value recovered from an author-declared `-> void` return in value \
                  context (an assignment right-hand side, a call receiver, or an argument) \
                  becomes a `:warning`. An explicit `-> void` is the strongest possible \"do not \
                  rely on this return\" signal, so the direct-dispatch case is FP-narrow; a \
                  bare-statement `void` result and a legitimate `top` value both stay silent. Off \
                  by default because a new required diagnostic is an ADR-50 WD1 compatibility \
                  change (ADR-100 WD2).",
        severity_overrides: &[("static.value-use.void", "warning")],
    },
];

/// The merged severity-override map the ACTIVE features impose for a selector
/// (reference `BleedingEdge.severity_overrides_for`) — composed BELOW the
/// user's own `severity_overrides:` and ABOVE the profile table in
/// [`crate::severity::resolve`]. Later features win on a (hypothetical) rule
/// collision, matching the reference's hash merge order.
#[must_use]
pub fn severity_overrides_for(
    selector: &BleedingEdgeSelector,
) -> Vec<(&'static str, ResolvedSeverity)> {
    let mut out: Vec<(&'static str, ResolvedSeverity)> = Vec::new();
    for f in FEATURES {
        if !selector.activates(f.id) {
            continue;
        }
        for (rule, sev) in f.severity_overrides {
            let Some(sev) = ResolvedSeverity::from_str(sev) else { continue };
            if let Some(slot) = out.iter_mut().find(|(r, _)| r == rule) {
                slot.1 = sev;
            } else {
                out.push((rule, sev));
            }
        }
    }
    out
}

/// `rigor show-bleedingedge` — print the overlay + what the cwd's config
/// adopts, byte-matching the reference command's text output.
pub fn cmd_show_bleedingedge(_args: &[String]) -> ExitCode {
    let cfg = Config::load(None);
    let selector = cfg.bleeding_edge_selector();
    println!("Bleeding-edge overlay (ADR-50 § WD2)");
    println!();
    println!("{} feature(s) queued for the next major:", FEATURES.len());
    println!();
    for f in FEATURES {
        println!("  {}", f.id);
        println!("    {}", f.summary);
        for (rule, sev) in f.severity_overrides {
            println!("    severity: {rule} → :{sev}");
        }
    }
    println!();
    let adopted: Vec<&str> =
        FEATURES.iter().map(|f| f.id).filter(|id| selector.activates(id)).collect();
    if adopted.is_empty() {
        println!("Your configuration adopts: (none)");
    } else {
        println!("Your configuration adopts: {}", adopted.join(", "));
    }
    ExitCode::SUCCESS
}
