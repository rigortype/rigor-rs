use super::*;

/// A minimal tier-1 project context (empty core index, no sidecar, generation
/// 0) for the pure `compute_diagnostics` / `hover` / `completion` unit tests.
fn project() -> ProjectContext {
    project_with_config(&Config::default())
}

/// The same minimal context, built from an explicit config — the seam the
/// stage-3 stamp unit tests drive (`severity_profile:` / `severity_overrides:`
/// / `bleeding_edge:` all reach `compute_diagnostics` through here).
fn project_with_config(cfg: &Config) -> ProjectContext {
    project_with_config_rooted(cfg, Path::new("/nonexistent-rigor-lsp-test-root"))
}

/// …and rooted at an explicit project root, which is what the `exclude:` gate
/// needs: the matcher re-spells a buffer path relative to the configured roots,
/// so a test that drives it must name a real on-disk root.
fn project_with_config_rooted(cfg: &Config, root: &Path) -> ProjectContext {
    ProjectContext {
        generation: 0,
        index: Arc::new(CoreIndex::new()),
        disable: cfg.disable_matcher(),
        folder: None,
        stamp: SeverityStamp::from_config(cfg),
        exclude: ExcludeMatcher::from_config(root, cfg),
        overlay: None,
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
    let (diags, _, _) = compute_diagnostics(&project(), &BufferPaths::default(), "x = \"hi\"\nx.lenght\n");
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
        compute_diagnostics(&project(), &BufferPaths::default(), "x = \"hi\"\nx.lenght # rigor:disable undefined-method\n").0;
    assert!(diags.is_empty(), "inline disable suppresses the diagnostic");
}

#[test]
fn diagnostics_clean_source_is_empty() {
    let (diags, _, _) = compute_diagnostics(&project(), &BufferPaths::default(), "x = \"hi\"\nx.upcase\n");
    assert!(diags.is_empty());
}

// ---------------------------------------------------------------------
// Stage-3 parity tail (ADR-8 SeverityStamp + the bleeding-edge gate).
// The end-to-end LSP-vs-`check` equalities live in
// `tests/lsp_check_parity.rs`; these pin the unit-level contract.
// ---------------------------------------------------------------------

/// A `severity_overrides:` entry resolving to `off` removes the diagnostic
/// ENTIRELY — the presence mismatch the pre-stamp LSP had (`check` drops it,
/// the editor still published a marker).
#[test]
fn stage3_stamp_drops_an_off_resolution() {
    let cfg = Config::parse_or_warn(
        "severity_overrides:\n  call.undefined-method: off\n",
        "test",
    );
    let (diags, _, _) =
        compute_diagnostics(&project_with_config(&cfg), &BufferPaths::default(), "x = \"hi\"\nx.lenght\n");
    assert!(diags.is_empty(), "an `off` resolution is DROPPED, not merely downgraded");
}

/// A non-`off` resolution re-stamps the published severity (here authored
/// `error` → `info`), so the editor shows the project's configured level.
#[test]
fn stage3_stamp_restamps_a_resolved_severity() {
    let cfg = Config::parse_or_warn(
        "severity_overrides:\n  call.undefined-method: info\n",
        "test",
    );
    let (diags, _, _) =
        compute_diagnostics(&project_with_config(&cfg), &BufferPaths::default(), "x = \"hi\"\nx.lenght\n");
    assert_eq!(diags.len(), 1);
    assert_eq!(
        diags[0].severity,
        Some(DiagnosticSeverity::INFORMATION),
        "the published severity is the RESOLVED one, not the authored `error`"
    );
}

/// A FAMILY override reaches the rule too (`severity::resolve`'s exact-id →
/// family fallback), so the LSP honours `call: off` exactly as `check` does.
#[test]
fn stage3_stamp_honours_a_family_override() {
    let cfg = Config::parse_or_warn("severity_overrides:\n  call: off\n", "test");
    let (diags, _, _) =
        compute_diagnostics(&project_with_config(&cfg), &BufferPaths::default(), "x = \"hi\"\nx.lenght\n");
    assert!(diags.is_empty(), "the `call` family override covers call.undefined-method");
}

/// Acceptance 3: the `internal-error` sentinel BYPASSES the stamp. Even a
/// config that names it explicitly cannot silence a per-file panic (the
/// reference's `rule.nil?` short-circuit).
#[test]
fn stage3_stamp_never_silences_internal_error() {
    let cfg = Config::parse_or_warn(
        "severity_overrides:\n  internal-error: off\n  call.undefined-method: off\n",
        "test",
    );
    let stamp = SeverityStamp::from_config(&cfg);

    let mut panic_diag = rigor_rules::Diagnostic {
        rule_id: "internal-error",
        start_offset: 0,
        end_offset: 0,
        message: "internal panic: boom".to_string(),
        severity: Severity::Error,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    };
    assert!(stamp.apply(&mut panic_diag), "internal-error survives an `off` config");
    assert_eq!(panic_diag.severity, Severity::Error, "and is not re-stamped either");

    // The control: an ordinary rule under the SAME config IS dropped, so the
    // survival above is the bypass and not an inert override.
    let mut ordinary = rigor_rules::Diagnostic {
        rule_id: "call.undefined-method",
        ..panic_diag.clone()
    };
    assert!(!stamp.apply(&mut ordinary), "an ordinary rule under the same config drops");
}

/// The `static.value-use.void` activation gate is `check`'s: off by default
/// (every shipped profile has it `:off`), promoted by the `use-of-void-value`
/// bleeding-edge feature, and resurrectable by a user override alone.
#[test]
fn stage3_void_rule_gate_matches_check() {
    let gate = |yaml: &str| SeverityStamp::from_config(&Config::parse_or_warn(yaml, "test")).void_rule_active;
    assert!(!gate(""), "off by default");
    assert!(!gate("bleeding_edge: false\n"));
    assert!(gate("bleeding_edge: true\n"), "`all` activates the feature");
    assert!(gate("bleeding_edge:\n  - use-of-void-value\n"));
    assert!(!gate("bleeding_edge:\n  - some-other-feature\n"));
    assert!(
        gate("severity_overrides:\n  static.value-use.void: warning\n"),
        "a user override alone resurrects the rule (it outranks the profile table)"
    );
    assert!(
        !gate("bleeding_edge: true\nseverity_overrides:\n  static.value-use.void: off\n"),
        "and a user `off` outranks the bleeding-edge promotion"
    );
}

#[test]
fn hover_reports_a_type() {
    let mut buffers = BufferTable::new();
    let uri: Uri = "file:///t.rb".parse().unwrap();
    buffers.open(&uri, "n = 42\n".to_string(), 1);
    let params = HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            position: Position { line: 0, character: 4 }, // on `42`
        },
        work_done_progress_params: Default::default(),
    };
    let h = hover(&project(), &buffers, &params, None).expect("a hover");
    match h.contents {
        HoverContents::Markup(m) => assert!(m.value.contains("42"), "{}", m.value),
        _ => panic!("expected markup hover"),
    }
}

/// Run completion at a 0-based (line, character) over a single buffer,
/// returning the candidate labels (empty when None).
fn complete(text: &str, line: u32, character: u32) -> Vec<String> {
    let mut buffers = BufferTable::new();
    let uri: Uri = "file:///c.rb".parse().unwrap();
    buffers.open(&uri, text.to_string(), 1);
    let params = CompletionParams {
        text_document_position: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    };
    match completion(&project(), &buffers, &params, None) {
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

/// LSP v4 item 1: `Process::` offers the NESTED CONSTANTS, not the singleton
/// methods it used to return.
#[test]
fn completion_namespace_children_on_scope_operator() {
    let labels = complete("Process::\n", 0, 9);
    assert!(labels.contains(&"Status".to_string()), "nested class: {labels:?}");
    assert!(labels.contains(&"UID".to_string()), "nested module: {labels:?}");
    assert!(!labels.contains(&"wait".to_string()), "not a singleton method: {labels:?}");
}

/// An uppercase-initial partial is still a constant position; the prefix is
/// dropped and the full child set returned (the client filters).
#[test]
fn completion_namespace_children_with_uppercase_partial() {
    let labels = complete("Process::St\n", 0, 11);
    assert!(labels.contains(&"Status".to_string()), "{labels:?}");
}

/// …but a LOWERCASE partial after `::` is a class-method call, which keeps
/// the singleton-method behaviour (this is where the reference keeps it too).
#[test]
fn completion_lowercase_after_scope_operator_stays_on_methods() {
    let labels = complete("Time::no\n", 0, 8);
    assert!(labels.contains(&"now".to_string()), "singleton method: {labels:?}");
    assert!(!labels.contains(&"Status".to_string()), "{labels:?}");
}

/// A namespace with no children (and an unknown one) yields a null
/// completion rather than an empty list — matching the reference's
/// `return nil if children.empty?`.
#[test]
fn completion_namespace_without_children_is_empty() {
    assert!(complete("Symbol::\n", 0, 8).is_empty());
    assert!(complete("NoSuchThing::\n", 0, 13).is_empty());
}

/// The parent comes from the AST, so a non-constant left side offers nothing
/// (`[1, 2]::Foo` is not a namespace).
#[test]
fn completion_namespace_on_non_constant_parent_is_empty() {
    assert!(complete("[1, 2]::\n", 0, 8).is_empty());
}

/// LSP v4 item 2: a PRIVATE method is never offered on an explicit receiver.
#[test]
fn completion_excludes_private_methods() {
    let labels = complete("s = \"hi\"\ns.\n", 1, 2);
    assert!(!labels.contains(&"respond_to_missing?".to_string()), "{labels:?}");
    assert!(!labels.contains(&"method_missing".to_string()), "{labels:?}");
    assert!(labels.contains(&"upcase".to_string()), "public still offered: {labels:?}");
}

/// …including on a class object, whose surface folds in `Module`'s private
/// reflection methods.
#[test]
fn completion_excludes_private_methods_on_a_class_object() {
    let labels = complete("Time.\n", 0, 5);
    assert!(!labels.contains(&"module_function".to_string()), "{labels:?}");
    assert!(!labels.contains(&"refine".to_string()), "{labels:?}");
    assert!(labels.contains(&"now".to_string()), "{labels:?}");
}

/// LSP v4 item 3: a union receiver offers only the methods present on EVERY
/// arm — `upcase` (String-only) and `times` (Integer-only) are both out,
/// while the shared `Object`/`Kernel` surface remains.
#[test]
fn completion_on_a_union_receiver_intersects_the_arms() {
    let src = "x = ARGV.empty? ? \"hi\" : 3\nx.\n";
    let labels = complete(src, 1, 2);
    assert!(!labels.is_empty(), "a union receiver still completes: {labels:?}");
    assert!(!labels.contains(&"upcase".to_string()), "String-only: {labels:?}");
    assert!(!labels.contains(&"times".to_string()), "Integer-only: {labels:?}");
    assert!(labels.contains(&"frozen?".to_string()), "shared surface: {labels:?}");
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
    let mut buffers = BufferTable::new();
    let uri: Uri = "file:///s.rb".parse().unwrap();
    buffers.open(&uri, src.to_string(), 1);
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
    let mut buffers = BufferTable::new();
    let uri: Uri = "file:///s.rb".parse().unwrap();
    buffers.open(&uri, "x = 1\nputs x\n".to_string(), 1);
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
    let mut buffers = BufferTable::new();
    let uri: Uri = "file:///t.rb".parse().unwrap();
    buffers.open(&uri, "s = \"hi\"\ns.upcase\n".to_string(), 1);
    let params = HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            position: Position { line: 1, character: 2 },
        },
        work_done_progress_params: Default::default(),
    };
    let h = hover(&project(), &buffers, &params, None).expect("a hover");
    let HoverContents::Markup(m) = h.contents else { panic!("markup") };
    assert!(m.value.contains("String#upcase"), "signature: {}", m.value);
    assert!(m.value.contains("arity"), "arity shown: {}", m.value);
    assert!(m.value.contains("*rigor: Call*"), "{}", m.value);
}

/// Hover value at a 0-based (line, char) over a single buffer (or empty).
fn hover_value(text: &str, line: u32, character: u32) -> String {
    let mut buffers = BufferTable::new();
    let uri: Uri = "file:///h.rb".parse().unwrap();
    buffers.open(&uri, text.to_string(), 1);
    let params = HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
    };
    match hover(&project(), &buffers, &params, None) {
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
    assert!(hover(&project(), &BufferTable::new(), &params, None).is_none());
}

#[test]
fn buffer_table_records_version_and_dirty() {
    // The BufferTable metadata (version, dirty) is maintained per ADR-0029
    // even though S1 branches on neither — the S2/S3 consumers arrive later.
    let mut t = BufferTable::new();
    let uri: Uri = "file:///b.rb".parse().unwrap();
    t.open(&uri, "a\n".to_string(), 1);
    let e = t.entries.get(&uri_key(&uri)).unwrap();
    assert_eq!(e.version, 1);
    assert!(!e.dirty, "an opened buffer is clean");
    t.change(&uri, "b\n".to_string(), 2);
    let e = t.entries.get(&uri_key(&uri)).unwrap();
    assert_eq!(e.version, 2);
    assert!(e.dirty, "a changed buffer is dirty");
    assert_eq!(t.text(&uri), Some("b\n"));
    t.close(&uri);
    assert_eq!(t.text(&uri), None);
}

// ---------------------------------------------------------------------
// Debouncer: pure, deterministic unit tests (explicit `Instant`s, no sleep).
// These prove the coalescing + cancel + earliest/take_due invariants without
// any wall-clock dependency — the timing seam the integration tests lean on.
// ---------------------------------------------------------------------

#[test]
fn debouncer_coalesces_and_last_deadline_wins() {
    let mut d = Debouncer::new();
    let u: Uri = "file:///a.rb".parse().unwrap();
    let t0 = Instant::now();
    // Two schedules for the same URI within the window: the second wins.
    d.schedule(&u, t0 + Duration::from_millis(200));
    d.schedule(&u, t0 + Duration::from_millis(500));
    assert_eq!(d.pending.len(), 1, "one pending entry per URI (coalesced)");
    assert_eq!(d.earliest(), Some(t0 + Duration::from_millis(500)));
    // Not due at +300 (the deadline moved out to +500).
    assert!(d.take_due(t0 + Duration::from_millis(300)).is_empty());
    // Due at +600: exactly the final entry, then removed.
    let due = d.take_due(t0 + Duration::from_millis(600));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].as_str(), "file:///a.rb");
    assert!(d.earliest().is_none(), "a fired entry is removed");
}

#[test]
fn debouncer_cancel_drops_pending() {
    let mut d = Debouncer::new();
    let u: Uri = "file:///a.rb".parse().unwrap();
    let t0 = Instant::now();
    d.schedule(&u, t0 + Duration::from_millis(100));
    d.cancel(&u); // didClose
    assert!(d.earliest().is_none());
    assert!(
        d.take_due(t0 + Duration::from_millis(200)).is_empty(),
        "a cancelled publish never fires"
    );
    d.cancel(&u); // idempotent
}

#[test]
fn debouncer_earliest_is_the_min_across_uris() {
    let mut d = Debouncer::new();
    let a: Uri = "file:///a.rb".parse().unwrap();
    let b: Uri = "file:///b.rb".parse().unwrap();
    let t0 = Instant::now();
    d.schedule(&a, t0 + Duration::from_millis(300));
    d.schedule(&b, t0 + Duration::from_millis(100));
    assert_eq!(d.earliest(), Some(t0 + Duration::from_millis(100)));
    // Only `b` is due at +150; `a`'s later deadline stays pending.
    let due = d.take_due(t0 + Duration::from_millis(150));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].as_str(), "file:///b.rb");
    assert_eq!(d.earliest(), Some(t0 + Duration::from_millis(300)));
}

// ---------------------------------------------------------------------
// Integration tests: the REAL loop over an in-memory connection.
//
// These drive `main_loop` through `lsp_server::Connection::memory()` and
// assert the EXACT published-message sequence. The expected sequences were
// captured from the pre-refactor (inline-publish) loop as the golden
// reference; the S1 `select!`/worker-channel refactor must reproduce them
// byte-for-byte.
// ---------------------------------------------------------------------

use lsp_server::{Notification, Request, RequestId};
use std::thread;
use std::time::Duration;

/// A running server loop over an in-memory connection, plus the client end.
struct Harness {
    client: Connection,
    server: Option<thread::JoinHandle<()>>,
}

impl Harness {
    /// Spawn the server loop with the default (200 ms) debounce.
    fn start() -> Self {
        Self::start_with_debounce(DEBOUNCE_DEFAULT)
    }

    /// Spawn the server loop on a thread (with an injected debounce interval)
    /// and complete the LSP handshake. Timing tests pass a SMALL interval
    /// (assert the deferred publish eventually arrives) or a LARGE one (assert
    /// it does NOT fire within a synchronous round-trip) — never a value the
    /// assertions race against.
    fn start_with_debounce(debounce: Duration) -> Self {
        Self::start_with_gate(debounce, production_gate())
    }

    /// Spawn the server loop with an injected debounce AND a worker gate (S3
    /// concurrency tests). The gate is called at the start of every rayon
    /// worker with the buffer version + project generation, so a test can hold a
    /// worker mid-flight (block until released) or force a panic — driving the
    /// version / generation / epoch stale-drop, one-in-flight, and never-stuck
    /// lifecycle deterministically, without any dependence on real rayon timing.
    /// The client advertises NO capabilities (no dynamic registration).
    fn start_with_gate(debounce: Duration, worker_gate: Arc<WorkerGate>) -> Self {
        Self::start_full(debounce, worker_gate, serde_json::json!({}))
    }

    /// Spawn the server loop, driving the client `initialize` with the given
    /// `client_caps` (S4): the server derives `watched_files_dynamic_registration`
    /// from the InitializeParams it receives, exactly as production does, so a
    /// test can assert the `client/registerCapability` handshake (or its absence).
    fn start_full(
        debounce: Duration,
        worker_gate: Arc<WorkerGate>,
        client_caps: serde_json::Value,
    ) -> Self {
        // No project root ⇒ `paths: ["lib"]` under a nonexistent dir ⇒ zero
        // project files ⇒ overlay OFF: every pre-S4b test keeps the exact
        // single-file `SourceIndex::build` behaviour it was written against.
        Self::start_project(
            debounce,
            worker_gate,
            client_caps,
            PathBuf::from("/nonexistent-rigor-lsp-test-root"),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        )
    }

    /// Spawn the server loop over a real on-disk project `root` with an
    /// injected overlay scale-guard `budget` (S4b). This is the authentic
    /// production boot: the tier-1 overlay is built by the SAME
    /// [`build_overlay`] call `run_stdio` makes, the config comes from
    /// `<root>/.rigor.yml` through the SAME [`read_project_config`], the guard
    /// is evaluated the same way, and both `window/showMessage` disclosures go
    /// out on the same connection — only the root and the budget are injected,
    /// so no test has to mutate the process-global cwd or race a wall clock.
    ///
    /// The pre-reload harness took a PARSED `Config` and injected it, which was
    /// "the same thing minus the file" only while the config was read exactly
    /// once. It is not any more: an injected config has no file behind it, so
    /// the first structural invalidation would reload it away — a divergence
    /// from production that the assertions could not have seen. Tests now write
    /// the YAML a user would write.
    fn start_project(
        debounce: Duration,
        worker_gate: Arc<WorkerGate>,
        client_caps: serde_json::Value,
        root: PathBuf,
        overlay_budget: Duration,
    ) -> Self {
        let (server_conn, client) = Connection::memory();
        let handle = thread::spawn(move || {
            let caps = serde_json::to_value(server_capabilities()).unwrap();
            // The authentic path: read the client's capabilities from the
            // InitializeParams the handshake returns (not discarded).
            let init_params = server_conn.initialize(caps).unwrap();
            let config_read = read_project_config(&root);
            let config_broken = config_read.is_err();
            if let Err(reason) = &config_read {
                send_show_message(
                    &server_conn,
                    MessageType::WARNING,
                    config_broken_at_startup_message(reason),
                )
                .unwrap();
            }
            let cfg = config_read.unwrap_or_default();
            let ctx = ServerContext {
                debounce,
                worker_gate,
                watched_files_dynamic_registration:
                    client_supports_watched_files_registration(&init_params),
                project_root: root.clone(),
                overlay_budget,
            };
            // `CoreIndex::new()` (not `build_core_index`) keeps the tests fast
            // and hermetic — no Gemfile.lock probing under a temp root.
            let index = Arc::new(CoreIndex::new());
            let build = build_overlay(&root, &cfg, &index);
            // The authentic startup posture: the first build is the guard's
            // first sample and can never disable on its own (hysteresis).
            let mut guard = OverlayGuard::new();
            if build.file_count > 0 {
                guard.record(build.merge, overlay_budget);
            }
            let overlay = (guard.enabled && build.file_count > 0).then_some(build.files);
            let project = Arc::new(ProjectContext {
                generation: 0,
                index,
                disable: cfg.disable_matcher(),
                folder: None,
                stamp: SeverityStamp::from_config(&cfg),
                exclude: ExcludeMatcher::from_config(&root, &cfg),
                overlay,
            });
            main_loop(&server_conn, &ctx, project, cfg, guard, config_broken).unwrap();
        });
        // Client-side handshake: initialize request → response → initialized.
        client
            .sender
            .send(Message::Request(Request::new(
                RequestId::from(1),
                "initialize".to_string(),
                serde_json::json!({ "capabilities": client_caps }),
            )))
            .unwrap();
        client
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("initialize response");
        client
            .sender
            .send(Message::Notification(Notification::new(
                "initialized".to_string(),
                serde_json::json!({}),
            )))
            .unwrap();
        Harness { client, server: Some(handle) }
    }

