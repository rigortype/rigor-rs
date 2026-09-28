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

/// One finding flattened exactly as `check` flattens it for the renderers.
fn rendered<'a>(path: &'a str, source: &str, d: &'a Diagnostic) -> Rendered<'a> {
    let (line, column) = line_col(source, d.start_offset);
    Rendered { path, line, column, severity: d.severity, rule_id: d.qualified_rule(), message: &d.message }
}

/// The single github annotation line for one diagnostic, from the REAL
/// renderer, so the exact string incl. escaping is asserted.
fn gh_line(path: &str, source: &str, d: &Diagnostic) -> String {
    diagnostic_formats::render_github(&[rendered(path, source, d)])
}

#[test]
fn github_error_line() {
    let d = diag("call.undefined-method", Severity::Error, "undefined method `lenght'");
    assert_eq!(
        gh_line("app.rb", "", &d),
        "::error file=app.rb,line=1,col=1,title=call.undefined-method::undefined method `lenght'"
    );
}

#[test]
fn github_warning_line() {
    let d = diag("some.rule", Severity::Warning, "watch out");
    assert_eq!(gh_line("a.rb", "", &d), "::warning file=a.rb,line=1,col=1,title=some.rule::watch out");
}

#[test]
fn github_info_is_notice() {
    let d = diag("internal-error", Severity::Info, "fyi");
    assert_eq!(gh_line("a.rb", "", &d), "::notice file=a.rb,line=1,col=1,title=internal-error::fyi");
}

#[test]
fn github_message_escaping() {
    // `%` -> %25 (done first), newline -> %0A, CR -> %0D; commas/colons in the
    // message body are NOT escaped (only property values escape those).
    let d = diag("r", Severity::Error, "100% off\nline two\r, a:b");
    assert_eq!(
        gh_line("p.rb", "", &d),
        "::error file=p.rb,line=1,col=1,title=r::100%25 off%0Aline two%0D, a:b"
    );
}

#[test]
fn github_property_escaping() {
    // A path with a comma/colon must be escaped in the property value.
    let d = diag("r", Severity::Error, "msg");
    assert_eq!(
        gh_line("a,b:c.rb", "", &d),
        "::error file=a%2Cb%3Ac.rb,line=1,col=1,title=r::msg"
    );
}

#[test]
fn github_line_col_from_source() {
    // `s.lenght` on line 2: offset of the `l` in lenght.
    let src = "s = \"x\"\ns.lenght\n";
    let off = src.find("lenght").unwrap();
    let d = Diagnostic { start_offset: off, ..diag("r", Severity::Error, "m") };
    assert_eq!(gh_line("f.rb", src, &d), "::error file=f.rb,line=2,col=3,title=r::m");
}

#[test]
fn line_col_counts_bytes_not_scalars() {
    // The reference reports `Prism start_column + 1`, and Prism's column is a
    // BYTE index into the line. Scalar counting agrees on ASCII and drifts
    // left of the oracle once a wider character precedes the token.
    let src = "\"ex\u{e4}mple\".lenght\n"; // ä = 2 bytes
    let off = src.find("lenght").unwrap();
    assert_eq!(line_col(src, off), (1, off + 1));
    assert_eq!(line_col(src, off).1, 12);

    // 4-byte scalar, and a second line so the line walk is exercised too.
    let src = "x = 1\n\"\u{1f48c}\".lenght\n";
    let off = src.find("lenght").unwrap();
    let line_start = src.find('\n').unwrap() + 1;
    assert_eq!(line_col(src, off), (2, off - line_start + 1));
    assert_eq!(line_col(src, off).1, 8);
}

#[test]
fn conformance_column_counts_characters() {
    // Oracle (PR #150 round 3): `type t = "é" %a{…}` puts the annotation
    // at column 16 upstream; bytes would say 17. One Unicode scalar per
    // character (here 16 before it), a combining mark counted apart.
    let src = "iface\ntype t = \"é😀e\u{301}\" %a{x}\n";
    let off = src.find("%a").unwrap();
    let fixed = off - char_column_delta(src, off);
    assert_eq!(line_col(src, fixed), (2, 17));
    assert_eq!(char_column_delta("abc %a", 4), 0);
}

#[test]
fn line_col_start_of_line_and_eof() {
    let src = "\u{1f48c}\nb\n";
    assert_eq!(line_col(src, 0), (1, 1));
    // Start of line 2 is column 1 regardless of line 1's byte width.
    assert_eq!(line_col(src, src.find('b').unwrap()), (2, 1));
    // An offset past the buffer clamps to its end rather than panicking.
    assert_eq!(line_col(src, 9_999), (3, 1));
    assert_eq!(line_col("", 7), (1, 1));
}

/// Capture github output for a slice of findings without spawning a process.
fn github_all(findings: &[(usize, String, String, Diagnostic)]) -> String {
    diagnostic_formats::render_github(&to_rendered(findings))
}

#[test]
fn github_empty_when_no_diagnostics() {
    assert_eq!(github_all(&[]), "");
}

/// The REAL SARIF renderer's output, re-parsed.
fn sarif_value(findings: &[(usize, String, String, Diagnostic)]) -> serde_json::Value {
    serde_json::from_str(&diagnostic_formats::render_sarif(&to_rendered(findings))).unwrap()
}

