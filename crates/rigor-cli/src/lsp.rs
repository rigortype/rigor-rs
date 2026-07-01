//! `rigor lsp [--transport=stdio] [--log=PATH]` (§12, ADR-0029) — the in-process
//! Language Server.
//!
//! v1 scope: stdio JSON-RPC (via the sync `lsp-server` scaffold — no async
//! runtime), `TextDocumentSyncKind::FULL` open buffers, live **diagnostics**
//! (`textDocument/publishDiagnostics`) and **hover** (`textDocument/hover`, a
//! type-of probe at the cursor). These two reuse the EXACT `check` / `type-of`
//! analysis path, so an editor sees byte-for-byte the same findings and types the
//! CLI does. Completion is the next slice (it needs a method-enumeration index API
//! plus receiver-before-trigger parsing; deferred, and not advertised as a
//! capability, so no editor calls it).
//!
//! Two-tier essence (ADR-0029): the RBS environment (`CoreIndex`) + config are
//! built ONCE at startup and reused across every request — the per-keystroke cost
//! is a single-file parse+lower+analyze, never the RBS-load floor. The heavier
//! two-tier machinery (generation counter, watched-files invalidation, worker
//! pool, debounce, temp-file `BufferBinding`, cross-file project context for open
//! buffers) is deferred; a single open buffer is analysed in-memory at file scope.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

use lsp_server::{Connection, Message, Response};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    MarkupContent, MarkupKind, NumberOrString, Position, PublishDiagnosticsParams, Range,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, Typer};
use rigor_parse::{comment_lines, lower, parse};
use rigor_rules::{analyze_with_source, filter_suppressed, Severity, SuppressSet};
use rigor_types::Interner;

use crate::config::Config;

/// `rigor lsp [--transport=stdio] [--log=PATH]`. Only `stdio` transport is
/// supported in v1 (ADR-0029); `--log` is accepted and reserved (server logs go
/// to stderr until wired). Returns exit 0 on a clean shutdown, 64 on a usage
/// error (unknown transport), 1 on a protocol/IO error.
pub fn cmd_lsp(args: &[String]) -> ExitCode {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            // `--transport=stdio` or `--transport stdio`.
            "--transport=stdio" => {}
            "--transport" => match it.next().map(String::as_str) {
                Some("stdio") => {}
                other => {
                    eprintln!("rigor lsp: only --transport=stdio is supported, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            a if a.starts_with("--transport=") => {
                eprintln!("rigor lsp: only --transport=stdio is supported, got {a:?}");
                return ExitCode::from(64);
            }
            // `--log=PATH` / `--log PATH` — accepted + reserved (ADR-0029).
            a if a.starts_with("--log=") => {}
            "--log" => {
                let _ = it.next();
            }
            other => {
                eprintln!("rigor lsp: unexpected argument {other:?}");
                return ExitCode::from(64);
            }
        }
    }

    match run_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rigor lsp: {e}");
            ExitCode::from(1)
        }
    }
}

/// Boot the stdio server: handshake, build the shared context once, run the loop.
fn run_stdio() -> Result<(), String> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = ServerCapabilities {
        // FULL sync: each edit resends the whole buffer (ADR-0029 — local stdio
        // bandwidth is irrelevant; UTF-16 incremental diffing is a later slice).
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    };
    let caps_value = serde_json::to_value(capabilities).map_err(|e| e.to_string())?;
    connection
        .initialize(caps_value)
        .map_err(|e| format!("initialize handshake failed: {e}"))?;

    // Two-tier essence: the RBS environment + config are built ONCE and reused
    // for the whole session (the per-keystroke path never pays the RBS-load floor).
    let cfg = Config::load(None);
    let ctx = ServerContext {
        index: CoreIndex::with_plugins(&cfg.plugins),
        disable: cfg.disable_matcher(),
    };
    let mut buffers: HashMap<String, String> = HashMap::new();

    main_loop(&connection, &ctx, &mut buffers)?;

    // Drop the connection BEFORE joining: the writer IO thread only terminates
    // when its channel disconnects, i.e. when the `Connection` (which owns the
    // sender) is dropped. Joining while `connection` is still alive would hang.
    drop(connection);
    io_threads.join().map_err(|e| e.to_string())?;
    Ok(())
}