    fn notify(&self, method: &str, params: serde_json::Value) {
        self.client
            .sender
            .send(Message::Notification(Notification::new(method.to_string(), params)))
            .unwrap();
    }

    fn request(&self, id: i32, method: &str, params: serde_json::Value) {
        self.client
            .sender
            .send(Message::Request(Request::new(
                RequestId::from(id),
                method.to_string(),
                params,
            )))
            .unwrap();
    }

    fn recv(&self) -> Message {
        self.client
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("a server message")
    }

    /// Wait up to `dur` for a message; `None` on timeout. Used to assert a
    /// debounced publish does NOT arrive before its interval elapses.
    fn try_recv(&self, dur: Duration) -> Option<Message> {
        self.client.receiver.recv_timeout(dur).ok()
    }

    /// The next message, asserted to be a `window/showMessage`, parsed. The
    /// disclosure channel for the sidecar posture, the overlay guard, and the
    /// config-reload state.
    fn recv_show_message(&self) -> ShowMessageParams {
        match self.recv() {
            Message::Notification(n) if n.method == "window/showMessage" => {
                serde_json::from_value(n.params).unwrap()
            }
            other => panic!("expected window/showMessage, got {other:?}"),
        }
    }

    /// The next message, asserted to be a `publishDiagnostics`, parsed.
    fn recv_diags(&self) -> PublishDiagnosticsParams {
        match self.recv() {
            Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                serde_json::from_value(n.params).unwrap()
            }
            other => panic!("expected publishDiagnostics, got {other:?}"),
        }
    }

    fn shutdown(&mut self) {
        self.request(999, "shutdown", serde_json::json!(null));
        match self.recv() {
            Message::Response(r) if r.id == RequestId::from(999) => {}
            other => panic!("expected shutdown response, got {other:?}"),
        }
        self.notify("exit", serde_json::json!(null));
        if let Some(h) = self.server.take() {
            h.join().unwrap();
        }
    }
}

/// A `didOpen` params JSON for `uri` / `text` / `version`.
fn open_params(uri: &str, text: &str, version: i32) -> serde_json::Value {
    serde_json::json!({
        "textDocument": { "uri": uri, "languageId": "ruby", "version": version, "text": text }
    })
}

#[test]
fn integration_didopen_publishes_one_diagnostic() {
    let mut h = Harness::start();
    h.notify(
        "textDocument/didOpen",
        open_params("file:///g.rb", "x = \"hi\"\nx.lenght\n", 1),
    );
    let d = h.recv_diags();
    assert_eq!(d.uri.as_str(), "file:///g.rb");
    assert_eq!(d.diagnostics.len(), 1, "exactly one diagnostic");
    let diag = &d.diagnostics[0];
    assert_eq!(
        diag.code,
        Some(NumberOrString::String("call.undefined-method".to_string()))
    );
    assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(diag.source.as_deref(), Some("rigor"));
    assert_eq!(diag.range.start, Position { line: 1, character: 2 });
    assert_eq!(diag.range.end, Position { line: 1, character: 8 });
    h.shutdown();
}

#[test]
fn integration_didchange_to_clean_republishes_empty() {
    // S2: didChange is now DEBOUNCED. With a small injected interval the
    // deferred publish still arrives (recv_diags waits up to 10 s); we assert
    // only that it arrives and is empty — no coalescing race here (one change).
    let mut h = Harness::start_with_debounce(Duration::from_millis(10));
    h.notify(
        "textDocument/didOpen",
        open_params("file:///g.rb", "x = \"hi\"\nx.lenght\n", 1),
    );
    // didOpen publishes IMMEDIATELY (not debounced): the one diagnostic.
    assert_eq!(h.recv_diags().diagnostics.len(), 1);
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb", "version": 2 },
            "contentChanges": [ { "text": "x = \"hi\"\nx.upcase\n" } ]
        }),
    );
    // The debounced publish fires ~10 ms later, carrying the (clean) content.
    let d = h.recv_diags();
    assert_eq!(d.uri.as_str(), "file:///g.rb");
    assert!(d.diagnostics.is_empty(), "clean content republishes an empty set");
    h.shutdown();
}

#[test]
fn integration_didchange_deferred_until_interval() {
    // A didChange's publish does NOT appear before the debounce interval, but
    // DOES after. Interval 150 ms; we assert nothing arrives in a 20 ms window
    // (comfortably < 150 ms, so no race), then that the publish arrives.
    let mut h = Harness::start_with_debounce(Duration::from_millis(150));
    h.notify("textDocument/didOpen", open_params("file:///g.rb", "n = 42\n", 1));
    assert!(h.recv_diags().diagnostics.is_empty(), "clean didOpen → empty (immediate)");
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb", "version": 2 },
            "contentChanges": [ { "text": "x = \"hi\"\nx.lenght\n" } ]
        }),
    );
    // Not yet: the deadline is 150 ms out, this window is only 20 ms.
    assert!(
        h.try_recv(Duration::from_millis(20)).is_none(),
        "no publish before the debounce interval elapses"
    );
    // After the interval: the debounced publish with the typo diagnostic.
    let d = h.recv_diags();
    assert_eq!(d.diagnostics.len(), 1, "debounced publish carries the diagnostic");
    assert_eq!(
        d.diagnostics[0].code,
        Some(NumberOrString::String("call.undefined-method".to_string()))
    );
    h.shutdown();
}

#[test]
fn integration_rapid_didchanges_coalesce_to_one_publish() {
    // Two rapid didChanges → exactly ONE publish carrying the FINAL content.
    // Both notifications are queued to the connection before the 120 ms
    // deadline can elapse, so the loop processes #1 (schedule) then #2
    // (reschedule) microseconds apart and fires once. The strict
    // last-writer-wins invariant is also proven deterministically in
    // `debouncer_coalesces_and_last_deadline_wins`.
    let mut h = Harness::start_with_debounce(Duration::from_millis(120));
    h.notify("textDocument/didOpen", open_params("file:///g.rb", "n = 42\n", 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    // #1: clean. #2 (final): a typo → one diagnostic.
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb", "version": 2 },
            "contentChanges": [ { "text": "x = \"hi\"\nx.upcase\n" } ]
        }),
    );
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb", "version": 3 },
            "contentChanges": [ { "text": "x = \"hi\"\nx.lenght\n" } ]
        }),
    );
    // Exactly one publish, of the FINAL content.
    let d = h.recv_diags();
    assert_eq!(d.diagnostics.len(), 1, "coalesced: one publish of the final content");
    assert_eq!(
        d.diagnostics[0].code,
        Some(NumberOrString::String("call.undefined-method".to_string()))
    );
    // No second publish: a hover round-trips as the very next message.
    h.request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb" },
            "position": { "line": 0, "character": 0 }
        }),
    );
    match h.recv() {
        Message::Response(r) => assert_eq!(r.id, RequestId::from(2)),
        other => panic!("expected hover response (a publish would mean a leaked debounce), got {other:?}"),
    }
    h.shutdown();
}

#[test]
fn integration_didclose_cancels_pending_no_stale_publish() {
    // A didClose BEFORE the deadline cancels the pending publish and clears
    // markers; NO stale publish fires afterward. A 30 s interval guarantees
    // the debounce cannot fire during this millisecond-scale test.
    let mut h = Harness::start_with_debounce(Duration::from_secs(30));
    h.notify("textDocument/didOpen", open_params("file:///g.rb", "n = 42\n", 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    // A change (schedules a publish 30 s out) then an immediate close.
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb", "version": 2 },
            "contentChanges": [ { "text": "x = \"hi\"\nx.lenght\n" } ]
        }),
    );
    h.notify(
        "textDocument/didClose",
        serde_json::json!({ "textDocument": { "uri": "file:///g.rb" } }),
    );
    // The didClose empty clear.
    let d = h.recv_diags();
    assert!(d.diagnostics.is_empty(), "didClose clears diagnostics");
    // No stale debounced publish: a hover round-trips as the next message
    // (the buffer is closed, so the result is null — but it's a Response).
    h.request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb" },
            "position": { "line": 0, "character": 0 }
        }),
    );
    match h.recv() {
        Message::Response(r) => assert_eq!(r.id, RequestId::from(2)),
        other => panic!("expected hover response (a publish would be a stale debounce), got {other:?}"),
    }
    h.shutdown();
}

#[test]
fn integration_hover_during_debounce_window_sees_latest_text_no_publish() {
    // Hover during the debounce window is answered SYNCHRONOUSLY from the
    // latest buffer text, and no publish precedes the response. 30 s interval
    // so the deferred publish cannot fire mid-test.
    let mut h = Harness::start_with_debounce(Duration::from_secs(30));
    h.notify("textDocument/didOpen", open_params("file:///g.rb", "s = \"hi\"\ns.upcase\n", 1));
    assert!(h.recv_diags().diagnostics.is_empty(), "clean didOpen → empty (immediate)");
    // Edit to a new expression; the buffer updates synchronously, publish
    // deferred 30 s.
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb", "version": 2 },
            "contentChanges": [ { "text": "n = 42\n" } ]
        }),
    );
    // Hover on the `42` in the LATEST text: the response comes back (not a
    // publish), and it reflects the edited content.
    h.request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": "file:///g.rb" },
            "position": { "line": 0, "character": 4 }
        }),
    );
    match h.recv() {
        Message::Response(r) => {
            assert_eq!(r.id, RequestId::from(2));
            let hover: Option<Hover> = serde_json::from_value(r.result.unwrap()).unwrap();
            let Some(Hover { contents: HoverContents::Markup(m), .. }) = hover else {
                panic!("expected a markup hover from the latest buffer text");
            };
            assert!(m.value.contains("42"), "hover sees the edited text: {}", m.value);
        }
        other => panic!("expected hover response (a publish would mean the debounce leaked), got {other:?}"),
    }
    h.shutdown();
}

#[test]
fn integration_didclose_publishes_empty() {
    let mut h = Harness::start();
    h.notify(
        "textDocument/didOpen",
        open_params("file:///g.rb", "x = \"hi\"\nx.lenght\n", 1),
    );
    assert_eq!(h.recv_diags().diagnostics.len(), 1);
    h.notify(
        "textDocument/didClose",
        serde_json::json!({ "textDocument": { "uri": "file:///g.rb" } }),
    );
    let d = h.recv_diags();
    assert_eq!(d.uri.as_str(), "file:///g.rb");
    assert!(d.diagnostics.is_empty(), "didClose clears diagnostics");
    h.shutdown();
}

#[test]
fn integration_hover_request_answers_like_inline() {
    let mut h = Harness::start();
    h.notify("textDocument/didOpen", open_params("file:///h.rb", "n = 42\n", 1));
    // A clean buffer's didOpen publishes an empty set first.
    assert!(h.recv_diags().diagnostics.is_empty());
    h.request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": "file:///h.rb" },
            "position": { "line": 0, "character": 4 }
        }),
    );
    match h.recv() {
        Message::Response(r) => {
            assert_eq!(r.id, RequestId::from(2));
            let hover: Option<Hover> = serde_json::from_value(r.result.unwrap()).unwrap();
            let Some(Hover { contents: HoverContents::Markup(m), .. }) = hover else {
                panic!("expected a markup hover");
            };
            assert!(m.value.contains("42"), "hover value: {}", m.value);
        }
        other => panic!("expected hover response, got {other:?}"),
    }
    h.shutdown();
}

// ---------------------------------------------------------------------
// S3 concurrency: real rayon workers, driven DETERMINISTICALLY via the
// worker-gate seam (hold a worker mid-flight / force a panic) + hover
// round-trips as synchronization barriers. NONE of these depend on
// wall-clock races: every ordering is pinned by the gate + FIFO message
// processing, so the version-guard / one-in-flight / no-lost-update /
// never-stuck invariants are established without a timing window.
// ---------------------------------------------------------------------

/// A worker-gate the test controls: a worker whose `version` is in `hold`
/// blocks until [`GateHandle::release`] is called for it; a worker whose
/// `version` is in `panic_on` panics (caught by the worker's `catch_unwind`).
struct GateHandle {
    releases: HashMap<i32, crossbeam_channel::Sender<()>>,
    gate: Arc<WorkerGate>,
}

impl GateHandle {
    /// Release a held worker so it proceeds to compute + send its result.
    fn release(&self, version: i32) {
        if let Some(tx) = self.releases.get(&version) {
            let _ = tx.send(());
        }
    }
}

/// Build a controllable [`WorkerGate`]: workers at a `hold` version block on a
/// per-version rendezvous until released; workers at a `panic_on` version
/// panic. One held worker per version (the tests hold exactly one).
fn gate_holding(hold: &[i32], panic_on: &[i32]) -> GateHandle {
    let mut releases = HashMap::new();
    let mut recvs: HashMap<i32, crossbeam_channel::Receiver<()>> = HashMap::new();
    for &v in hold {
        let (tx, rx) = crossbeam_channel::unbounded();
        releases.insert(v, tx);
        recvs.insert(v, rx);
    }
    let panics: HashSet<i32> = panic_on.iter().copied().collect();
    let gate: Arc<WorkerGate> = Arc::new(move |version: i32, _generation: u64| {
        if panics.contains(&version) {
            panic!("test gate: forced panic for version {version}");
        }
        if let Some(rx) = recvs.get(&version) {
            let _ = rx.recv(); // block until the test releases this version.
        }
    });
    GateHandle { releases, gate }
}

/// A `didChange` params JSON (FULL sync: the whole buffer as one change).
fn change_params(uri: &str, text: &str, version: i32) -> serde_json::Value {
    serde_json::json!({
        "textDocument": { "uri": uri, "version": version },
        "contentChanges": [ { "text": text } ]
    })
}

/// Round-trip a hover request as a SYNCHRONIZATION BARRIER: the loop services
/// messages in FIFO order, so once this response returns, every earlier message
/// (e.g. a preceding `didChange`) has been fully processed. The next server
/// message MUST be the hover `Response`; a `publishDiagnostics` arriving here
/// would mean a stale/leaked diagnostic escaped.
fn hover_sync(h: &Harness, id: i32, uri: &str) {
    h.request(
        id,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 0 }
        }),
    );
    match h.recv() {
        Message::Response(r) => assert_eq!(r.id, RequestId::from(id)),
        other => panic!(
            "expected hover response (a publish here would mean a leaked/stale diagnostic), got {other:?}"
        ),
    }
}

const TYPO: &str = "x = \"hi\"\nx.lenght\n"; // one `call.undefined-method`.
const CLEAN: &str = "x = \"hi\"\nx.upcase\n"; // zero diagnostics.

#[test]
fn integration_s3_edit_during_flight_drops_stale_and_publishes_final_once() {
    // The core no-lost-update case. Hold the v1 worker mid-flight; edit to v2
    // while it is blocked; release v1. Its v1 result is STALE (buffer is v2) →
    // DROPPED, and a re-dispatch analyses v2 → the FINAL content publishes
    // exactly once. 30 s debounce so ONLY the stale-drop re-dispatch (never the
    // clock) drives the final publish — fully deterministic.
    let g = gate_holding(&[1], &[]);
    let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
    // v1 = a TYPO (1 diag). If v1 leaked, we'd observe a 1-diagnostic publish.
    h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1));
    // Edit to v2 = CLEAN while the v1 worker is blocked in the gate. The buffer
    // updates synchronously; no second worker spawns (one-in-flight).
    h.notify("textDocument/didChange", change_params("file:///g.rb", CLEAN, 2));
    // Barrier: guarantee the loop has processed the v2 didChange before release.
    hover_sync(&h, 100, "file:///g.rb");
    // Release v1: Computed{v1} arrives, current==v2 ⇒ stale ⇒ dropped +
    // re-dispatched ⇒ v2 worker ⇒ publishes the CLEAN final content.
    g.release(1);
    let d = h.recv_diags();
    assert!(
        d.diagnostics.is_empty(),
        "the FINAL (v2, clean) content is published; the stale v1 was dropped: {:?}",
        d.diagnostics
    );
    // Exactly once: no further publish (the debounce was cancelled when the v2
    // worker was spawned). A hover round-trips as the very next message.
    hover_sync(&h, 101, "file:///g.rb");
    h.shutdown();
}

#[test]
fn integration_s3_burst_edits_coalesce_to_final_no_stale_publish() {
    // Concurrency stress: many rapid edits while ONE worker is in flight. The
    // one-in-flight gate means v2..v5 never spawn a worker; only the LAST
    // version is re-dispatched after the stale v1 drop → exactly one publish of
    // the final content, and NO intermediate/stale version ever publishes.
    let g = gate_holding(&[1], &[]);
    let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
    h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1)); // v1 held
    // Burst: v2..v5 TYPO (would each be 1 diag), v6 CLEAN (the final content).
    for v in 2..=5 {
        h.notify("textDocument/didChange", change_params("file:///g.rb", TYPO, v));
    }
    h.notify("textDocument/didChange", change_params("file:///g.rb", CLEAN, 6));
    hover_sync(&h, 100, "file:///g.rb"); // all edits processed; buffer == v6.
    g.release(1); // v1 stale ⇒ dropped ⇒ re-dispatch v6 ⇒ publish CLEAN.
    let d = h.recv_diags();
    assert!(
        d.diagnostics.is_empty(),
        "only the final v6 (clean) content publishes; no intermediate/stale version escaped: {:?}",
        d.diagnostics
    );
    hover_sync(&h, 101, "file:///g.rb"); // exactly one publish.
    h.shutdown();
}

#[test]
fn integration_s3_worker_panic_does_not_stick_the_uri() {
    // A panicking worker must not strand its URI in flight. v1's worker panics
    // in the gate → caught by the worker's `catch_unwind` → an empty Computed is
    // still sent → in-flight clears → v1 (current) publishes empty. A LATER edit
    // is then analysed + published normally, proving the URI is not stuck.
    let g = gate_holding(&[], &[1]);
    let mut h = Harness::start_with_gate(Duration::from_millis(10), g.gate.clone());
    h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1));
    // The panicked v1 worker yields a caught (empty) result — not a hang.
    let d = h.recv_diags();
    assert!(
        d.diagnostics.is_empty(),
        "a panicked worker yields a caught empty result, not a stuck URI: {:?}",
        d.diagnostics
    );
    // Not stuck: a subsequent edit (v2, a typo) is dispatched (debounced 10 ms)
    // and published like normal.
    h.notify("textDocument/didChange", change_params("file:///g.rb", TYPO, 2));
    let d2 = h.recv_diags();
    assert_eq!(
        d2.diagnostics.len(),
        1,
        "a later edit is still analysed and published — the URI was not stuck"
    );
    h.shutdown();
}

#[test]
fn integration_s3_shutdown_with_worker_in_flight_does_not_hang() {
    // Shutdown must not wait on a detached rayon worker. Hold a worker
    // mid-flight, then shut down: the loop returns promptly (the join is on the
    // LOOP thread, not the rayon worker); the results channel drops, so the
    // worker's eventual send is a no-op. Release the worker AFTER shutdown so it
    // is not leaked blocked on a rayon pool thread.
    let g = gate_holding(&[1], &[]);
    let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
    h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1)); // v1 held
    hover_sync(&h, 100, "file:///g.rb"); // the worker is spawned + in flight.
    h.shutdown(); // must return without waiting for the held worker.
    g.release(1); // detached worker proceeds; its send finds the rx gone (no-op).
}

// ---------------------------------------------------------------------
// S4: tier-1 ProjectContext generation + watched-files/config invalidation
// + dynamic registration + the close+reopen open-epoch nit. All driven
// DETERMINISTICALLY via the worker-gate seam + hover FIFO barriers — no
// wall-clock races (30 s debounce where a stray timer would interfere).
// ---------------------------------------------------------------------

/// A `workspace/didChangeWatchedFiles` payload naming one changed `uri`.
fn watched_change(uri: &str) -> serde_json::Value {
    serde_json::json!({ "changes": [ { "uri": uri, "type": 2 } ] })
}

/// A recording gate that holds every GENERATION-0 worker (until released) and
/// records `(version, generation)` for every worker it gates. Keying the hold on
/// generation (not version) lets the re-dispatched new-generation worker run
/// freely, while the recording proves whether a fresh worker ran under the new
/// generation — the observable signature of a generation stale-drop.
struct GenGate {
    release_gen0: crossbeam_channel::Sender<()>,
    calls: Arc<std::sync::Mutex<Vec<(i32, u64)>>>,
    gate: Arc<WorkerGate>,
}

