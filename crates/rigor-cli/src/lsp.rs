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
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Hover,
    HoverContents, HoverParams, HoverProviderCapability, MarkupContent, MarkupKind, NumberOrString,
    OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities, SymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, Typer};
use rigor_parse::{comment_lines, lower, parse, Node};
use rigor_rules::{analyze_with_source, filter_suppressed, Severity, SuppressSet};
use rigor_types::{Interner, Type, TypeId};

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
        // Member-access method completion, triggered on `.` and `:` (the second
        // `:` of `::`). The server returns the full unfiltered candidate set;
        // client-side fuzzy matching narrows it (ADR-0029).
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
            ..Default::default()
        }),
        // Outline: classes/modules/methods as a nested symbol tree.
        document_symbol_provider: Some(OneOf::Left(true)),
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
                    "textDocument/completion" => {
                        match req.extract::<CompletionParams>("textDocument/completion") {
                            Ok((id, params)) => {
                                let items = completion(ctx, buffers, &params);
                                let resp = Response::new_ok(id, items);
                                connection
                                    .sender
                                    .send(Message::Response(resp))
                                    .map_err(|e| e.to_string())?;
                            }
                            Err(e) => eprintln!("rigor lsp: bad completion params: {e:?}"),
                        }
                    }
                    "textDocument/documentSymbol" => {
                        match req.extract::<DocumentSymbolParams>("textDocument/documentSymbol") {
                            Ok((id, params)) => {
                                let syms = document_symbols(buffers, &params);
                                let resp = Response::new_ok(id, syms);
                                connection
                                    .sender
                                    .send(Message::Response(resp))
                                    .map_err(|e| e.to_string())?;
                            }
                            Err(e) => eprintln!("rigor lsp: bad documentSymbol params: {e:?}"),
                        }
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
/// and render a node-aware markdown card. A `Call` shows `receiver#method →
/// return` (plus the RBS arity when the receiver class is core-known); a constant
/// shows `Name : type`; anything else shows the inferred type + node kind. Reuses
/// the `type-of` node-locator + type renderer. Returns `None` when the buffer is
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
        let (start, end) = ast.get(node_id).span();
        let type_render = crate::type_of::render_type(&interner, &ctx.index, ty);

        // Extract owned node bits so later `&mut interner` calls don't clash with
        // the `&ast` borrow of `node`.
        let call_bits = match ast.get(node_id) {
            Node::Call { receiver, method, .. } => Some((*receiver, method.clone())),
            _ => None,
        };
        let const_name = match ast.get(node_id) {
            Node::ConstantRead { name, .. } if !name.is_empty() => Some(name.clone()),
            _ => None,
        };
        // Definition-site hover (hovering on a `class`/`module`/`def` name): a
        // signature line built from the node, no typing needed.
        let def_sig = match ast.get(node_id) {
            Node::ClassDef { name, superclass_path, .. } if !name.is_empty() => Some(match superclass_path {
                Some(sup) => format!("class {name} < {sup}"),
                None => format!("class {name}"),
            }),
            Node::ModuleDef { name, .. } if !name.is_empty() => Some(format!("module {name}")),
            Node::Definition { name: Some(n), params, .. } => Some(match params {
                Some(ps) if !ps.is_empty() => format!("def {n}({})", ps.join(", ")),
                _ => format!("def {n}"),
            }),
            _ => None,
        };
        let kind = crate::type_of::node_kind(ast.get(node_id));

        let body = if let Some((receiver, method)) = call_bits {
            let recv_ty = receiver.map(|r| typer.type_of(&ast, r, &env, &mut interner));
            let recv_disp = recv_ty
                .map(|rt| receiver_display(&ctx.index, &typer, &interner, rt))
                .unwrap_or_else(|| "self".to_string());
            let mut sig = format!("{recv_disp}#{method} → {type_render}");
            if let Some(cls) = recv_ty.and_then(|rt| ctx.index.class_name_of(&interner, rt)) {
                if let Some((min, max)) = ctx.index.method_arity(cls, &method) {
                    let max_s = max.map_or_else(|| "∞".to_string(), |m| m.to_string());
                    sig.push_str(&format!("  (arity {min}..{max_s})"));
                }
            }
            format!("```ruby\n{sig}\n```\n\n*rigor: Call*")
        } else if let Some(sig) = def_sig {
            format!("```ruby\n{sig}\n```\n\n*rigor: definition*")
        } else if let Some(name) = const_name {
            format!("```ruby\n{name} : {type_render}\n```\n\n*rigor: Constant*")
        } else {
            format!("```ruby\n{type_render}\n```\n\n*rigor: {kind}*")
        };
        Some((body, start, end))
    }));

    let (value, start, end) = result.ok().flatten()?;
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
// Completion (member-access method completion on `.` / `::`)
// ---------------------------------------------------------------------------

