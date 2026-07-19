//! `rigor mcp` (§12, ADR-0029) — a read-only Model Context Protocol server over
//! stdio, so an AI agent can analyse Ruby with rigor as a tool.
//!
//! Transport: MCP stdio = newline-delimited JSON-RPC 2.0 (one message per line,
//! no embedded newlines) — simpler than LSP's `Content-Length` framing, so this
//! is hand-rolled on `serde_json` (already a dep) with no async runtime and no
//! new dependency, keeping the single binary self-contained + offline.
//!
//! Tools (all READ-ONLY, all operating on source passed in the call — the server
//! never reads or writes the filesystem):
//! - `check`    — analyse Ruby source, return the diagnostics (the `check` path).
//! - `type_of`  — the inferred type of the expression at a 1-based line/column.
//! - `explain`  — look up the rule catalogue.
//! - `outline`  — the structural outline (classes / modules / methods).
//! - `triage`   — the structured diagnostic triage (distribution / selectors /
//!   hotspots / summary / hints) of the source (ADR-23).
//! - `annotate` — rigor's inferred type for every source line (`{ line => type }`).
//!
//! Like the LSP server, the RBS environment + config are built ONCE at startup
//! and reused; every tool call is a single-file parse+lower+analyze. Panic
//! isolation (ADR-0016): a tool that trips the parser returns an `isError` result,
//! never crashing the server.

use std::io::{self, BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

use serde_json::{json, Value};

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, Typer};
use rigor_parse::{comment_lines, lower, parse};
use rigor_rules::{
    analyze_with_source, catalog, filter_suppressed, shadowed_rescue_diagnostics,
    suppression_marker_diagnostics, Diagnostic,
    SuppressSet,
};
use rigor_types::Interner;

use crate::config::Config;

/// The protocol version advertised when the client sends none (a recent stable
/// MCP revision). When the client sends one, the server echoes it back (the most
/// compatible response — the client then decides whether to proceed).
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// `rigor mcp` — run the stdio MCP server. Accepts (and ignores, reserved) a
/// `--transport=stdio` flag for symmetry with `lsp`. Returns exit 0 on a clean
/// EOF shutdown, 1 on an IO error, 64 on a usage error.
pub fn cmd_mcp(args: &[String]) -> ExitCode {
    for arg in args {
        match arg.as_str() {
            "--transport=stdio" | "--transport" | "stdio" => {}
            other => {
                eprintln!("rigor mcp: unexpected argument {other:?} (only stdio transport)");
                return ExitCode::from(64);
            }
        }
    }
    match run_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rigor mcp: {e}");
            ExitCode::from(1)
        }
    }
}

/// The session-stable context: the RBS index + config-derived suppression set,
/// built once at startup and reused for every tool call.
struct ServerContext {
    index: CoreIndex,
    disable: SuppressSet,
}

/// The stdio read/dispatch/respond loop. Reads one JSON-RPC message per line;
/// responds to requests (those with an `id`), silently accepts notifications.
fn run_stdio() -> Result<(), String> {
    let cfg = Config::load(None);
    let ctx = ServerContext {
        index: CoreIndex::for_project(&cfg.plugins, &cfg.all_signature_dirs(std::path::Path::new("."))),
        disable: cfg.disable_matcher(),
    };

    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue, // ignore an unparseable line (robustness).
        };
        // A notification (no `id`) gets no response.
        let Some(id) = msg.get("id").cloned() else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let response = match dispatch(&ctx, method, &params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        };
        let line = serde_json::to_string(&response).map_err(|e| e.to_string())?;
        writeln!(stdout, "{line}").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Dispatch a request method to its handler. `Ok(result)` becomes a JSON-RPC
