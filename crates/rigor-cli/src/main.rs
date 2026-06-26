//! CLI (ADR-0015): present the reference's full command surface; commands not
//! yet implemented in this phase report a clear message with a distinct exit
//! code, never a cryptic "unknown command".
//!
//! The tracer-bullet slice wires `rigor check <file...>` end to end: read ->
//! parse (ADR-0003) -> lower (ADR-0012) -> run rules (ADR-0005/0030) -> print.
//!
//! Per-file panic isolation (ADR-0016): each file's parse+lower+analyze is
//! wrapped in `std::panic::catch_unwind`. A panic skips the file but emits a
//! synthetic `internal-error` diagnostic for it and continues — the run never
//! aborts due to one file's bug or malformed input.
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::process::ExitCode;

use rigor_index::CoreIndex;
use rigor_parse::{lower, parse};
use rigor_rules::{analyze_with_source, catalog, Diagnostic, Severity};
use rigor_types::Interner;

mod config;
use config::Config;

/// The reference's full subcommand surface (ADR-0015).
const COMMANDS: &[&str] = &[
    "check", "annotate", "type-of", "trace", "type-scan", "explain", "diff",
    "sig-gen", "baseline", "triage", "coverage", "plugins", "plugin", "lsp",
    "mcp", "skill", "docs", "init",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => cmd_check(&args[1..]),
        Some(cmd) if COMMANDS.contains(&cmd) => {
            eprintln!("rigor-rs: `{cmd}` is recognized but not yet implemented in this phase");
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("rigor-rs: unknown command `{other}`");
            ExitCode::from(2)
        }
        None => {
            eprintln!("rigor-rs (pre-alpha). usage: rigor <command>");
            eprintln!("commands: {}", COMMANDS.join(", "));
            ExitCode::from(2)
        }
    }
}