/// The session-stable context (ADR-0029 `ProjectContext`, minimal form): the RBS
/// index + the config-derived suppression set, both built once at startup.
struct ServerContext {
    index: CoreIndex,
    disable: SuppressSet,
}

/// The synchronous dispatch loop. Requests are answered inline; notifications
/// mutate the buffer table and (re)publish diagnostics. `shutdown`/`exit` are
/// handled by the scaffold's `handle_shutdown`.
fn main_loop(
    connection: &Connection,
    ctx: &ServerContext,
    buffers: &mut HashMap<String, String>,
) -> Result<(), String> {
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection
                    .handle_shutdown(&req)
                    .map_err(|e| e.to_string())?
                {
                    return Ok(());
                }
                match req.method.as_str() {
                    "textDocument/hover" => {
                        let resp = match req.extract::<HoverParams>("textDocument/hover") {
                            Ok((id, params)) => {
                                let hover = hover(ctx, buffers, &params);
                                Response::new_ok(id, hover)
                            }
                            Err(e) => {
                                // Malformed params — reply null so the client isn't
                                // left waiting (id is unknown on extract error, so
                                // this can only happen on a truly bad message).
                                eprintln!("rigor lsp: bad hover params: {e:?}");
                                continue;
                            }
                        };
                        connection
                            .sender
                            .send(Message::Response(resp))
                            .map_err(|e| e.to_string())?;
                    }
                    // Unknown request: reply with a MethodNotFound-ish null result
                    // so the client doesn't hang (we advertise a small surface).
                    _ => {
                        let resp = Response::new_ok(req.id, serde_json::Value::Null);
                        connection
                            .sender
                            .send(Message::Response(resp))
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            Message::Notification(not) => match not.method.as_str() {
                "textDocument/didOpen" => {
                    if let Ok(p) = not.extract::<DidOpenTextDocumentParams>("textDocument/didOpen") {
                        let uri = p.text_document.uri;
                        let text = p.text_document.text;
                        publish(connection, ctx, &uri, &text)?;
                        buffers.insert(uri_key(&uri), text);
                    }
                }
                "textDocument/didChange" => {
                    if let Ok(p) =
                        not.extract::<DidChangeTextDocumentParams>("textDocument/didChange")
                    {
                        // FULL sync: the last content change IS the whole buffer.
                        if let Some(change) = p.content_changes.into_iter().last() {
                            let uri = p.text_document.uri;
                            publish(connection, ctx, &uri, &change.text)?;
                            buffers.insert(uri_key(&uri), change.text);
                        }
                    }
                }
                "textDocument/didClose" => {
                    if let Ok(p) = not.extract::<DidCloseTextDocumentParams>("textDocument/didClose")
                    {
                        let uri = p.text_document.uri;
                        buffers.remove(&uri_key(&uri));
                        // Clear inline markers by publishing an empty set.
                        send_diagnostics(connection, &uri, Vec::new())?;
                    }
                }
                _ => {}
            },
            Message::Response(_) => {}
        }
    }
    Ok(())
}

/// A stable string key for a document URI (the buffer table is keyed by it).
fn uri_key(uri: &Uri) -> String {
    uri.as_str().to_string()
}

/// Analyse `text` and publish its diagnostics for `uri`.
fn publish(
    connection: &Connection,
    ctx: &ServerContext,
    uri: &Uri,
    text: &str,
) -> Result<(), String> {
    let diags = compute_diagnostics(ctx, text);
    send_diagnostics(connection, uri, diags)
}

/// Send a `textDocument/publishDiagnostics` notification.
fn send_diagnostics(
    connection: &Connection,
    uri: &Uri,
    diagnostics: Vec<Diagnostic>,
) -> Result<(), String> {
    let params = PublishDiagnosticsParams { uri: uri.clone(), diagnostics, version: None };
    let not = lsp_server::Notification::new(
        "textDocument/publishDiagnostics".to_string(),
        params,
    );
    connection
        .sender
        .send(Message::Notification(not))
        .map_err(|e| e.to_string())
}

