//! `rigor docs` (§11) — print the rule documentation rigor-rs ships, offline.
//!
//! ## Parity note — what this serves vs the reference
//!
//! The reference's `docs` (ADR-74) is a bundled-MANUAL renderer: the `rigortype`
//! gem ships `docs/install.md`, `docs/llms.txt`, and the full user-facing manual
//! + handbook (`docs/manual/*.md`, `docs/handbook/*.md`), and `docs <name>`
//!   prints those prose pages from disk (with `--list` / `--path` flags + an
//!   `llms.txt` index).
//!
//! **The standalone rigor-rs build bundles none of that prose.** Rather than
//! fabricate manual content, this implements the tractable CORE over the
//! documented content rigor-rs DOES ship — the rule catalogue (the `explain`
//! command's `RuleCatalog` port):
//!
//! - `rigor docs`            — list the documented rules (id + one-line summary).
//! - `rigor docs <rule-id>`  — print that rule's documentation (the same per-rule
//!   reference `explain <rule-id>` renders: summary,
//!   severity-by-profile, fires-when / does-not-fire,
//!   suppression, docs URL). Canonical id, legacy alias,
//!   and family token (`call`/`flow`/…) all resolve.
//! - unknown id → a stderr error listing the documented rules + exit 64 (matching
//!   `explain`'s unknown-rule behaviour; the reference's `docs` lists docs + exits
//!   1, but reusing `explain`'s contract keeps the two rule-doc paths consistent).
//!
//! **Deferred** (no bundled prose corpus in the standalone build): the reference's
//! manual / handbook / install pages, the `llms.txt` index, and the
//! `--list` / `--path` flags that address those files. `docs` prints a one-line
//! note pointing at the web manual for that material.

use std::process::ExitCode;

/// Where the full manual prose lives (the reference bundles it; the standalone
/// build does not). Same home the `explain` catalogue's doc URLs anchor under.
const MANUAL_HOME: &str = "https://github.com/rigortype/rigor/blob/main/docs/manual/";

/// `rigor docs [<rule-id>]` — list documented rules, or print one rule's doc.
/// Exit 0 on success, 64 on an unknown rule or usage error.
pub fn cmd_docs(args: &[String]) -> ExitCode {
    let mut token: Option<&str> = None;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" | "help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("rigor docs: unsupported flag `{other}`");
                eprintln!("(the standalone build documents rules only — see usage with --help)");
                return ExitCode::from(64);
            }
            other => {
                if token.is_some() {
                    eprintln!("rigor docs: unexpected argument `{other}`");
                    return ExitCode::from(64);
                }
                token = Some(other);
            }
        }
    }

    match token {
        None => {
            render_index();
            ExitCode::SUCCESS
        }
        Some(tok) => {
            if crate::explain::render_rule_doc(tok) {
                ExitCode::SUCCESS
            } else {
                eprintln!("Unknown doc: {tok}");
                eprintln!("rigor docs documents rules — run `rigor docs` to list them.");
                ExitCode::from(64)
            }
        }
    }
}

/// The no-argument listing: the documented rules (id + one-line summary), framed
/// with a note that the full manual prose is web-only in the standalone build.
fn render_index() {
    println!("Rigor — offline rule documentation (rigor-rs standalone build)");
    println!();
    println!("This build documents the rules it implements. Print one with");
    println!("`rigor docs <rule-id>` (canonical id, legacy alias, or family token).");
    println!();
    println!("Documented rules:");
    println!();
    for (id, summary) in crate::explain::catalogue_index() {
        // Same column width as `explain`'s index for a familiar look.
        println!("  {id:<33} {summary}");
    }
    println!();
    println!("The full user manual / handbook prose is not bundled in the standalone");
    println!("build; read it online at {MANUAL_HOME}.");
}

fn print_usage() {
    println!("Usage: rigor docs [<rule-id>]");
    println!();
    println!("  rigor docs                Print the documented-rule index");
    println!("  rigor docs <rule-id>      Print that rule's documentation");
    println!();
    println!("`<rule-id>` accepts a canonical id (`flow.dead-assignment`), a legacy");
    println!("alias (`dead-assignment`), or a family token (`flow`, `call`, …).");
    println!();
    println!("Note: the standalone build documents rules only. The full manual /");
    println!("handbook prose the reference bundles is web-only — see {MANUAL_HOME}.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_lists_the_implemented_rules() {
        // The docs index reuses the explain catalogue, so it must list the same
        // canonical rule ids.
        let ids: Vec<&str> = crate::explain::catalogue_index()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert!(ids.contains(&"flow.dead-assignment"));
        assert!(ids.contains(&"call.undefined-method"));
        // Catalogue is non-trivial.
        assert!(ids.len() >= 7);
    }

    #[test]
    fn known_rule_renders() {
        // A canonical id, a legacy alias, and a family token all resolve.
        assert!(crate::explain::render_rule_doc("flow.dead-assignment"));
        assert!(crate::explain::render_rule_doc("dead-assignment"));
        assert!(crate::explain::render_rule_doc("flow"));
    }

    #[test]
    fn unknown_rule_does_not_render() {
        assert!(!crate::explain::render_rule_doc("bogus.not-a-rule"));
    }

    #[test]
    fn cmd_unknown_exits_64() {
        let code = cmd_docs(&["bogus".to_string()]);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(64)));
    }

    #[test]
    fn cmd_index_and_known_rule_exit_0() {
        assert_eq!(
            format!("{:?}", cmd_docs(&[])),
            format!("{:?}", ExitCode::SUCCESS)
        );
        assert_eq!(
            format!("{:?}", cmd_docs(&["flow.dead-assignment".to_string()])),
            format!("{:?}", ExitCode::SUCCESS)
        );
    }
}