/// `rigor check [--format text|json] <file...>` — analyze each file and print
/// its diagnostics. Exit 1 if any diagnostic is found, 0 if none, 64 on a usage
/// error (ADR-0030 exit codes).
fn cmd_check(args: &[String]) -> ExitCode {
    let mut format = OutputFormat::Text;
    let mut files: Vec<&str> = Vec::new();
    let mut explicit_config: Option<&str> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some("text") => format = OutputFormat::Text,
                Some("json") => format = OutputFormat::Json,
                Some("github") => format = OutputFormat::Github,
                Some("sarif") => format = OutputFormat::Sarif,
                other => {
                    eprintln!(
                        "rigor check: --format expects `text`, `json`, `github`, or `sarif`, got {other:?}"
                    );
                    return ExitCode::from(64);
                }
            },
            "--config" => match it.next() {
                Some(path) => explicit_config = Some(path),
                None => {
                    eprintln!("rigor check: --config expects a path");
                    return ExitCode::from(64);
                }
            },
            other => files.push(other),
        }
    }

    if files.is_empty() {
        eprintln!("rigor check: expected at least one file");
        return ExitCode::from(64);
    }

    // Load `.rigor.yml` (explicit `--config` path, else cwd auto-discovery).
    // Config ONLY suppresses/scopes diagnostics; it never changes analysis.
    // Degrades to default (= inert) on any error, so the differential harness —
    // which runs from a directory with no `.rigor.yml` — is unaffected.
    let cfg = Config::load(explicit_config.map(Path::new));
    let disable_matcher = cfg.disable_matcher();

    let index = CoreIndex::new();
    // Each entry: (input_order_key, path, source_or_empty, diagnostic).
    // The order key is the file's index in `files`, used to keep diagnostics
    // grouped per file in INPUT order even though the project pass emits
    // parse/lower-panic diagnostics (stage 1) before analyze diagnostics (stage
    // 3). `source` is empty string for internal-error diagnostics (no source to
    // compute line/col from — the offset is 0).
    let mut findings: Vec<(usize, String, String, Diagnostic)> = Vec::new();
    let mut had_io_error = false;

    // PROJECT PASS (ADR-0023 cross-file): the bare-constant singleton gate must
    // know every class the PROJECT defines (across ALL files) so a model
    // referenced where it is not defined (`Group.where(...)`) is never
    // singleton-typed and stays silent. So we parse+lower EVERY file first,
    // collect their owned ASTs, build ONE project-wide SourceIndex, then analyze
    // each file against it.
    //
    // Per-file panic isolation (ADR-0016) is preserved at BOTH stages: a file
    // that panics in parse/lower is dropped from the project set with a synthetic
    // internal-error diagnostic (and never contributes class names); a file that
    // panics in analyze likewise yields the synthetic diagnostic.

    // Stage 1: read + parse + lower every file (panic-isolated). Keep the per-file
    // source text and owned AST for files that lowered cleanly, in input order.
    struct Prepared {
        order: usize,
        path: String,
        source: String,
        ast: rigor_parse::LoweredAst,
        /// The file's comments as `(1-based line, text)`, captured inside the
        /// parse closure (the borrowed `ParseResult` cannot escape it). Drives
        /// in-source `# rigor:disable` suppression in stage 3.
        comments: Vec<(usize, String)>,
    }
    let mut prepared: Vec<Prepared> = Vec::new();

    for (order, path) in files.iter().enumerate() {
        // Config `exclude:` — skip the file entirely before reading/analyzing it
        // (no diagnostics, no internal-error, no project-index contribution).
        if cfg.is_excluded(path) {
            continue;
        }

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rigor check: cannot read {path}: {e}");
                had_io_error = true;
                continue;
            }
        };

        let path_owned = path.to_string();
        let source_bytes = source.as_bytes().to_vec();
        let lowered = panic::catch_unwind(AssertUnwindSafe(|| {
            let result = parse(&source_bytes);
            // Capture comments here: the borrowed `ParseResult` cannot escape
            // this closure, so collect the owned `(line, text)` vec alongside
            // the AST (ADR-0012 lifetime boundary).
            let comments = rigor_parse::comment_lines(&result, &source_bytes);
            (lower(&result), comments)
        }));

        match lowered {
            Ok((ast, comments)) => {
                prepared.push(Prepared { order, path: path_owned, source, ast, comments });
            }
            Err(panic_val) => {
                let msg = panic_message(&panic_val);
                eprintln!("rigor check: internal panic on {path}: {msg}");
                findings.push((order, path_owned, String::new(), internal_error_diag(msg)));
            }
        }
    }

    // Stage 2: build ONE project-wide source index from all cleanly-lowered ASTs.
    let asts: Vec<&rigor_parse::LoweredAst> = prepared.iter().map(|p| &p.ast).collect();
    let project_source = rigor_infer::SourceIndex::build_project(&asts, &index);

    // Stage 3: analyze each file against the shared project source (panic-isolated),
    // emitting diagnostics grouped per file in input order.
    for p in &prepared {
        let Prepared { order, path, source, ast, comments } = p;
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let mut interner = Interner::new();
            analyze_with_source(ast, &mut interner, &index, &project_source)
        }));

        match result {
            Ok(diags) => {
                // Apply in-source `# rigor:disable` / `-file` suppression per file
                // (line numbers are per file). Pair each diagnostic with its
                // 1-based line, filter, then push the survivors.
                let with_lines: Vec<(usize, Diagnostic)> = diags
                    .into_iter()
                    .map(|diag| (line_col(source, diag.start_offset).0, diag))
                    .collect();
                for (_line, diag) in rigor_rules::filter_suppressed(with_lines, comments) {
                    // Config `disable:` — drop diagnostics whose rule matches the
                    // expanded disable set (composes with inline suppression; the
                    // internal-error sentinel is never matched by it).
                    if disable_matcher.suppresses(diag.rule_id) {
                        continue;
                    }
                    findings.push((*order, path.clone(), source.clone(), diag));
                }
            }
            Err(panic_val) => {
                let msg = panic_message(&panic_val);
                eprintln!("rigor check: internal panic on {path}: {msg}");
                findings.push((*order, path.clone(), String::new(), internal_error_diag(msg)));
            }
        }
    }

    // Restore input order: stage 1 (parse/lower panics) and stage 3 (analyze)
    // push interleaved, so stable-sort by the file's input-order key to recover
    // the per-file grouping in input order. Stable keeps a file's own diagnostics
    // in source order.
    findings.sort_by_key(|(order, _, _, _)| *order);

    match format {
        OutputFormat::Text => print_text(&findings),
        OutputFormat::Json => print_json(&findings),
        OutputFormat::Github => print_github(&findings),
        OutputFormat::Sarif => print_sarif(&findings),
    }

    if had_io_error {
        ExitCode::from(1)
    } else if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

// ---------------------------------------------------------------------------
// Output formatters
// ---------------------------------------------------------------------------

/// Human format: `path:line:col: <severity>: <message>` (1-based line/col).
/// Severity is rendered from the diagnostic's actual severity field — not
/// hardcoded — so warning/info diagnostics render correctly.
fn print_text(findings: &[(usize, String, String, Diagnostic)]) {
    for (_order, path, source, diag) in findings {
        let (line, col) = line_col(source, diag.start_offset);
        println!("{path}:{line}:{col}: {}: {}", diag.severity.as_str(), diag.message);
    }
}