/// A stub method name injected at the cursor so a possibly-incomplete buffer
/// (`x.`, `x.up`) parses cleanly into a `Call` whose receiver we can type. Chosen
/// to be a valid, collision-unlikely lowercase identifier.
const COMPLETION_STUB: &str = "rigorCompletionHole";

/// Answer `textDocument/completion`: if the cursor sits after a `.`/`::` member
/// access, resolve the receiver's type and return its callable methods. Returns
/// `None` (a null completion) when the cursor isn't in a member-access context,
/// the buffer is unknown, or the receiver type is unresolved.
///
/// Robust to incomplete input via **placeholder injection**: a stub method name
/// is spliced in right after the separator (replacing any half-typed name), so
/// the parser yields a `Call { receiver, method: <stub> }` regardless of what the
/// user has typed. The receiver node is typed with the same `Typer` `hover`/`check`
/// use; its class drives instance- vs singleton-method enumeration. The half-typed
/// prefix is intentionally dropped — the client filters the full set by it.
fn completion(
    ctx: &ServerContext,
    buffers: &HashMap<String, String>,
    params: &CompletionParams,
) -> Option<CompletionResponse> {
    let tdp = &params.text_document_position;
    let text = buffers.get(&uri_key(&tdp.text_document.uri))?;
    let offset = position_to_offset(text, tdp.position)?;
    let bytes = text.as_bytes();

    // Scan back over any half-typed identifier to find where it starts.
    let mut ident_start = offset;
    while ident_start > 0 && is_ident_byte(bytes[ident_start - 1]) {
        ident_start -= 1;
    }
    // The separator must sit immediately before the (possibly empty) identifier:
    // `::` (constant/class scope) or a plain `.` (not part of a `..`/`...` range).
    let is_member_access = (ident_start >= 2 && &text[ident_start - 2..ident_start] == "::")
        || (ident_start >= 1
            && bytes[ident_start - 1] == b'.'
            && !(ident_start >= 2 && bytes[ident_start - 2] == b'.'));
    if !is_member_access {
        return None; // not a member-access completion context.
    }
    let stub_at = ident_start; // where the stub name begins (right after the sep).

    // Splice the stub in after the separator, dropping any half-typed name.
    let synth = format!("{}{}{}", &text[..ident_start], COMPLETION_STUB, &text[offset..]);

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower(&parse(synth.as_bytes()));
        // Our injected call is the unique `Call` whose method-name token starts
        // exactly at `stub_at`.
        let receiver = ast.iter().find_map(|(_, n)| match n {
            Node::Call { receiver, message_span, .. } if message_span.0 == stub_at => Some(*receiver),
            _ => None,
        })??;
        let source = SourceIndex::build(&ast, &ctx.index);
        let typer = Typer::with_source(&ctx.index, &source);
        let mut interner = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut interner);
        let ty = typer.type_of(&ast, receiver, &env, &mut interner);
        Some(method_names_for(&ctx.index, &typer, &interner, ty))
    }));

    let names = result.ok().flatten()?;
    if names.is_empty() {
        return None;
    }
    let items: Vec<CompletionItem> = names
        .into_iter()
        .map(|m| CompletionItem {
            label: m.to_string(),
            kind: Some(CompletionItemKind::METHOD),
            ..Default::default()
        })
        .collect();
    Some(CompletionResponse::Array(items))
}

/// Resolve the receiver type to the set of callable method names: singleton
/// (class-object) methods for a `Type::Singleton` receiver (a bare class
/// constant), else instance methods on the receiver's concrete core class. Empty
/// when the class isn't resolvable (a `Dynamic`/project/unknown receiver ⇒ no
/// completion, never a guess).
fn method_names_for(
    index: &CoreIndex,
    typer: &Typer<'_>,
    interner: &Interner,
    ty: TypeId,
) -> Vec<&'static str> {
    if let Type::Singleton(class) = interner.get(ty) {
        return match typer.source().class_name_for_id(*class) {
            Some(name) => index.singleton_method_names(name),
            None => Vec::new(),
        };
    }
    match index.class_name_of(interner, ty) {
        Some(name) => index.instance_method_names(name),
        None => Vec::new(),
    }
}