fn gate_recording_hold_gen0() -> GenGate {
    let (tx, rx) = crossbeam_channel::unbounded::<()>();
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls_w = Arc::clone(&calls);
    let gate: Arc<WorkerGate> = Arc::new(move |version: i32, generation: u64| {
        calls_w.lock().unwrap().push((version, generation));
        if generation == 0 {
            let _ = rx.recv(); // block gen-0 workers until released.
        }
    });
    GenGate { release_gen0: tx, calls, gate }
}

#[test]
fn integration_s4_generation_stale_drop_after_invalidate() {
    // A worker in flight when an invalidation bumps the generation has its
    // result DROPPED; a fresh dispatch under the new generation publishes. The
    // gen-0 worker is held; a relevant watched-files change bumps the generation
    // to 1; on release the gen-0 result is generation-stale → dropped +
    // re-dispatched → a gen-1 worker publishes.
    let g = gate_recording_hold_gen0();
    let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
    // didOpen v1 (CLEAN) → worker (v1, gen0) spawns and blocks in the gate.
    h.notify("textDocument/didOpen", open_params("file:///g.rb", CLEAN, 1));
    hover_sync(&h, 100, "file:///g.rb"); // barrier: the gen0 worker is in flight.
    // Invalidate via a relevant watched-files change → generation → 1; re-analyse
    // open buffers (the URI is in flight → no-op; the eventual gen-drop covers it).
    h.notify("workspace/didChangeWatchedFiles", watched_change("file:///proj/.rigor.yml"));
    hover_sync(&h, 101, "file:///g.rb"); // barrier: the invalidation is processed.
    // Release the gen0 worker → its result is generation-stale (gen0 != gen1) →
    // DROPPED + re-dispatched → a fresh (v1, gen1) worker publishes the clean set.
    g.release_gen0.send(()).unwrap();
    let d = h.recv_diags();
    assert!(
        d.diagnostics.is_empty(),
        "the fresh gen-1 result publishes (clean): {:?}",
        d.diagnostics
    );
    hover_sync(&h, 102, "file:///g.rb"); // exactly one publish.
    // Proof of the generation drop: a worker ran under generation 1 (the
    // re-dispatch). Without the generation guard the gen-0 result would have
    // published directly and NO gen-1 worker would ever have run.
    let calls = g.calls.lock().unwrap().clone();
    assert!(
        calls.iter().any(|&(_, genr)| genr == 0),
        "the initial worker ran under generation 0: {calls:?}"
    );
    assert!(
        calls.iter().any(|&(_, genr)| genr == 1),
        "a re-dispatched worker ran under the new generation (proves the stale \
         gen-0 result was dropped, not published): {calls:?}"
    );
    h.shutdown();
}

#[test]
fn integration_s4_watched_files_relevant_reanalyzes_all_open_buffers() {
    // A relevant `didChangeWatchedFiles` (`.rigor.yml`) invalidates + re-analyses
    // ALL open buffers — both `a.rb` and `b.rb` re-publish. 30 s debounce so only
    // the invalidation (never a timer) drives the re-publishes.
    let mut h = Harness::start_with_debounce(Duration::from_secs(30));
    h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    h.notify("textDocument/didOpen", open_params("file:///b.rb", CLEAN, 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    h.notify(
        "workspace/didChangeWatchedFiles",
        watched_change("file:///proj/.rigor.yml"),
    );
    // Both buffers re-publish (worker order is nondeterministic; collect a set).
    let mut seen = std::collections::HashSet::new();
    seen.insert(h.recv_diags().uri.as_str().to_string());
    seen.insert(h.recv_diags().uri.as_str().to_string());
    assert!(
        seen.contains("file:///a.rb") && seen.contains("file:///b.rb"),
        "both open buffers re-analysed after invalidate: {seen:?}"
    );
    h.shutdown();
}

#[test]
fn integration_s4_watched_files_unrelated_does_not_invalidate() {
    // An unrelated watched path (a `.txt`) does NOT invalidate → no re-analysis.
    let mut h = Harness::start_with_debounce(Duration::from_secs(30));
    h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    h.notify(
        "workspace/didChangeWatchedFiles",
        watched_change("file:///proj/notes.txt"),
    );
    // No re-publish: a hover round-trips as the very next message (a publish here
    // would mean the unrelated change wrongly invalidated).
    hover_sync(&h, 100, "file:///a.rb");
    h.shutdown();
}

#[test]
fn integration_s4_did_change_configuration_reanalyzes() {
    // `didChangeConfiguration` always invalidates + re-analyses open buffers.
    let mut h = Harness::start_with_debounce(Duration::from_secs(30));
    h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    h.notify(
        "workspace/didChangeConfiguration",
        serde_json::json!({ "settings": {} }),
    );
    let d = h.recv_diags();
    assert_eq!(d.uri.as_str(), "file:///a.rb", "the open buffer re-analysed");
    assert!(d.diagnostics.is_empty());
    h.shutdown();
}

#[test]
fn integration_s4_buffer_didchange_never_invalidates() {
    // A buffer `didChange` NEVER invalidates: only the EDITED buffer re-publishes;
    // an untouched second open buffer is NOT re-analysed (an invalidate would
    // re-publish BOTH — see `..._reanalyzes_all_open_buffers`).
    let mut h = Harness::start_with_debounce(Duration::from_millis(10));
    h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    h.notify("textDocument/didOpen", open_params("file:///b.rb", CLEAN, 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    // Edit ONLY a.rb → its debounced publish carries the typo; b.rb stays quiet.
    h.notify("textDocument/didChange", change_params("file:///a.rb", TYPO, 2));
    let d = h.recv_diags();
    assert_eq!(d.uri.as_str(), "file:///a.rb", "only the edited buffer republishes");
    assert_eq!(d.diagnostics.len(), 1);
    // Prove b.rb did NOT re-publish (no invalidation): a hover on b.rb round-trips
    // as the next message.
    hover_sync(&h, 100, "file:///b.rb");
    h.shutdown();
}

#[test]
fn integration_s4_dynamic_registration_sent_when_advertised() {
    // Client advertises `didChangeWatchedFiles.dynamicRegistration` → the server
    // sends a `client/registerCapability` request after `initialized`.
    let caps = serde_json::json!({
        "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } }
    });
    let mut h = Harness::start_full(DEBOUNCE_DEFAULT, production_gate(), caps);
    match h.recv() {
        Message::Request(r) => {
            assert_eq!(r.method, "client/registerCapability");
            // Reply so the request isn't left outstanding (the server ignores it).
            h.client
                .sender
                .send(Message::Response(Response::new_ok(r.id, serde_json::Value::Null)))
                .unwrap();
        }
        other => panic!("expected client/registerCapability, got {other:?}"),
    }
    h.shutdown();
}

#[test]
fn integration_s4_no_registration_when_not_advertised_but_watched_files_still_honored() {
    // Client does NOT advertise dynamic registration → NO `client/registerCapability`
    // is sent (the first server message is the didOpen publish, not a request);
    // yet a subsequently-received `didChangeWatchedFiles` is STILL honoured (the
    // static-registration degrade path — no regression).
    let mut h = Harness::start_full(DEBOUNCE_DEFAULT, production_gate(), serde_json::json!({}));
    h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
    // `recv_diags` panics on a Request, so this asserts no registration preceded it.
    let d = h.recv_diags();
    assert_eq!(d.uri.as_str(), "file:///a.rb");
    h.notify(
        "workspace/didChangeWatchedFiles",
        watched_change("file:///proj/.rigor.yml"),
    );
    let d2 = h.recv_diags();
    assert_eq!(
        d2.uri.as_str(),
        "file:///a.rb",
        "didChangeWatchedFiles is honoured even without dynamic registration"
    );
    h.shutdown();
}

#[test]
fn integration_s4_close_reopen_version_reuse_drops_stale_preclose_worker() {
    // The S3 reopen-identity nit. A pre-close worker is held in flight; then
    // didClose; then didOpen REUSING version 1 (VS Code resends version 1 on
    // reopen) with DIFFERENT (clean) content. Version matches and the generation
    // is unchanged (project-scoped — a reopen never bumps it), so ONLY the
    // open-epoch closes this: the pre-close worker (a TYPO) is epoch-dropped and
    // the reopened CLEAN content is analysed fresh.
    let g = gate_holding(&[1], &[]);
    let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
    // v1 = TYPO, worker held in flight (open-epoch 1).
    h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1));
    hover_sync(&h, 100, "file:///g.rb"); // the pre-close worker is in flight.
    // Close (open-epoch → 2) — clears markers with an empty publish.
    h.notify(
        "textDocument/didClose",
        serde_json::json!({ "textDocument": { "uri": "file:///g.rb" } }),
    );
    assert!(h.recv_diags().diagnostics.is_empty(), "didClose clears markers");
    // Reopen REUSING version 1 with CLEAN content (open-epoch → 3). The reopen's
    // dispatch no-ops (the pre-close worker is still in flight); its content is
    // picked up by the epoch-drop re-dispatch.
    h.notify("textDocument/didOpen", open_params("file:///g.rb", CLEAN, 1));
    hover_sync(&h, 101, "file:///g.rb"); // the reopen is processed.
    // Two release tokens (same version 1): the first unblocks the pre-close
    // worker; the second is buffered for the epoch-drop re-dispatch (also v1).
    g.release(1);
    g.release(1);
    // The pre-close worker returns: version matches (1) and generation matches,
    // but its EPOCH (1) != current (3) → DROPPED + re-dispatched → the reopened
    // CLEAN content publishes. Under S3 (no epoch guard) the stale TYPO would
    // have published (1 diagnostic) — this empty publish proves the epoch drop.
    let d = h.recv_diags();
    assert!(
        d.diagnostics.is_empty(),
        "the stale pre-close (TYPO) worker was epoch-dropped; the reopened CLEAN \
         content publishes: {:?}",
        d.diagnostics
    );
    hover_sync(&h, 102, "file:///g.rb"); // exactly one publish.
    h.shutdown();
}

#[test]
fn watched_file_relevance_matches_the_config_and_signature_surface() {
    // The invalidation surface is unchanged from S4 — `.rigor.yml`,
    // `Gemfile.lock`, project `*.rb`, `sig/**/*.rbs` — but review N3 SPLIT it
    // by cost: only the config/signature files force a full tier-1 rebuild; a
    // project `.rb` re-harvests just that file's AST entry.
    let src = |u: &str| classify_watched_files(&watched_change(u));
    assert_eq!(src("file:///p/.rigor.yml"), WatchedChange::Structural);
    assert_eq!(src("file:///p/Gemfile.lock"), WatchedChange::Structural);
    assert_eq!(src("file:///p/sig/user.rbs"), WatchedChange::Structural);
    assert_eq!(src("file:///p/sig/models/user.rbs"), WatchedChange::Structural);
    assert_eq!(
        src("file:///p/app/models/user.rb"),
        WatchedChange::Sources(vec!["file:///p/app/models/user.rb".to_string()]),
        "a project source save is the CHEAP path, not a full rebuild"
    );
    // Not on the surface at all: an `.rbs` outside a `sig/` dir, unrelated files,
    // and a malformed/empty payload.
    assert_eq!(src("file:///p/vendor/other.rbs"), WatchedChange::None);
    assert_eq!(src("file:///p/notes.txt"), WatchedChange::None);
    assert_eq!(src("file:///p/README.md"), WatchedChange::None);
    assert_eq!(classify_watched_files(&serde_json::json!({})), WatchedChange::None);

    // A batch mixing both kinds is STRUCTURAL: the full rebuild re-harvests
    // every source anyway, so the cheap path would be redundant work.
    let mixed = serde_json::json!({ "changes": [
        { "uri": "file:///p/lib/a.rb" },
        { "uri": "file:///p/.rigor.yml" }
    ]});
    assert_eq!(classify_watched_files(&mixed), WatchedChange::Structural);
}

#[test]
fn overlay_guard_needs_consecutive_samples_in_both_directions() {
    // Review N2: one sample is not a classifier. Measured on an idle machine at
    // 3 117 files, six consecutive rebuilds straddled a 100 ms
    // budget (93.9 / 93.7 / 90.9 / 106.2 / 94.7 / 92.9 ms) — a single-sample
    // sticky guard is a per-session coin flip with no recovery. Hysteresis in
    // BOTH directions is what makes the posture track the project.
    let budget = Duration::from_millis(100);
    let over = Duration::from_millis(150);
    let under = Duration::from_millis(50);
    let mut g = OverlayGuard::new();
    assert!(g.enabled, "a session starts cross-file");

    // One spike does NOT disable…
    assert_eq!(g.record(over, budget), GuardVerdict::Unchanged);
    assert!(g.enabled);
    // …and a good sample RESETS the streak, so an isolated outlier among
    // healthy samples can never accumulate into a trip.
    assert_eq!(g.record(under, budget), GuardVerdict::Unchanged);
    assert_eq!(g.record(over, budget), GuardVerdict::Unchanged);
    assert!(g.enabled, "spike, good, spike must not disable");

    // Two CONSECUTIVE over-budget samples do.
    assert_eq!(g.record(over, budget), GuardVerdict::Disabled);
    assert!(!g.enabled);

    // Recovery is ASYMMETRIC: a SINGLE under-budget sample re-enables. While
    // OFF the only samples come from structural invalidations, so requiring two
    // consecutive ones made recovery near-unreachable — and the "no restart
    // needed" disclosure false. A wrong re-enable is cheap: once ON, dispatch
    // samples are plentiful and it self-corrects within two of them.
    assert_eq!(g.record(under, budget), GuardVerdict::ReEnabled);
    assert!(g.enabled);

    // …and the re-enabled guard still needs two consecutive over-budget samples
    // to trip again (recovery does not leave it hair-trigger).
    assert_eq!(g.record(over, budget), GuardVerdict::Unchanged);
    assert_eq!(g.record(over, budget), GuardVerdict::Disabled);
    assert_eq!(g.record(under, budget), GuardVerdict::ReEnabled);

    // A sample exactly AT the budget is inside it (the test is `>`).
    let mut edge = OverlayGuard::new();
    assert_eq!(edge.record(budget, budget), GuardVerdict::Unchanged);
    assert!(edge.enabled);
}

// ---------------------------------------------------------------------
// S4b — cross-file overlay (mini-spec 20260719-lsp-s4b-overlay-mini-spec.md)
// ---------------------------------------------------------------------

/// A throwaway on-disk project for the S4b overlay tests: a uniquely-named
/// temp dir with a `lib/` the default config's `paths: ["lib"]` discovers.
/// Removed on drop. Unique per test (pid + a process-global counter), so the
/// suite stays parallel-safe WITHOUT ever mutating the process cwd — that is
/// what [`ServerContext::project_root`] is injectable for.
struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join(format!("rigor_lsp_s4b_{tag}_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("lib")).unwrap();
        // Canonicalize the ROOT once: macOS's temp dir is a symlink
        // (`/var` → `/private/var`), and the overlay compares CANONICAL paths.
        let root = std::fs::canonicalize(&root).unwrap();
        Self { root }
    }

    /// Write (or overwrite) the project's `.rigor.yml`. The server reads it
    /// through the production loader, so a test drives config exactly as a user
    /// does — including a `yaml` that does not parse.
    fn write_config(&self, yaml: &str) {
        std::fs::write(self.root.join(".rigor.yml"), yaml).unwrap();
    }

    /// Delete the project's `.rigor.yml` (the "user removed their config" case,
    /// which reloads to DEFAULTS rather than keeping the last good one).
    fn remove_config(&self) {
        std::fs::remove_file(self.root.join(".rigor.yml")).unwrap();
    }

    /// The `file:` URI of the project's `.rigor.yml` — what a client names in
    /// the `didChangeWatchedFiles` payload after the editor saves it.
    fn config_uri(&self) -> String {
        format!("file://{}", self.root.join(".rigor.yml").display())
    }

    /// Write (or overwrite) `lib/<name>` and return its canonical path.
    fn write(&self, name: &str, text: &str) -> PathBuf {
        let p = self.root.join("lib").join(name);
        std::fs::write(&p, text).unwrap();
        std::fs::canonicalize(&p).unwrap()
    }

    /// The `file:` URI for `lib/<name>` (must already exist).
    fn uri(&self, name: &str) -> String {
        format!("file://{}", self.root.join("lib").join(name).display())
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The cross-file fixture: `Base#helper` is PUBLIC in one file, and `Sub`
/// (another file) overrides it as PRIVATE. `def.override-visibility-reduced`
/// fires on `sub.rb` — but ONLY with project-wide context: a single-file index
/// for `sub.rb` cannot see `Base`, so today's LSP misses it entirely. Verified
/// against the real CLI: `rigor check lib/sub.rb` is silent, `rigor check lib`
/// reports it.
const BASE_RB: &str = "class Base\n  def helper\n  end\nend\n";
const SUB_RB: &str = "class Sub < Base\n  private\n\n  def helper\n  end\nend\n";

/// Reproduce `check`'s stage 2 + 3 for `target` over the whole project file
/// set, INDEPENDENTLY of the LSP's overlay code: parse+lower every project
/// file, `SourceIndex::build_project` over all of them, then
/// `analyze_with_source_and_folder` on the target's AST. This is the
/// `rigor check <project>` semantics the S4b overlay must reproduce; the
/// parity test compares the LSP's published set against it.
fn check_project_diagnostics(root: &Path, target: &str) -> Vec<(String, u32)> {
    let cfg = Config::default();
    let index = CoreIndex::new();
    let paths = project_files(root, &cfg);
    let asts: Vec<LoweredAst> = paths
        .iter()
        .map(|p| lower(&parse(&std::fs::read(p).unwrap())))
        .collect();
    let refs: Vec<&LoweredAst> = asts.iter().collect();
    let project_source = SourceIndex::build_project(&refs, &index);
    let target_path = std::fs::canonicalize(root.join("lib").join(target)).unwrap();
    let text = std::fs::read_to_string(&target_path).unwrap();
    let target_ast = lower(&parse(text.as_bytes()));
    let mut interner = Interner::new();
    let diags = analyze_with_source_and_folder(
        &target_ast,
        &mut interner,
        &index,
        &project_source,
        None,
    );
    diags
        .iter()
        .map(|d| {
            (d.rule_id.to_string(), offset_to_position(&text, d.start_offset).line)
        })
        .collect()
}

/// The `(rule id, 0-based line)` pairs of a published diagnostic set — the
/// comparable shape for the parity assertion.
fn diag_keys(params: &PublishDiagnosticsParams) -> Vec<(String, u32)> {
    params
        .diagnostics
        .iter()
        .map(|d| {
            let code = match &d.code {
                Some(NumberOrString::String(s)) => s.clone(),
                other => format!("{other:?}"),
            };
            (code, d.range.start.line)
        })
        .collect()
}

#[test]
fn integration_s4b_saved_buffer_matches_project_check_not_single_file() {
    // ACCEPTANCE 1 (the keystone): for a SAVED (non-dirty) buffer, the LSP's
    // diagnostics equal what `check` produces for that file WITH project
    // context — on a case where cross-file context CHANGES the answer.
    let p = TempProject::new("parity");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    // A long debounce: didOpen publishes immediately, and no stray timer can
    // interfere with the single-publish assertion.
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
    let published = h.recv_diags();
    assert_eq!(published.uri.as_str(), p.uri("sub.rb"));

    // (a) PARITY: identical to check's project-wide answer for this file.
    let expected = check_project_diagnostics(&p.root, "sub.rb");
    assert_eq!(
        diag_keys(&published),
        expected,
        "LSP diagnostics must equal `check`'s project-wide diagnostics for the file"
    );

    // (b) The answer is genuinely CROSS-FILE: exactly the override-visibility
    // finding, which needs `Base` from the OTHER file.
    assert_eq!(
        diag_keys(&published),
        vec![("def.override-visibility-reduced".to_string(), 3)],
        "the cross-file override finding, on the `def helper` line"
    );

    // (c) …and today's single-file index MISSES it — so this test would fail
    // without the overlay. (Directly: the pre-S4b `compute_diagnostics` path.)
    let (single_file, _, _) = compute_diagnostics(&project(), &BufferPaths::default(), SUB_RB);
    assert!(
        single_file.is_empty(),
        "a single-file index cannot see `Base`, so it finds nothing: {single_file:?}"
    );
    h.shutdown();
}

#[test]
fn integration_s4b_dirty_buffer_overlay_changes_diagnostics_without_a_save() {
    // ACCEPTANCE 2: removing the overriding method in the OPEN BUFFER changes
    // that file's diagnostics with NO save — the on-disk `sub.rb` still holds
    // the override throughout.
    let p = TempProject::new("dirty");
    p.write("base.rb", BASE_RB);
    let on_disk = p.write("sub.rb", SUB_RB);
    let mut h = Harness::start_project(
        Duration::from_millis(10),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1, "saved buffer: the override fires");

    // Rename the override away in the BUFFER only (no write to disk).
    let edited = "class Sub < Base\n  private\n\n  def unrelated\n  end\nend\n";
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": p.uri("sub.rb"), "version": 2 },
            "contentChanges": [ { "text": edited } ]
        }),
    );
    let after = h.recv_diags();
    assert!(
        after.diagnostics.is_empty(),
        "the buffer no longer overrides `Base#helper` → the finding is gone: {:?}",
        after.diagnostics
    );
    assert_eq!(
        std::fs::read_to_string(&on_disk).unwrap(),
        SUB_RB,
        "the on-disk file was never written — the change was buffer-only"
    );

    // Put it back in the buffer: the finding returns (the overlay is live, not
    // a one-shot).
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": p.uri("sub.rb"), "version": 3 },
            "contentChanges": [ { "text": SUB_RB } ]
        }),
    );
    assert_eq!(h.recv_diags().diagnostics.len(), 1, "restoring the override refires it");
    h.shutdown();
}