/// JSON format: a flat array of objects matching the reference's field
/// set and order (ADR-0030):
///
///   path, line, column, severity, rule, source_family, message,
///   [receiver_type,] [method_name,]          ← omit-when-nil
///   [evidence_tier,] [documentation_url]     ← from RuleCatalog, omit unknown
///
/// `path/line/column/rule` are always present; the harness reads these.
/// Hand-rolled (no serde dependency) — the field set is small and fixed.
fn print_json(findings: &[(usize, String, String, Diagnostic)]) {
    let mut buf = String::from("[");
    for (idx, (_order, path, source, diag)) in findings.iter().enumerate() {
        if idx > 0 {
            buf.push(',');
        }
        let (line, col) = line_col(source, diag.start_offset);

        buf.push('{');
        // Mandatory fields — always present.
        push_kv_str(&mut buf, "path", path, true);
        push_kv_num(&mut buf, "line", line);
        push_kv_num(&mut buf, "column", col);
        push_kv_str(&mut buf, "severity", diag.severity.as_str(), false);
        push_kv_str(&mut buf, "rule", diag.rule_id, false);
        push_kv_str(&mut buf, "source_family", diag.source_family, false);
        push_kv_str(&mut buf, "message", &diag.message, false);

        // Optional call-dispatch fields — omit when None.
        if let Some(rt) = &diag.receiver_type {
            push_kv_str(&mut buf, "receiver_type", rt, false);
        }
        if let Some(mn) = &diag.method_name {
            push_kv_str(&mut buf, "method_name", mn, false);
        }

        // Per-rule catalogue fields — omit for unknown rules (e.g. internal-error).
        if let Some(entry) = catalog(diag.rule_id) {
            push_kv_str(&mut buf, "evidence_tier", entry.evidence_tier, false);
            push_kv_str(&mut buf, "documentation_url", entry.documentation_url, false);
        }

        buf.push('}');
    }
    buf.push(']');
    println!("{buf}");
}

/// GitHub Actions workflow-command format: one annotation line per diagnostic,
/// `::{level} file={path},line={line},col={col}::{message}`. Levels map
/// Error→error, Warning→warning, Info→notice. The message body is escaped per
/// GitHub's annotation rules (`%`/`\r`/`\n`); property values additionally
/// escape `,`/`:`. Emits nothing when there are no diagnostics. ADDITIVE — does
/// not touch text/json.
fn print_github(findings: &[(usize, String, String, Diagnostic)]) {
    for (_order, path, source, diag) in findings {
        let (line, col) = line_col(source, diag.start_offset);
        let level = match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "notice",
        };
        println!(
            "::{level} file={},line={line},col={col}::{}",
            gh_escape_prop(path),
            gh_escape_data(&diag.message),
        );
    }
}

/// Escape an annotation message body (the part after `::`): `%`→`%25`, then
/// `\r`→`%0D`, `\n`→`%0A` so a multi-line message stays on one annotation line.
fn gh_escape_data(s: &str) -> String {
    s.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

/// Escape an annotation property value (`file`/`line`/`col`): the data escapes
/// plus `,`→`%2C` and `:`→`%3A` so a value containing them can't break the
/// `key=value,key=value` structure.
fn gh_escape_prop(s: &str) -> String {
    gh_escape_data(s).replace(',', "%2C").replace(':', "%3A")
}

/// SARIF 2.1.0 format: a single SARIF log object with one `result` per
/// diagnostic and a deduped `rules` list (first-appearance order). Severity maps
/// Error→"error", Warning→"warning", Info→"note". Always emits the full object,
/// even with zero results. Built as a `serde_json::Value` and pretty-printed.
/// ADDITIVE — does not touch text/json.
fn print_sarif(findings: &[(usize, String, String, Diagnostic)]) {
    use serde_json::{json, Value};

    let mut rules: Vec<Value> = Vec::new();
    let mut seen_rules: Vec<&str> = Vec::new();
    let mut results: Vec<Value> = Vec::new();

    for (_order, path, source, diag) in findings {
        if !seen_rules.contains(&diag.rule_id) {
            seen_rules.push(diag.rule_id);
            rules.push(json!({ "id": diag.rule_id }));
        }

        let (line, col) = line_col(source, diag.start_offset);
        let level = match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "note",
        };

        results.push(json!({
            "ruleId": diag.rule_id,
            "level": level,
            "message": { "text": diag.message },
            "locations": [{
                "physicalLocation": {
                    "artifactLocation": { "uri": path },
                    "region": { "startLine": line, "startColumn": col }
                }
            }]
        }));
    }

    let log = json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "rigor-rs",
                    "informationUri": "https://github.com/rigortype/rigor",
                    "rules": rules
                }
            },
            "results": results
        }]
    });

    println!("{}", serde_json::to_string_pretty(&log).unwrap());
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