/// `result`; `Err((code, message))` a JSON-RPC `error` (protocol-level).
fn dispatch(ctx: &ServerContext, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(initialize_result(params)),
        // Handshake completion + keepalive — succeed with an empty result.
        "notifications/initialized" | "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_catalog() })),
        "tools/call" => Ok(tools_call(ctx, params)),
        // JSON-RPC "method not found".
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

/// The `initialize` result: echo the client's protocol version (most compatible),
/// advertise the `tools` capability, and identify the server.
fn initialize_result(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "rigor-rs", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The tool catalogue (name + description + JSON-Schema input) returned by
/// `tools/list`.
fn tool_catalog() -> Value {
    json!([
        {
            "name": "check",
            "description": "Analyze Ruby source with rigor's inference-first static analysis and \
                            return the diagnostics (undefined methods, wrong arity, flow issues, …). \
                            Sound-subset: it never reports a diagnostic it cannot prove.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "The Ruby source code to analyze." },
                    "path": { "type": "string", "description": "Optional filename for the report (display only)." }
                },
                "required": ["source"]
            }
        },
        {
            "name": "type_of",
            "description": "Report rigor's inferred type of the Ruby expression at a 1-based \
                            line/column in the given source.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "The Ruby source code." },
                    "line": { "type": "integer", "description": "1-based line number." },
                    "column": { "type": "integer", "description": "1-based column number." }
                },
                "required": ["source", "line", "column"]
            }
        },
        {
            "name": "explain",
            "description": "Look up rigor's rule catalogue. With no `rule`, returns the id + \
                            summary of every rule. With a `rule` (canonical id, legacy alias, or \
                            family token like `flow`), returns that rule's full metadata (summary, \
                            fires-when, suppression, severity, docs URL).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "rule": { "type": "string", "description": "Rule id / alias / family token (optional)." }
                },
                "required": []
            }
        },
        {
            "name": "outline",
            "description": "Return the structural outline of Ruby source — a nested tree of its \
                            classes, modules, and methods with 1-based line ranges.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "The Ruby source code." }
                },
                "required": ["source"]
            }
        },
        {
            "name": "triage",
            "description": "Analyze Ruby source and return a structured triage of the diagnostics \
                            (ADR-23): a rule-id distribution, a class/method selectors axis, per-file \
                            hotspots, a severity summary, and heuristic hints — the aggregate stats an \
                            agent uses to prioritise instead of reading the raw per-line list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "The Ruby source code to analyze." },
                    "path": { "type": "string", "description": "Optional filename for the report (display only)." }
                },
                "required": ["source"]
            }
        },
        {
            "name": "annotate",
            "description": "Return rigor's inferred type for every source line as a `{ line => type }` \
                            map (the xmpfilter `#=> <type>` view) — a quick way to see what the engine \
                            infers throughout a file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "The Ruby source code." }
                },
                "required": ["source"]
            }
        },
        {
            "name": "coverage",
            "description": "Report type-precision coverage: the ratio of expressions Rigor types as \
                            Constant / Nominal / shaped / refined (precise) vs Dynamic or top (opaque). \
                            Returns JSON. Useful for measuring the impact of adding new fold rules.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Files or directories to scan (required)"
                    },
                    "config": { "type": "string", "description": "Path to .rigor.yml (optional)" }
                },
                "required": ["paths"]
            }
        },
        {
            "name": "sig_gen",
            "description": "Generate RBS skeleton signatures inferred from Ruby source FILES (read-only, \
                            like `sig-gen --print --format=json`). Returns a JSON report of candidates \
                            with their classification (new-method / tighter-return) and inferred return \
                            type. Takes file/dir PATHS (not inline source), resolved on the server's \
                            filesystem.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Files or directories to generate signatures for (defaults to the \
                                        configured `paths:`)."
                    },
                    "config": { "type": "string", "description": "Path to .rigor.yml (optional)." }
                }
            }
        }
    ])
}

/// Handle `tools/call`: route to the named tool. A tool-level failure (bad args,
/// unknown tool, analysis panic) is reported as an `isError` result — visible to
/// the model — rather than a protocol error, per the MCP convention.
fn tools_call(ctx: &ServerContext, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    let outcome = match name {
        "check" => tool_check(ctx, &args),
        "type_of" => tool_type_of(ctx, &args),
        "explain" => tool_explain(&args),
        "outline" => tool_outline(&args),
        "triage" => tool_triage(ctx, &args),
        "annotate" => tool_annotate(ctx, &args),
        "sig_gen" => tool_sig_gen(&args),
        "coverage" => tool_coverage(&args),
        other => Err(format!("unknown tool: {other}")),
    };
    match outcome {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
        Err(msg) => json!({ "content": [{ "type": "text", "text": msg }], "isError": true }),
    }
}