#[test]
fn integration_s4b_buffer_replaces_rather_than_adds_the_on_disk_ast() {
    // ACCEPTANCE 3 (the FP-safety pin): a method RENAMED in the buffer must not
    // leave the on-disk name resolvable. On disk `Api#fetch` returns a String,
    // so `Api.new.fetch.lenght` is a hard error. The buffer renames the DEF to
    // `grab` while leaving the call site — so `Api#fetch` no longer exists and
    // the (in-source-only) receiver goes lenient: NO diagnostic.
    //
    // Under ADD semantics the on-disk AST would still register `Api#fetch ->
    // String` and the diagnostic would keep firing — a stale, WRONG type. That
    // is exactly the false positive replacement buys, verified against the CLI:
    // a `lib/` holding BOTH versions reports the error; the renamed file alone
    // does not.
    let p = TempProject::new("replace");
    p.write("other.rb", "class Other\nend\n");
    let on_disk_text =
        "class Api\n  def fetch\n    \"s\"\n  end\nend\n\nApi.new.fetch.lenght\n";
    p.write("main.rb", on_disk_text);
    let mut h = Harness::start_project(
        Duration::from_millis(10),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    // Control: the SAVED buffer reproduces the on-disk finding (the fixture is
    // live, so the empty set below cannot be vacuous).
    h.notify("textDocument/didOpen", open_params(&p.uri("main.rb"), on_disk_text, 1));
    assert_eq!(
        diag_keys(&h.recv_diags()),
        vec![("call.undefined-method".to_string(), 6)],
        "saved buffer: `.lenght` on the String `fetch` returns"
    );

    let renamed = "class Api\n  def grab\n    \"s\"\n  end\nend\n\nApi.new.fetch.lenght\n";
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": p.uri("main.rb"), "version": 2 },
            "contentChanges": [ { "text": renamed } ]
        }),
    );
    let after = h.recv_diags();
    assert!(
        after.diagnostics.is_empty(),
        "REPLACE: the on-disk `Api#fetch` is gone, so nothing types the receiver. \
         A non-empty set here means the on-disk AST was ADDED alongside the buffer's \
         (double registration = a stale type = an FP): {:?}",
        after.diagnostics
    );
    h.shutdown();
}

#[test]
fn integration_s4b_deleted_on_disk_file_still_replaces_never_appends() {
    // REGRESSION (review B1): a buffer whose file is DELETED or RENAMED on
    // disk while it stays open — a `git checkout`, a `git stash`, an IDE
    // rename — must STILL replace tier-1's held AST for that path, not be
    // appended alongside it. Appending double-registers the file: the stale
    // on-disk `Api#fetch -> String` would keep typing the receiver and the
    // rename-away would keep firing `undefined method 'lenght'` — the exact
    // wrong-type FP the REPLACE rule exists to prevent.
    //
    // The trap was `fs::canonicalize` failing on a nonexistent path and the
    // resulting `None` being read as "no on-disk identity" (an untitled
    // buffer, where appending IS right). Resolving the PARENT distinguishes
    // the two.
    let p = TempProject::new("deleted");
    p.write("other.rb", "class Other\nend\n");
    let on_disk_text =
        "class Api\n  def fetch\n    \"s\"\n  end\nend\n\nApi.new.fetch.lenght\n";
    let main = p.write("main.rb", on_disk_text);
    let mut h = Harness::start_project(
        Duration::from_millis(10),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("main.rb"), on_disk_text, 1));
    assert_eq!(
        h.recv_diags().diagnostics.len(),
        1,
        "control: the saved buffer reproduces the on-disk finding"
    );

    // The file vanishes from disk; the buffer stays open and is edited.
    std::fs::remove_file(&main).unwrap();
    let renamed = "class Api\n  def grab\n    \"s\"\n  end\nend\n\nApi.new.fetch.lenght\n";
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": p.uri("main.rb"), "version": 2 },
            "contentChanges": [ { "text": renamed } ]
        }),
    );
    let after = h.recv_diags();
    assert!(
        after.diagnostics.is_empty(),
        "a deleted-on-disk buffer must REPLACE its stale held AST, not be appended \
         next to it (double registration = a stale type = an FP): {:?}",
        after.diagnostics
    );
    h.shutdown();
}

#[test]
fn integration_s4b_scale_guard_falls_back_to_single_file_and_discloses() {
    // ACCEPTANCE 4, updated for the hysteresis guard (review N2): with the
    // budget forced to zero EVERY rebuild is over it, so the guard
    // trips on the second consecutive sample — startup is #1, the first
    // dispatch is #2 — then DISCLOSES via `window/showMessage` (the ADR-0036
    // posture-disclosure precedent) and falls back to the single-file index,
    // which cannot see `Base`.
    //
    // Deterministic: `Duration::ZERO` makes every sample over-budget by
    // construction, so no wall clock, threshold or corpus size is raced. The
    // ORDER is fixed too — `handle_result` publishes first and records the
    // sample after, so the first publish is still the overlay's answer.
    let p = TempProject::new("guard");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    let mut h = Harness::start_project(
        Duration::from_millis(10),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        Duration::ZERO,
    );
    // Sample #1 (startup) alone must NOT disable — this is the anti-coin-flip
    // property: the first publish is still the full cross-file answer.
    h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
    assert_eq!(
        h.recv_diags().diagnostics.len(),
        1,
        "one over-budget sample must not disable the overlay"
    );
    // That dispatch WAS sample #2 → the guard trips and discloses.
    match h.recv() {
        Message::Notification(n) if n.method == "window/showMessage" => {
            let params: ShowMessageParams = serde_json::from_value(n.params).unwrap();
            assert_eq!(params.typ, MessageType::WARNING);
            assert!(
                params.message.contains("cross-file diagnostics disabled")
                    && params.message.contains("single-file scope"),
                "the posture is disclosed, never silently degraded: {}",
                params.message
            );
            assert!(
                params.message.contains("no restart needed"),
                "the disclosure must say the decision is re-evaluated, not permanent: {}",
                params.message
            );
        }
        other => panic!("expected the scale-guard showMessage, got {other:?}"),
    }
    // The NEXT dispatch runs on the single-file fallback: `Base` is invisible,
    // so the cross-file finding is gone.
    h.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": p.uri("sub.rb"), "version": 2 },
            "contentChanges": [ { "text": SUB_RB } ]
        }),
    );
    let d = h.recv_diags();
    assert!(
        d.diagnostics.is_empty(),
        "guard tripped ⇒ single-file fallback ⇒ the cross-file finding is absent: {:?}",
        d.diagnostics
    );
    h.shutdown();
}

#[test]
fn integration_s4b_guard_stays_enabled_on_a_healthy_project() {
    // The complement of the trip test, and the property the old single-sample
    // sticky guard could not offer: under a generous budget the overlay stays
    // ON across many dispatches — no drift into the fallback, and NO
    // `window/showMessage` churn (asserted by a hover round-tripping as the
    // very next message after each publish).
    let p = TempProject::new("healthy");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    let mut h = Harness::start_project(
        Duration::from_millis(10),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        Duration::from_secs(30),
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1);
    for version in 2..=6 {
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": p.uri("sub.rb"), "version": version },
                "contentChanges": [ { "text": SUB_RB } ]
            }),
        );
        assert_eq!(
            h.recv_diags().diagnostics.len(),
            1,
            "dispatch {version}: the overlay must still be on"
        );
        // A hover answers next ⇒ no showMessage was queued behind the publish.
        hover_sync(&h, 500 + version, &p.uri("sub.rb"));
    }
    h.shutdown();
}

#[test]
fn integration_s4b_watched_file_save_reharvests_the_project_asts() {
    // ACCEPTANCE 5 (S4 plumbing × S4b substrate): a SAVED edit to a project file
    // that is NOT open changes the open buffer's diagnostics, because
    // `invalidate` re-harvests the held files. This is the pay-off S4 deferred
    // ("the cross-file benefit lands in S4b").
    let p = TempProject::new("watched");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1, "the override fires against Base");

    // `Base#helper` is deleted ON DISK; `sub.rb` (the open buffer) is untouched.
    p.write("base.rb", "class Base\nend\n");
    h.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 2 } ] }),
    );
    let after = h.recv_diags();
    assert!(
        after.diagnostics.is_empty(),
        "the re-harvested project ASTs no longer define `Base#helper`, so the \
         override finding is gone: {:?}",
        after.diagnostics
    );
    h.shutdown();
}

#[test]
fn integration_s4b_source_save_reharvests_only_that_file_and_deletion_removes_it() {
    // Review N3: a project `.rb` save must re-harvest ONLY that file's AST
    // entry — not re-parse the project on the loop thread. Behaviourally that
    // has to be INDISTINGUISHABLE from the old full rebuild, so this drives the
    // three transitions through the public protocol.
    let p = TempProject::new("reharvest");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    p.write("untouched.rb", "class Untouched\n  def helper\n  end\nend\n");
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1, "the override fires against Base");

    // (1) EDIT an existing held file → its entry is replaced in place.
    p.write("base.rb", "class Base\nend\n");
    h.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 2 } ] }),
    );
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "`Base#helper` is gone ⇒ nothing is overridden"
    );

    // (2) RESTORE it → the entry is replaced again (not duplicated: a duplicate
    // `Base` would still resolve `helper` and the count would stay at 1 either
    // way, so the real proof is transition (3) below).
    p.write("base.rb", BASE_RB);
    h.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 2 } ] }),
    );
    assert_eq!(h.recv_diags().diagnostics.len(), 1, "restoring Base#helper refires it");

    // (3) DELETE it → the entry is REMOVED. This is what a naive incremental
    // update gets wrong (a vanished file has nothing to re-parse, so a
    // replace-only implementation would silently keep the stale AST forever).
    std::fs::remove_file(p.root.join("lib/base.rb")).unwrap();
    h.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 3 } ] }),
    );
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "a deleted project file must be dropped from the held table"
    );

    // (4) A NEW in-scope file takes the full-rebuild path (ordering fidelity)
    // and is picked up.
    p.write("base.rb", BASE_RB);
    h.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 1 } ] }),
    );
    assert_eq!(h.recv_diags().diagnostics.len(), 1, "a re-created file is re-harvested");
    h.shutdown();
}

// =======================================================================
// Held harvest — the tier-1 table carries each file's `Harvest` beside its
// AST, so a dispatch harvests only the dirty buffer and calls
// `SourceIndex::merge` directly instead of `build_project`.
// =======================================================================

/// The PRE-slice [`overlay_source_index`] body, kept VERBATIM as the oracle
/// (the `probes_s92` / `probes_s94` pattern): assemble the held ASTs with the
/// dirty buffer's file REPLACED, then `SourceIndex::build_project` — which
/// re-harvests every file on the spot. The ONLY edit is destructuring the held
/// entry's third member away, because the table's arity is what this slice
/// changed; the control flow, the order, and the REPLACE/append rule are
/// character-for-character today's.
fn overlay_source_index_legacy(
    project: &ProjectContext,
    path: Option<&Path>,
    ast: &LoweredAst,
) -> SourceIndex {
    let Some(overlay) = &project.overlay else {
        return SourceIndex::build(ast, &project.index);
    };
    let mut refs: Vec<&LoweredAst> = Vec::with_capacity(overlay.files.len() + 1);
    let mut replaced = false;
    for (p, held, _) in &overlay.files {
        if path == Some(p.as_path()) {
            refs.push(ast);
            replaced = true;
        } else {
            refs.push(held);
        }
    }
    if !replaced {
        refs.push(ast);
    }
    SourceIndex::build_project(&refs, &project.index)
}

/// Every diagnostic the analysis pass produces for `ast` against `source`, in
/// full (rule, span, message) — the observable the equivalence is defined on.
fn diags_against(
    source: &SourceIndex,
    index: &CoreIndex,
    ast: &LoweredAst,
) -> Vec<(String, usize, usize, String)> {
    let mut interner = Interner::new();
    analyze_with_source_and_folder(ast, &mut interner, index, source, None)
        .iter()
        .map(|d| {
            (d.rule_id.to_string(), d.start_offset, d.end_offset, d.message.clone())
        })
        .collect()
}

#[test]
fn held_harvest_dispatch_index_equals_the_pre_slice_rebuild() {
    // THE EQUIVALENCE. A dispatch's index used to be `build_project` over the
    // held ASTs with the buffer's REPLACED; it is now `merge` over the held
    // HARVESTS with the buffer's pair replaced. `build_project` IS
    // `merge(asts.map(harvest))` (#92) and a harvest is a pure function of
    // `(AST, frozen CoreIndex)` — both pinned by the held table — so the two
    // must agree on every buffer, on both branches (REPLACE and append) and in
    // both postures (overlay on and off).
    let p = TempProject::new("held_harvest_equiv");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    p.write("helpers.rb", "def helper_fn\n  1\nend\n");
    p.write("main.rb", "helper_fn\n");
    p.write("consts.rb", "module Cfg\n  LIMIT = 5\nend\n");
    p.write("both.rb", "def both_fn\n  1\nend\nboth_fn\n");

    let cfg = Config::default();
    let (_ctx, st) = session_for(&p.root, cfg, OVERLAY_BUILD_BUDGET_DEFAULT);
    let project = &st.project;
    assert_eq!(
        project.overlay.as_ref().expect("overlay live").files.len(),
        6,
        "precondition: all six project files are held"
    );
    let in_project = |name: &str| std::fs::canonicalize(p.root.join("lib").join(name)).unwrap();

    // (path handed to the dispatch, buffer text) — each row is a real editor
    // state, and together they touch every ordered harvest field the merge
    // replays: classes, the override index, toplevel defs, constant writes.
    let cases: Vec<(&str, Option<PathBuf>, &str)> = vec![
        // A held file, buffer == disk: the no-edit dispatch.
        ("sub unchanged", Some(in_project("sub.rb")), SUB_RB),
        // A held file, buffer differs: the override is made public again.
        (
            "sub public",
            Some(in_project("sub.rb")),
            "class Sub < Base\n  def helper\n  end\nend\n",
        ),
        // The buffer REDEFINES the base the other files subclass.
        ("base emptied", Some(in_project("base.rb")), "class Base\nend\n"),
        // A toplevel call whose definer lives in ANOTHER file (harvest pass 1c).
        ("main calls toplevel", Some(in_project("main.rb")), "helper_fn\n"),
        // …and the definer's own buffer renaming it away.
        ("helpers renamed", Some(in_project("helpers.rb")), "def other_fn\n  1\nend\n"),
        // A constant write (harvest C5a) edited in the buffer.
        ("consts edited", Some(in_project("consts.rb")), "module Cfg\n  LIMIT = 9\nend\n"),
        // THE REPLACE DISCRIMINATORS — the only cases that can tell a REPLACED
        // buffer harvest from a KEPT held one, because the buffer's own
        // diagnostics depend on a fact only the buffer's harvest carries:
        //   * the buffer ADDS a toplevel `def` the file on disk does not have
        //     (a held harvest ⇒ the call below is `unresolved-toplevel`), and
        //   * the buffer REMOVES one the file on disk does have while still
        //     calling it (a held harvest ⇒ the removed name still resolves,
        //     which is the "renamed away but still resolvable" false negative
        //     the REPLACE rule exists to prevent).
        (
            "buffer adds a toplevel def",
            Some(in_project("main.rb")),
            "def local_fn\n  1\nend\nlocal_fn\n",
        ),
        ("buffer removes its own toplevel def", Some(in_project("both.rb")), "both_fn\n"),
        // NOT a project file ⇒ the APPEND branch (an unsaved/untitled buffer).
        ("untitled buffer", None, "class Sub < Base\n  private\n\n  def helper\n  end\nend\n"),
        // A `file:` buffer outside the project ⇒ also the append branch.
        ("outside project", Some(PathBuf::from("/nowhere/at/all/x.rb")), SUB_RB),
    ];

    let mut nonempty = 0usize;
    for (label, path, text) in &cases {
        let ast = lower(&parse(text.as_bytes()));
        let legacy = overlay_source_index_legacy(project, path.as_deref(), &ast);
        let (fresh, sample) = overlay_source_index(project, path.as_deref(), &ast);
        assert!(sample.is_some(), "[{label}] the overlay was live, so it owes a sample");
        let want = diags_against(&legacy, &project.index, &ast);
        let got = diags_against(&fresh, &project.index, &ast);
        assert_eq!(got, want, "[{label}] the held-harvest index diverged from the rebuild");
        if !want.is_empty() {
            nonempty += 1;
        }
    }
    // Non-vacuity: the comparison must not be "two empty lists agree" everywhere,
    // and the corpus must actually contain a CROSS-FILE finding.
    assert!(nonempty >= 2, "the case set produced almost no diagnostics: {nonempty}");
    let unchanged = lower(&parse(SUB_RB.as_bytes()));
    let (idx, _) = overlay_source_index(project, Some(&in_project("sub.rb")), &unchanged);
    let cross = diags_against(&idx, &project.index, &unchanged);
    assert_eq!(
        cross.len(),
        1,
        "precondition: `Sub#helper` reducing `Base#helper`'s visibility is a \
         CROSS-FILE finding that a single-file index cannot produce: {cross:?}"
    );

    // …and with the overlay OFF the fallback is still literally the same call.
    let off = ProjectContext {
        generation: 0,
        index: Arc::clone(&project.index),
        disable: Config::default().disable_matcher(),
        folder: None,
        stamp: SeverityStamp::from_config(&Config::default()),
        exclude: ExcludeMatcher::from_config(&p.root, &Config::default()),
        overlay: None,
    };
    let ast = lower(&parse(SUB_RB.as_bytes()));
    let (fresh, sample) = overlay_source_index(&off, Some(&in_project("sub.rb")), &ast);
    assert!(sample.is_none(), "no overlay ⇒ no guard sample");
    assert_eq!(
        diags_against(&fresh, &project.index, &ast),
        diags_against(
            &overlay_source_index_legacy(&off, Some(&in_project("sub.rb")), &ast),
            &project.index,
            &ast,
        ),
    );
}

#[test]
fn held_harvest_save_makes_the_new_content_visible_cross_file() {
    // The re-harvest swaps BOTH halves. A save takes the incremental
    // `reharvest_sources` path, which replaces one entry in place — and the
    // entry now carries a `Harvest` as well as an AST. The merge reads its
    // cross-file facts ONLY from harvests, so an implementation that swapped
    // the AST and kept the stale harvest would keep serving the PRE-SAVE
    // content to every other file, with a fresh AST sitting beside it as
    // camouflage.
    //
    // Driven through `call.unresolved-toplevel` (harvest pass 1c, a pure union)
    // rather than the override index the other re-harvest tests use, so the two
    // cover different harvest fields.
    let p = TempProject::new("held_harvest_save");
    p.write("helpers.rb", "def helper_fn\n  1\nend\n");
    p.write("main.rb", "helper_fn\n");
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("main.rb"), "helper_fn\n", 1));
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "precondition: another file's toplevel `def` resolves the call"
    );

    // The definer is renamed away ON DISK; `main.rb` (the open buffer) is
    // untouched, so only the re-harvested facts can change the answer.
    p.write("helpers.rb", "def other_fn\n  1\nend\n");
    h.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [ { "uri": p.uri("helpers.rb"), "type": 2 } ] }),
    );
    let after = h.recv_diags();
    assert_eq!(
        after.diagnostics.len(),
        1,
        "the saved file no longer defines `helper_fn`, so the toplevel call is \
         unresolved — a STALE held harvest would still resolve it: {:?}",
        after.diagnostics
    );

    // …and back: restoring the definer clears it again (a swap, not a one-way
    // invalidation).
    p.write("helpers.rb", "def helper_fn\n  1\nend\n");
    h.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [ { "uri": p.uri("helpers.rb"), "type": 2 } ] }),
    );
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "restoring the definer re-resolves the call"
    );
    h.shutdown();
}

/// A complete structural fingerprint of a held overlay: every entry's path AND
/// its AST's full `Debug` dump, IN ORDER. Comparing this against a fresh
/// `build_overlay` is the strong form of N3's invariant — not just "the same
/// files" but the same ASTs in the same positions, which is what
/// `merge`'s order-sensitive multi-pass replay actually consumes.
/// A symlink alias for a project root, removed on drop — the "workspace
/// reached through a symlink" shape (the normal macOS `/tmp` → `/private/tmp`
/// case, a symlinked project dir, a symlinked home).
struct AliasLink(PathBuf);