/// An ASCII identifier byte (`[A-Za-z0-9_]`) — used to scan a half-typed name.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Display a receiver's type as a class name for a hover signature: a bare class
/// constant renders `singleton(Name)`, a concrete core instance its class name,
/// and anything else falls back to the general type render (e.g. `Dynamic[top]`).
fn receiver_display(
    index: &CoreIndex,
    typer: &Typer<'_>,
    interner: &Interner,
    ty: TypeId,
) -> String {
    if let Type::Singleton(class) = interner.get(ty) {
        return typer
            .source()
            .class_name_for_id(*class)
            .map_or_else(|| "singleton(?)".to_string(), |n| format!("singleton({n})"));
    }
    index
        .class_name_of(interner, ty)
        .map_or_else(|| crate::type_of::render_type(interner, index, ty), |n| n.to_string())
}

// ---------------------------------------------------------------------------
// Document symbols (outline: classes / modules / methods)
// ---------------------------------------------------------------------------

/// Answer `textDocument/documentSymbol`: a nested outline of the buffer's
/// classes, modules, and methods, built from the lowered AST. Returns `None`
/// (null) for an unknown buffer or a file with no definitions. Panic-isolated.
fn document_symbols(
    buffers: &HashMap<String, String>,
    params: &DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
    let text = buffers.get(&uri_key(&params.text_document.uri))?;
    let syms = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower(&parse(text.as_bytes()));
        crate::outline::build(&ast).iter().map(|s| to_document_symbol(s, text)).collect::<Vec<_>>()
    }))
    .ok()?;
    if syms.is_empty() {
        return None;
    }
    Some(DocumentSymbolResponse::Nested(syms))
}

