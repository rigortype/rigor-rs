//! `rigor diff <baseline.json> [paths...]` — a faithful port of the reference's
//! `DiffCommand` (`lib/rigor/cli/diff_command.rb`).
//!
//! Compares the current `rigor check` diagnostics against a saved baseline JSON
//! (the output of a previous `rigor check --format=json` run) and prints the
//! delta:
//!
//! - **new** — diagnostics in the current run not in the baseline (typically a
//!   regression introduced in this PR).
//! - **fixed** — diagnostics in the baseline that no longer appear (progress).
//!
//! Matching identity is the tuple `(path, line, column, rule, source_family,
//! message)`; an edit that moves a diagnostic to a new line surfaces as one
//! fixed + one new pair. Exit code is `1` when any new diagnostic appears, `0`
//! otherwise — so a PR that adds errors fails CI while legacy errors recorded in
//! the baseline do not.
//!
//! This is a lighter-weight sibling of the ADR-22 baseline system: `diff` needs
//! no `.rigor-baseline.yml` bookkeeping — just two JSON snapshots — and is the
//! natural fit for a "compare against `main`'s output" CI gate.
//!
//! The current run is analyzed in the Ruby-free SOUND SUBSET (no sidecar folder):
//! per ADR-0037 the diagnostic set is identical full-vs-subset on every measured
//! corpus (folds fire only on rare pinned literals), so `diff` stays a
//! deterministic, dependency-free delta tool rather than engaging the sidecar /
//! exit-69 machinery that a default `check` would.

use std::path::Path;
use std::process::ExitCode;

use rigor_rules::{catalog, Diagnostic};
use serde_json::{json, Value};

const USAGE: &str = "Usage: rigor diff [options] <baseline.json> [paths...]";

/// The identity fields (reference `KEY_FIELDS`). Two diagnostics are "the same"
/// iff these six field values match.
const KEY_FIELDS: [&str; 6] =
    ["path", "line", "column", "rule", "source_family", "message"];

/// `rigor diff [--format text|json] [--current PATH] [--config PATH]
/// <baseline.json> [paths...]`.
pub fn cmd_diff(args: &[String]) -> ExitCode {
    let mut format = Format::Text;
    let mut current_path: Option<&str> = None;
    let mut explicit_config: Option<&str> = None;
    let mut positionals: Vec<&str> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some("text") => format = Format::Text,
                Some("json") => format = Format::Json,
                other => {
                    eprintln!("rigor diff: --format expects `text` or `json`, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            "--current" => match it.next() {
                Some(p) => current_path = Some(p),
                None => {
                    eprintln!("rigor diff: --current expects a path");
                    return ExitCode::from(64);
                }
            },
            "--config" => match it.next() {
                Some(p) => explicit_config = Some(p),
                None => {
                    eprintln!("rigor diff: --config expects a path");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--format=") => {
                match &other["--format=".len()..] {
                    "text" => format = Format::Text,
                    "json" => format = Format::Json,
                    v => {
                        eprintln!("rigor diff: --format expects `text` or `json`, got {v:?}");
                        return ExitCode::from(64);
                    }
                }
            }
            other if other.starts_with("--current=") => {
                current_path = Some(&other["--current=".len()..]);
            }
            other if other.starts_with("--config=") => {
                explicit_config = Some(&other["--config=".len()..]);
            }
            other => positionals.push(other),
        }
    }

    let Some((baseline_path, paths)) = positionals.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };

    let Some(baseline) = load_diagnostics(baseline_path) else {
        return ExitCode::from(64);
    };
    let current = match current_path {
        Some(p) => match load_diagnostics(p) {
            Some(c) => c,
            None => return ExitCode::from(64),
        },
        None => run_current(explicit_config, paths),
    };

    let diff = compute_diff(&baseline, &current);
    match format {
        Format::Text => write_diff_text(&diff, baseline_path, baseline.len(), current.len()),
        Format::Json => write_diff_json(&diff, baseline_path, baseline.len(), current.len()),
    }

    if diff.new.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[derive(Clone, Copy)]
enum Format {
    Text,
    Json,
}

/// The delta: diagnostics gained (`new`) and resolved (`fixed`), each a full JSON
/// diagnostic object (so `--format json` echoes every field, not just identity).
struct Diff {
    new: Vec<Value>,
    fixed: Vec<Value>,
}