impl AliasLink {
    fn to(root: &Path) -> Self {
        let alias = root.parent().unwrap().join(format!(
            "{}_alias",
            root.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_file(&alias);
        std::os::unix::fs::symlink(root, &alias).unwrap();
        Self(alias)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for AliasLink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Enter a directory for the duration of a scope, restoring the previous cwd on
/// drop — INCLUDING on a panic, so a failing assertion cannot leave the rest of
/// the suite in the wrong directory.
///
/// The process cwd is global, so this also serialises every test that needs it
/// behind one mutex. Only the `project_root = "."` (production-shape) tests take
/// it; every other test injects an absolute root precisely to avoid this.
struct CwdGuard {
    prev: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl CwdGuard {
    fn enter(dir: &Path) -> Self {
        static CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = CWD.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        Self { prev, _lock: lock }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev);
    }
}

/// Path + AST per held entry. The HARVEST is deliberately not rendered here
/// (it has no `Debug`, by design — it is an internal merge input): it is a pure
/// function of `(AST, frozen CoreIndex)` produced at the single site
/// [`held_pair`], so an equal AST under an equal core implies an equal harvest.
/// What pins the harvest SWAP is behavioural — see
/// `held_harvest_save_makes_the_new_content_visible_cross_file`.
fn overlay_fingerprint(files: &ProjectFiles) -> Vec<(String, String)> {
    files
        .files
        .iter()
        .map(|(p, a, _)| (p.to_string_lossy().into_owned(), format!("{a:?}")))
        .collect()
}

/// A `Session` over a real temp project, for driving [`reharvest_sources`]
/// directly (the incremental path is loop-thread state, not protocol surface).
fn session_for(root: &Path, cfg: Config, budget: Duration) -> (ServerContext, Session) {
    let ctx = ServerContext {
        debounce: Duration::from_secs(30),
        worker_gate: production_gate(),
        watched_files_dynamic_registration: false,
        project_root: root.to_path_buf(),
        overlay_budget: budget,
    };
    let index = Arc::new(CoreIndex::new());
    let build = build_overlay(root, &cfg, &index);
    let (results_tx, _rx) = crossbeam_channel::unbounded();
    let st = Session {
        buffers: BufferTable::new(),
        debouncer: Debouncer::new(),
        in_flight: HashSet::new(),
        epochs: HashMap::new(),
        project: Arc::new(ProjectContext {
            generation: 0,
            index,
            disable: cfg.disable_matcher(),
            folder: None,
            stamp: SeverityStamp::from_config(&cfg),
            exclude: ExcludeMatcher::from_config(root, &cfg),
            overlay: Some(build.files),
        }),
        cfg,
        config_broken: false,
        results_tx,
        guard: OverlayGuard::new(),
        crossfile: CrossFileCache::new(),
    };
    (ctx, st)
}

/// How the project root is SPELLED to the server — the axis review R-1 hid
/// behind. Every other test injects an absolute temp dir; production passes
/// `"."` and the workspace is routinely reached through a symlink, where the
/// URI spelling and the canonicalized root differ.
#[derive(Clone, Copy, Debug)]
enum RootSpelling {
    /// `project_root` = the absolute canonical temp dir (what tests inject).
    Absolute,
    /// `project_root` = `"."` with the cwd entered through a SYMLINK to the
    /// project, and URIs spelled through that symlink — the production shape.
    DotThroughSymlink,
}

#[test]
fn incremental_reharvest_is_byte_identical_to_a_full_rebuild() {
    // THE N3 INVARIANT (adversarial review of PR #43): after ANY sequence of
    // incremental updates, the held set must equal what a full rebuild of the
    // same tree would produce — entries, ASTs, and ORDER.
    //
    // The first implementation decided membership with a `ProjectScope`
    // predicate that re-derived bare-`check`'s discovery rule, and diverged from
    // it four ways (a deleted DIRECTORY, a symlinked `.rb` held under its
    // out-of-root canonical path, `paths: ["."]`, and the same canonical path
    // held twice). This test drives all of them plus the ordinary transitions,
    // across BOTH `paths:` shapes and BOTH root spellings, comparing against the
    // ground truth each time — so it fails on ANY divergence, not only the ones
    // that were found.
    for paths_yaml in ["paths:\n  - lib\n", "paths:\n  - \".\"\n"] {
        for spelling in [RootSpelling::Absolute, RootSpelling::DotThroughSymlink] {
            differential_run(paths_yaml, spelling);
        }
    }
}

fn differential_run(paths_yaml: &str, spelling: RootSpelling) {
    let cfg: Config = serde_yaml::from_str(paths_yaml).unwrap();
    let cfg2: Config = serde_yaml::from_str(paths_yaml).unwrap();
    let p = TempProject::new("differential");
    p.write("a.rb", "class A\nend\n");
    p.write("b.rb", BASE_RB);
    std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
    std::fs::write(p.root.join("lib/nested/c.rb"), "class C\nend\n").unwrap();
    // A symlinked `.rb` FILE: bare-`check` discovery harvests it (a symlink to a
    // file matches `Dir.glob`), and it is held under its CANONICAL, out-of-`lib`
    // path. Under `paths: ["."]` BOTH it and its target are walked, so the same
    // canonical path is held twice.
    std::fs::create_dir_all(p.root.join("shared")).unwrap();
    std::fs::write(p.root.join("shared/linked.rb"), "class Linked\nend\n").unwrap();
    std::os::unix::fs::symlink(p.root.join("shared/linked.rb"), p.root.join("lib/linked.rb"))
        .unwrap();

    // The root as the SERVER sees it, and the prefix URIs are spelled with.
    let alias = matches!(spelling, RootSpelling::DotThroughSymlink)
        .then(|| AliasLink::to(&p.root));
    let _cwd = alias.as_ref().map(|a| CwdGuard::enter(a.path()));
    let ctx_root = match &alias {
        Some(_) => PathBuf::from("."),
        None => p.root.clone(),
    };
    // URIs are spelled through the alias in the symlink case — the whole point:
    // the decoded spelling then differs from every canonical form.
    let uri_root = alias.as_ref().map_or_else(|| p.root.clone(), |a| a.path().to_path_buf());
    let uri = |name: &str| format!("file://{}", uri_root.join("lib").join(name).display());

    let (ctx, mut st) = session_for(&ctx_root, cfg2, OVERLAY_BUILD_BUDGET_DEFAULT);
    let index = Arc::new(CoreIndex::new());
    let check = |st: &Session, label: &str| {
        // Ground truth: a full rebuild under the SAME root spelling.
        let truth = build_overlay(&ctx_root, &cfg, &index);
        let held = st.project.overlay.as_ref().expect("overlay stays live");
        assert_eq!(
            overlay_fingerprint(held),
            overlay_fingerprint(&truth.files),
            "[{paths_yaml:?} / {spelling:?} / {label}] incremental state diverged \
             from a full rebuild"
        );
    };
    check(&st, "initial");

    // (1) EDIT a held file — the fast path (replace in place).
    p.write("a.rb", "class A\n  def extra\n  end\nend\n");
    reharvest_sources(&ctx, &mut st, &[uri("a.rb")]);
    check(&st, "edit held file");

    // (2) EDIT through the SYMLINK's path. The event names lib/linked.rb; the
    // held entry is shared/linked.rb. Canonicalization is what makes these the
    // same entry — the scope predicate got this wrong (probe C).
    std::fs::write(p.root.join("shared/linked.rb"), "class Linked\n  def x\n  end\nend\n")
        .unwrap();
    reharvest_sources(&ctx, &mut st, &[uri("linked.rb")]);
    check(&st, "edit via symlink path");

    // (3) DELETE a held file whose parent still exists — removed in place.
    std::fs::remove_file(p.root.join("lib/b.rb")).unwrap();
    reharvest_sources(&ctx, &mut st, &[uri("b.rb")]);
    check(&st, "delete file");

    // (4) CREATE a new file — not held ⇒ full rebuild ⇒ correct ORDER (an
    //     append would put it last; `build_overlay` sorts).
    p.write("aa.rb", "class Aa\nend\n");
    reharvest_sources(&ctx, &mut st, &[uri("aa.rb")]);
    check(&st, "create file");

    // (5) DELETE A DIRECTORY — the path no longer resolves even via its parent,
    //     so it is not decidable incrementally (probes A / E / F). Under the
    //     symlinked spelling this is exactly R-1: the decoded path cannot be
    //     compared soundly against a canonical root, so it must NOT be ignored.
    std::fs::remove_dir_all(p.root.join("lib/nested")).unwrap();
    reharvest_sources(&ctx, &mut st, &[uri("nested/c.rb")]);
    check(&st, "delete directory");

    // (6) An out-of-project `.rb` event: ignored (or full-rebuilt) — either way
    //     the state must still match the ground truth.
    reharvest_sources(&ctx, &mut st, &["file:///nowhere/at/all/x.rb".to_string()]);
    check(&st, "unrelated file");

    // (7) A BATCH mixing an edit, a delete and a creation in one payload.
    p.write("a.rb", "class A\nend\n");
    std::fs::remove_file(p.root.join("lib/aa.rb")).unwrap();
    p.write("z.rb", "class Z\nend\n");
    reharvest_sources(&ctx, &mut st, &[uri("a.rb"), uri("aa.rb"), uri("z.rb")]);
    check(&st, "mixed batch");
}

#[test]
fn overlay_off_when_the_project_has_no_files() {
    // No `lib/` ⇒ nothing to overlay ⇒ the overlay stays OFF and the guard does
    // NOT trip (an empty project is not an over-budget one, so no misleading
    // disclosure). This is the posture every pre-S4b test runs under.
    let root = std::env::temp_dir().join(format!("rigor_lsp_s4b_empty_{}", std::process::id()));
    let build = build_overlay(&root, &Config::default(), &CoreIndex::new());
    assert_eq!(build.file_count, 0, "no project files");
    assert!(build.files.files.is_empty());
    // An empty project must NOT feed the guard: a ~0 ms build of nothing is not
    // evidence the project is fast, and counting it would let an empty tree
    // re-enable an overlay that a real one had disabled. The callers gate on
    // `file_count > 0`; assert the posture stays untouched and silent.
    let mut guard = OverlayGuard::new();
    guard.enabled = false;
    assert_eq!(
        overlay_guard_message(&GuardVerdict::Unchanged, 0, Duration::ZERO, Duration::ZERO),
        None,
        "no posture flip ⇒ no disclosure"
    );
    assert!(!guard.enabled, "an empty project neither trips nor recovers the guard");
}

#[test]
fn project_files_follow_bare_check_discovery() {
    // The overlay's file set is bare-`check`'s: the `paths:` roots expanded
    // recursively and SORTED per root, minus config `exclude:`.
    let p = TempProject::new("discovery");
    p.write("b.rb", "class B\nend\n");
    p.write("a.rb", "class A\nend\n");
    std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
    std::fs::write(p.root.join("lib/nested/c.rb"), "class C\nend\n").unwrap();
    std::fs::write(p.root.join("lib/notruby.txt"), "nope\n").unwrap();
    let found = project_files(&p.root, &Config::default());
    let names: Vec<String> = found
        .iter()
        .map(|f| f.rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["a.rb", "b.rb", "c.rb"], "sorted, recursive, `.rb` only");

    // `exclude:` prunes, exactly as `check`'s per-file gate does.
    let cfg: Config = serde_yaml::from_str("exclude:\n  - \"**/nested/**\"\n").unwrap();
    let pruned = project_files(&p.root, &cfg);
    assert_eq!(pruned.len(), 2, "the excluded dir is not harvested: {pruned:?}");
}

// ---------------------------------------------------------------------
// Config `exclude:` for the OPEN BUFFER — `check`'s stage-1 file filter.
//
// `check` skips an excluded file before it is even read (`main.rs`
// `Stage1::Excluded`), so it reports no rows for it; the LSP published
// markers for it anyway. The gate below is the same filter, applied to the
// buffer, against the same path SPELLING discovery matches on.
// ---------------------------------------------------------------------

#[test]
fn exclude_gate_agrees_with_bare_check_discovery() {
    // THE INVARIANT (PR #45 review): a buffer is excluded IFF EVERY discovery
    // spelling of that file is excluded. `check` analyses a file if ANY name it
    // was walked under survives `exclude:`, and one file can be walked under
    // several names — a symlinked `.rb` under the LINK's name, an overlapping
    // `paths:` pair under two roots. The first cut of this gate re-derived ONE
    // canonical spelling and silently dropped three shapes `check` analyses
    // (B1/B2/B3 in the note); this differential is what pins the invariant.
    //
    // Driven across `paths:` shapes (incl. the OVERLAPPING multi-root case),
    // pattern sets, root spellings, AND both gate tiers — with a symlinked `.rb`
    // in every fixture, because the symlink and multi-root axes are exactly the
    // two the original 24-run matrix lacked.
    for paths_yaml in [
        "paths:\n  - lib\n",
        "paths:\n  - \".\"\n",
        "paths:\n  - \".\"\n  - lib\n",
        "paths:\n  - lib\n  - \".\"\n",
    ] {
        for patterns in [
            "exclude: []\n",
            "exclude:\n  - \"**/vendor/**\"\n",
            "exclude:\n  - \"**/b.rb\"\n",
            "exclude:\n  - \"**/*.rb\"\n",
            "exclude:\n  - \"lib/a.rb\"\n",
            "exclude:\n  - \"./lib/a.rb\"\n",
            "exclude:\n  - \"./lib/**\"\n",
            "exclude:\n  - \"lib/real.rb\"\n",
            "exclude:\n  - \"vendor/**\"\n",
        ] {
            for spelling in [RootSpelling::Absolute, RootSpelling::DotThroughSymlink] {
                for overlay in [OverlayTier::Live, OverlayTier::Off] {
                    exclude_agreement_run(paths_yaml, patterns, spelling, overlay);
                }
            }
        }
    }
}

/// Which tier of the gate a differential run exercises: with the overlay LIVE
/// the discovery-membership tier answers, with it OFF (the scale guard tripped)
/// the spelling fallback must reach the same verdict on its own.
#[derive(Clone, Copy, Debug)]
enum OverlayTier {
    Live,
    Off,
}

fn exclude_agreement_run(
    paths_yaml: &str,
    patterns: &str,
    spelling: RootSpelling,
    tier: OverlayTier,
) {
    let yaml = format!("{paths_yaml}{patterns}");
    let cfg: Config = serde_yaml::from_str(&yaml).unwrap();
    // The same `paths:` with NO `exclude:` — the unfiltered discovery set, i.e.
    // every candidate the gate has to answer about.
    let unfiltered: Config = serde_yaml::from_str(paths_yaml).unwrap();

    let p = TempProject::new("exclgate");
    p.write("a.rb", "class A\nend\n");
    p.write("b.rb", "class B\nend\n");
    p.write("real.rb", "class Real\nend\n");
    std::fs::create_dir_all(p.root.join("lib/vendor")).unwrap();
    std::fs::write(p.root.join("lib/vendor/v.rb"), "class V\nend\n").unwrap();
    // AXIS 1 (review N1): a symlinked `.rb` FILE inside `lib`, pointing OUT of
    // `lib`. `collect_rb_files` includes it (matching `Dir.glob`), so discovery
    // walks it under `lib/shared.rb` while its canonical path is
    // `<root>/vendor/shared.rb` — the two names carry DIFFERENT `exclude:`
    // verdicts, which is regression B1.
    std::fs::create_dir_all(p.root.join("vendor")).unwrap();
    std::fs::write(p.root.join("vendor/shared.rb"), "class Shared\nend\n").unwrap();
    std::os::unix::fs::symlink(p.root.join("vendor/shared.rb"), p.root.join("lib/shared.rb"))
        .unwrap();
    // …and one pointing INSIDE `lib` (regression B2: `exclude: ["lib/real.rb"]`
    // prunes the target's own spelling but not the link's).
    std::os::unix::fs::symlink(p.root.join("lib/real.rb"), p.root.join("lib/link.rb")).unwrap();

    let alias = matches!(spelling, RootSpelling::DotThroughSymlink)
        .then(|| AliasLink::to(&p.root));
    let _cwd = alias.as_ref().map(|a| CwdGuard::enter(a.path()));
    let root = match &alias {
        Some(_) => PathBuf::from("."),
        None => p.root.clone(),
    };
    // URIs are spelled the way the client would spell them — through the alias
    // in the symlink case, and NEVER with the symlinked file resolved.
    let uri_root = alias.as_ref().map_or_else(|| p.root.clone(), |a| a.path().to_path_buf());

    let all = project_files(&root, &unfiltered);
    let kept = project_files(&root, &cfg);
    let matcher = ExcludeMatcher::from_config(&root, &cfg);
    // The tier-1 substrate: the post-`exclude:` discovery set, canonicalized —
    // exactly what `build_overlay` holds. Built from `kept` directly so the test
    // pins the MEMBERSHIP rule rather than re-testing the harvest.
    let empty_core = CoreIndex::new();
    let held = ProjectFiles {
        files: kept
            .iter()
            .filter_map(|f| {
                let ast = Arc::new(lower(&parse(b"")));
                let harvest = Arc::new(SourceIndex::harvest(&ast, &empty_core));
                Some((std::fs::canonicalize(f).ok()?, ast, harvest))
            })
            .collect(),
    };
    let overlay = match tier {
        OverlayTier::Live => Some(&held),
        OverlayTier::Off => None,
    };

    // GROUND TRUTH: `check` analyses a file iff SOME discovery spelling of it
    // survived `exclude:`. Keyed on the canonical path, because that is the
    // file's identity — two spellings of one file share it.
    let mut analysed: std::collections::HashMap<PathBuf, bool> =
        std::collections::HashMap::new();
    for f in &all {
        let Ok(canonical) = std::fs::canonicalize(f) else { continue };
        *analysed.entry(canonical).or_insert(false) |= kept.contains(f);
    }

    for f in &all {
        let Ok(canonical) = std::fs::canonicalize(f) else { continue };
        // The buffer the editor would open for this discovery spelling: the URI
        // names the file as DISCOVERY did (through the link, through the alias),
        // which is what makes the symlink axis observable at all.
        let rel = spelling_relative_to_root(f, &root);
        let buf = BufferPaths::for_uri(
            &format!("file://{}", uri_root.join(&rel).display())
                .parse::<Uri>()
                .unwrap(),
        );
        assert_eq!(
            matcher.excludes(&buf, overlay),
            !analysed[&canonical],
            "[{yaml:?} / {spelling:?} / {tier:?}] the buffer gate and bare-`check` \
             discovery disagree about {f}"
        );
    }
    // Non-vacuity of the loop itself: the `**/*.rb` case must prune everything
    // and the empty case nothing, so the comparison above is exercised on BOTH
    // answers somewhere in the matrix.
    if patterns.contains("**/*.rb\"") {
        assert!(kept.is_empty(), "[{yaml:?}] the catch-all pattern prunes the whole set");
    }
    if patterns == "exclude: []\n" {
        assert_eq!(kept.len(), all.len(), "[{yaml:?}] no patterns ⇒ nothing pruned");
    }
    // N3 (review): under `RootSpelling::Absolute` the relative patterns
    // (`lib/a.rb`, `./lib/**`, `vendor/**`) match nothing on EITHER side, so
    // those cells agree vacuously. They are kept because they cost nothing and
    // guard the absolute-root path against a future change that starts matching
    // them; the discriminating cells are the `DotThroughSymlink` ones, which is
    // the production root shape anyway.
}

/// A discovery spelling (`lib/a.rb`, `./lib/a.rb`, or an absolute one) reduced
/// to its path relative to the project root, so a client URI can be built for
/// it WITHOUT resolving any symlink on the way.
fn spelling_relative_to_root(spelling: &str, root: &Path) -> PathBuf {
    let path = Path::new(spelling);
    if let Ok(rel) = path.strip_prefix(root) {
        return rel.to_path_buf();
    }
    // A relative spelling: strip a leading `./` and it is already root-relative.
    path.strip_prefix("./").unwrap_or(path).to_path_buf()
}

#[test]
fn exclude_gate_leaves_pathless_and_out_of_workspace_buffers_alone() {
    // Two cases where NO `check` invocation from this root names the file, so
    // there is no spelling to match and the gate must not guess: an untitled /
    // non-`file:` buffer (`path == None`), and a file outside the workspace.
    // Both keep exactly today's behaviour.
    let p = TempProject::new("exclscope");
    let inside = p.write("a.rb", "class A\nend\n");
    let cfg: Config = serde_yaml::from_str("exclude:\n  - \"**/*.rb\"\n").unwrap();
    let matcher = ExcludeMatcher::from_config(&p.root, &cfg);

    assert!(
        matcher.excludes(&buffer_at(&inside), None),
        "the control: an in-project file IS excluded"
    );
    assert!(
        !matcher.excludes(&BufferPaths::default(), None),
        "an untitled buffer has no name to match"
    );

    let outside = TempProject::new("exclscope_other");
    let elsewhere = outside.write("a.rb", "class A\nend\n");
    assert!(
        !matcher.excludes(&buffer_at(&elsewhere), None),
        "a buffer outside the workspace is not `check`'s to exclude"
    );
}

/// The [`BufferPaths`] an editor would send for an existing on-disk `path`.
fn buffer_at(path: &Path) -> BufferPaths {
    BufferPaths::for_uri(&format!("file://{}", path.display()).parse::<Uri>().unwrap())
}

#[test]
fn exclude_gate_uses_the_root_relative_spelling_outside_paths() {
    // A buffer inside the workspace but outside every `paths:` root: bare
    // `check` never discovers it, so the ONLY run that reports on it is an
    // explicit `rigor check spec/x.rb` from the project root — which matches
    // `exclude:` against exactly that root-relative spelling. The gate uses it,
    // so such a buffer is silenced iff that `check` run would report nothing.
    //
    // This does NOT touch the S4b/N5 divergence (an out-of-`paths:` buffer is
    // still analysed against the full project index): it only decides whether
    // the buffer is analysed at all, on the same input `check` decides it on.
    let p = TempProject::new("exclout");
    std::fs::create_dir_all(p.root.join("spec")).unwrap();
    let spec = p.root.join("spec/x_spec.rb");
    std::fs::write(&spec, "class X\nend\n").unwrap();
    let spec = std::fs::canonicalize(&spec).unwrap();
    // The production root shape: `project_root = "."` with the cwd IN the
    // project, which is what makes a relative `exclude:` pattern meaningful.
    let _cwd = CwdGuard::enter(&p.root);

    let buf = buffer_at(&spec);
    let matching: Config = serde_yaml::from_str("exclude:\n  - \"spec/**\"\n").unwrap();
    assert!(
        ExcludeMatcher::from_config(&PathBuf::from("."), &matching).excludes(&buf, None),
        "an out-of-`paths:` buffer is excluded exactly when `rigor check \
         spec/x_spec.rb` from the project root would report nothing for it"
    );
    // The control: a pattern that does NOT cover it leaves it analysed, exactly
    // as today (the N5 divergence is untouched by this slice).
    let other: Config = serde_yaml::from_str("exclude:\n  - \"vendor/**\"\n").unwrap();
    assert!(!ExcludeMatcher::from_config(&PathBuf::from("."), &other).excludes(&buf, None));
}

#[test]
fn exclude_gate_never_drops_a_symlinked_file_check_analyses() {
    // Regressions B1/B2 at the matcher seam (the E2E versions live in
    // `lsp_check_parity.rs`). `collect_rb_files` deliberately INCLUDES symlinked
    // `.rb` files (`main.rs`, the 2026-07-06 audit correction matching
    // `Dir.glob`), so discovery walks the LINK's name — and the link's name and
    // the target's name can carry opposite `exclude:` verdicts.
    let p = TempProject::new("exclsymlink");
    std::fs::create_dir_all(p.root.join("vendor")).unwrap();
    std::fs::write(p.root.join("vendor/shared.rb"), "class Shared\nend\n").unwrap();
    std::os::unix::fs::symlink(p.root.join("vendor/shared.rb"), p.root.join("lib/shared.rb"))
        .unwrap();
    p.write("real.rb", "class Real\nend\n");
    std::os::unix::fs::symlink(p.root.join("lib/real.rb"), p.root.join("lib/link.rb")).unwrap();
    let _cwd = CwdGuard::enter(&p.root);
    let root = PathBuf::from(".");

    // B1: `lib/shared.rb` → `vendor/shared.rb`, excluded by `**/vendor/**`.
    // Discovery keeps `lib/shared.rb`, so `check` analyses it.
    let b1: Config = serde_yaml::from_str("exclude:\n  - \"**/vendor/**\"\n").unwrap();
    assert!(project_files(&root, &b1).iter().any(|f| f.ends_with("lib/shared.rb")));
    assert!(
        !ExcludeMatcher::from_config(&root, &b1)
            .excludes(&buffer_at(&p.root.join("lib/shared.rb")), None),
        "B1: the link's own name survives `exclude:`, so `check` analyses the file"
    );
    // The control: a pattern covering BOTH the link's name and the target's
    // leaves no surviving spelling, so the same file IS excluded — proving the
    // gate is live and that tier 3 rescues only a genuinely surviving name.
    let both: Config =
        serde_yaml::from_str("paths:\n  - \".\"\nexclude:\n  - \"**/shared.rb\"\n").unwrap();
    assert!(
        ExcludeMatcher::from_config(&root, &both)
            .excludes(&buffer_at(&p.root.join("lib/shared.rb")), None),
        "the control: with EVERY spelling excluded the same buffer is dropped"
    );

    // B2: `lib/link.rb` → `lib/real.rb`, excluded by `lib/real.rb`. Discovery
    // keeps `lib/link.rb`, so `check` analyses the content under that name.
    let b2: Config = serde_yaml::from_str("exclude:\n  - \"lib/real.rb\"\n").unwrap();
    assert!(project_files(&root, &b2).iter().any(|f| f.ends_with("lib/link.rb")));
    assert!(
        !ExcludeMatcher::from_config(&root, &b2)
            .excludes(&buffer_at(&p.root.join("lib/link.rb")), None),
        "B2: the link's name is not excluded, so the buffer must be analysed"
    );
}

#[test]
fn exclude_gate_needs_every_root_spelling_excluded() {
    // Regression B3: under OVERLAPPING `paths:` roots one file is walked twice,
    // and `check` analyses it as long as ONE spelling survives. Both root orders
    // are driven, because the first cut returned on the first containing root
    // and so gave an ORDER-DEPENDENT answer.
    let p = TempProject::new("exclmultiroot");
    p.write("a.rb", "class A\nend\n");
    let _cwd = CwdGuard::enter(&p.root);
    let root = PathBuf::from(".");
    let buf = buffer_at(&p.root.join("lib/a.rb"));

    for order in ["paths:\n  - \".\"\n  - lib\n", "paths:\n  - lib\n  - \".\"\n"] {
        let cfg: Config =
            serde_yaml::from_str(&format!("{order}exclude:\n  - \"./lib/**\"\n")).unwrap();
        // Discovery yields `./lib/a.rb` (pruned) AND `lib/a.rb` (kept).
        let kept = project_files(&root, &cfg);
        assert!(kept.iter().any(|f| f == "lib/a.rb"), "[{order:?}] one spelling survives");
        assert!(
            !ExcludeMatcher::from_config(&root, &cfg).excludes(&buf, None),
            "[{order:?}] B3: one surviving spelling means `check` analyses the file"
        );
    }
    // The control: a pattern covering BOTH spellings does exclude it.
    let both: Config =
        serde_yaml::from_str("paths:\n  - \".\"\n  - lib\nexclude:\n  - \"**/a.rb\"\n")
            .unwrap();
    assert!(project_files(&root, &both).is_empty());
    assert!(ExcludeMatcher::from_config(&root, &both).excludes(&buf, None));
}

#[test]
fn an_excluded_buffer_computes_no_diagnostics() {
    // The gate at the `compute_diagnostics` seam, with its control in the same
    // test: the SAME buffer, SAME content, only the config differs.
    let p = TempProject::new("exclcompute");
    let path = p.write("typo.rb", TYPO);

    let buf = buffer_at(&path);
    let control = compute_diagnostics(
        &project_with_config_rooted(&Config::default(), &p.root),
        &buf,
        TYPO,
    );
    assert_eq!(control.0.len(), 1, "the control: the rule fires unconfigured");

    let cfg: Config = serde_yaml::from_str("exclude:\n  - \"**/typo.rb\"\n").unwrap();
    let excluded = compute_diagnostics(&project_with_config_rooted(&cfg, &p.root), &buf, TYPO);
    assert!(
        excluded.0.is_empty(),
        "an `exclude:`d buffer publishes NOTHING, as `check` reports nothing: {:?}",
        excluded.0
    );
}

#[test]
fn swap_project_rebuilds_the_exclude_matcher_from_the_session_config() {
    // The mechanism behind "a buffer that BECOMES excluded ends up cleared":
    // the gate is config-derived and rebuilt by `swap_project`, so it follows
    // `st.cfg` on every `invalidate`. It cannot be driven end to end today
    // because `.rigor.yml` is read ONCE at startup (`invalidate` rebuilds from
    // the same `st.cfg`, matching the reference's `ProjectContext#invalidate!`)
    // — so the transition is exercised HERE, at the seam a config-reload slice
    // would feed, rather than claimed untested.
    let p = TempProject::new("exclswap");
    let path = p.write("a.rb", TYPO);
    let buf = buffer_at(&path);
    let (ctx, mut st) = session_for(&p.root, Config::default(), OVERLAY_BUILD_BUDGET_DEFAULT);
    assert!(
        !st.project.exclude.excludes(&buf, None),
        "the control: nothing is excluded under the starting config"
    );

    // The config gains an `exclude:` entry covering the open buffer…
    st.cfg = serde_yaml::from_str("exclude:\n  - \"**/a.rb\"\n").unwrap();
    let index = Arc::clone(&st.project.index);
    let overlay = st.project.overlay.clone();
    swap_project(&ctx, &mut st, index, overlay);

    assert!(
        st.project.exclude.excludes(&buf, None),
        "the rebuilt context's gate follows `st.cfg` — no stale matcher survives"
    );
    assert_eq!(st.project.generation, 1, "and the swap bumped the generation as always");
}

#[test]
fn integration_excluded_buffer_publishes_empty_and_a_sibling_still_fires() {
    // End to end through the real loop: an `exclude:`d buffer gets an EMPTY
    // publish (a publish, not a silent skip — that is what clears the editor's
    // markers), while a non-excluded sibling in the same project still gets its
    // diagnostics. The sibling is the over-broadness control.
    let p = TempProject::new("exclloop");
    p.write("skipped.rb", TYPO);
    p.write("kept.rb", TYPO);
    p.write_config("exclude:\n  - \"**/skipped.rb\"\n");
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );

    h.notify("textDocument/didOpen", open_params(&p.uri("skipped.rb"), TYPO, 1));
    let d = h.recv_diags();
    assert_eq!(d.uri.as_str(), p.uri("skipped.rb"));
    assert!(
        d.diagnostics.is_empty(),
        "an excluded buffer publishes an EMPTY set (clearing any markers): {:?}",
        d.diagnostics
    );

    h.notify("textDocument/didOpen", open_params(&p.uri("kept.rb"), TYPO, 1));
    let d = h.recv_diags();
    assert_eq!(d.uri.as_str(), p.uri("kept.rb"));
    assert_eq!(
        d.diagnostics.len(),
        1,
        "the control: a non-excluded sibling still gets its diagnostics — the \
         filter is not over-broad: {:?}",
        d.diagnostics
    );

    // An `invalidate` (didChangeConfiguration) re-analyses every open buffer:
    // the excluded one must come back EMPTY again, not with regained markers,
    // which is what proves `swap_project` carried the gate across the rebuild.
    h.notify("workspace/didChangeConfiguration", serde_json::json!({ "settings": {} }));
    let mut seen = std::collections::HashMap::new();
    for _ in 0..2 {
        let d = h.recv_diags();
        seen.insert(d.uri.as_str().to_string(), d.diagnostics.len());
    }
    assert_eq!(seen.get(&p.uri("skipped.rb")), Some(&0), "still empty after invalidate");
    assert_eq!(seen.get(&p.uri("kept.rb")), Some(&1), "and the sibling still fires");
    h.shutdown();
}

// ---------------------------------------------------------------------
// Config reload (2026-08-01) — `.rigor.yml` is re-parsed by every structural
// `invalidate`, so an edit takes effect without restarting the server. Driven
// end to end: a real file on disk, the real watched-files notification, and
// assertions on what the server PUBLISHES — a unit test on `reload_config`
// alone would miss every ordering property below.
// ---------------------------------------------------------------------

#[test]
fn integration_config_reload_disable_takes_effect_without_a_restart() {
    // THE HEADLINE: editing `.rigor.yml` changes the published set in the SAME
    // session. Before this slice the watcher fired, the context rebuilt, and the
    // republished answer was byte-identical to the stale one.
    let p = TempProject::new("cfgreload");
    p.write("t.rb", TYPO);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
    assert_eq!(
        h.recv_diags().diagnostics.len(),
        1,
        "no config yet: the typo fires"
    );

    // The user adds a `disable:` and saves. The editor's watcher names the file.
    p.write_config("disable:\n  - call.undefined-method\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "the NEW `disable:` is honoured on the next publish — no restart"
    );

    // …and removing it again restores the diagnostic (the reload is a re-read,
    // not a one-way accumulation of rules).
    p.write_config("paths:\n  - lib\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    assert_eq!(
        h.recv_diags().diagnostics.len(),
        1,
        "dropping `disable:` brings the diagnostic back"
    );
    h.shutdown();
}

#[test]
fn integration_config_reload_honours_a_deleted_config_as_defaults() {
    // DELETING `.rigor.yml` is NOT the broken-file case: absent means the
    // defaults genuinely ARE the configuration, so it reloads to them
    // immediately and says nothing. This is the discriminator that forced
    // `ConfigRead` to separate `Absent` from `Malformed`.
    let p = TempProject::new("cfgdelete");
    p.write("t.rb", TYPO);
    p.write_config("disable:\n  - call.undefined-method\n");
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "the on-disk `disable:` is read at STARTUP too"
    );

    p.remove_config();
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    // The very next message is the publish — no `window/showMessage`, because a
    // missing config is not an error to disclose.
    assert_eq!(
        h.recv_diags().diagnostics.len(),
        1,
        "a deleted config reloads to DEFAULTS (not 'keep the last good one')"
    );
    h.shutdown();
}

#[test]
fn integration_malformed_config_keeps_the_last_good_one_and_warns_once() {
    // The case the feature lives or dies on: an editor writes `.rigor.yml` on
    // every save, so the server sees half-written YAML constantly.
    //
    // (a) a broken file keeps the LAST GOOD config — `Config::load`'s one-shot
    //     answer (silently substitute the defaults) would drop the user's whole
    //     `disable:` list and flood the buffer mid-keystroke;
    // (b) the warning fires on the TRANSITION, not per save;
    // (c) fixing the file reloads it and says so.
    let p = TempProject::new("cfgbroken");
    p.write("t.rb", TYPO);
    p.write_config("disable:\n  - call.undefined-method\n");
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
    assert!(h.recv_diags().diagnostics.is_empty(), "the good config suppresses it");

    // Save 1 of a half-written file.
    p.write_config("disable: [call.undefined-method\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    let msg = h.recv_show_message();
    assert_eq!(msg.typ, MessageType::WARNING);
    assert!(
        msg.message.contains("keeping the last good configuration"),
        "the disclosure names which config is in force: {}",
        msg.message
    );
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "(a) the last good `disable:` still suppresses the typo — the defaults \
         would have published it"
    );

    // Save 2, still broken. No second popup: the next message is the publish.
    p.write_config("disable: [call.undefined-method, still-unterminated\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "(b) a second broken save re-publishes but does NOT warn again"
    );

    // Fixed — and the fix drops `disable:`, so the diagnostic comes back.
    p.write_config("paths:\n  - lib\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    let msg = h.recv_show_message();
    assert_eq!(msg.typ, MessageType::INFO);
    assert!(
        msg.message.contains("reloaded"),
        "(c) recovery is announced: {}",
        msg.message
    );
    assert_eq!(
        h.recv_diags().diagnostics.len(),
        1,
        "and the fixed config is the one now in force"
    );
    h.shutdown();
}

#[test]
fn integration_config_broken_at_startup_falls_back_to_defaults_and_says_so() {
    // A session that BOOTS on a broken config has no last good one to keep, so
    // it takes the defaults `check` would — but it still records the broken
    // state, so the eventual fix announces itself rather than landing silently.
    let p = TempProject::new("cfgbootbroken");
    p.write("t.rb", TYPO);
    p.write_config("disable: [call.undefined-method\n");
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    let msg = h.recv_show_message();
    assert_eq!(msg.typ, MessageType::WARNING);
    assert!(
        msg.message.contains("DEFAULT settings"),
        "startup says DEFAULTS, not 'last good' — there was never a good one: {}",
        msg.message
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1, "defaults ⇒ the typo fires");

    p.write_config("disable:\n  - call.undefined-method\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    let msg = h.recv_show_message();
    assert_eq!(msg.typ, MessageType::INFO, "the broken-at-boot state recovers: {msg:?}");
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "and the now-readable config takes effect"
    );
    h.shutdown();
}

#[test]
fn integration_config_reload_beats_a_worker_already_in_flight() {
    // THE ORDERING PROPERTY. A worker dispatched under the OLD config is in
    // flight when the config changes. Its answer is now wrong, and it is the
    // NEWER message — so publishing it would leave the editor showing markers
    // the user's saved config forbids, permanently (nothing re-dispatches after
    // it). The generation guard the S4 slice built is what covers this: the
    // reload happens inside `invalidate`, which bumps the generation, so the
    // in-flight result is stale on the generation axis exactly as it would be
    // for an index rebuild. No second invalidation mechanism, no new race.
    let p = TempProject::new("cfgflight");
    p.write("t.rb", TYPO);
    let g = gate_recording_hold_gen0();
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        g.gate.clone(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    // didOpen → a gen-0 worker spawns and blocks in the gate, having read the
    // config that has no `disable:`.
    h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
    hover_sync(&h, 100, &p.uri("t.rb")); // barrier: the gen-0 worker is in flight.

    p.write_config("disable:\n  - call.undefined-method\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    hover_sync(&h, 101, &p.uri("t.rb")); // barrier: the reload is processed.

    g.release_gen0.send(()).unwrap();
    let d = h.recv_diags();
    assert!(
        d.diagnostics.is_empty(),
        "the in-flight gen-0 result (computed under the OLD config, so carrying \
         the diagnostic) is DROPPED, and the re-dispatch publishes under the new \
         config: {:?}",
        d.diagnostics
    );
    hover_sync(&h, 102, &p.uri("t.rb")); // exactly one publish — no stale follow-up.
    let calls = g.calls.lock().unwrap().clone();
    assert!(
        calls.iter().any(|&(_, genr)| genr == 1),
        "a worker ran under the post-reload generation (proves the drop + \
         re-dispatch, not a lucky publish): {calls:?}"
    );
    h.shutdown();
}

#[test]
fn integration_config_reload_picks_up_a_new_exclude_for_an_open_buffer() {
    // `exclude:` is STAGE-1 in `check` (the file is never analysed), and the LSP
    // reproduces it as an empty publish. It is rebuilt by `swap_project` from
    // `st.cfg`, so it rides the reload with no extra wiring — asserted because
    // "rides for free" is exactly the kind of claim that silently stops being
    // true.
    let p = TempProject::new("cfgexclude");
    p.write("t.rb", TYPO);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1);

    p.write_config("exclude:\n  - \"**/t.rb\"\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "a newly-excluded open buffer publishes EMPTY (clearing its markers)"
    );
    h.shutdown();
}

#[test]
fn integration_did_change_configuration_also_re_reads_the_file() {
    // `workspace/didChangeConfiguration` still ignores its client-specific
    // payload, but it no longer rebuilds from the startup parse — it re-reads
    // `.rigor.yml` like any structural invalidation. A client that sends this
    // instead of a watched-file event (several do) gets the same answer.
    let p = TempProject::new("cfgdidchange");
    p.write("t.rb", TYPO);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1);

    p.write_config("disable:\n  - call.undefined-method\n");
    h.notify("workspace/didChangeConfiguration", serde_json::json!({ "settings": {} }));
    assert!(
        h.recv_diags().diagnostics.is_empty(),
        "didChangeConfiguration re-reads the file, not just the context"
    );
    h.shutdown();
}

#[test]
fn integration_config_reload_is_not_triggered_by_a_source_save() {
    // A `.rb` save takes the CHEAP `reharvest_sources` path (review N3), which
    // deliberately does not touch the config — a source file cannot change it.
    // Proven by making the on-disk config disagree with the live one: if a
    // source save reloaded, the still-firing diagnostic would vanish.
    let p = TempProject::new("cfgsrcsave");
    p.write("t.rb", TYPO);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1);

    p.write_config("disable:\n  - call.undefined-method\n");
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.uri("t.rb")));
    assert_eq!(
        h.recv_diags().diagnostics.len(),
        1,
        "a source save re-harvests ASTs only — the config surface is untouched"
    );
    // …and the config-file event that follows the save does apply it.
    h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
    assert!(h.recv_diags().diagnostics.is_empty());
    h.shutdown();
}