/// Run the single-file analysis path over `text` and map the findings to LSP
/// diagnostics. Reuses the exact `check` pipeline (parse → lower → build a
/// single-file `SourceIndex` → `analyze_with_source`), plus the inline
/// `# rigor:disable` and config `disable:` suppression, so the editor's inline
/// markers match `rigor check` on the same content. Panic-isolated (ADR-0016): a
/// malformed buffer that trips the parser yields no diagnostics, never a crash.
fn compute_diagnostics(ctx: &ServerContext, text: &str) -> Vec<Diagnostic> {
    let bytes = text.as_bytes().to_vec();
    let analysed = panic::catch_unwind(AssertUnwindSafe(|| {
        let result = parse(&bytes);
        let comments = comment_lines(&result, &bytes);
        let ast = lower(&result);
        let source = SourceIndex::build(&ast, &ctx.index);
        let mut interner = Interner::new();
        let diags = analyze_with_source(&ast, &mut interner, &ctx.index, &source);
        (diags, comments)
    }));

    let (diags, comments) = match analysed {
        Ok(pair) => pair,
        Err(_) => return Vec::new(),
    };

    // Inline `# rigor:disable` suppression (same as `check`): key each diag on its
    // 1-based line, filter, then drop config-`disable:`d rules.
    let with_lines: Vec<(usize, rigor_rules::Diagnostic)> = diags
        .into_iter()
        .map(|d| (offset_to_position(text, d.start_offset).line as usize + 1, d))
        .collect();

    filter_suppressed(with_lines, &comments)
        .into_iter()
        .filter(|(_, d)| !ctx.disable.suppresses(d.rule_id))
        .map(|(_, d)| to_lsp_diagnostic(text, &d))
        .collect()
}

/// Map one rigor `Diagnostic` to an LSP `Diagnostic`. `source` = `"rigor"`,
/// `code` = the rule id, severity per ADR-0029 (`error`→Error, `warning`→Warning,
/// `info`→Information). The range is the diagnostic's byte span, resolved to
/// 0-based UTF-16 LSP positions.
fn to_lsp_diagnostic(text: &str, d: &rigor_rules::Diagnostic) -> Diagnostic {
    let start = offset_to_position(text, d.start_offset);
    let end = offset_to_position(text, d.end_offset.max(d.start_offset));
    let severity = match d.severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
    };
    Diagnostic {
        range: Range { start, end },
        severity: Some(severity),
        code: Some(NumberOrString::String(d.rule_id.to_string())),
        source: Some("rigor".to_string()),
        message: d.message.clone(),
        ..Default::default()
    }
}

/// Answer `textDocument/hover`: locate the deepest node under the cursor, type it,
/// and render a small markdown card (node kind + inferred type). Reuses the
/// `type-of` node-locator + type renderer. Returns `None` when the buffer is
/// unknown, the position is out of range, or no node covers it — a null hover.
fn hover(
    ctx: &ServerContext,
    buffers: &HashMap<String, String>,
    params: &HoverParams,
) -> Option<Hover> {
    let pos = &params.text_document_position_params;
    let text = buffers.get(&uri_key(&pos.text_document.uri))?;
    let offset = position_to_offset(text, pos.position)?;

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower(&parse(text.as_bytes()));
        let node_id = crate::type_of::locate_node(&ast, offset)?;
        let source = SourceIndex::build(&ast, &ctx.index);
        let typer = Typer::with_source(&ctx.index, &source);
        let mut interner = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut interner);
        let ty = typer.type_of(&ast, node_id, &env, &mut interner);
        let node = ast.get(node_id);
        let (start, end) = node.span();
        let kind = crate::type_of::node_kind(node);
        let rendered = crate::type_of::render_type(&interner, &ctx.index, ty);
        Some((kind, rendered, start, end))
    }));

    let (kind, rendered, start, end) = result.ok().flatten()?;
    let value = format!("```ruby\n{rendered}\n```\n\n*rigor: {kind}*");
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(Range {
            start: offset_to_position(text, start),
            end: offset_to_position(text, end),
        }),
    })
}

// ---------------------------------------------------------------------------
// Position <-> byte-offset (LSP: 0-based line, 0-based UTF-16 `character`)
// ---------------------------------------------------------------------------