/// The `check` tool: run the analysis pipeline on the source and return the
/// diagnostics as a pretty JSON array (the same field set as `rigor check
/// --format json`, keyed for agent consumption).
fn tool_check(ctx: &ServerContext, args: &Value) -> Result<String, String> {
    let source = args.get("source").and_then(Value::as_str).ok_or("missing `source` string")?;
    let path = args.get("path").and_then(Value::as_str).unwrap_or("<mcp>");

    let bytes = source.as_bytes().to_vec();
    let analysed = panic::catch_unwind(AssertUnwindSafe(|| {
        let result = parse(&bytes);
        let comments = comment_lines(&result, &bytes);
        let ast = lower(&result);
        let src = SourceIndex::build(&ast, &ctx.index);
        let mut interner = Interner::new();
        let mut diags = analyze_with_source(&ast, &mut interner, &ctx.index, &src);
        diags.extend(shadowed_rescue_diagnostics(&ast, &ctx.index, &src, source));
        (diags, comments)
    }))
    .map_err(|_| "internal error: analysis panicked on this source".to_string())?;

    let (mut diags, comments) = analysed;
    diags.extend(suppression_marker_diagnostics(&comments));
    let with_lines: Vec<(usize, Diagnostic)> =
        diags.into_iter().map(|d| (line_col(source, d.start_offset).0, d)).collect();
    let kept = filter_suppressed(with_lines, &comments);

    let items: Vec<Value> = kept
        .into_iter()
        .filter(|(_, d)| !ctx.disable.suppresses(d.rule_id))
        .map(|(_, d)| {
            let (line, column) = line_col(source, d.start_offset);
            let mut obj = json!({
                "path": path,
                "line": line,
                "column": column,
                "severity": d.severity.as_str(),
                "rule": d.rule_id,
                "message": d.message,
            });
            if let Some(entry) = catalog(d.rule_id) {
                obj["documentation_url"] = json!(entry.documentation_url);
            }
            obj
        })
        .collect();

    let report = json!({ "diagnostics": items, "count": items.len() });
    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
}

/// The `triage` tool: analyze the source, then return the structured triage
/// report (distribution / selectors / hotspots / summary / hints) as JSON. Runs
/// the SAME suppression + disable filter as `check` before aggregating.
fn tool_triage(ctx: &ServerContext, args: &Value) -> Result<String, String> {
    let source = args.get("source").and_then(Value::as_str).ok_or("missing `source` string")?;
    let path = args.get("path").and_then(Value::as_str).unwrap_or("<mcp>");

    let bytes = source.as_bytes().to_vec();
    let analysed = panic::catch_unwind(AssertUnwindSafe(|| {
        let result = parse(&bytes);
        let comments = comment_lines(&result, &bytes);
        let ast = lower(&result);
        let src = SourceIndex::build(&ast, &ctx.index);
        let mut interner = Interner::new();
        let mut diags = analyze_with_source(&ast, &mut interner, &ctx.index, &src);
        diags.extend(shadowed_rescue_diagnostics(&ast, &ctx.index, &src, source));
        (diags, comments)
    }))
    .map_err(|_| "internal error: analysis panicked on this source".to_string())?;

    let (mut diags, comments) = analysed;
    diags.extend(suppression_marker_diagnostics(&comments));
    let with_lines: Vec<(usize, Diagnostic)> =
        diags.into_iter().map(|d| (line_col(source, d.start_offset).0, d)).collect();
    let kept: Vec<Diagnostic> = filter_suppressed(with_lines, &comments)
        .into_iter()
        .filter(|(_, d)| !ctx.disable.suppresses(d.rule_id))
        .map(|(_, d)| d)
        .collect();
    let pairs: Vec<(&str, &Diagnostic)> = kept.iter().map(|d| (path, d)).collect();
    Ok(crate::triage::report_json_for(&pairs, 10, false))
}

/// The `annotate` tool: return the `{ line => type }` map for the source.
fn tool_annotate(ctx: &ServerContext, args: &Value) -> Result<String, String> {
    let source = args.get("source").and_then(Value::as_str).ok_or("missing `source` string")?;
    panic::catch_unwind(AssertUnwindSafe(|| crate::annotate::annotations_json(&ctx.index, source)))
        .map_err(|_| "internal error: annotation panicked on this source".to_string())
}