/// Adapt a shared [`crate::outline::SymNode`] into an LSP `DocumentSymbol`
/// (byte-offset spans → 0-based UTF-16 ranges; kind → `SymbolKind`).
fn to_document_symbol(s: &crate::outline::SymNode, text: &str) -> DocumentSymbol {
    use crate::outline::SymKind;
    let kids: Vec<DocumentSymbol> = s.children.iter().map(|c| to_document_symbol(c, text)).collect();
    let to_range = |(a, b): (usize, usize)| Range {
        start: offset_to_position(text, a),
        end: offset_to_position(text, b),
    };
    let kind = match s.kind {
        SymKind::Class => SymbolKind::CLASS,
        SymKind::Module => SymbolKind::MODULE,
        SymKind::Method => SymbolKind::METHOD,
    };
    #[allow(deprecated)] // `deprecated` field is required by the struct literal.
    DocumentSymbol {
        name: s.name.clone(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: to_range(s.full),
        selection_range: to_range(s.sel),
        children: if kids.is_empty() { None } else { Some(kids) },
    }
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

    /// Run completion at a 0-based (line, character) over a single buffer,
    /// returning the candidate labels (empty when None).
    fn complete(text: &str, line: u32, character: u32) -> Vec<String> {
        let mut buffers = HashMap::new();
        let uri: Uri = "file:///c.rb".parse().unwrap();
        buffers.insert(uri_key(&uri), text.to_string());
        let params = CompletionParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        match completion(&ctx(), &buffers, &params) {
            Some(CompletionResponse::Array(items)) => items.into_iter().map(|i| i.label).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn completion_instance_methods_on_a_string() {
        // `s = "hi"\ns.` — cursor right after the dot on line 2 (char 2).
        let labels = complete("s = \"hi\"\ns.\n", 1, 2);
        assert!(labels.contains(&"upcase".to_string()), "has upcase: {labels:?}");
        assert!(labels.contains(&"length".to_string()), "has length: {labels:?}");
    }

    #[test]
    fn completion_with_partial_prefix_still_lists_full_set() {
        // `s = "hi"\ns.up` — cursor after `up`; the half-typed prefix is dropped,
        // the FULL instance-method set is returned (client filters by `up`).
        let labels = complete("s = \"hi\"\ns.up\n", 1, 4);
        assert!(labels.contains(&"upcase".to_string()), "{labels:?}");
    }

    #[test]
    fn completion_integer_methods() {
        let labels = complete("n = 3\nn.\n", 1, 2);
        assert!(labels.contains(&"times".to_string()), "has times: {labels:?}");
    }

    #[test]
    fn completion_singleton_methods_on_a_class_constant() {
        // `Time.` — a bare toplevel RBS class constant types to Singleton(Time),
        // so completion offers class (singleton) methods like `now`.
        let labels = complete("Time.\n", 0, 5);
        assert!(labels.contains(&"now".to_string()), "has Time.now: {labels:?}");
    }

    #[test]
    fn completion_not_in_member_access_is_empty() {
        // A bare local write, cursor after `1` — no `.`/`::` before it.
        assert!(complete("x = 1\n", 0, 5).is_empty());
    }

    #[test]
    fn completion_on_dynamic_receiver_is_empty() {
        // `foo.` where `foo` is unbound ⇒ Dynamic receiver ⇒ no completion (no guess).
        assert!(complete("foo.\n", 0, 4).is_empty());
    }

    #[test]
    fn document_symbols_nest_methods_under_classes() {
        let src = "class Foo\n  def bar\n  end\n  def baz\n  end\nend\nmodule M\nend\n";
        let mut buffers = HashMap::new();
        let uri: Uri = "file:///s.rb".parse().unwrap();
        buffers.insert(uri_key(&uri), src.to_string());
        let params = DocumentSymbolParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let resp = document_symbols(&buffers, &params).expect("symbols");
        let roots = match resp {
            DocumentSymbolResponse::Nested(v) => v,
            _ => panic!("expected nested"),
        };
        // Two roots: class Foo, module M.
        assert_eq!(roots.len(), 2);
        let foo = roots.iter().find(|s| s.name == "Foo").expect("Foo");
        assert_eq!(foo.kind, SymbolKind::CLASS);
        // Foo nests two methods.
        let kids = foo.children.as_ref().expect("methods under Foo");
        let mut names: Vec<&str> = kids.iter().map(|k| k.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["bar", "baz"]);
        assert!(kids.iter().all(|k| k.kind == SymbolKind::METHOD));
        let m = roots.iter().find(|s| s.name == "M").expect("M");
        assert_eq!(m.kind, SymbolKind::MODULE);
    }

    #[test]
    fn document_symbols_empty_for_scriptish_file() {
        let mut buffers = HashMap::new();
        let uri: Uri = "file:///s.rb".parse().unwrap();
        buffers.insert(uri_key(&uri), "x = 1\nputs x\n".to_string());
        let params = DocumentSymbolParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        assert!(document_symbols(&buffers, &params).is_none());
    }

    #[test]
    fn hover_call_shows_receiver_method_signature() {
        // `s = "hi"\ns.upcase` — hover on `upcase` (line 2, char 3) shows a
        // `String#upcase → …` signature with the RBS arity.
        let mut buffers = HashMap::new();
        let uri: Uri = "file:///t.rb".parse().unwrap();
        buffers.insert(uri_key(&uri), "s = \"hi\"\ns.upcase\n".to_string());
        let params = HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line: 1, character: 2 },
            },
            work_done_progress_params: Default::default(),
        };
        let h = hover(&ctx(), &buffers, &params).expect("a hover");
        let HoverContents::Markup(m) = h.contents else { panic!("markup") };
        assert!(m.value.contains("String#upcase"), "signature: {}", m.value);
        assert!(m.value.contains("arity"), "arity shown: {}", m.value);
        assert!(m.value.contains("*rigor: Call*"), "{}", m.value);
    }

    /// Hover value at a 0-based (line, char) over a single buffer (or empty).
    fn hover_value(text: &str, line: u32, character: u32) -> String {
        let mut buffers = HashMap::new();
        let uri: Uri = "file:///h.rb".parse().unwrap();
        buffers.insert(uri_key(&uri), text.to_string());
        let params = HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
        };
        match hover(&ctx(), &buffers, &params) {
            Some(Hover { contents: HoverContents::Markup(m), .. }) => m.value,
            _ => String::new(),
        }
    }

    #[test]
    fn hover_on_a_def_shows_its_signature() {
        // `def greet(name)` — hover on the method name (line 1, char 4).
        let v = hover_value("def greet(name)\n  name\nend\n", 0, 4);
        assert!(v.contains("def greet(name)"), "{v}");
        assert!(v.contains("*rigor: definition*"), "{v}");
    }

    #[test]
    fn hover_on_a_class_shows_its_header() {
        // `class Foo < Bar` — hover on the class name (line 1, char 6).
        let v = hover_value("class Foo < Bar\nend\n", 0, 6);
        assert!(v.contains("class Foo < Bar"), "{v}");
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