/// The `initialize` root precedence, pinned field by field. Pure — no
/// filesystem, no cwd — so it can assert the RULE without the `chdir` half.
#[test]
fn requested_root_follows_the_protocols_modernity_order() {
    let folders = |uris: &[&str]| {
        serde_json::Value::Array(
            uris.iter()
                .map(|u| serde_json::json!({ "uri": u, "name": u.rsplit('/').next() }))
                .collect(),
        )
    };

    // Nothing named ⇒ no root, no disclosure: the cwd default stands.
    assert_eq!(requested_root(&serde_json::json!({})), RootRequest::default());
    // `null` and `[]` are both "no folder open" and must NOT be read as roots.
    assert_eq!(
        requested_root(&serde_json::json!({ "workspaceFolders": null, "rootUri": null })),
        RootRequest::default()
    );
    assert_eq!(
        requested_root(&serde_json::json!({ "workspaceFolders": [] })),
        RootRequest::default()
    );

    // `workspaceFolders` (current) BEATS `rootUri` (deprecated) beats
    // `rootPath` (legacy) — all three present, most modern wins.
    let all = serde_json::json!({
        "workspaceFolders": folders(&["file:///w/folders"]),
        "rootUri": "file:///w/uri",
        "rootPath": "/w/path",
    });
    assert_eq!(requested_root(&all).path.as_deref(), Some(Path::new("/w/folders")));
    // …drop the most modern and the next one takes over, twice.
    let no_folders = serde_json::json!({ "rootUri": "file:///w/uri", "rootPath": "/w/path" });
    assert_eq!(requested_root(&no_folders).path.as_deref(), Some(Path::new("/w/uri")));
    let legacy = serde_json::json!({ "rootPath": "/w/path" });
    assert_eq!(requested_root(&legacy).path.as_deref(), Some(Path::new("/w/path")));
    // `rootPath` is a PLAIN PATH, not a URI: it is taken verbatim, and an
    // empty string is not a path.
    assert_eq!(requested_root(&serde_json::json!({ "rootPath": "" })), RootRequest::default());

    // Percent-escapes decode through the SAME decoder buffer URIs use.
    assert_eq!(
        requested_root(&serde_json::json!({ "rootUri": "file:///w/my%20proj" }))
            .path
            .as_deref(),
        Some(Path::new("/w/my proj"))
    );

    // A virtual workspace has no local directory: no path, but it IS named,
    // which is what makes the fallback disclosed rather than silent.
    let virt = requested_root(&serde_json::json!({ "rootUri": "vscode-vfs://host/repo" }));
    assert_eq!(virt.path, None);
    assert_eq!(virt.named.as_deref(), Some("vscode-vfs://host/repo"));
    // …and a non-`file:` FOLDER falls through to a usable `rootUri` rather
    // than aborting (the forgiving direction).
    let mixed = requested_root(&serde_json::json!({
        "workspaceFolders": folders(&["vscode-vfs://host/repo"]),
        "rootUri": "file:///w/real",
    }));
    assert_eq!(mixed.path.as_deref(), Some(Path::new("/w/real")));

    // MULTI-ROOT: the first folder wins and the rest are recorded for the
    // disclosure. Order is the client's, and it is what we pin.
    let multi = requested_root(&serde_json::json!({
        "workspaceFolders": folders(&["file:///w/a", "file:///w/b", "file:///w/c"]),
    }));
    assert_eq!(multi.path.as_deref(), Some(Path::new("/w/a")));
    assert_eq!(multi.ignored_folders, vec!["b".to_string(), "c".to_string()]);
    // A SINGLE folder is not a degradation and owes nothing.
    let single = requested_root(&serde_json::json!({
        "workspaceFolders": folders(&["file:///w/a"]),
    }));
    assert!(single.ignored_folders.is_empty());
}

