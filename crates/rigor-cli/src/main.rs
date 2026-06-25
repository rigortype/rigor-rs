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
use std::process::ExitCode;

use rigor_index::CoreIndex;
use rigor_parse::{lower, parse};
use rigor_rules::{analyze, catalog, Diagnostic, Severity};
use rigor_types::Interner;

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

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some("text") => format = OutputFormat::Text,
                Some("json") => format = OutputFormat::Json,
                other => {
                    eprintln!("rigor check: --format expects `text` or `json`, got {other:?}");
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

    let index = CoreIndex::new();
    // Each entry: (path, source_or_empty, diagnostic).
    // `source` is empty string for internal-error diagnostics (no source to
    // compute line/col from — the offset is 0).
    let mut findings: Vec<(String, String, Diagnostic)> = Vec::new();
    let mut had_io_error = false;

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rigor check: cannot read {path}: {e}");
                had_io_error = true;
                continue;
            }
        };

        // ADR-0016 never-crash: wrap parse+lower+analyze in catch_unwind.
        // On a panic we emit a synthetic internal-error diagnostic and continue.
        // `AssertUnwindSafe` is sound here: we discard all borrowed state from
        // the closure on unwind; `index` and `source` are not mutated inside.
        let path_owned = path.to_string();
        let source_bytes = source.as_bytes().to_vec();
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let result = parse(&source_bytes);
            let ast = lower(&result);
            let mut interner = Interner::new();
            analyze(&ast, &mut interner, &index)
        }));

        match result {
            Ok(diags) => {
                for diag in diags {
                    findings.push((path_owned.clone(), source.clone(), diag));
                }
            }
            Err(panic_val) => {
                // Convert the panic into a synthetic internal-error diagnostic
                // so the run continues and the caller can see which file failed.
                let msg = panic_message(&panic_val);
                eprintln!("rigor check: internal panic on {path}: {msg}");
                findings.push((
                    path_owned.clone(),
                    String::new(), // no source — offset 0 maps to line 1, col 1
                    Diagnostic {
                        rule_id: "internal-error",
                        start_offset: 0,
                        end_offset: 0,
                        message: format!("internal error while analysing file: {msg}"),
                        severity: Severity::Error,
                        source_family: "builtin",
                        receiver_type: None,
                        method_name: None,
                    },
                ));
            }
        }
    }

    match format {
        OutputFormat::Text => print_text(&findings),
        OutputFormat::Json => print_json(&findings),
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
fn print_text(findings: &[(String, String, Diagnostic)]) {
    for (path, source, diag) in findings {
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
fn print_json(findings: &[(String, String, Diagnostic)]) {
    let mut buf = String::from("[");
    for (idx, (path, source, diag)) in findings.iter().enumerate() {
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
enum OutputFormat {
    Text,
    Json,
}