fn finding(rule: &'static str, sev: Severity, msg: &str) -> (usize, String, String, Diagnostic) {
    (0, "f.rb".to_string(), String::new(), diag(rule, sev, msg))
}

/// A finding with NO rule — what `parse_diagnostics` produces.
fn finding_ruleless(msg: &str) -> (usize, String, String, Diagnostic) {
    (0, "f.rb".to_string(), String::new(), diag(rigor_rules::NO_RULE, Severity::Error, msg))
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
    assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "Rigor");
}

/// A parse error carries NO rule. `--format json` must spell that as JSON
/// `null`, exactly as the reference's `"rule" => rule` with `rule` nil —
/// **not** as `""`.
///
/// This is not cosmetic. `harness/lib.rb`'s `DiagKey` is
/// `(rule, line, column)`; `""` is a different key from `nil`, so the row
/// would score as a coverage gap AND an unregistered extra at once — the
/// slice would manufacture a false positive in the gate that grades it.
/// `parse_diagnostics` is 1:1 with Prism's `errors` — no dedupe, no
/// per-line collapsing — and reads NOTHING from `warnings`. Oracle-measured
/// at pin `ffb456b0`: `def sum_of int a, int b` gives the reference three
/// rows on one line at columns 15 / 16 / 23, and a file with one Prism
/// warning and zero errors gives it none.
#[test]
fn parse_diagnostics_are_one_per_prism_error_and_ignore_warnings() {
    let src = b"def sum_of int a, int b\n  a + b\nend\n";
    let result = parse(src);
    let diags = parse_diagnostics(&result);
    assert_eq!(diags.len(), 3, "three errors on one line stay three rows: {diags:?}");
    let source = std::str::from_utf8(src).unwrap();
    let positions: Vec<(usize, usize)> =
        diags.iter().map(|d| line_col(source, d.start_offset)).collect();
    assert_eq!(positions, vec![(1, 15), (1, 16), (1, 23)]);
    for d in &diags {
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.rule_id, rigor_rules::NO_RULE);
        assert!(d.qualified_rule().is_none());
        assert_eq!(d.source_family, "builtin");
    }
    assert_eq!(diags[0].message, "expected a delimiter to close the parameters");

    // Warnings are not errors: a file Prism warns about but parses cleanly
    // produces no parse diagnostics.
    let warned = parse(b"x = 1\nif x = 2\n  puts 'a'\nend\n");
    assert!(warned.warnings().next().is_some(), "the probe must actually warn");
    assert!(parse_diagnostics(&warned).is_empty());
}