#[test]
fn uri_path_decoding_round_trips_a_spaced_path() {
    // The overlay's REPLACE lookup keys on the canonical path decoded from the
    // buffer URI, so percent-escapes must decode (an editor sends `%20`).
    assert_eq!(percent_decode("/a/b%20c/d.rb"), "/a/b c/d.rb");
    assert_eq!(percent_decode("/plain/path.rb"), "/plain/path.rb");
    assert_eq!(percent_decode("/bad/%zz.rb"), "/bad/%zz.rb", "invalid escapes pass through");
    let p = TempProject::new("uri");
    let canonical = p.write("with space.rb", "x = 1\n");
    let uri: Uri = format!("file://{}", p.root.join("lib").join("with%20space.rb").display())
        .parse()
        .unwrap();
    assert_eq!(uri_to_canonical_path(&uri).as_deref(), Some(canonical.as_path()));
    // A non-`file:` URI, or one whose DIRECTORY does not exist, has no on-disk
    // identity ⇒ `None` ⇒ the overlay APPENDS the buffer instead of replacing.
    let untitled: Uri = "untitled:Untitled-1".parse().unwrap();
    assert!(uri_to_canonical_path(&untitled).is_none());
    let missing: Uri = "file:///no/such/directory/anywhere.rb".parse().unwrap();
    assert!(uri_to_canonical_path(&missing).is_none());

    // But a file that is GONE from an EXISTING directory still resolves (review
    // B1): it keeps the identity tier 1 recorded, so the REPLACE lookup hits.
    let gone = p.write("gone.rb", "x = 1\n");
    std::fs::remove_file(&gone).unwrap();
    let gone_uri: Uri = p.uri("gone.rb").parse().unwrap();
    assert_eq!(
        uri_to_canonical_path(&gone_uri).as_deref(),
        Some(gone.as_path()),
        "a deleted file keeps its canonical identity via its parent directory"
    );
}

/// PROBE A [BLOCKING]: `rm -rf` of a SUBDIRECTORY holding project files.
/// `uri_to_canonical_path` resolves a deleted file via its PARENT; when the
/// parent directory is gone too it returns `None`, and `reharvest_sources`
/// `continue`s (lsp.rs:1070) — so the stale AST is never removed. The pre-N3
/// full rebuild dropped it.
#[test]
fn probe_a_directory_deletion_leaves_stale_asts() {
    let p = TempProject::new("probe_a");
    std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
    std::fs::write(p.root.join("lib/nested/base.rb"), BASE_RB).unwrap();
    p.write("sub.rb", SUB_RB);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
    assert_eq!(h.recv_diags().diagnostics.len(), 1, "override fires against Base");

    // rm -rf lib/nested  (a `git checkout` that drops a directory)
    std::fs::remove_dir_all(p.root.join("lib/nested")).unwrap();
    let gone = format!("file://{}", p.root.join("lib/nested/base.rb").display());
    h.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [ { "uri": gone, "type": 3 } ] }),
    );
    let d = h.recv_diags();
    assert!(
        d.diagnostics.is_empty(),
        "PROBE A FAILED: stale AST for a file in a deleted DIRECTORY survives \
         the incremental re-harvest: {:?}",
        d.diagnostics
    );
    h.shutdown();
}

/// PROBE D [BLOCKING, fixed]: the guard's OFF->ON flip used to happen in
/// `handle_result`, which never re-installs the overlay. Samples arrive from
/// workers dispatched BEFORE the disable-swap (a concurrent per-URI dispatch, a
/// buffer closed mid-flight), so under-budget stragglers after a trip re-ENABLED
/// the guard while `project.overlay` was still `None` — terminal, because with
/// no overlay no further sample is ever produced, and the user was told
/// "re-enabled ... for 0 files" (the count read from the already-emptied
/// overlay).
///
/// The fix: worker samples are IGNORED while the guard is disabled. They
/// necessarily predate the disable-swap, so they carry no information about the
/// current posture. Recovery lives in `invalidate`, where the overlay is being
/// rebuilt anyway (see the companion test below).
#[test]
fn probe_d_worker_samples_are_ignored_while_the_guard_is_disabled() {
    let p = TempProject::new("probe_d");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    let (server_conn, _client) = Connection::memory();
    let (ctx, mut st) = session_for(&p.root, Config::default(), Duration::from_millis(100));
    assert_eq!(st.project.overlay.as_ref().unwrap().files.len(), 2);

    // A result whose buffer is not open is DROPPED for publishing but still
    // reaches the guard — the minimal reachable shape of "a sample from a
    // dispatch that predates the current posture".
    let uri: Uri = p.uri("sub.rb").parse().unwrap();
    let feed = |st: &mut Session, ms: u64| {
        handle_result(
            &server_conn,
            &ctx,
            st,
            Computed {
                uri: uri.clone(),
                version: 1,
                generation: 0,
                epoch: 0,
                diags: Vec::new(),
                overlay_build: Some(Duration::from_millis(ms)),
                project_index: None,
            },
        )
        .unwrap();
    };
    feed(&mut st, 150); // over #1
    feed(&mut st, 150); // over #2 -> Disabled, overlay dropped
    assert!(!st.guard.enabled, "two over-budget samples disable");
    assert!(st.project.overlay.is_none(), "the held files are dropped");

    // Stragglers must NOT flip the posture from here.
    feed(&mut st, 10);
    feed(&mut st, 10);
    assert!(
        !st.guard.enabled,
        "a worker sample that predates the disable cannot re-enable the guard"
    );
    assert!(
        st.project.overlay.is_none(),
        "and the overlay must never be ENABLED-but-empty (that state is terminal)"
    );
    drop(st);
}

#[test]
fn guard_recovers_through_a_structural_rebuild() {
    // The companion to probe D: recovery is real, and it happens where the
    // overlay is rebuilt anyway. A single under-budget sample re-enables
    // (asymmetric hysteresis), the freshly built ASTs are installed, and the
    // disclosure reports the count from the NEW overlay — not 0.
    let p = TempProject::new("recover");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    let (mut ctx, mut st) = session_for(&p.root, Config::default(), Duration::ZERO);

    // Trip it with two over-budget samples (budget ZERO => every sample is over).
    let (server_conn, _client) = Connection::memory();
    let uri: Uri = p.uri("sub.rb").parse().unwrap();
    for _ in 0..2 {
        handle_result(
            &server_conn,
            &ctx,
            &mut st,
            Computed {
                uri: uri.clone(),
                version: 1,
                generation: 0,
                epoch: 0,
                diags: Vec::new(),
                overlay_build: Some(Duration::from_millis(1)),
                project_index: None,
            },
        )
        .unwrap();
    }
    assert!(!st.guard.enabled && st.project.overlay.is_none(), "tripped");

    // A structural invalidation under a generous budget: ONE under-budget
    // sample restores the overlay.
    ctx.overlay_budget = Duration::from_secs(30);
    // `invalidate` can owe more than one disclosure now (a config-reload state
    // change, then the guard flip); pick the guard's out of the batch.
    let disclosures = invalidate(&ctx, &mut st);
    let msg = disclosures
        .iter()
        .map(|(_, m)| m.clone())
        .find(|m| m.contains("re-enabled"))
        .unwrap_or_else(|| panic!("a posture flip discloses: {disclosures:?}"));
    assert!(st.guard.enabled, "one under-budget rebuild re-enables");
    let restored = st.project.overlay.as_ref().expect("the overlay is re-installed");
    assert_eq!(restored.files.len(), 2, "with the freshly harvested ASTs");
    assert!(
        msg.contains("re-enabled") && msg.contains("2 files"),
        "the disclosure reports the NEW overlay's count, not the emptied one: {msg}"
    );
}