/// The `sig_gen` tool: generate RBS-skeleton candidates for the given FILE paths
/// (read-only, `sig-gen --print --format=json`), returning the `{ "candidates":
/// [...] }` JSON. Unlike the source-string tools this reads files from the
/// server's filesystem (reference `rigor_sig_gen` takes `paths`). Panic-isolated.
fn tool_sig_gen(args: &Value) -> Result<String, String> {
    let paths: Vec<String> = args
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let config = args.get("config").and_then(Value::as_str).map(std::path::Path::new);
    panic::catch_unwind(AssertUnwindSafe(|| crate::sig_gen::mcp_report_json(&path_refs, config)))
        .map_err(|_| "internal error: sig-gen panicked".to_string())
}

/// The `coverage` tool: the type-precision coverage report for the given FILE
/// paths (read-only; reference `rigor_coverage` shells `coverage --format=json`).
/// Output is byte-identical to the CLI's `coverage --format=json`. Like
/// `sig_gen`, it reads files from the server's filesystem. Panic-isolated.
fn tool_coverage(args: &Value) -> Result<String, String> {
    let paths: Vec<String> = args
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let config = args.get("config").and_then(Value::as_str).map(std::path::Path::new);
    panic::catch_unwind(AssertUnwindSafe(|| crate::coverage::mcp_coverage_json(&paths, config)))
        .map_err(|_| "internal error: coverage scan panicked".to_string())?
}

/// The `type_of` tool: type the deepest expression at a 1-based (line, column).
fn tool_type_of(ctx: &ServerContext, args: &Value) -> Result<String, String> {
    let source = args.get("source").and_then(Value::as_str).ok_or("missing `source` string")?;
    let line = args.get("line").and_then(Value::as_u64).ok_or("missing `line` integer")? as usize;
    let column =
        args.get("column").and_then(Value::as_u64).ok_or("missing `column` integer")? as usize;

    let offset = crate::type_of::position_to_offset(source, line, column)?;
    let rendered = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower(&parse(source.as_bytes()));
        let node_id = crate::type_of::locate_node(&ast, offset)?;
        let src = SourceIndex::build(&ast, &ctx.index);
        let typer = Typer::with_source(&ctx.index, &src);
        let mut interner = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut interner);
        let ty = typer.type_of(&ast, node_id, &env, &mut interner);
        let node = ast.get(node_id);
        Some((
            crate::type_of::node_kind(node).to_string(),
            crate::type_of::render_type(&interner, &ctx.index, &src, ty),
        ))
    }))
    .map_err(|_| "internal error: typing panicked on this source".to_string())?;

    let (node, ty) = rendered.ok_or_else(|| {
        format!("no expression found at line {line}, column {column}")
    })?;
    let report = json!({ "line": line, "column": column, "node": node, "type": ty });
    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
}

/// The `explain` tool: look up the rule catalogue (all rules, or one rule's full
/// metadata by id / alias / family token). Reuses the `explain` command's table.
fn tool_explain(args: &Value) -> Result<String, String> {
    let query = args.get("rule").and_then(Value::as_str);
    let v = crate::explain::explain_json(query)?;
    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
}

/// The `outline` tool: the nested class/module/method structure of the source,
/// with 1-based line ranges. Reuses the shared outline builder (the same one the
/// LSP `documentSymbol` handler uses).
fn tool_outline(args: &Value) -> Result<String, String> {
    let source = args.get("source").and_then(Value::as_str).ok_or("missing `source` string")?;
    let syms = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower(&parse(source.as_bytes()));
        crate::outline::build(&ast)
    }))
    .map_err(|_| "internal error: parsing panicked on this source".to_string())?;

    let tree: Vec<Value> = syms.iter().map(|s| outline_json(s, source)).collect();
    let report = json!({ "outline": tree, "count": syms.len() });
    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
}

/// Adapt a shared outline node into JSON with 1-based line ranges.
fn outline_json(s: &crate::outline::SymNode, source: &str) -> Value {
    let (start_line, _) = line_col(source, s.full.0);
    let (end_line, _) = line_col(source, s.full.1.min(source.len()));
    let mut obj = json!({
        "name": s.name,
        "kind": s.kind.label(),
        "startLine": start_line,
        "endLine": end_line,
    });
    if !s.children.is_empty() {
        obj["children"] = Value::Array(s.children.iter().map(|c| outline_json(c, source)).collect());
    }
    obj
}