#[test]
fn ruleless_diagnostic_serialises_rule_as_json_null() {
    let findings = vec![finding_ruleless("unexpected 'else', ignoring it")];
    let doc = json_document(&findings);
    assert!(doc.contains(r#""rule":null"#), "rule must be the null literal: {doc}");
    assert!(!doc.contains(r#""rule":"""#), "an empty-string rule is a DIFFERENT key: {doc}");

    // It re-parses, and `rule` is really null (not the four-character
    // string "null").
    let v: serde_json::Value = serde_json::from_str(&doc).unwrap();
    assert!(v[0]["rule"].is_null());
    assert_eq!(v[0]["severity"], "error");
    assert_eq!(v[0]["source_family"], "builtin");
    // No catalogue enrichment for a rule that does not exist.
    assert!(v[0].get("evidence_tier").is_none());

    // A rule-carrying row is untouched.
    let doc = json_document(&[finding("call.undefined-method", Severity::Error, "e1")]);
    assert!(doc.contains(r#""rule":"call.undefined-method""#), "{doc}");
}

/// SARIF: the reference declares no rule for a ruleless diagnostic
/// (`filter_map(&:qualified_rule)`) and omits the result's `ruleId`
/// (`entry["ruleId"] = rule_id if rule_id`) — measured against the
/// reference at pin `ffb456b0`, which emitted `"rules": []` and results
/// with no `ruleId` key.
#[test]
fn sarif_omits_rule_id_for_a_ruleless_diagnostic() {
    let findings = vec![
        finding_ruleless("unexpected 'else', ignoring it"),
        finding("call.undefined-method", Severity::Error, "e1"),
    ];
    let v = sarif_value(&findings);
    let results = v["runs"][0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].get("ruleId").is_none(), "no ruleId key at all: {}", results[0]);
    assert_eq!(results[0]["level"], "error");
    assert_eq!(results[1]["ruleId"], "call.undefined-method");

    let ids: Vec<&str> = v["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["call.undefined-method"], "the ruleless row declares no rule");
}

/// A ruleless row prints the unsuffixed `Diagnostic#to_s` base (the
/// reference returns `base` when `qualified_rule` is nil); a ruled row gets
/// ` [rule]`.
#[test]
fn text_row_brackets_the_rule_only_when_there_is_one() {
    let findings = vec![
        finding_ruleless("unexpected 'else', ignoring it"),
        finding("call.undefined-method", Severity::Error, "e1"),
    ];
    assert_eq!(
        diagnostic_formats::render_text(&to_rendered(&findings)),
        "f.rb:1:1: error: unexpected 'else', ignoring it\n\
         f.rb:1:1: error: e1 [call.undefined-method]\n\
         \n\
         2 error(s) in 1 file(s)\n"
    );
}

#[test]
fn sarif_empty_still_valid() {
    let v = sarif_value(&[]);
    assert_eq!(v["version"], "2.1.0");
    assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
    assert_eq!(v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap().len(), 0);
}

/// ADR-0040 — a directory arg expands to its `**/*.rb` (recursive), skipping
/// hidden dirs and non-`.rb` files; a missing path and an existing non-`.rb`
/// file become the two `PathError` kinds.
#[test]
fn expand_check_paths_dir_recursion_and_errors() {
    let root = std::env::temp_dir().join(format!("rigor_expand_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(root.join(".hidden")).unwrap();
    std::fs::write(root.join("a.rb"), b"x = 1\n").unwrap();
    std::fs::write(root.join("sub/b.rb"), b"y = 2\n").unwrap();
    std::fs::write(root.join(".hidden/h.rb"), b"z = 3\n").unwrap();
    std::fs::write(root.join("n.txt"), b"nope\n").unwrap();

    let root_s = root.to_string_lossy().into_owned();
    let (files, errs) = expand_check_paths_excluding(&[root_s.as_str()], &[]);
    let names: Vec<String> = files
        .iter()
        .map(|f| Path::new(f).file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"a.rb".to_string()), "top-level .rb included");
    assert!(names.contains(&"b.rb".to_string()), "nested .rb included");
    assert!(!names.iter().any(|n| n == "h.rb"), "hidden dir skipped");
    assert!(!names.iter().any(|n| n == "n.txt"), "non-.rb skipped in dir walk");
    assert!(errs.is_empty(), "a valid dir yields no path errors");

    // A missing path and an existing non-.rb file → the two PathError kinds.
    let txt = root.join("n.txt").to_string_lossy().into_owned();
    let missing = root.join("gone.rb").to_string_lossy().into_owned();
    let (f2, e2) = expand_check_paths_excluding(&[missing.as_str(), txt.as_str()], &[]);
    assert!(f2.is_empty());
    assert_eq!(e2.len(), 2);
    assert!(e2[0].not_found, "missing path is not_found");
    assert!(!e2[1].not_found, "existing non-.rb is not_found=false");

    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #201 — the analyzed expansion IS `reject_excluded` output:
/// directory hits matching `BUILTIN_EXCLUDES + exclude:` are dropped with
/// `File.fnmatch?`-no-flags semantics (`**/node_modules/**` prunes
/// `lib/node_modules/x.rb`, and `a/**/b` does NOT match `a/b` — the
/// `glob::Pattern` drift the retired stage-1 gate had), while an explicit
/// `.rb` root is kept VERBATIM even when it matches an exclude pattern
/// (`accept_as_ruby_file?` never consults the list).
#[test]
fn expand_check_paths_excluding_dir_filtered_file_verbatim() {
    let root = std::env::temp_dir().join(format!("rigor_expand_excl_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib/node_modules")).unwrap();
    std::fs::create_dir_all(root.join("a/x")).unwrap();
    std::fs::write(root.join("lib/ok.rb"), b"class Ok\nend\n").unwrap();
    std::fs::write(root.join("lib/ext.rb"), b"class Ext\nend\n").unwrap();
    std::fs::write(root.join("lib/node_modules/x.rb"), b"class X\nend\n").unwrap();
    std::fs::write(root.join("a/b.rb"), b"class DirectB\nend\n").unwrap();
    std::fs::write(root.join("a/x/b.rb"), b"class DeepB\nend\n").unwrap();

    let lib = root.join("lib").to_string_lossy().into_owned();
    let a = root.join("a").to_string_lossy().into_owned();
    let ext = root.join("lib/ext.rb").to_string_lossy().into_owned();
    let xrb = root.join("lib/node_modules/x.rb").to_string_lossy().into_owned();
    // `Configuration#exclude_patterns`: BUILTIN_EXCLUDES + the user list —
    // `exclude: ["<root>/lib/ext.rb", "<root>/a/**/b"]`.
    let excludes = vec![
        "**/vendor/bundle/**".to_string(),
        "**/.bundle/**".to_string(),
        "**/node_modules/**".to_string(),
        ext.clone(),
        format!("{a}/**/b.rb"),
    ];

    // Directory roots: builtin + user patterns prune the expanded hits.
    let (files, errs) = expand_check_paths_excluding(&[lib.as_str()], &excludes);
    assert!(errs.is_empty(), "a valid dir yields no path errors");
    assert_eq!(
        files,
        vec![root.join("lib/ok.rb").to_string_lossy().into_owned()],
        "node_modules (builtin) and ext.rb (user) pruned; got {files:?}"
    );
    // `a/**/b` collapsed to `a/*/b`: `a/x/b.rb` excluded, `a/b.rb` kept —
    // where `glob::Pattern` would have dropped both.
    let (files_a, _) = expand_check_paths_excluding(&[a.as_str()], &excludes);
    assert_eq!(
        files_a,
        vec![root.join("a/b.rb").to_string_lossy().into_owned()],
        "fnmatch-no-flags: a/**/b.rb must not match a/b.rb; got {files_a:?}"
    );

    // The very same paths as explicit `.rb` roots are kept verbatim.
    let (files2, errs2) =
        expand_check_paths_excluding(&[ext.as_str(), xrb.as_str()], &excludes);
    assert!(errs2.is_empty(), "existing .rb files yield no path errors");
    assert_eq!(files2, vec![ext.clone(), xrb.clone()]);

    let _ = std::fs::remove_dir_all(&root);
}

/// 2026-07-06 audit #1: an ERROR-severity finding fails the run, a warning /
/// info does not — EXCEPT the synthetic `internal-error` (info-severity for
/// harness reasons), which must fail the run: a panicked analysis never
/// exits 0.
#[test]
fn finding_fails_run_severity_and_internal_error() {
    assert!(finding_fails_run(&diag("call.undefined-method", Severity::Error, "boom")));
    assert!(!finding_fails_run(&diag("call.unresolved-toplevel", Severity::Warning, "w")));
    assert!(!finding_fails_run(&diag("some.info-rule", Severity::Info, "i")));
    assert!(finding_fails_run(&internal_error_diag("panicked".to_string())));
}

/// 2026-07-06 audit #3: the dir walk matches Ruby's `Dir.glob("**/*.rb")` on
/// symlinks — a symlinked `.rb` FILE is included, a symlinked DIRECTORY is
/// not traversed.
#[cfg(unix)]
#[test]
fn collect_rb_files_symlink_semantics() {
    let root = std::env::temp_dir().join(format!("rigor_symlink_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("real_dir")).unwrap();
    std::fs::write(root.join("real.rb"), b"x = 1\n").unwrap();
    std::fs::write(root.join("real_dir/inner.rb"), b"y = 2\n").unwrap();
    std::os::unix::fs::symlink(root.join("real.rb"), root.join("link.rb")).unwrap();
    std::os::unix::fs::symlink(root.join("real_dir"), root.join("link_dir")).unwrap();

    let mut out = Vec::new();
    collect_rb_files(&root, &mut out);
    let names: Vec<String> = out
        .iter()
        .map(|f| Path::new(f).file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"real.rb".to_string()));
    assert!(names.contains(&"inner.rb".to_string()));
    assert!(names.contains(&"link.rb".to_string()), "symlinked FILE matched (Dir.glob does)");
    assert_eq!(
        names.iter().filter(|n| *n == "inner.rb").count(),
        1,
        "symlinked DIR not traversed (no duplicate inner.rb via link_dir)"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// ADR-0040 — bad-path severity: `warning`+`(skipped)` when files were found,
/// else `error`; synthetic rule ids; injected ahead of the code findings.
#[test]
fn prepend_path_errors_severity_and_placement() {
    let errs = vec![
        PathError { path: "gone.rb".into(), not_found: true },
        PathError { path: "x.txt".into(), not_found: false },
    ];
    // any_files = true ⇒ warnings, "(skipped)" suffix.
    let mut findings = vec![(0usize, "a.rb".to_string(), "x = 1\n".to_string(),
        diag("call.undefined-method", Severity::Error, "boom"))];
    prepend_path_errors(&mut findings, &errs, true);
    assert_eq!(findings.len(), 3);
    assert_eq!(findings[0].3.rule_id, "path.not-found");
    assert_eq!(findings[0].3.severity, Severity::Warning);
    assert!(findings[0].3.message.ends_with("(skipped)"));
    assert_eq!(findings[1].3.rule_id, "path.not-ruby");
    assert_eq!(findings[2].3.rule_id, "call.undefined-method", "code finding kept, after errors");

    // any_files = false ⇒ errors, no suffix.
    let mut empty = Vec::new();
    prepend_path_errors(&mut empty, &errs, false);
    assert_eq!(empty[0].3.severity, Severity::Error);
    assert!(!empty[0].3.message.ends_with("(skipped)"));
}

// --- ADR-22 slice 5 `--baseline-strict` -------------------------------

#[test]
fn strict_delta_str_matches_ruby_to_s() {
    // Ruby: delta.positive? ? "+#{delta}" : delta.to_s
    assert_eq!(strict_delta_str(1), "+1");
    assert_eq!(strict_delta_str(5), "+5");
    assert_eq!(strict_delta_str(0), "0"); // not positive → bare 0
    assert_eq!(strict_delta_str(-1), "-1");
    assert_eq!(strict_delta_str(-3), "-3");
}

#[test]
fn drift_status_word_lowercase() {
    assert_eq!(drift_status_word(DriftStatus::Over), "over");
    assert_eq!(drift_status_word(DriftStatus::Cleared), "cleared");
    assert_eq!(drift_status_word(DriftStatus::Reducible), "reducible");
    assert_eq!(drift_status_word(DriftStatus::Within), "within");
}

#[test]
fn strict_report_byte_format_over_and_sorted() {
    // A baseline with two buckets; audit against findings that push c.rb
    // over (count 1, actual 2 → Δ+1 over) and leave a.rb reducible (count 3,
    // actual 1 → Δ-2 reducible). Assert exact bytes AND the (file, rule) sort.
    let text = "---\nversion: 1\nignored:\n\
                - file: c.rb\n  rule: r2\n  count: 1\n\
                - file: a.rb\n  rule: r1\n  count: 3\n";
    let b = Baseline::parse(text, "t").unwrap();
    let d_a = diag("r1", Severity::Error, "m");
    let d_c = diag("r2", Severity::Error, "m");
    let entries = vec![
        ("a.rb".to_string(), &d_a),
        ("c.rb".to_string(), &d_c),
        ("c.rb".to_string(), &d_c),
    ];
    let rows = b.audit(&entries);
    let drifted: Vec<&baseline::DriftRow> =
        rows.iter().filter(|r| r.status != DriftStatus::Within).collect();
    assert_eq!(drifted.len(), 2);
    let out = format_strict_drift(&drifted, ".rigor-baseline.yml");
    assert_eq!(
        out,
        "rigor: --baseline-strict — 2 bucket(s) drifted from .rigor-baseline.yml:\n\
         \x20 a.rb  [r1]  3 → 1  (Δ-2, reducible)\n\
         \x20 c.rb  [r2]  1 → 2  (Δ+1, over)\n\
         rigor: run `rigor baseline regenerate` to refresh the baseline.\n"
    );
}

#[test]
fn strict_report_cleared_bucket() {
    // A bucket with no live diagnostics → cleared, Δ-N.
    let text = "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r1\n  count: 2\n";
    let b = Baseline::parse(text, "t").unwrap();
    let rows = b.audit(&[]);
    let drifted: Vec<&baseline::DriftRow> =
        rows.iter().filter(|r| r.status != DriftStatus::Within).collect();
    let out = format_strict_drift(&drifted, "bl.yml");
    assert_eq!(
        out,
        "rigor: --baseline-strict — 1 bucket(s) drifted from bl.yml:\n\
         \x20 a.rb  [r1]  2 → 0  (Δ-2, cleared)\n\
         rigor: run `rigor baseline regenerate` to refresh the baseline.\n"
    );
}

#[test]
fn strict_within_bucket_is_not_a_violation() {
    // actual == count → Within → not in the drifted set → no violation.
    let text = "---\nversion: 1\nignored:\n- file: a.rb\n  rule: r1\n  count: 1\n";
    let b = Baseline::parse(text, "t").unwrap();
    let d = diag("r1", Severity::Error, "m");
    let rows = b.audit(&[("a.rb".to_string(), &d)]);
    let drifted: Vec<&baseline::DriftRow> =
        rows.iter().filter(|r| r.status != DriftStatus::Within).collect();
    assert!(drifted.is_empty());
}

/// Upstream #684 — an explicit `check a.rb` still scans the configured
/// `paths:` for DISCOVERY (`expand_paths(paths | argv)`): an accessor
/// declared by an unlisted `lib/` file suppresses `x.zz` exactly as in a
/// project-mode run, while a method nothing declares still fires (the
/// must-still-fire control).
#[test]
fn analyze_files_discovers_config_paths_declarations() {
    let root = std::env::temp_dir().join(format!("rigor_widen_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(
        root.join("lib/ext.rb"),
        b"class String\n  attr_accessor :zz\nend\n",
    )
    .unwrap();
    std::fs::write(root.join("a.rb"), b"x = \"s\"\nx.zz\nx.no_such_zz\n").unwrap();

    let mut cfg = Config::default();
    cfg.paths = vec![root.join("lib").to_string_lossy().into_owned()];
    let a_rb = root.join("a.rb").to_string_lossy().into_owned();
    let (findings, io_err) = analyze_files(
        &[a_rb.as_str()],
        Some(&[a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(!io_err);
    let messages: Vec<&str> = findings.iter().map(|(_, _, _, d)| d.message.as_str()).collect();
    assert!(
        !messages.iter().any(|m| m.contains("`zz'")),
        "cross-file `attr_accessor :zz` suppresses; got {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("`no_such_zz'")),
        "an undeclared method still fires; got {messages:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `Configuration.resolve_path_key!` — a DECLARED `paths:` entry is
/// `File.expand_path`'d against the CONFIG FILE's directory, not the
/// process cwd: `--config cfg/.rigor.yml` + `paths: ["lib"]` discovers
/// `cfg/lib`, and a stray cwd `lib/` is NOT walked. The fixture puts
/// the accessor under `cfg/lib` and a bait decl under cwd `lib` —
/// cwd-relative resolution would read the bait and miss the accessor.
#[test]
fn analyze_files_declared_paths_resolve_against_config_dir() {
    let root = std::env::temp_dir().join(format!("rigor_widen_cfg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg/lib")).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("cfg/.rigor.yml"), b"paths:\n  - lib\n").unwrap();
    // The file the DECLARED `paths:` must reach: `String#zz` accessor.
    std::fs::write(
        root.join("cfg/lib/ext.rb"),
        b"class String\n  attr_accessor :zz\nend\n",
    )
    .unwrap();
    // Bait only a cwd-relative `lib` expansion would walk: `Foo#bar`'s
    // Integer return would FP `1.upcase` if discovered.
    std::fs::write(root.join("lib/foo.rb"), b"class Foo\n  def bar = 1\nend\n").unwrap();
    std::fs::write(
        root.join("a.rb"),
        b"x = \"s\"\nx.zz\nFoo.new.bar.upcase\n",
    )
    .unwrap();

    let crate::config::ConfigRead::Parsed(cfg) =
        Config::read(&root.join("cfg/.rigor.yml"))
    else {
        panic!("config must parse");
    };
    assert!(cfg.paths_explicitly_declared());
    let a_rb = root.join("a.rb").to_string_lossy().into_owned();
    let (findings, io_err) = analyze_files(
        &[a_rb.as_str()],
        Some(&[a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(!io_err);
    let messages: Vec<&str> = findings.iter().map(|(_, _, _, d)| d.message.as_str()).collect();
    assert!(
        !messages.iter().any(|m| m.contains("`zz'")),
        "config-relative `paths:` sees cfg/lib's accessor; got {messages:?}"
    );
    assert!(
        !messages.iter().any(|m| m.contains("`upcase'")),
        "cwd `lib/` is NOT walked for a declared `paths:`; got {messages:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `project_discovery_expansion`'s size gate
/// (`widened.files.size > expansion.files.size`): `check a.rb a.rb`
/// expands to the same file set as the `paths | argv` union (the argv
/// repeat dedups at the ROOT level), so discovery does NOT widen and
/// `lib/`'s `Foo` decl stays unseen — where a single `check a.rb`
/// (strictly larger widened set) DOES discover it. The oracle fires
/// `upcase for 1` on the single-arg row and goes silent on the dup.
#[test]
fn analyze_files_dup_argv_does_not_widen() {
    let root = std::env::temp_dir().join(format!("rigor_widen_dup_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/foo.rb"), b"class Foo\n  def bar = 1\nend\n").unwrap();
    std::fs::write(root.join("a.rb"), b"Foo.new.bar.upcase\n").unwrap();

    // `paths` spelled absolute so no `chdir` is needed (the cwd-relative
    // default `["lib"]` would walk the test process's cwd, not `root`):
    // a mutated `Config::default()` has no `present_keys`, so the entry
    // is NOT "declared" and is used verbatim — exactly the default's
    // resolution rule.
    let mut cfg = Config::default();
    cfg.paths = vec![root.join("lib").to_string_lossy().into_owned()];
    let a_rb = root.join("a.rb").to_string_lossy().into_owned();

    // Single arg: widened = [lib/foo.rb, a.rb] > [a.rb] ⇒ widens ⇒
    // `Foo#bar` resolves to Integer ⇒ `upcase` fires (oracle: `for 1`).
    let (one, _) = analyze_files(
        &[a_rb.as_str()],
        Some(&[a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(
        one.iter().any(|(_, _, _, d)| d.message.contains("`upcase'")),
        "widened discovery must see Foo#bar; got {one:?}"
    );

    // Dup arg: widened = expand(paths | [a.rb]) — the repeat dedups —
    // same size as the expansion ⇒ no widen ⇒ Foo unseen ⇒ silent.
    let (two, _) = analyze_files(
        &[a_rb.as_str(), a_rb.as_str()],
        Some(&[a_rb.as_str(), a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(
        two.is_empty(),
        "dup argv must not widen discovery; got {two:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Bare `check` / `baseline` (`argv_roots == None` — the run's roots are
/// the config `paths:` fallback): upstream `paths == configuration.paths`
/// makes `widen_discovery_to_project?` unconditionally false, so
/// discovery never reaches beyond the analyzed set — even for a
/// `--config cfg/.rigor.yml` whose declared `paths:` names a DIFFERENT
/// directory than the analyzed roots.
#[test]
fn analyze_files_no_widen_on_config_fallback() {
    let root = std::env::temp_dir().join(format!("rigor_widen_bare_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg/lib")).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("cfg/.rigor.yml"), b"paths:\n  - lib\n").unwrap();
    std::fs::write(root.join("cfg/lib/foo.rb"), b"class Foo\n  def bar = 1\nend\n").unwrap();
    std::fs::write(root.join("lib/a.rb"), b"Foo.new.bar.upcase\n").unwrap();

    let crate::config::ConfigRead::Parsed(cfg) =
        Config::read(&root.join("cfg/.rigor.yml"))
    else {
        panic!("config must parse");
    };
    // The analyzed file is the cwd-`lib` expansion of the fallback
    // roots (#198 — the roots side stays cwd-relative). With no argv
    // there must be NO discovery widening: `Foo` stays unresolved and
    // `upcase` cannot fire.
    let a_rb = root.join("lib/a.rb").to_string_lossy().into_owned();
    let (findings, io_err) = analyze_files(
        &[a_rb.as_str()],
        None,
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(!io_err);
    assert!(
        findings.is_empty(),
        "config-fallback runs never widen discovery; got {findings:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `reject_excluded` applies INSIDE `expand_paths`, so an `exclude:`d
/// (or `BUILTIN_EXCLUDES`d) file shrinks BOTH the widened count and the
/// discovery set. Here `exclude: ["*zz.rb"]` (fnmatch-no-flags: `*` spans
/// `/`) drops `cfg/lib/zz.rb` from the widened expansion, making
/// `check a.rb a.rb`'s widened set the same size as its expansion — no
/// widening, `Foo` unseen, `upcase` silent. The single-arg row still
/// widens and fires (the must-still-fire control).
#[test]
fn analyze_files_exclude_shrinks_widened_count() {
    let root = std::env::temp_dir().join(format!("rigor_widen_excl_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg/lib")).unwrap();
    std::fs::write(
        root.join("cfg/.rigor.yml"),
        b"paths:\n  - lib\nexclude:\n  - \"*zz.rb\"\n",
    )
    .unwrap();
    std::fs::write(root.join("cfg/lib/foo.rb"), b"class Foo\n  def bar = 1\nend\n").unwrap();
    std::fs::write(root.join("cfg/lib/zz.rb"), b"class Zz\nend\n").unwrap();
    std::fs::write(root.join("a.rb"), b"Foo.new.bar.upcase\n").unwrap();

    let crate::config::ConfigRead::Parsed(cfg) =
        Config::read(&root.join("cfg/.rigor.yml"))
    else {
        panic!("config must parse");
    };
    let a_rb = root.join("a.rb").to_string_lossy().into_owned();

    // Single arg: widened = [foo.rb, a.rb] > [a.rb] ⇒ widens ⇒ fires.
    let (one, _) = analyze_files(
        &[a_rb.as_str()],
        Some(&[a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(
        one.iter().any(|(_, _, _, d)| d.message.contains("`upcase'")),
        "widened discovery must see Foo#bar; got {one:?}"
    );

    // Dup arg: widened = [foo.rb, a.rb] (zz.rb excluded inside the
    // expansion) — same size as [a.rb, a.rb] ⇒ no widen ⇒ silent.
    let (two, _) = analyze_files(
        &[a_rb.as_str(), a_rb.as_str()],
        Some(&[a_rb.as_str(), a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(
        two.is_empty(),
        "excluded files must shrink the widened count; got {two:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `BUILTIN_EXCLUDES` prune discovery too: a decl under
/// `lib/node_modules/` must neither count toward the widened set nor be
/// discovered. `lib/ok.rb` keeps the widened set strictly larger (so
/// widening DOES run — the assertion is about the discovery SET, not the
/// count), `node_modules/x.rb`'s `Foo#bar` stays unseen, and the
/// `Ok.new.qqq` control proves discovery ran and analysis still fires.
#[test]
fn analyze_files_builtin_excludes_drop_discovery_items() {
    let root = std::env::temp_dir().join(format!("rigor_widen_bex_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib/node_modules")).unwrap();
    std::fs::write(root.join("lib/ok.rb"), b"class Ok\n  def zz = 1\nend\n").unwrap();
    std::fs::write(
        root.join("lib/node_modules/x.rb"),
        b"class Foo\n  def bar = 1\nend\n",
    )
    .unwrap();
    std::fs::write(root.join("a.rb"), b"Foo.new.bar.upcase\nOk.new.zz.upcase\n").unwrap();

    let mut cfg = Config::default();
    cfg.paths = vec![root.join("lib").to_string_lossy().into_owned()];
    let a_rb = root.join("a.rb").to_string_lossy().into_owned();

    let (findings, _) = analyze_files(
        &[a_rb.as_str()],
        Some(&[a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    let messages: Vec<&str> =
        findings.iter().map(|(_, _, _, d)| d.message.as_str()).collect();
    // `Ok.new.zz.upcase` firing (Integer#upcase) proves ok.rb WAS
    // discovered; the absence of a SECOND `upcase` (Foo#bar's) proves
    // node_modules/x.rb was not.
    let upcase = messages.iter().filter(|m| m.contains("`upcase'")).count();
    assert_eq!(
        upcase, 1,
        "exactly the Ok-row `upcase` fires; node_modules decl must not be \
         discovered; got {messages:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `configuration.paths | paths` dedups the RECEIVER too: `paths:`
/// entries that `File.expand_path` to the same root (`lib`, `./lib`,
/// `lib/`) are ONE union element, so the widened expansion is not
/// inflated by repeating a directory.
#[test]
fn analyze_files_repeated_paths_dedup_in_union() {
    let root = std::env::temp_dir().join(format!("rigor_widen_dupp_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg/lib")).unwrap();
    std::fs::write(
        root.join("cfg/.rigor.yml"),
        b"paths:\n  - lib\n  - ./lib\n  - lib/\n",
    )
    .unwrap();
    std::fs::write(root.join("cfg/lib/foo.rb"), b"class Foo\n  def bar = 1\nend\n").unwrap();
    std::fs::write(root.join("a.rb"), b"Foo.new.bar.upcase\n").unwrap();

    let crate::config::ConfigRead::Parsed(cfg) =
        Config::read(&root.join("cfg/.rigor.yml"))
    else {
        panic!("config must parse");
    };
    assert!(cfg.paths_explicitly_declared());
    let a_rb = root.join("a.rb").to_string_lossy().into_owned();

    // Dup argv with a deduped union: widened = [foo.rb, a.rb] == size of
    // the [a.rb, a.rb] expansion ⇒ no widen ⇒ silent. (Without the
    // receiver-side dedup the triple `lib` roots count foo.rb thrice.)
    let (two, _) = analyze_files(
        &[a_rb.as_str(), a_rb.as_str()],
        Some(&[a_rb.as_str(), a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(
        two.is_empty(),
        "repeated `paths:` roots must dedup out of the union; got {two:?}"
    );

    // Control: the single-arg row still widens and fires.
    let (one, _) = analyze_files(
        &[a_rb.as_str()],
        Some(&[a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    assert!(
        one.iter().any(|(_, _, _, d)| d.message.contains("`upcase'")),
        "single-arg row still widens; got {one:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #201 — a `[`/`\\` `exclude:` pattern is decided EXACTLY now, not
/// declined: `fnmatch` is the full `dir.c` port, so the widened expansion
/// answers `File.fnmatch?("lib/[f]oo.rb", "<abs>/lib/foo.rb")` itself. The
/// declared `paths: [lib]` expands to an ABSOLUTE root, so the relative
/// pattern cannot match the widened hit — `foo.rb` stays, widening runs,
/// and `Foo.new.bar.upcase` fires exactly as the reference's does
/// (probe-measured; `"s".nope` is the still-analysed control).
#[test]
fn analyze_files_class_pattern_decided_exactly() {
    let root = std::env::temp_dir().join(format!("rigor_widen_und_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg/lib")).unwrap();
    std::fs::write(
        root.join("cfg/.rigor.yml"),
        b"paths:\n  - lib\nexclude:\n  - \"lib/[f]oo.rb\"\n",
    )
    .unwrap();
    std::fs::write(root.join("cfg/lib/foo.rb"), b"class Foo\n  def bar = 1\nend\n").unwrap();
    std::fs::write(root.join("a.rb"), b"Foo.new.bar.upcase\n\"s\".nope\n").unwrap();

    let crate::config::ConfigRead::Parsed(cfg) =
        Config::read(&root.join("cfg/.rigor.yml"))
    else {
        panic!("config must parse");
    };
    let a_rb = root.join("a.rb").to_string_lossy().into_owned();
    let (findings, _) = analyze_files(
        &[a_rb.as_str()],
        Some(&[a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    let messages: Vec<&str> =
        findings.iter().map(|(_, _, _, d)| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("`upcase'")),
        "exact `fnmatch` decides the class pattern — widening must run; got {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("`nope'")),
        "a.rb still analysed; got {messages:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The same `[` pattern on the dup-argv shape (`check app app`): the exact
/// `fnmatch` decides `[q]zz.rb` matches nothing in the widened expansion,
/// so the counts stay 2 vs 2 — no widening, no `upcase` — the reference's
/// answer, reached by matching rather than by declining.
#[test]
fn analyze_files_class_pattern_no_match_on_dup_argv() {
    let root = std::env::temp_dir().join(format!("rigor_widen_und2_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg/lib")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(
        root.join("cfg/.rigor.yml"),
        b"paths:\n  - lib\nexclude:\n  - \"[q]zz.rb\"\n",
    )
    .unwrap();
    std::fs::write(root.join("cfg/lib/foo.rb"), b"class Foo\n  def bar = 1\nend\n").unwrap();
    std::fs::write(
        root.join("app/a.rb"),
        b"Foo.new.bar.upcase\n\"s\".nope\n",
    )
    .unwrap();

    let crate::config::ConfigRead::Parsed(cfg) =
        Config::read(&root.join("cfg/.rigor.yml"))
    else {
        panic!("config must parse");
    };
    let a_rb = root.join("app/a.rb").to_string_lossy().into_owned();
    let (findings, _) = analyze_files(
        &[a_rb.as_str(), a_rb.as_str()],
        Some(&[a_rb.as_str(), a_rb.as_str()]),
        &cfg,
        "check",
        None,
        &config::BleedingEdgeSelector::None,
        false,
    );
    let messages: Vec<&str> =
        findings.iter().map(|(_, _, _, d)| d.message.as_str()).collect();
    assert!(
        !messages.iter().any(|m| m.contains("`upcase'")),
        "[q]zz.rb matches nothing — widened count stays equal; got {messages:?}"
    );
    assert_eq!(
        messages.iter().filter(|m| m.contains("`nope'")).count(),
        2,
        "both argv occurrences analysed; got {messages:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The leading-period half of the fnmatch fix is exercised at the
/// matcher level (`conformance_gate::fnmatch_matches_ruby_no_flags`);
/// the e2e `check ./app ./app` row is probe-verified because a
/// `./`-spelled root needs a controlled cwd.
#[test]
fn exclude_fnmatch_leading_period_rule() {
    let excludes = vec!["*gen.rb".to_string()];
    // `File.fnmatch?("*gen.rb", "./app/gen.rb")` is false — `./app/gen.rb`
    // must NOT be excluded from the widened/expansion counts.
    assert!(!exclude_fnmatch(&excludes, "./app/gen.rb"));
    assert!(exclude_fnmatch(&excludes, "app/gen.rb"));
}