/// PROBE E [BLOCKING]: `touches_configured_root` compares the URI's DECODED
/// spelling against the CANONICALIZED configured root. When the two spellings
/// differ — a workspace reached through a symlink, the normal macOS shape
/// (`/tmp` -> `/private/tmp`) — and the path cannot be canonicalized (a DELETED
/// DIRECTORY, the only case where the decoded spelling is the sole candidate),
/// the predicate answers "ignore" for an event a full rebuild WOULD have acted
/// on, and the stale AST survives. B-1's residue.
#[test]
fn probe_e_alias_spelled_uri_for_a_deleted_directory_is_wrongly_ignored() {
    let p = TempProject::new("probe_e");
    p.write("a.rb", "class A\nend\n");
    std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
    std::fs::write(p.root.join("lib/nested/c.rb"), "class C\nend\n").unwrap();
    let alias = p.root.parent().unwrap().join(format!(
        "{}_alias",
        p.root.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_file(&alias);
    std::os::unix::fs::symlink(&p.root, &alias).unwrap();

    let cfg = Config::default(); // paths: ["lib"]
    let cfg2 = Config::default();
    let (ctx, mut st) = session_for(&p.root, cfg2, OVERLAY_BUILD_BUDGET_DEFAULT);
    let index = Arc::new(CoreIndex::new());
    assert_eq!(st.project.overlay.as_ref().unwrap().files.len(), 2, "a.rb + nested/c.rb");

    // rm -rf lib/nested, announced under the ALIAS spelling.
    std::fs::remove_dir_all(p.root.join("lib/nested")).unwrap();
    let gone = format!("file://{}", alias.join("lib/nested/c.rb").display());
    let uri: Uri = gone.parse().unwrap();
    assert!(
        uri_to_canonical_path(&uri).is_none(),
        "precondition: a deleted directory leaves no canonical form"
    );
    assert!(
        !watched_event_is_ignorable(&ctx, &st.cfg, None, &uri),
        "PROBE E FAILED (rule): the event is treated as ignorable although a full \
         rebuild would act on it, so the stale entry is never dropped"
    );

    reharvest_sources(&ctx, &mut st, &[gone]);
    let truth = build_overlay(&p.root, &cfg, &index);
    let held = st.project.overlay.as_ref().unwrap();
    let _ = std::fs::remove_file(&alias);
    assert_eq!(
        overlay_fingerprint(held),
        overlay_fingerprint(&truth.files),
        "PROBE E FAILED (state): incremental kept a stale AST for a file in a \
         deleted directory"
    );
}

/// PROBE F [BLOCKING, fixed]: the LITERAL production shape — `project_root =
/// "."` with the workspace entered through a SYMLINK, as an editor launched
/// from the user-visible path gives it.
///
/// This shape used to be the one the scope comparison could not survive. With
/// `project_root = "."`, `join_root` yields the RELATIVE `"lib"`, so the
/// literal-root comparison could never match an absolute candidate and the
/// decision rested entirely on the canonicalized root — against which the URI's
/// decoded, symlink-spelled path does not match either. A deleted-directory
/// event was therefore judged out of scope and its stale AST kept. (That
/// literal-root arm is now deleted, and the ignore rule short-circuits before
/// any comparison when the path does not resolve — see
/// `watched_event_is_ignorable`.)
///
/// The assertion is cwd-independent TODAY only because of that short-circuit,
/// so the production cwd shape is still set up deliberately: if the
/// short-circuit is ever removed, this test must go back to exercising the
/// comparison under the spelling that broke it.
#[test]
fn probe_f_production_root_shape_never_ignores_a_symlinked_deleted_directory() {
    let p = TempProject::new("probe_f");
    p.write("a.rb", "class A\nend\n");
    std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
    std::fs::write(p.root.join("lib/nested/c.rb"), "class C\nend\n").unwrap();
    // Declared before the cwd guard so it is dropped AFTER it: the cwd is
    // restored first, then the symlink is removed.
    let alias = AliasLink::to(&p.root);
    // Enter the workspace through the SYMLINK. `CwdGuard` holds the process-wide
    // cwd mutex for the rest of this scope and restores the previous directory
    // on drop — including on a panic — so no concurrently-running test can
    // observe (or be stranded by) this mutation.
    let _cwd = CwdGuard::enter(alias.path());

    let ctx = ServerContext {
        debounce: Duration::from_secs(30),
        worker_gate: production_gate(),
        watched_files_dynamic_registration: false,
        project_root: PathBuf::from("."), // <- production
        overlay_budget: OVERLAY_BUILD_BUDGET_DEFAULT,
    };
    let cfg = Config::default();
    std::fs::remove_dir_all(p.root.join("lib/nested")).unwrap();
    let gone = format!("file://{}", alias.path().join("lib/nested/c.rb").display());
    let uri: Uri = gone.parse().unwrap();
    let canonical = uri_to_canonical_path(&uri);
    assert!(canonical.is_none(), "precondition: deleted directory");
    assert!(
        !watched_event_is_ignorable(&ctx, &cfg, canonical.as_deref(), &uri),
        "PROBE F FAILED: under the production root shape a deleted-directory \
         event spelled through the workspace symlink is IGNORED"
    );
}

// =======================================================================
// Cross-file cache — the per-URI last-good project index the SYNCHRONOUS
// handlers answer from (mini-spec 20260826-lsp-crossfile-cache-mini-spec.md).
// =======================================================================

/// `Beta#label` returns a `String` — a tier-4b method return, so a caller in
/// ANOTHER file types only with project-wide context. Defined in `beta.rb`.
const XF_BETA_RB: &str = "class Beta\n  def label\n    \"x\"\n  end\nend\n";

/// The buffer under test, in a DIFFERENT file from `Beta`. Deliberately
/// PARSEABLE: the cache is written by the diagnostics dispatch, and a
/// dispatch over a buffer Prism cannot parse produces no index at all (it
/// returns early, exactly as it already did for its diagnostics).
///
/// * `w = Beta.new.label` — hover on `label` (line 0, char 16) is
///   `Beta#label → String` only cross-file.
/// * `w.upcase` — completion right after the dot (line 1, char 2) needs `w`
///   to be a `String`, which needs `Beta#label`'s return, which needs
///   `beta.rb`.
const XF_ALPHA_RB: &str = "w = Beta.new.label\nw.upcase\n";

/// A project namespace whose children live in a project `.rb`, not in RBS.
const XF_WRAPPER_RB: &str =
    "module Wrapper\n  class Inner\n  end\n  module Mixin\n  end\nend\n";

/// The kind-conflict fixture: the project reopens core `Process` and declares
/// `Status` as a MODULE, where core RBS declares it a CLASS.
const XF_PROCESS_RB: &str = "module Process\n  module Status\n  end\nend\n";

/// Round-trip a hover request and return its markdown body (empty on a null
/// hover). Unlike [`hover_sync`] this reads the CONTENT, which is what the
/// cross-file property is stated in.
fn xf_hover_request(h: &Harness, id: i32, uri: &str, line: u32, character: u32) -> String {
    h.request(
        id,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    match h.recv() {
        Message::Response(r) => {
            assert_eq!(r.id, RequestId::from(id));
            r.result
                .and_then(|v| serde_json::from_value::<Hover>(v).ok())
                .map(|hv| match hv.contents {
                    HoverContents::Markup(m) => m.value,
                    other => panic!("expected markup hover, got {other:?}"),
                })
                .unwrap_or_default()
        }
        other => panic!("expected a hover response, got {other:?}"),
    }
}

/// Round-trip a completion request and return `(label, kind)` per item.
fn xf_completion_request(
    h: &Harness,
    id: i32,
    uri: &str,
    line: u32,
    character: u32,
) -> Vec<(String, Option<CompletionItemKind>)> {
    h.request(
        id,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    match h.recv() {
        Message::Response(r) => {
            assert_eq!(r.id, RequestId::from(id));
            match r.result.and_then(|v| {
                serde_json::from_value::<Option<CompletionResponse>>(v).ok().flatten()
            }) {
                Some(CompletionResponse::Array(items)) => {
                    items.into_iter().map(|i| (i.label, i.kind)).collect()
                }
                _ => Vec::new(),
            }
        }
        other => panic!("expected a completion response, got {other:?}"),
    }
}

/// Just the labels, for the set assertions.
fn xf_labels(items: &[(String, Option<CompletionItemKind>)]) -> Vec<String> {
    items.iter().map(|(l, _)| l.clone()).collect()
}

// --- family 1: cross-file hover ---------------------------------------

#[test]
fn crossfile_hover_answers_from_the_cached_project_index() {
    // THE KEYSTONE for hover. A class defined in file B, hovered in open file
    // A: single-file BEFORE A's first dispatch has published, cross-file
    // after. Holding the v1 worker mid-flight is what makes "before any
    // dispatch" a real, deterministic state rather than a race.
    let p = TempProject::new("xf_hover");
    p.write("beta.rb", XF_BETA_RB);
    p.write("alpha.rb", XF_ALPHA_RB);
    let g = gate_holding(&[1], &[]);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        g.gate.clone(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("alpha.rb"), XF_ALPHA_RB, 1));

    // MISS — the only dispatch is still blocked in the gate, so no entry
    // exists and the hover is today's single-file answer.
    let before = xf_hover_request(&h, 10, &p.uri("alpha.rb"), 0, 16);
    assert!(
        before.contains("Dynamic[top]#label"),
        "before any dispatch the hover must fall back to the single-file index: {before}"
    );

    // Let the dispatch finish: publishing is also where the cache is written.
    g.release(1);
    assert!(h.recv_diags().diagnostics.is_empty(), "the fixture is clean");

    // HIT — `Beta` lives in the OTHER file and now types.
    let after = xf_hover_request(&h, 11, &p.uri("alpha.rb"), 0, 16);
    assert!(
        after.contains("Beta#label → String"),
        "after the dispatch the hover must see `Beta` from beta.rb: {after}"
    );
    assert_ne!(before, after, "the two postures must actually differ");
    h.shutdown();
}

// --- family 2: cross-file method completion ----------------------------

#[test]
fn crossfile_completion_answers_from_the_cached_project_index() {
    // Same shape for method completion: `w = Beta.new.label` makes `w` a
    // String ONLY if `Beta#label`'s return is known, which needs beta.rb.
    let p = TempProject::new("xf_completion");
    p.write("beta.rb", XF_BETA_RB);
    p.write("alpha.rb", XF_ALPHA_RB);
    let g = gate_holding(&[1], &[]);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        g.gate.clone(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("alpha.rb"), XF_ALPHA_RB, 1));

    // MISS — a receiver whose class lives in another file is unresolved, so
    // the list is null/empty (never a guess).
    let before = xf_completion_request(&h, 20, &p.uri("alpha.rb"), 1, 2);
    assert!(
        before.is_empty(),
        "before any dispatch the receiver is unresolved: {before:?}"
    );

    g.release(1);
    assert!(h.recv_diags().diagnostics.is_empty());

    // HIT — `w` types as String, so the String instance surface is offered.
    let after = xf_labels(&xf_completion_request(&h, 21, &p.uri("alpha.rb"), 1, 2));
    assert!(after.contains(&"upcase".to_string()), "String#upcase offered: {after:?}");
    assert!(after.contains(&"length".to_string()), "String#length offered");
    assert!(
        !after.contains(&"respond_to_missing?".to_string()),
        "and the LSP-v4 private filter still applies on a cache hit"
    );
    h.shutdown();
}

// --- family 3: `Foo::` namespace completion ----------------------------

#[test]
fn crossfile_namespace_completion_offers_project_source_children() {
    // The LSP-v4 note's withheld item: a nested constant declared in a
    // project `.rb` (never in RBS) is offered after `Foo::`. `Wrapper::Inner`
    // is spelled as an uppercase PARTIAL so the buffer parses — the cache is
    // written by a diagnostics dispatch, which needs a parseable buffer.
    let p = TempProject::new("xf_namespace");
    p.write("wrapper.rb", XF_WRAPPER_RB);
    let use_rb = "Wrapper::Inner\n";
    p.write("use.rb", use_rb);
    let g = gate_holding(&[1], &[]);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        g.gate.clone(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("use.rb"), use_rb, 1));

    // MISS — RBS knows no `Wrapper`, so the answer is the null completion the
    // RBS-only path has always returned.
    let before = xf_completion_request(&h, 30, &p.uri("use.rb"), 0, 14);
    assert!(before.is_empty(), "RBS-only ⇒ no children for a project namespace: {before:?}");

    g.release(1);
    let _ = h.recv_diags();

    // HIT — both children, each with its DECLARED kind.
    let after = xf_completion_request(&h, 31, &p.uri("use.rb"), 0, 14);
    assert_eq!(
        after,
        vec![
            ("Inner".to_string(), Some(CompletionItemKind::CLASS)),
            ("Mixin".to_string(), Some(CompletionItemKind::MODULE)),
        ],
        "immediate children only, name-sorted, class-vs-module rendered"
    );
    h.shutdown();
}

#[test]
fn crossfile_namespace_completion_keeps_rbs_results_and_rbs_wins_a_kind_conflict() {
    // Two properties in one fixture, because they are the same risk: the
    // union must not disturb the RBS answer.
    //
    // (a) RBS RESULTS UNCHANGED — `Process::`'s children are byte-identical
    //     with and without a cache entry.
    // (b) KIND CONFLICT — the project declares `module Process::Status`
    //     where core RBS declares a CLASS. The name appears ONCE, as a CLASS.
    let p = TempProject::new("xf_kind");
    p.write("proc.rb", XF_PROCESS_RB);
    let use_rb = "Process::Status\n";
    p.write("use.rb", use_rb);
    let (_ctx, st) = session_for(&p.root, Config::default(), OVERLAY_BUILD_BUDGET_DEFAULT);

    // The exact index a dispatch for `use.rb` would hand back.
    let ast = lower(&parse(use_rb.as_bytes()));
    let canonical = std::fs::canonicalize(p.root.join("lib").join("use.rb")).unwrap();
    let (cached, sample) = overlay_source_index(&st.project, Some(&canonical), &ast);
    assert!(sample.is_some(), "precondition: the overlay is live");
    assert_eq!(
        cached.namespace_children("Process"),
        vec![("Status", true)],
        "precondition: the project's own view says Status is a MODULE"
    );

    let mut buffers = BufferTable::new();
    let uri: Uri = p.uri("use.rb").parse().unwrap();
    buffers.open(&uri, use_rb.to_string(), 1);
    let params = CompletionParams {
        text_document_position: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            position: Position { line: 0, character: 15 },
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    };
    let items = |cached: Option<&SourceIndex>| match completion(
        &st.project,
        &buffers,
        &params,
        cached,
    ) {
        Some(CompletionResponse::Array(v)) => {
            v.into_iter().map(|i| (i.label, i.kind, i.detail)).collect::<Vec<_>>()
        }
        _ => Vec::new(),
    };
    let rbs_only = items(None);
    let unioned = items(Some(&cached));
    assert!(!rbs_only.is_empty(), "precondition: core RBS knows Process's children");
    assert_eq!(
        unioned, rbs_only,
        "(a) a namespace whose only project child collides with an RBS one must \
         be byte-identical to the RBS-only answer"
    );
    let statuses: Vec<_> = unioned.iter().filter(|(l, ..)| l == "Status").collect();
    assert_eq!(statuses.len(), 1, "(b) deduplicated: Status appears once");
    assert_eq!(
        statuses[0].1,
        Some(CompletionItemKind::CLASS),
        "(b) RBS is the declaration of record ⇒ its CLASS kind wins over the \
         project's `module Status`"
    );
}

// --- family 4: the same-URI guard --------------------------------------

#[test]
fn crossfile_cache_is_never_read_across_uris() {
    // THE CORRECTNESS GUARD. An index cached for A carries A's dirty buffer
    // REPLACING A's on-disk file, so answering B from it would serve A's
    // unsaved edits for B — the double-registration hazard the REPLACE rule
    // exists to prevent, moved into the query handlers. With A dispatched and
    // B's worker held mid-flight, B must still get the single-file answer.
    let p = TempProject::new("xf_same_uri");
    p.write("beta.rb", XF_BETA_RB);
    p.write("alpha.rb", XF_ALPHA_RB);
    p.write("other.rb", XF_ALPHA_RB);
    // B opens at version 7 so the gate can hold ONLY B's worker.
    let g = gate_holding(&[7], &[]);
    let mut h = Harness::start_project(
        Duration::from_secs(30),
        g.gate.clone(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    // A dispatches to completion, so A's entry exists.
    h.notify("textDocument/didOpen", open_params(&p.uri("alpha.rb"), XF_ALPHA_RB, 1));
    assert!(h.recv_diags().diagnostics.is_empty());
    // B opens; its worker blocks, so B has NO entry of its own.
    h.notify("textDocument/didOpen", open_params(&p.uri("other.rb"), XF_ALPHA_RB, 7));

    let in_b = xf_hover_request(&h, 40, &p.uri("other.rb"), 0, 16);
    assert!(
        in_b.contains("Dynamic[top]#label"),
        "B must NOT be answered from A's cached index: {in_b}"
    );
    assert!(
        !in_b.contains("Beta#label"),
        "an index cached for one URI carries THAT URI's overlay: {in_b}"
    );
    // The control: the very same question in A, whose entry does exist,
    // resolves — so the assertion above is a same-URI guard, not a broken
    // fixture.
    let in_a = xf_hover_request(&h, 41, &p.uri("alpha.rb"), 0, 16);
    assert!(in_a.contains("Beta#label → String"), "control: A still resolves: {in_a}");

    g.release(7);
    let _ = h.recv_diags();
    h.shutdown();
}

// --- family 5: eviction / population gate / invalidation ---------------

/// Feed `handle_result` a LIVE result for `uri` carrying `index` — the exact
/// shape a worker sends after an overlay-on dispatch.
fn xf_feed_live(
    connection: &Connection,
    ctx: &ServerContext,
    st: &mut Session,
    uri: &Uri,
    index: Option<Arc<SourceIndex>>,
    sample: Option<Duration>,
) {
    let generation = st.project.generation;
    let epoch = current_epoch(st, uri);
    let version = st.buffers.current_version(uri).expect("an open buffer");
    handle_result(
        connection,
        ctx,
        st,
        Computed {
            uri: uri.clone(),
            version,
            generation,
            epoch,
            diags: Vec::new(),
            overlay_build: sample,
            project_index: index,
        },
    )
    .unwrap();
}

#[test]
fn crossfile_cache_didclose_evicts_the_uris_entry() {
    // `didClose` drops the entry outright: a closed document can receive no
    // hover/completion request, so holding its index is pure retention.
    let p = TempProject::new("xf_close");
    p.write("beta.rb", XF_BETA_RB);
    p.write("alpha.rb", XF_ALPHA_RB);
    let (ctx, mut st) = session_for(&p.root, Config::default(), OVERLAY_BUILD_BUDGET_DEFAULT);
    let (server_conn, _client) = Connection::memory();
    let uri: Uri = p.uri("alpha.rb").parse().unwrap();
    st.buffers.open(&uri, XF_ALPHA_RB.to_string(), 1);

    let ast = lower(&parse(XF_ALPHA_RB.as_bytes()));
    let canonical = std::fs::canonicalize(p.root.join("lib").join("alpha.rb")).unwrap();
    let (index, sample) = overlay_source_index(&st.project, Some(&canonical), &ast);
    xf_feed_live(&server_conn, &ctx, &mut st, &uri, Some(index), sample);
    assert!(st.crossfile.contains(&uri), "a live overlay dispatch populates");

    handle_message(
        &server_conn,
        &ctx,
        &mut st,
        Message::Notification(Notification::new(
            "textDocument/didClose".to_string(),
            serde_json::json!({ "textDocument": { "uri": p.uri("alpha.rb") } }),
        )),
    )
    .unwrap();
    assert!(!st.crossfile.contains(&uri), "didClose evicts the URI's entry");
    assert_eq!(st.crossfile.len(), 0);
}

#[test]
fn crossfile_cache_is_never_populated_by_a_guard_off_dispatch() {
    // The population gate, at its source: with the overlay OFF a dispatch
    // builds the SAME single-file index hover/completion already build per
    // request, so caching it would buy nothing and cost staleness. The index
    // slot and the guard sample are `Some` on exactly the same condition.
    let p = TempProject::new("xf_guard_off");
    p.write("beta.rb", XF_BETA_RB);
    p.write("alpha.rb", XF_ALPHA_RB);
    let (ctx, mut st) = session_for(&p.root, Config::default(), OVERLAY_BUILD_BUDGET_DEFAULT);
    let (server_conn, _client) = Connection::memory();
    let uri: Uri = p.uri("alpha.rb").parse().unwrap();
    st.buffers.open(&uri, XF_ALPHA_RB.to_string(), 1);
    let buf = BufferPaths::for_uri(&uri);

    // Overlay ON: both slots filled.
    let (_d, sample_on, index_on) = compute_diagnostics(&st.project, &buf, XF_ALPHA_RB);
    assert!(sample_on.is_some() && index_on.is_some(), "overlay on ⇒ an index to cache");

    // Overlay OFF (the guard-tripped / empty-project posture): both empty.
    let index = Arc::clone(&st.project.index);
    swap_project(&ctx, &mut st, index, None);
    let (_d, sample_off, index_off) = compute_diagnostics(&st.project, &buf, XF_ALPHA_RB);
    assert!(sample_off.is_none(), "precondition: no overlay ⇒ no guard sample");
    assert!(index_off.is_none(), "and no cache payload — the two move together");

    // …and a result carrying no index caches nothing, even on the live path.
    xf_feed_live(&server_conn, &ctx, &mut st, &uri, None, None);
    assert_eq!(st.crossfile.len(), 0, "a guard-off dispatch populates nothing");
}

#[test]
fn crossfile_cache_is_cleared_by_the_generation_bump_a_guard_trip_makes() {
    // Invalidation, driven by the S4b forced-low-threshold pattern: with the
    // budget forced to ZERO every sample is over it, so the SECOND sample
    // trips the guard, which calls `swap_project` — a generation bump — and
    // the whole cache dies with the context it was built against.
    let p = TempProject::new("xf_generation");
    p.write("beta.rb", XF_BETA_RB);
    p.write("alpha.rb", XF_ALPHA_RB);
    let (ctx, mut st) = session_for(&p.root, Config::default(), Duration::ZERO);
    let (server_conn, _client) = Connection::memory();
    let uri: Uri = p.uri("alpha.rb").parse().unwrap();
    st.buffers.open(&uri, XF_ALPHA_RB.to_string(), 1);

    let ast = lower(&parse(XF_ALPHA_RB.as_bytes()));
    let canonical = std::fs::canonicalize(p.root.join("lib").join("alpha.rb")).unwrap();
    let feed = |st: &mut Session| {
        let (index, sample) = overlay_source_index(&st.project, Some(&canonical), &ast);
        xf_feed_live(&server_conn, &ctx, st, &uri, Some(index), sample);
    };

    // Sample #1: over budget, but hysteresis means no trip — the entry is
    // written and readable.
    feed(&mut st);
    let generation = st.project.generation;
    assert_eq!(st.crossfile.len(), 1, "the first live dispatch populates");
    assert!(st.crossfile.get(&uri, generation).is_some(), "and it reads back");

    // Sample #2: the guard trips → `swap_project` → generation bump.
    feed(&mut st);
    assert!(!st.guard.enabled, "precondition: two over-budget samples disable the overlay");
    assert!(st.project.generation > generation, "precondition: the guard trip bumped it");
    assert_eq!(
        st.crossfile.len(),
        0,
        "every entry was built against the superseded context — the cache is cleared"
    );
}

#[test]
fn crossfile_cache_reader_rejects_a_stale_generation_and_caps_at_eight() {
    // Two belt-and-braces properties of the container itself, stated where
    // they can be seen: `swap_project` already clears the map, so a reader
    // can only ever meet a matching generation — the check is what makes that
    // an assertion rather than an assumption. And the LRU cap bounds a
    // pathological session (a bulk "open every file") without touching the
    // arithmetic for a realistic one.
    let mut cache = CrossFileCache::new();
    let core = Arc::new(CoreIndex::new());
    let index = || Arc::new(SourceIndex::build(&lower(&parse(b"class Z\nend\n")), &core));
    let uri: Uri = "file:///gen.rb".parse().unwrap();

    cache.store(&uri, index(), 3);
    assert!(cache.get(&uri, 4).is_none(), "a bumped generation is not served");
    assert!(cache.get(&uri, 3).is_some(), "the generation it was built under is");

    // Fill past the cap; the least-recently-used entries go, the touched one
    // stays.
    let uris: Vec<Uri> = (0..CROSSFILE_CACHE_CAP + 4)
        .map(|i| format!("file:///lru{i}.rb").parse().unwrap())
        .collect();
    for u in &uris {
        cache.store(u, index(), 3);
        // Keep the FIRST one hot, so it survives on recency, not insertion order.
        let _ = cache.get(&uris[0], 3);
    }
    assert_eq!(cache.len(), CROSSFILE_CACHE_CAP, "the cap holds");
    assert!(cache.contains(&uris[0]), "the repeatedly-touched entry survives");
    assert!(!cache.contains(&uris[1]), "the least-recently-used one was evicted");

    cache.clear();
    assert_eq!(cache.len(), 0, "clear drops everything");
}

// --- family 6: the diagnostics path is byte-inert -----------------------

#[test]
fn crossfile_cache_leaves_the_published_diagnostics_byte_identical() {
    // The Arc hand-back must be OBSERVATIONALLY INERT on the diagnostics
    // path. Pinned three ways over a fixture whose answer is genuinely
    // cross-file (`Sub#helper` reducing `Base#helper`'s visibility):
    //
    //   (a) the published payload equals `check`'s project-wide answer for
    //       the file (the S4b acceptance bar, restated against the new
    //       return shape);
    //   (b) the published payload is byte-equal to an explicit golden JSON —
    //       so a change to the SERIALIZED notification, not just the rule
    //       set, fails here;
    //   (c) interleaving the new cache READERS (a hover and a completion)
    //       between two dispatches does not move the second payload.
    let p = TempProject::new("xf_inert");
    p.write("base.rb", BASE_RB);
    p.write("sub.rb", SUB_RB);
    let mut h = Harness::start_project(
        Duration::from_millis(10),
        production_gate(),
        serde_json::json!({}),
        p.root.clone(),
        OVERLAY_BUILD_BUDGET_DEFAULT,
    );
    h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
    let first = h.recv_diags();

    // (a) parity with `check`'s project-wide answer.
    assert_eq!(
        diag_keys(&first),
        check_project_diagnostics(&p.root, "sub.rb"),
        "the published set must still equal `check`'s project-wide answer"
    );

    // (b) the exact serialized payload.
    let payload = |d: &PublishDiagnosticsParams| {
        serde_json::to_string(&d.diagnostics).unwrap()
    };
    assert_eq!(
        payload(&first),
        format!(
            "[{{\"range\":{{\"start\":{{\"line\":3,\"character\":6}},\
             \"end\":{{\"line\":3,\"character\":12}}}},\"severity\":2,\
             \"code\":\"def.override-visibility-reduced\",\"source\":\"rigor\",\
             \"message\":{}}}]",
            serde_json::to_string(&first.diagnostics[0].message).unwrap()
        ),
        "the published diagnostic's BYTES are unchanged by the Arc hand-back"
    );

    // (c) the readers are inert: hover + completion between dispatches.
    let hovered = xf_hover_request(&h, 60, &p.uri("sub.rb"), 3, 6);
    assert!(!hovered.is_empty(), "precondition: the hover actually answered");
    let _ = xf_completion_request(&h, 61, &p.uri("sub.rb"), 3, 6);
    h.notify("textDocument/didChange", change_params(&p.uri("sub.rb"), SUB_RB, 2));
    let second = h.recv_diags();
    assert_eq!(
        payload(&second),
        payload(&first),
        "a dispatch that follows a cache READ publishes the same bytes"
    );
    h.shutdown();
}

#[test]
fn crossfile_cache_hit_declines_the_same_file_literal_constant_fold() {
    // ISSUE #102 — FLIPPED. This test used to pin a real regression: the
    // per-file constant gate compared `LoweredAst::file_id`, a process-global
    // counter stamped at `lower()`, so "the same file" meant "the same
    // `lower()` call". The cached index was merged against the DISPATCH
    // WORKER's lowering; `hover` lowers the buffer AFRESH; the ids differed
    // and the gate declined, degrading `FOO : 5` to `FOO : Dynamic[top]` on
    // every cache hit.
    //
    // The identity is now the file's CANONICAL PATH, which both lowerings
    // carry, so the hit and the miss agree. The name is kept so the history
    // is traceable from the issue and the impl notes.
    //
    // BOTH indices come from production code — the cached one from a real
    // `compute_diagnostics` dispatch (the worker's own lowering), the miss
    // from `hover`'s single-file fallback — so nothing here fakes the
    // identity the assertion is about.
    let p = TempProject::new("xf_const");
    let consts = "FOO = 5\nFOO\n";
    p.write("consts.rb", consts);
    p.write("other.rb", "class Other\nend\n");
    let (_ctx, st) = session_for(&p.root, Config::default(), OVERLAY_BUILD_BUDGET_DEFAULT);
    let uri: Uri = p.uri("consts.rb").parse().unwrap();
    let buf = BufferPaths::for_uri(&uri);
    let (_diags, sample, cached) = compute_diagnostics(&st.project, &buf, consts);
    assert!(sample.is_some(), "precondition: the overlay is on for this project");
    let cached = cached.expect("a live overlay dispatch hands its project index back");

    let mut buffers = BufferTable::new();
    buffers.open(&uri, consts.to_string(), 1);
    let params = HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            position: Position { line: 1, character: 0 },
        },
        work_done_progress_params: Default::default(),
    };
    let body = |cached: Option<&SourceIndex>| {
        match hover(&st.project, &buffers, &params, cached) {
            Some(Hover { contents: HoverContents::Markup(m), .. }) => m.value,
            _ => String::new(),
        }
    };
    assert!(body(None).contains("FOO : 5"), "the miss still folds: {}", body(None));
    assert!(
        body(Some(&cached)).contains("FOO : 5"),
        "and so does the HIT — the gate is keyed on the file, not the lowering: {}",
        body(Some(&cached))
    );
}

#[test]
fn crossfile_cache_hit_still_gates_a_cross_file_constant_per_file() {
    // The other half of #102: closing the SAME-file fold must not open a
    // CROSS-file one. The reference rebuilds its in-source constant-value
    // table per file, so a constant assigned in `consts.rb` and read in
    // `reader.rb` is silent there — and stays silent here, on a cache hit,
    // where the whole project's harvest is in the index the hover reads.
    let p = TempProject::new("xf_const_xfile");
    let consts = "BAR = 5\n";
    let reader = "BAR\n";
    p.write("consts.rb", consts);
    p.write("reader.rb", reader);
    let (_ctx, st) = session_for(&p.root, Config::default(), OVERLAY_BUILD_BUDGET_DEFAULT);
    let uri: Uri = p.uri("reader.rb").parse().unwrap();
    let buf = BufferPaths::for_uri(&uri);
    let (_diags, sample, cached) = compute_diagnostics(&st.project, &buf, reader);
    assert!(sample.is_some(), "precondition: the overlay is on for this project");
    let cached = cached.expect("a live overlay dispatch hands its project index back");

    let mut buffers = BufferTable::new();
    buffers.open(&uri, reader.to_string(), 1);
    let params = HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            position: Position { line: 0, character: 0 },
        },
        work_done_progress_params: Default::default(),
    };
    let body = match hover(&st.project, &buffers, &params, Some(&cached)) {
        Some(Hover { contents: HoverContents::Markup(m), .. }) => m.value,
        _ => String::new(),
    };
    assert!(
        body.contains("BAR : Dynamic[top]"),
        "a constant assigned in ANOTHER file must not fold (the oracle is silent): {body}"
    );
}
