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
//! - unknown id → `name_error` like the reference: "Unknown doc: <name>" plus the
//!   documented-rule list on stderr, exit 1 (the reference prints its manual /
//!   handbook list; this build prints the rule catalogue it actually serves).
//!
//! **Deferred** (no bundled prose corpus in the standalone build): the reference's
//! manual / handbook / install pages, the `llms.txt` index, and the
//! `--list` / `--path` flags that address those files. `docs` prints a one-line
//! note pointing at the web manual for that material.

use std::process::ExitCode;

/// Where the full manual prose lives (the reference bundles it; the standalone
/// build does not). Same home the `explain` catalogue's doc URLs anchor under —
/// the published docs site, which carries no git ref (see
/// `explain::DOCUMENTATION_BASE`; the old `blob/main` path 404ed on every
/// emission because the rigor repository has never had a `main` branch).
const MANUAL_HOME: &str = "https://rigor.typedduck.fail/manual/";

/// `rigor docs [<rule-id>]` — list documented rules, or print one rule's doc.
///
/// Mirrors the reference's manual dispatch (`DocsCommand#run`): the positional
/// slot is always a doc *name*, so an unrecognised first argument — including a
/// dashed one like `--bogus` — resolves as a name and hits `name_error`
/// ("Unknown doc: …" + the doc list, exit 1), never a usage error. Only the
/// grammar words `-h`/`--help`/`help`, `--list`, `--path` and `--print` are
/// special. Exit 0 on success, 1 on an unknown doc name, 64 on a usage error.
pub fn cmd_docs(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None => {
            render_index();
            ExitCode::SUCCESS
        }
        Some("-h" | "--help" | "help") => {
            print_usage();
            ExitCode::SUCCESS
        }
        // The reference's `--list` prints the bundled-docs table; this build's
        // doc corpus is the rule catalogue, so the index is the analogue.
        Some("--list") => {
            render_index();
            ExitCode::SUCCESS
        }
        // `--path` prints a bundled file's path — the standalone build ships no
        // prose files, so the flag is named explicitly rather than silently
        // resolving to a rule.
        Some("--path") => {
            eprintln!("rigor docs: --path is not supported by rigor-rs");
            eprintln!("(the standalone build documents rules only — see usage with --help)");
            ExitCode::from(64)
        }
        Some("--print") => print_named(args.get(1).map(String::as_str)),
        Some(name) => print_named(Some(name)),
    }
}

/// `run_print(name)`: nil is a usage error (64); an unresolvable name is
/// `name_error` — "Unknown doc: …" + the available list on stderr, exit 1.
fn print_named(name: Option<&str>) -> ExitCode {
    match name {
        None => {
            eprintln!("a doc name is required");
            print_usage_stderr();
            ExitCode::from(64)
        }
        Some(tok) => {
            if crate::explain::render_rule_doc(tok) {
                ExitCode::SUCCESS
            } else {
                eprintln!("Unknown doc: {tok}");
                eprintln!("Available docs (try `rigor docs --list`):");
                for (id, _) in crate::explain::catalogue_index() {
                    eprintln!("  {id}");
                }
                ExitCode::from(1)
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
    println!("Usage: rigor docs [<rule-id>] [--list] [--print <rule-id>]");
    println!();
    println!("  rigor docs                Print the documented-rule index");
    println!("  rigor docs <rule-id>      Print that rule's documentation");
    println!("  rigor docs --list         List the documented rules");
    println!("  rigor docs --print <id>   Print that rule's documentation");
    println!();
    println!("`<rule-id>` accepts a canonical id (`flow.dead-assignment`), a legacy");
    println!("alias (`dead-assignment`), or a family token (`flow`, `call`, …).");
    println!();
    println!("Note: the standalone build documents rules only. The full manual /");
    println!("handbook prose the reference bundles is web-only — see {MANUAL_HOME}.");
}

/// `usage_error` writes to stderr in the reference.
fn print_usage_stderr() {
    eprintln!("Usage: rigor docs [<rule-id>] [--list] [--print <rule-id>]");
    eprintln!("(the standalone build documents rules only — see `rigor docs --help`)");
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
    fn cmd_unknown_exits_1_like_name_error() {
        // `name_error` in the reference returns 1 — even for a dashed token,
        // which the grammar resolves as a doc name, never a flag.
        for argv in ["bogus", "--bogus"] {
            let code = cmd_docs(&[argv.to_string()]);
            assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(1)));
        }
    }

    #[test]
    fn cmd_grammar_words() {
        // --list renders the index; extra positionals after a name are ignored
        // (the reference reads only argv.first).
        assert_eq!(
            format!("{:?}", cmd_docs(&["--list".to_string()])),
            format!("{:?}", ExitCode::SUCCESS)
        );
        assert_eq!(
            format!(
                "{:?}",
                cmd_docs(&["flow.dead-assignment".to_string(), "extra".to_string()])
            ),
            format!("{:?}", ExitCode::SUCCESS)
        );
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