/// 1-based `(line, column)` from a byte offset (columns in Unicode scalars), the
/// same mapping the text/json reporters use.
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
    (line, source[line_start..clamped].chars().count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ServerContext {
        ServerContext {
            index: CoreIndex::new(),
            disable: Config::default().disable_matcher(),
        }
    }

    #[test]
    fn initialize_echoes_protocol_and_names_server() {
        let r = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(r["protocolVersion"], "2024-11-05");
        assert_eq!(r["serverInfo"]["name"], "rigor-rs");
        assert!(r["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_defaults_protocol_when_absent() {
        let r = initialize_result(&json!({}));
        assert_eq!(r["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn tools_list_advertises_the_tools() {
        let tools = tool_catalog();
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"check"));
        assert!(names.contains(&"type_of"));
        assert!(names.contains(&"explain"));
        assert!(names.contains(&"triage"));
        assert!(names.contains(&"annotate"));
        assert!(names.contains(&"sig_gen"));
        assert!(names.contains(&"coverage"));
        // Every tool declares an object inputSchema.
        for t in tools.as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object");
        }
        // The source-consuming tools declare a `source` property.
        for t in tools.as_array().unwrap() {
            if matches!(t["name"].as_str(), Some("check") | Some("type_of")) {
                assert!(t["inputSchema"]["properties"]["source"].is_object());
            }
        }
    }

    #[test]
    fn check_tool_reports_a_typo() {
        let out = tools_call(&ctx(), &json!({
            "name": "check",
            "arguments": { "source": "x = \"hi\"\nx.lenght\n" }
        }));
        assert_eq!(out["isError"], false);
        let text = out["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["diagnostics"][0]["rule"], "call.undefined-method");
        assert_eq!(parsed["diagnostics"][0]["line"], 2);
    }

    #[test]
    fn triage_tool_returns_structured_report() {
        let out = tools_call(&ctx(), &json!({
            "name": "triage",
            "arguments": { "source": "\"x\".frist\n", "path": "t.rb" }
        }));
        assert_eq!(out["isError"], false);
        let text = out["content"][0]["text"].as_str().unwrap();
        let report: Value = serde_json::from_str(text).unwrap();
        assert_eq!(report["summary"]["total"], 1);
        assert_eq!(report["selectors"][0]["receiver"], "String");
        assert_eq!(report["selectors"][0]["method"], "frist");
    }

    #[test]
    fn annotate_tool_returns_line_types() {
        let out = tools_call(&ctx(), &json!({
            "name": "annotate",
            "arguments": { "source": "x = [1, 2].first\n" }
        }));
        assert_eq!(out["isError"], false);
        let text = out["content"][0]["text"].as_str().unwrap();
        let ann: Value = serde_json::from_str(text).unwrap();
        // The Tuple-projection fold gives `[1, 2].first` → `1`.
        assert_eq!(ann["annotations"]["1"], "1");
    }

    #[test]
    fn sig_gen_tool_returns_candidates_for_a_file() {
        let dir = std::env::temp_dir().join(format!("rigor_mcp_siggen_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("g.rb");
        std::fs::write(&file, "class Greeter\n  def greeting\n    \"hi\"\n  end\nend\n").unwrap();
        let out = tools_call(&ctx(), &json!({
            "name": "sig_gen",
            "arguments": { "paths": [file.to_str().unwrap()] }
        }));
        assert_eq!(out["isError"], false);
        let text = out["content"][0]["text"].as_str().unwrap();
        let report: Value = serde_json::from_str(text).unwrap();
        assert_eq!(report["candidates"][0]["class"], "Greeter");
        assert_eq!(report["candidates"][0]["method"], "greeting");
        assert_eq!(report["candidates"][0]["rbs"], "def greeting: () -> \"hi\"");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_tool_respects_inline_suppression() {
        let out = tools_call(&ctx(), &json!({
            "name": "check",
            "arguments": { "source": "x = \"hi\"\nx.lenght # rigor:disable undefined-method\n" }
        }));
        let text = out["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["count"], 0);
    }

    #[test]
    fn type_of_tool_reports_type() {
        let out = tools_call(&ctx(), &json!({
            "name": "type_of",
            "arguments": { "source": "n = 42\n", "line": 1, "column": 5 }
        }));
        assert_eq!(out["isError"], false);
        let text = out["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["type"], "42");
        assert_eq!(parsed["node"], "IntegerLit");
    }

    #[test]
    fn explain_tool_lists_all_rules_with_no_arg() {
        let out = tools_call(&ctx(), &json!({ "name": "explain", "arguments": {} }));
        assert_eq!(out["isError"], false);
        let parsed: Value = serde_json::from_str(out["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(parsed["count"].as_u64().unwrap() > 5);
        let ids: Vec<&str> =
            parsed["rules"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"call.undefined-method"));
    }

    #[test]
    fn explain_tool_returns_full_metadata_for_a_rule() {
        // A legacy alias resolves to the canonical rule with full fields.
        let out = tools_call(&ctx(), &json!({
            "name": "explain", "arguments": { "rule": "undefined-method" }
        }));
        assert_eq!(out["isError"], false);
        let parsed: Value = serde_json::from_str(out["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed["count"], 1);
        let r = &parsed["rules"][0];
        assert_eq!(r["id"], "call.undefined-method");
        assert!(r["fires_when"].is_array());
        assert!(r["documentation_url"].as_str().unwrap().contains("call-undefined-method"));
    }

    #[test]
    fn explain_tool_unknown_rule_is_an_error() {
        let out = tools_call(&ctx(), &json!({
            "name": "explain", "arguments": { "rule": "no-such-rule" }
        }));
        assert_eq!(out["isError"], true);
    }

    #[test]
    fn outline_tool_returns_nested_structure() {
        let src = "class Foo\n  def bar\n  end\nend\nmodule M\nend\n";
        let out = tools_call(&ctx(), &json!({ "name": "outline", "arguments": { "source": src } }));
        assert_eq!(out["isError"], false);
        let parsed: Value = serde_json::from_str(out["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed["count"], 2);
        let foo = parsed["outline"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "Foo")
            .unwrap();
        assert_eq!(foo["kind"], "class");
        assert_eq!(foo["startLine"], 1);
        assert_eq!(foo["children"][0]["name"], "bar");
        assert_eq!(foo["children"][0]["kind"], "method");
    }

    #[test]
    fn unknown_tool_is_an_error_result() {
        let out = tools_call(&ctx(), &json!({ "name": "nope", "arguments": {} }));
        assert_eq!(out["isError"], true);
        assert!(out["content"][0]["text"].as_str().unwrap().contains("unknown tool"));
    }

    #[test]
    fn check_tool_missing_source_is_an_error() {
        let out = tools_call(&ctx(), &json!({ "name": "check", "arguments": {} }));
        assert_eq!(out["isError"], true);
    }

    #[test]
    fn dispatch_unknown_method_is_jsonrpc_error() {
        let e = dispatch(&ctx(), "frobnicate", &Value::Null).unwrap_err();
        assert_eq!(e.0, -32601);
    }

    #[test]
    fn coverage_tool_reports_the_precision_summary() {
        // A file whose 4 expressions split 3 constant / 1 dynamic_top (the same
        // shape as harness fixture 01).
        let dir = std::env::temp_dir().join(format!("rigor-mcp-cov-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.rb");
        std::fs::write(&file, "s = \"Hello\"\ns.lenght\n").unwrap();

        let out = tools_call(
            &ctx(),
            &json!({ "name": "coverage", "arguments": { "paths": [file.to_str().unwrap()] } }),
        );
        assert_eq!(out["isError"], false);
        let text = out["content"][0]["text"].as_str().unwrap();
        let report: Value = serde_json::from_str(text).unwrap();
        assert_eq!(report["summary"]["expressions_typed"], 4);
        assert_eq!(report["by_tier"]["constant"]["count"], 3);
        assert_eq!(report["by_tier"]["dynamic_top"]["count"], 1);
        // Byte-identity with the CLI `--format=json` path: the tool IS the CLI
        // renderer over the same scan.
        let cli = crate::coverage::mcp_coverage_json(
            &[file.to_str().unwrap().to_string()],
            None,
        )
        .unwrap();
        assert_eq!(text, cli);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn coverage_tool_bad_path_is_an_error() {
        let out = tools_call(
            &ctx(),
            &json!({ "name": "coverage", "arguments": { "paths": ["/nonexistent/xyz.rb"] } }),
        );
        assert_eq!(out["isError"], true);
        assert!(out["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not a file or directory"));
    }
}