/// Byte offset → LSP `Position` (0-based line, 0-based UTF-16 character). The
/// column is counted in UTF-16 code units per the LSP default position encoding.
fn offset_to_position(text: &str, offset: usize) -> Position {
    let offset = offset.min(text.len());
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (i, b) in text.as_bytes().iter().enumerate() {
        if i >= offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let character: u32 = text[line_start..offset]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum();
    Position { line, character }
}

/// LSP `Position` → byte offset. Walks to the 0-based `line`, then advances
/// `character` UTF-16 code units into it; a position past the line's end clamps to
/// the line end (LSP semantics). Returns `None` if the line is past EOF.
fn position_to_offset(text: &str, pos: Position) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut line = 0u32;
    let mut idx = 0usize;
    while line < pos.line {
        match bytes.get(idx) {
            Some(b'\n') => {
                line += 1;
                idx += 1;
            }
            Some(_) => idx += 1,
            None => return None, // line past end of buffer
        }
    }
    let line_start = idx;
    let line_end = text[line_start..]
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(text.len());
    let mut u16_count = 0u32;
    for (i, c) in text[line_start..line_end].char_indices() {
        if u16_count >= pos.character {
            return Some(line_start + i);
        }
        u16_count += c.len_utf16() as u32;
    }
    Some(line_end)
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
    fn position_roundtrip_ascii() {
        let text = "s = \"hi\"\ns.upcase\n";
        // line 1 (0-based), char 2 → the `u` of upcase.
        let off = position_to_offset(text, Position { line: 1, character: 2 }).unwrap();
        assert_eq!(&text[off..off + 6], "upcase");
        let back = offset_to_position(text, off);
        assert_eq!(back, Position { line: 1, character: 2 });
    }

    #[test]
    fn position_utf16_multibyte() {
        // "é" is 1 UTF-16 unit but 2 UTF-8 bytes; "𐐷" is 2 UTF-16 units, 4 bytes.
        let text = "x = 'é𐐷z'\n";
        // Walk to the `z`: chars before it on line 0 are x,space,=,space,',é,𐐷.
        let z = text.find('z').unwrap();
        let pos = offset_to_position(text, z);
        // UTF-16 units before z: x(1) (1)=(1) (1)'(1) é(1) 𐐷(2) = 8.
        assert_eq!(pos, Position { line: 0, character: 8 });
        assert_eq!(position_to_offset(text, pos).unwrap(), z);
    }

    #[test]
    fn diagnostics_flag_a_typo() {
        // `"hi".lenght` — undefined method, one diagnostic.
        let diags = compute_diagnostics(&ctx(), "x = \"hi\"\nx.lenght\n");
        assert_eq!(diags.len(), 1, "one undefined-method diagnostic");
        let d = &diags[0];
        assert_eq!(d.source.as_deref(), Some("rigor"));
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.code, Some(NumberOrString::String("call.undefined-method".to_string())));
        assert_eq!(d.range.start.line, 1); // 0-based: line 2 in the file
    }

    #[test]
    fn diagnostics_respect_inline_suppression() {
        // A `# rigor:disable <rule>` on the line suppresses the finding, like
        // `check` (a bare `# rigor:disable` with no rule token is a no-op — it
        // needs a rule, matching the reference's `\s+(rules)` directive grammar).
        let diags =
            compute_diagnostics(&ctx(), "x = \"hi\"\nx.lenght # rigor:disable undefined-method\n");
        assert!(diags.is_empty(), "inline disable suppresses the diagnostic");
    }

    #[test]
    fn diagnostics_clean_source_is_empty() {
        let diags = compute_diagnostics(&ctx(), "x = \"hi\"\nx.upcase\n");
        assert!(diags.is_empty());
    }

    #[test]
    fn hover_reports_a_type() {
        let mut buffers = HashMap::new();
        let uri: Uri = "file:///t.rb".parse().unwrap();
        buffers.insert(uri_key(&uri), "n = 42\n".to_string());
        let params = HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line: 0, character: 4 }, // on `42`
            },
            work_done_progress_params: Default::default(),
        };
        let h = hover(&ctx(), &buffers, &params).expect("a hover");
        match h.contents {
            HoverContents::Markup(m) => assert!(m.value.contains("42"), "{}", m.value),
            _ => panic!("expected markup hover"),
        }
    }

    #[test]
    fn hover_unknown_buffer_is_none() {
        let params = HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: "file:///missing.rb".parse().unwrap(),
                },
                position: Position { line: 0, character: 0 },
            },
            work_done_progress_params: Default::default(),
        };
        assert!(hover(&ctx(), &HashMap::new(), &params).is_none());
    }
}