/// Load a diagnostics array from a JSON file. Accepts both shapes a `rigor check
/// --format json` run can produce across tools: a bare array (rigor-rs) or an
/// object carrying a `"diagnostics"` array (the reference). Returns `None` (after
/// a stderr message) on a missing file or invalid JSON — the caller exits 64.
fn load_diagnostics(path: &str) -> Option<Vec<Value>> {
    if !Path::new(path).is_file() {
        eprintln!("Baseline file not found: {path}");
        return None;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Cannot read {path}: {e}");
            return None;
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Some(
            map.get("diagnostics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        ),
        Ok(Value::Array(arr)) => Some(arr),
        Ok(_) => Some(Vec::new()),
        Err(e) => {
            eprintln!("Invalid JSON in {path}: {e}");
            None
        }
    }
}

/// Run `rigor check` over the paths (or the config `paths:` when none given) and
/// return the diagnostics as JSON objects field-identical to `check --format
/// json`, so a rigor-rs-produced baseline matches by identity.
fn run_current(explicit_config: Option<&str>, paths: &[&str]) -> Vec<Value> {
    let cfg = crate::Config::load(explicit_config.map(Path::new));

    let config_paths: Vec<&str>;
    let roots: &[&str] = if paths.is_empty() {
        config_paths = cfg.paths.iter().map(String::as_str).collect();
        &config_paths
    } else {
        paths
    };

    let (expanded_owned, _path_errors) = crate::expand_check_paths(roots);
    let expanded: Vec<&str> = expanded_owned.iter().map(String::as_str).collect();
    // Sound subset (folder = None): diagnostic-identical to full fidelity per
    // ADR-0037, and keeps `diff` Ruby-free / hard-error-free.
    let (findings, _io) = crate::analyze_files(&expanded, &cfg, "diff", None);

    findings
        .iter()
        .map(|(_order, path, source, diag)| diagnostic_value(path, source, diag))
        .collect()
}

/// Build one diagnostic JSON object with the same fields (and omit-when-none
/// rules) as `crate::print_json`, so identity + full-object echo are stable
/// against a rigor-rs `check --format json` baseline.
fn diagnostic_value(path: &str, source: &str, diag: &Diagnostic) -> Value {
    let (line, col) = crate::line_col(source, diag.start_offset);
    let mut map = serde_json::Map::new();
    map.insert("path".into(), json!(path));
    map.insert("line".into(), json!(line));
    map.insert("column".into(), json!(col));
    map.insert("severity".into(), json!(diag.severity.as_str()));
    map.insert("rule".into(), json!(diag.rule_id));
    map.insert("source_family".into(), json!(diag.source_family));
    map.insert("message".into(), json!(diag.message));
    if let Some(rt) = &diag.receiver_type {
        map.insert("receiver_type".into(), json!(rt));
    }
    if let Some(mn) = &diag.method_name {
        map.insert("method_name".into(), json!(mn));
    }
    if let Some(entry) = catalog(diag.rule_id) {
        map.insert("evidence_tier".into(), json!(entry.evidence_tier));
        map.insert("documentation_url".into(), json!(entry.documentation_url));
    }
    Value::Object(map)
}

fn compute_diff(baseline: &[Value], current: &[Value]) -> Diff {
    let baseline_keys: std::collections::HashSet<_> =
        baseline.iter().map(identity_for).collect();
    let current_keys: std::collections::HashSet<_> =
        current.iter().map(identity_for).collect();

    let new = current
        .iter()
        .filter(|d| !baseline_keys.contains(&identity_for(d)))
        .cloned()
        .collect();
    let fixed = baseline
        .iter()
        .filter(|d| !current_keys.contains(&identity_for(d)))
        .cloned()
        .collect();
    Diff { new, fixed }
}

/// The identity tuple for a diagnostic — the six [`KEY_FIELDS`], each rendered to
/// its canonical JSON text (an absent field ⇒ `null`), so numbers and strings
/// compare by value regardless of source shape.
fn identity_for(diagnostic: &Value) -> Vec<String> {
    KEY_FIELDS
        .iter()
        .map(|k| diagnostic.get(*k).unwrap_or(&Value::Null).to_string())
        .collect()
}

fn write_diff_text(diff: &Diff, baseline_path: &str, baseline_count: usize, current_count: usize) {
    println!(
        "# diff against {baseline_path} ({baseline_count} baseline / {current_count} current)"
    );
    for d in &diff.new {
        println!("+ NEW   {}", render_diagnostic(d));
    }
    for d in &diff.fixed {
        println!("- FIXED {}", render_diagnostic(d));
    }
    println!();
    println!("{} new, {} fixed", diff.new.len(), diff.fixed.len());
}

fn write_diff_json(diff: &Diff, baseline_path: &str, baseline_count: usize, current_count: usize) {
    let payload = json!({
        "baseline": baseline_path,
        "baseline_count": baseline_count,
        "current_count": current_count,
        "new": diff.new,
        "fixed": diff.fixed,
    });
    // serde_json (no `preserve_order` feature) sorts object keys alphabetically,
    // so both the top-level keys and each echoed diagnostic's keys differ in ORDER
    // from the reference's insertion order (`new`/`fixed`; `path`/`line`/…). The
    // delta is machine-readable and JSON key order is not significant, so this is
    // left as-is rather than flipping the crate-wide `preserve_order` feature
    // (which would perturb the `explain`/`sarif` JSON) or hand-building the tree.
    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
}

/// `path:line:column [qualified_rule] message` (reference `render_diagnostic`).
fn render_diagnostic(d: &Value) -> String {
    let rule = qualified_rule_for(d);
    format!(
        "{}:{}:{} [{}] {}",
        plain(d.get("path")),
        plain(d.get("line")),
        plain(d.get("column")),
        rule,
        plain(d.get("message")),
    )
}

/// The rule id qualified by its `source_family` (reference `qualified_rule_for`):
/// a `builtin` / empty / absent family renders the bare rule, else `family.rule`.
fn qualified_rule_for(d: &Value) -> String {
    let rule = plain(d.get("rule"));
    let family = plain(d.get("source_family"));
    if family.is_empty() || family == "builtin" {
        rule
    } else {
        format!("{family}.{rule}")
    }
}

/// A JSON scalar as plain text: a string unquoted, a number as its digits, and
/// null / absent as the empty string.
fn plain(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(path: &str, line: i64, rule: &str, msg: &str) -> Value {
        json!({
            "path": path, "line": line, "column": 1,
            "severity": "error", "rule": rule,
            "source_family": "builtin", "message": msg,
        })
    }

    #[test]
    fn new_and_fixed_partition() {
        let baseline = vec![
            diag("a.rb", 1, "call.undefined-method", "old"),
            diag("b.rb", 2, "call.wrong-arity", "kept"),
        ];
        let current = vec![
            diag("b.rb", 2, "call.wrong-arity", "kept"),
            diag("c.rb", 3, "call.undefined-method", "fresh"),
        ];
        let diff = compute_diff(&baseline, &current);
        assert_eq!(diff.new.len(), 1);
        assert_eq!(plain(diff.new[0].get("path")), "c.rb");
        assert_eq!(diff.fixed.len(), 1);
        assert_eq!(plain(diff.fixed[0].get("path")), "a.rb");
    }

    #[test]
    fn identical_runs_have_no_delta() {
        let d = vec![diag("a.rb", 1, "call.undefined-method", "x")];
        let diff = compute_diff(&d, &d);
        assert!(diff.new.is_empty());
        assert!(diff.fixed.is_empty());
    }

    #[test]
    fn moved_line_is_one_fixed_and_one_new() {
        // Same diagnostic at a different line ⇒ fixed(old) + new(current).
        let baseline = vec![diag("a.rb", 1, "call.undefined-method", "x")];
        let current = vec![diag("a.rb", 5, "call.undefined-method", "x")];
        let diff = compute_diff(&baseline, &current);
        assert_eq!(diff.new.len(), 1);
        assert_eq!(diff.fixed.len(), 1);
    }

    #[test]
    fn qualified_rule_respects_family() {
        assert_eq!(qualified_rule_for(&diag("a", 1, "call.x", "m")), "call.x");
        let plugin = json!({"rule": "custom", "source_family": "rails"});
        assert_eq!(qualified_rule_for(&plugin), "rails.custom");
        let empty = json!({"rule": "call.x", "source_family": ""});
        assert_eq!(qualified_rule_for(&empty), "call.x");
    }

    #[test]
    fn load_diagnostics_accepts_bare_array_and_object() {
        let dir = std::env::temp_dir().join(format!("rigor_diff_load_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let arr = dir.join("arr.json");
        std::fs::write(&arr, r#"[{"path":"a.rb","line":1}]"#).unwrap();
        assert_eq!(load_diagnostics(arr.to_str().unwrap()).unwrap().len(), 1);

        let obj = dir.join("obj.json");
        std::fs::write(&obj, r#"{"diagnostics":[{"path":"a.rb"},{"path":"b.rb"}]}"#).unwrap();
        assert_eq!(load_diagnostics(obj.to_str().unwrap()).unwrap().len(), 2);

        // Missing file ⇒ None.
        assert!(load_diagnostics(dir.join("nope.json").to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