/// Push `,"key":value_str` (or just `"key":value_str` when `first`).
/// `raw_number`: caller controls whether to quote the value.
fn push_kv_str(buf: &mut String, key: &str, value: &str, first: bool) {
    if !first {
        buf.push(',');
    }
    buf.push_str(&json_string(key));
    buf.push(':');
    buf.push_str(&json_string(value));
}

fn push_kv_num(buf: &mut String, key: &str, value: usize) {
    buf.push(',');
    buf.push_str(&json_string(key));
    buf.push(':');
    buf.push_str(&value.to_string());
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Compute 1-based (line, column) from a UTF-8 byte offset into `source`.
/// Columns are counted in Unicode scalar values for the line, which matches the
/// reference's column semantics closely enough for the tracer bullet.
// TODO(spec): align column counting with the reference's exact unit (it adds 1
// to Prism's 0-based byte/char column) once a parity fixture pins it.
fn line_col(source: &str, byte_offset: usize) -> (usize, usize) {
    if source.is_empty() {
        return (1, 1);
    }
    let clamped = byte_offset.min(source.len());
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (i, b) in source.as_bytes().iter().enumerate() {
        if i >= clamped {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    // Column = char count between the line start and the offset, plus 1.
    let col = source[line_start..clamped].chars().count() + 1;
    (line, col)
}

/// Build the synthetic `internal-error` diagnostic emitted when a file panics
/// during parse/lower/analyze (ADR-0016 never-crash). `:info`, never `:error`:
/// it is a rigor-rs-specific out-of-band signal with no reference counterpart, so
/// info-severity excludes it from the differential harness's error/warning parity
/// gate — a crashed file never counts as a false positive.
fn internal_error_diag(msg: String) -> Diagnostic {
    Diagnostic {
        rule_id: "internal-error",
        start_offset: 0,
        end_offset: 0,
        message: format!("internal error while analysing file: {msg}"),
        severity: Severity::Info,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    }
}

/// Extract a human-readable description from a panic payload.
fn panic_message(payload: &dyn std::any::Any) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Minimal JSON string escaper for the small, ASCII-ish strings we emit.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Output format for `rigor check` (ADR-0014; text default, json nice-to-have).
/// `Github`/`Sarif` are ADDITIVE CI-oriented formats — they do not affect the
/// text/json output the differential harness depends on.
enum OutputFormat {
    Text,
    Json,
    Github,
    Sarif,
}

// ---------------------------------------------------------------------------
// Tests for the additive CI output formats (github / sarif).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// A `Diagnostic` with the given severity/message; offset 0 so line/col are
    /// 1/1 for an empty source, or computed from a provided source.
    fn diag(rule_id: &'static str, severity: Severity, message: &str) -> Diagnostic {
        Diagnostic {
            rule_id,
            start_offset: 0,
            end_offset: 0,
            message: message.to_string(),
            severity,
            source_family: "builtin",
            receiver_type: None,
            method_name: None,
        }
    }

    /// Build the single github annotation line for one diagnostic (mirrors what
    /// `print_github` prints), so we can assert the exact string incl. escaping.
    fn gh_line(path: &str, source: &str, d: &Diagnostic) -> String {
        let (line, col) = line_col(source, d.start_offset);
        let level = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "notice",
        };
        format!(
            "::{level} file={},line={line},col={col}::{}",
            gh_escape_prop(path),
            gh_escape_data(&d.message),
        )
    }

    #[test]
    fn github_error_line() {
        let d = diag("call.undefined-method", Severity::Error, "undefined method `lenght'");
        assert_eq!(
            gh_line("app.rb", "", &d),
            "::error file=app.rb,line=1,col=1::undefined method `lenght'"
        );
    }

    #[test]
    fn github_warning_line() {
        let d = diag("some.rule", Severity::Warning, "watch out");
        assert_eq!(gh_line("a.rb", "", &d), "::warning file=a.rb,line=1,col=1::watch out");
    }

    #[test]
    fn github_info_is_notice() {
        let d = diag("internal-error", Severity::Info, "fyi");
        assert_eq!(gh_line("a.rb", "", &d), "::notice file=a.rb,line=1,col=1::fyi");
    }

    #[test]
    fn github_message_escaping() {
        // `%` -> %25 (done first), newline -> %0A, CR -> %0D; commas/colons in the
        // message body are NOT escaped (only property values escape those).
        let d = diag("r", Severity::Error, "100% off\nline two\r, a:b");
        assert_eq!(
            gh_line("p.rb", "", &d),
            "::error file=p.rb,line=1,col=1::100%25 off%0Aline two%0D, a:b"
        );
    }

    #[test]
    fn github_property_escaping() {
        // A path with a comma/colon must be escaped in the property value.
        let d = diag("r", Severity::Error, "msg");
        assert_eq!(
            gh_line("a,b:c.rb", "", &d),
            "::error file=a%2Cb%3Ac.rb,line=1,col=1::msg"
        );
    }

    #[test]
    fn github_line_col_from_source() {
        // `s.lenght` on line 2: offset of the `l` in lenght.
        let src = "s = \"x\"\ns.lenght\n";
        let off = src.find("lenght").unwrap();
        let d = Diagnostic { start_offset: off, ..diag("r", Severity::Error, "m") };
        assert_eq!(gh_line("f.rb", src, &d), "::error file=f.rb,line=2,col=3::m");
    }

    /// Capture github output for a slice of findings without spawning a process.
    fn github_all(findings: &[(usize, String, String, Diagnostic)]) -> String {
        findings
            .iter()
            .map(|(_o, p, s, d)| gh_line(p, s, d))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn github_empty_when_no_diagnostics() {
        assert_eq!(github_all(&[]), "");
    }

    /// Build the SARIF value tree the same way `print_sarif` does, so we can
    /// assert on the parsed structure.
    fn sarif_value(findings: &[(usize, String, String, Diagnostic)]) -> serde_json::Value {
        use serde_json::{json, Value};
        let mut rules: Vec<Value> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        let mut results: Vec<Value> = Vec::new();
        for (_o, path, source, d) in findings {
            if !seen.contains(&d.rule_id) {
                seen.push(d.rule_id);
                rules.push(json!({ "id": d.rule_id }));
            }
            let (line, col) = line_col(source, d.start_offset);
            let level = match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "note",
            };
            results.push(json!({
                "ruleId": d.rule_id,
                "level": level,
                "message": { "text": d.message },
                "locations": [{ "physicalLocation": {
                    "artifactLocation": { "uri": path },
                    "region": { "startLine": line, "startColumn": col }
                }}]
            }));
        }
        json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "runs": [{
                "tool": { "driver": {
                    "name": "rigor-rs",
                    "informationUri": "https://github.com/rigortype/rigor",
                    "rules": rules
                }},
                "results": results
            }]
        })
    }

    fn finding(rule: &'static str, sev: Severity, msg: &str) -> (usize, String, String, Diagnostic) {
        (0, "f.rb".to_string(), String::new(), diag(rule, sev, msg))
    }

    #[test]
    fn sarif_structure_and_levels() {
        let findings = vec![
            finding("call.undefined-method", Severity::Error, "e1"),
            finding("some.warn", Severity::Warning, "w1"),
            finding("internal-error", Severity::Info, "i1"),
            // duplicate rule id — must not produce a second rules entry.
            finding("call.undefined-method", Severity::Error, "e2"),
        ];
        let v = sarif_value(&findings);

        // Round-trips through serde_json (it already is a Value, but assert the
        // pretty string re-parses, mirroring the real output path).
        let pretty = serde_json::to_string_pretty(&v).unwrap();
        let v: serde_json::Value = serde_json::from_str(&pretty).unwrap();

        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["$schema"], "https://json.schemastore.org/sarif-2.1.0.json");

        let results = v["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 4, "one result per diagnostic");
        assert_eq!(results[0]["level"], "error");
        assert_eq!(results[1]["level"], "warning");
        assert_eq!(results[2]["level"], "note"); // Info -> note
        assert_eq!(results[0]["ruleId"], "call.undefined-method");
        assert_eq!(results[0]["message"]["text"], "e1");
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "f.rb"
        );

        // Deduped rules, first-appearance order.
        let ids: Vec<&str> = v["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["call.undefined-method", "some.warn", "internal-error"]);
        assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "rigor-rs");
    }

    #[test]
    fn sarif_empty_still_valid() {
        let v = sarif_value(&[]);
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
        assert_eq!(v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap().len(), 0);
    }
}
