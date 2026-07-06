//! `rigor triage [paths]` (ADR-23) — a faithful port of the reference's
//! `Triage` + `TriageRenderer` + `TriageCommand`, SCOPED to the statistical core.
//!
//! Runs the same analysis as `rigor check`, then summarises the diagnostic
//! stream — a rule-id **distribution**, a class/method **selectors** axis
//! (ADR-61 agent-friendly stats), and per-file **hotspots** — instead of the raw
//! per-line list. Read-only and advisory; ALWAYS exits 0 (an inspection command,
//! not a gate — `rigor check` remains the gate).
//!
//! ## Scope — the `hints` Catalogue
//!
//! The fourth section, `hints`, ports the reference Catalogue's COUNT / rule-based
//! recognisers — H7 (unresolved-toplevel), H5 (systemic single-file cluster), H6
//! (low-count genuine bugs) — which fire on rigor-rs's diagnostic set and match
//! the reference byte-for-byte, preserving the recogniser precedence + claiming.
//! The ECOSYSTEM recognisers are DEFERRED (H1 ActiveSupport core-ext, H2/H2K
//! project monkey-patch, H3 `gem-without-rbs`, H4 ActiveRecord relation): they key
//! on ActiveSupport/ActiveRecord method tables, `:info` plugin-recognition
//! notices, cross-file `project_definition_site` provenance, or `Array[…]`
//! relation receivers that rigor-rs does not produce — so on a rigor-rs diagnostic
//! set they never fire, and the reference (fed the same source) does not fire them
//! either, so the ported subset is parity-faithful for rigor-rs-witnessable code.
//!
//! ## Parity notes
//!
//! - Text output is byte-gated against `rigor triage --no-hints`. Counts,
//!   distribution, hotspots and the summary are pure over the diagnostic fields
//!   (`rule`/`severity`/`path`) `check` already emits with parity.
//! - `selectors` receivers apply the reference's exact `normalize_receiver` fold
//!   to rigor-rs's own `receiver_type` strings; the common scalar cases
//!   (`"x"`→`String`, `5`→`Integer`, `:s`→`Symbol`, `nil`→`nil`, a nominal/
//!   `singleton(C)`) match byte-for-byte. The one systematic divergence is that
//!   rigor-rs types an array/hash literal receiver as its NOMINAL class (`Array`/
//!   `Hash`) where the reference keeps the value-pinned tuple/shape display
//!   (`[1, 2, 3]`), so that selector row's receiver — and thus its alphabetic
//!   sort position — differs. This is rigor-rs's tool-wide `receiver_type`
//!   spelling, not a triage defect (the same rendering-divergence convention
//!   `type-of` documents); the counts, distribution, hotspots and summary are
//!   byte-identical to `triage --no-hints`.
//! - `--format json` is emitted via serde_json, which sorts object keys
//!   alphabetically (no `preserve_order` feature) — both the top-level keys and
//!   the nested `rules`/`by_rule` maps differ in ORDER from the reference's
//!   insertion/`-count` order. JSON key order is not significant (counts live in
//!   the values), so this is documented rather than hand-built.

use std::process::ExitCode;

use rigor_rules::{Diagnostic, Severity};
use serde_json::json;

const BAR_WIDTH: usize = 24;
const SELECTOR_ROWS: usize = 15;

/// A diagnostic paired with the path it was reported in (`path` rides the
/// findings tuple, not the `Diagnostic`). The aggregation works over these.
type DiagRef<'a> = (&'a str, &'a Diagnostic);

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Distribution,
    Selectors,
    Hotspots,
    Hints,
}

/// `rigor triage [--format text|json] [--top N] [--include-info] [--no-hints |
/// --selectors-only | --hints-only] [--config PATH] [paths...]`.
pub fn cmd_triage(args: &[String]) -> ExitCode {
    let mut format = "text";
    let mut top = 10usize;
    let mut include_info = false;
    // Default sections: hints deferred, so the default equals `--no-hints`.
    let mut sections =
        vec![Section::Distribution, Section::Selectors, Section::Hotspots, Section::Hints];
    let mut explicit_config: Option<&str> = None;
    let mut paths: Vec<&str> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some(f @ ("text" | "json")) => format = f,
                other => {
                    eprintln!("rigor triage: --format expects `text` or `json`, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--format=") => match &other["--format=".len()..] {
                f @ ("text" | "json") => format = f,
                v => {
                    eprintln!("rigor triage: unsupported format: {v}");
                    return ExitCode::from(64);
                }
            },
            "--top" => match it.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(n) => top = n,
                None => {
                    eprintln!("rigor triage: --top expects an integer");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--top=") => match other["--top=".len()..].parse::<usize>() {
                Ok(n) => top = n,
                Err(_) => {
                    eprintln!("rigor triage: --top expects an integer");
                    return ExitCode::from(64);
                }
            },
            "--include-info" => include_info = true,
            "--no-hints" => {
                sections = vec![Section::Distribution, Section::Selectors, Section::Hotspots];
            }
            "--selectors-only" => sections = vec![Section::Selectors],
            "--hints-only" => sections = vec![Section::Hints],
            "--config" => match it.next() {
                Some(p) => explicit_config = Some(p),
                None => {
                    eprintln!("rigor triage: --config expects a path");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--config=") => {
                explicit_config = Some(&other["--config=".len()..]);
            }
            other => paths.push(other),
        }
    }

    let cfg = crate::Config::load(explicit_config.map(std::path::Path::new));
    let config_paths: Vec<&str>;
    let roots: &[&str] = if paths.is_empty() {
        config_paths = cfg.paths.iter().map(String::as_str).collect();
        &config_paths
    } else {
        &paths
    };
    let (expanded_owned, _errs) = crate::expand_check_paths(roots);
    let expanded: Vec<&str> = expanded_owned.iter().map(String::as_str).collect();
    let (findings, _io) = crate::analyze_files(&expanded, &cfg, "triage", None);

    // (path, diagnostic) pairs — `path` rides the findings tuple, not the diag.
    let diags: Vec<DiagRef> =
        findings.iter().map(|(_o, path, _src, d)| (path.as_str(), d)).collect();

    let report = analyze(&diags, top, sections.contains(&Section::Hints), include_info);
    match format {
        "json" => println!("{}", report_json(&report)),
        _ => print!("{}", render_text(&report, &sections)),
    }
    // Always 0 — inspection, not a gate.
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Aggregation (reference `Triage.analyze`)
// ---------------------------------------------------------------------------

struct Summary {
    total: usize,
    error: usize,
    warning: usize,
    info: usize,
}

struct RuleCount {
    rule: String,
    count: usize,
}

struct Selector {
    receiver: Option<String>,
    method: String,
    count: usize,
    files: usize,
    /// `(rule, count)` sorted by `-count, rule` (reference `selector_for`).
    rules: Vec<(String, usize)>,
}

struct Hotspot {
    file: String,
    count: usize,
    by_rule: Vec<(String, usize)>,
}

/// One heuristic hint (reference `Triage::Hint`).
struct Hint {
    id: &'static str,
    confidence: &'static str,
    diagnostic_count: usize,
    summary: String,
    action: &'static str,
}

struct Report {
    summary: Summary,
    distribution: Vec<RuleCount>,
    selectors: Vec<Selector>,
    hotspots: Vec<Hotspot>,
    hints: Vec<Hint>,
    include_info: bool,
}

fn analyze(diags: &[DiagRef], top: usize, run_hints: bool, include_info: bool) -> Report {
    // The summary reports the FULL stream; the volume views route out `:info`
    // unless `--include-info` (WD6). rigor-rs emits no `:info` today, so routing
    // is inert, but it is ported faithfully.
    let routed: Vec<DiagRef> = if include_info {
        diags.to_vec()
    } else {
        diags.iter().copied().filter(|(_, d)| d.severity != Severity::Info).collect()
    };
    Report {
        summary: build_summary(diags),
        distribution: build_distribution(&routed),
        selectors: build_selectors(&routed),
        hotspots: build_hotspots(&routed, top),
        // Hints see the FULL stream (the per-recogniser info-guard is inside).
        hints: if run_hints { recognise(diags, include_info) } else { Vec::new() },
        include_info,
    }
}

const UNRESOLVED_TOPLEVEL_RULE: &str = "call.unresolved-toplevel";
const SYSTEMIC_THRESHOLD: usize = 8;
const GENUINE_BUG_MAX_COUNT: usize = 5;

/// Run the heuristic-hint recognisers in precedence order, each claiming the
/// diagnostics it matches so a later recogniser cannot re-claim them (reference
/// `Catalogue.recognise`). rigor-rs ports the count / rule-based recognisers that
/// fire on its diagnostic set — H7 (unresolved-toplevel), H5 (systemic cluster),
/// H6 (genuine bugs); the ecosystem recognisers (H1 ActiveSupport, H2/H2K
/// monkey-patch, H3 gem-without-rbs, H4 ActiveRecord relation) are deferred —
/// they need method tables / `:info` notices / cross-file provenance / `Array[…]`
/// relation receivers that rigor-rs does not produce, and would never fire here.
/// The preserved order (H7 before the H5/H6 catch-alls) matches the reference.
fn recognise(diags: &[DiagRef], include_info: bool) -> Vec<Hint> {
    let mut claimed = vec![false; diags.len()];
    let mut hints = Vec::new();
    // H7 sees the full pool; H5/H6 are `:info`-guarded unless --include-info.
    for (recogniser, info_guarded) in [
        (h7_unresolved_toplevel as fn(&[DiagRef], &[usize]) -> Option<(Hint, Vec<usize>)>, false),
        (h5_systemic_cluster, true),
        (h6_genuine_bugs, true),
    ] {
        let pool: Vec<usize> = (0..diags.len())
            .filter(|&i| !claimed[i])
            .filter(|&i| include_info || !info_guarded || diags[i].1.severity != Severity::Info)
            .collect();
        if let Some((hint, matched)) = recogniser(diags, &pool) {
            for i in matched {
                claimed[i] = true;
            }
            hints.push(hint);
        }
    }
    hints
}

/// H7 — implicit-self toplevel calls that resolve to nothing.
fn h7_unresolved_toplevel(diags: &[DiagRef], pool: &[usize]) -> Option<(Hint, Vec<usize>)> {
    let matched: Vec<usize> =
        pool.iter().copied().filter(|&i| qualified_rule(diags[i].1) == UNRESOLVED_TOPLEVEL_RULE).collect();
    if matched.is_empty() {
        return None;
    }
    let mut files: Vec<&str> = matched.iter().map(|&i| diags[i].0).collect();
    files.sort_unstable();
    files.dedup();
    let methods = top_methods(matched.iter().map(|&i| diags[i].1.method_name.as_deref()));
    let summary = format!(
        "{} toplevel call(s) resolve to nothing visible across {} file(s) ({methods})",
        matched.len(),
        files.len()
    );
    Some((
        Hint {
            id: "unresolved-toplevel",
            confidence: "possible",
            diagnostic_count: matched.len(),
            summary,
            action: "If a monkey-patch or required helper defines these, list its \
                     file in `.rigor.yml`'s `pre_eval:` (ADR-17); otherwise they may \
                     be genuine typos or missing requires.",
        },
        matched,
    ))
}

/// H5 — a single (file, rule) cluster of at least [`SYSTEMIC_THRESHOLD`]
/// diagnostics (the largest such cluster wins).
fn h5_systemic_cluster(diags: &[DiagRef], pool: &[usize]) -> Option<(Hint, Vec<usize>)> {
    use std::collections::HashMap;
    let mut groups: HashMap<(&str, String), Vec<usize>> = HashMap::new();
    for &i in pool {
        groups.entry((diags[i].0, qualified_rule(diags[i].1))).or_default().push(i);
    }
    let (path, rule, matched) = groups
        .into_iter()
        .filter(|(_, v)| v.len() >= SYSTEMIC_THRESHOLD)
        // max by size, then a stable tiebreak on (path, rule) for determinism.
        .max_by(|a, b| {
            a.1.len().cmp(&b.1.len()).then_with(|| b.0.cmp(&a.0))
        })
        .map(|((p, r), v)| (p, r, v))?;
    Some((
        Hint {
            id: "systemic-file-cluster",
            confidence: "likely",
            diagnostic_count: matched.len(),
            summary: format!("{}× `{rule}` concentrated in {path}", matched.len()),
            action: "Likely systemic in this file — one fix may clear many; \
                     or a strong baseline candidate (ADR-22).",
        },
        matched,
    ))
}

/// H6 — every rule whose total count is low (1..=[`GENUINE_BUG_MAX_COUNT`]) —
/// the localised bugs, not systemic noise.
fn h6_genuine_bugs(diags: &[DiagRef], pool: &[usize]) -> Option<(Hint, Vec<usize>)> {
    use std::collections::HashMap;
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for &i in pool {
        groups.entry(qualified_rule(diags[i].1)).or_default().push(i);
    }
    let mut small: Vec<(String, Vec<usize>)> =
        groups.into_iter().filter(|(_, v)| (1..=GENUINE_BUG_MAX_COUNT).contains(&v.len())).collect();
    if small.is_empty() {
        return None;
    }
    // `rules` string is sorted (reference `.sort`); matched is every small diag.
    small.sort_by(|a, b| a.0.cmp(&b.0));
    let rules = small.iter().map(|(r, v)| format!("{r}×{}", v.len())).collect::<Vec<_>>().join(", ");
    let matched: Vec<usize> = small.into_iter().flat_map(|(_, v)| v).collect();
    Some((
        Hint {
            id: "genuine-bugs",
            confidence: "likely",
            diagnostic_count: matched.len(),
            summary: format!("low-count, scattered rules ({rules})"),
            action: "Review these first — low-count diagnostics are usually the \
                     localised bugs Rigor caught, not systemic noise.",
        },
        matched,
    ))
}

/// The `method×count` summary of a diagnostic set's method names, top 5 by
/// `-count, method` (reference `top_methods`).
fn top_methods<'a>(names: impl Iterator<Item = Option<&'a str>>) -> String {
    use std::collections::HashMap;
    let mut tally: HashMap<&str, usize> = HashMap::new();
    for n in names.flatten() {
        *tally.entry(n).or_insert(0) += 1;
    }
    let mut rows: Vec<(&str, usize)> = tally.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    rows.into_iter().take(5).map(|(m, c)| format!("{m}×{c}")).collect::<Vec<_>>().join(" ")
}

fn build_summary(diags: &[DiagRef]) -> Summary {
    let mut error = 0;
    let mut warning = 0;
    let mut info = 0;
    for (_, d) in diags {
        match d.severity {
            Severity::Error => error += 1,
            Severity::Warning => warning += 1,
            Severity::Info => info += 1,
        }
    }
    Summary { total: diags.len(), error, warning, info }
}

/// The qualified rule id (reference `rule_key`/`Diagnostic#qualified_rule`): a
/// `builtin` / empty family renders the bare (already-canonical) rule, else
/// `family.rule`.
fn qualified_rule(d: &Diagnostic) -> String {
    let family = d.source_family;
    if family.is_empty() || family == "builtin" {
        d.rule_id.to_string()
    } else {
        format!("{family}.{}", d.rule_id)
    }
}

fn build_distribution(diags: &[DiagRef]) -> Vec<RuleCount> {
    let counts = group_count(diags.iter().map(|(_, d)| qualified_rule(d)));
    let mut rows: Vec<RuleCount> =
        counts.into_iter().map(|(rule, count)| RuleCount { rule, count }).collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.rule.cmp(&b.rule)));
    rows
}

fn build_selectors(diags: &[DiagRef]) -> Vec<Selector> {
    use std::collections::HashMap;
    // key = (receiver-or-None, method); value = the member (path, diag) list.
    let mut groups: HashMap<(Option<String>, String), Vec<DiagRef>> = HashMap::new();
    for (path, d) in diags {
        let Some(method) = &d.method_name else { continue };
        let receiver = selector_receiver(d.receiver_type.as_deref());
        groups.entry((receiver, method.clone())).or_default().push((path, d));
    }
    let mut selectors: Vec<Selector> = groups
        .into_iter()
        .map(|((receiver, method), group)| {
            let rules = sorted_rule_counts(&group);
            let files = {
                let mut fs: Vec<&str> = group.iter().map(|(p, _)| *p).collect();
                fs.sort_unstable();
                fs.dedup();
                fs.len()
            };
            Selector { receiver, method, count: group.len(), files, rules }
        })
        .collect();
    // sort_by [-count, receiver.to_s (nil→""), method]
    selectors.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.receiver.as_deref().unwrap_or("").cmp(b.receiver.as_deref().unwrap_or("")))
            .then_with(|| a.method.cmp(&b.method))
    });
    selectors
}

/// The receiver token a diagnostic buckets under: the reference `normalize_receiver`
/// fold applied to `receiver_type`, falling back to the raw string it cannot
/// reduce, and `None` for a method-only (no-receiver) diagnostic.
fn selector_receiver(receiver_type: Option<&str>) -> Option<String> {
    let rt = receiver_type?;
    normalize_receiver(rt).or_else(|| Some(rt.to_string()))
}

/// Folds a receiver display token to the class the diagnostics should bucket
/// under, so the selector axis does not fragment one method across every distinct
/// literal receiver. A faithful port of `Triage.normalize_receiver`.
fn normalize_receiver(token: &str) -> Option<String> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    let body = t.strip_prefix('-').unwrap_or(t);
    if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
        return Some("Integer".to_string());
    }
    if is_float_literal(t) {
        return Some("Float".to_string());
    }
    if t.starts_with('"') || t.starts_with('\'') {
        return Some("String".to_string());
    }
    if t.starts_with(':') {
        return Some("Symbol".to_string());
    }
    // singleton(C::D) → C::D
    if let Some(inner) = t.strip_prefix("singleton(").and_then(|s| s.strip_suffix(')')) {
        if is_class_path(inner) {
            return Some(inner.to_string());
        }
    }
    if t.starts_with("Array[") {
        return Some(t.to_string());
    }
    // A generic head `C[...]` → C.
    if let Some(idx) = t.find('[') {
        let head = &t[..idx];
        if is_class_path(head) {
            return Some(head.to_string());
        }
    }
    if is_class_path(t) {
        return Some(t.to_string());
    }
    None
}

/// `\A-?\d+\.\d+\z` — a plain decimal float literal.
fn is_float_literal(t: &str) -> bool {
    let body = t.strip_prefix('-').unwrap_or(t);
    match body.split_once('.') {
        Some((int, frac)) => {
            !int.is_empty()
                && !frac.is_empty()
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// `\A[\w:]+\z` — a bare class/constant path (word chars and `:` only, non-empty).
fn is_class_path(t: &str) -> bool {
    !t.is_empty() && t.bytes().all(|b| b == b':' || b == b'_' || b.is_ascii_alphanumeric())
}

fn build_hotspots(diags: &[DiagRef], top: usize) -> Vec<Hotspot> {
    use std::collections::HashMap;
    let mut groups: HashMap<&str, Vec<DiagRef>> = HashMap::new();
    for (path, d) in diags {
        groups.entry(path).or_default().push((path, d));
    }
    let mut spots: Vec<Hotspot> = groups
        .into_iter()
        .map(|(file, group)| Hotspot {
            file: file.to_string(),
            count: group.len(),
            by_rule: sorted_rule_counts(&group),
        })
        .collect();
    spots.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.file.cmp(&b.file)));
    spots.truncate(top);
    spots
}

/// `(qualified_rule, count)` pairs sorted by `-count, rule` (the reference's
/// per-group breakdown ordering, shared by selectors' `rules` and hotspots'
/// `by_rule`).
fn sorted_rule_counts(group: &[DiagRef]) -> Vec<(String, usize)> {
    let mut rows: Vec<(String, usize)> =
        group_count(group.iter().map(|(_, d)| qualified_rule(d))).into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// Count occurrences of each key (insertion-order-agnostic; callers sort).
fn group_count<I: IntoIterator<Item = String>>(items: I) -> std::collections::HashMap<String, usize> {
    let mut map = std::collections::HashMap::new();
    for k in items {
        *map.entry(k).or_insert(0) += 1;
    }
    map
}

// ---------------------------------------------------------------------------
// Text rendering (reference `TriageRenderer#text`)
// ---------------------------------------------------------------------------

fn render_text(report: &Report, sections: &[Section]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for section in sections {
        match section {
            Section::Distribution => blocks.push(distribution_block(report)),
            Section::Selectors => blocks.push(selectors_block(report)),
            Section::Hotspots => blocks.push(hotspots_block(report)),
            Section::Hints => blocks.push(hints_block(report)),
        }
    }
    format!("{}\n", blocks.join("\n\n"))
}

fn distribution_block(report: &Report) -> String {
    let s = &report.summary;
    let max = report.distribution.iter().map(|r| r.count).max().unwrap_or(1);
    let info_suffix = if s.info > 0 { format!(" / {} info", s.info) } else { String::new() };
    let mut lines = vec![format!(
        "Diagnostic distribution — {} total ({} error / {} warning{})",
        s.total, s.error, s.warning, info_suffix
    )];
    if !report.include_info && s.info > 0 {
        lines.push(format!(
            "  {} info diagnostic(s) hidden below (mostly plugin recognition trace) \
             — pass --include-info to route them",
            s.info
        ));
    }
    for row in &report.distribution {
        lines.push(format!("  {:<32} {:>5}  {}", row.rule, row.count, bar(row.count, max)));
    }
    lines.join("\n")
}

fn selectors_block(report: &Report) -> String {
    if report.selectors.is_empty() {
        return "Selectors — by class / method\n  (none)".to_string();
    }
    let mut lines =
        vec!["Selectors — by class / method (top 15; full list in --format json)".to_string()];
    for sel in report.selectors.iter().take(SELECTOR_ROWS) {
        let label = match &sel.receiver {
            Some(r) => format!("{r}#{}", sel.method),
            None => sel.method.clone(),
        };
        lines.push(format!("  {:<44} {:>5}  {:>3} file(s)", label, sel.count, sel.files));
    }
    lines.join("\n")
}

fn hotspots_block(report: &Report) -> String {
    if report.hotspots.is_empty() {
        return "Hotspot files\n  (none)".to_string();
    }
    let mut lines = vec!["Hotspot files".to_string()];
    for spot in &report.hotspots {
        let by_rule = spot
            .by_rule
            .iter()
            .map(|(rule, count)| format!("{rule}×{count}"))
            .collect::<Vec<_>>()
            .join("  ");
        lines.push(format!("  {:<40} {:>4}  {}", spot.file, spot.count, by_rule));
    }
    lines.join("\n")
}

/// The `Hints` text section (reference `TriageRenderer#hints_block`).
fn hints_block(report: &Report) -> String {
    if report.hints.is_empty() {
        return "Hints\n  (no heuristic hints)".to_string();
    }
    let mut lines = vec!["Hints — heuristics, verify before acting".to_string()];
    for h in &report.hints {
        lines.push(String::new());
        lines.push(format!("  [{} {}]  {} diagnostic(s)", h.confidence, h.id, h.diagnostic_count));
        lines.push(format!("    {}", h.summary));
        lines.push(format!("    → {}", h.action));
    }
    lines.join("\n")
}

fn bar(count: usize, max: usize) -> String {
    let mut filled = (count * BAR_WIDTH).checked_div(max).unwrap_or(0);
    if filled == 0 && count > 0 {
        filled = 1;
    }
    "█".repeat(filled)
}

// ---------------------------------------------------------------------------
// JSON rendering (reference `Triage.report_to_h`)
// ---------------------------------------------------------------------------

fn report_json(report: &Report) -> String {
    let map = |pairs: &[(String, usize)]| -> serde_json::Map<String, serde_json::Value> {
        pairs.iter().map(|(k, v)| (k.clone(), json!(v))).collect()
    };
    let payload = json!({
        "summary": {
            "total": report.summary.total,
            "error": report.summary.error,
            "warning": report.summary.warning,
            "info": report.summary.info,
        },
        "distribution": report.distribution.iter()
            .map(|r| json!({ "rule": r.rule, "count": r.count }))
            .collect::<Vec<_>>(),
        "selectors": report.selectors.iter()
            .map(|s| json!({
                "receiver": s.receiver,
                "method": s.method,
                "count": s.count,
                "files": s.files,
                "rules": map(&s.rules),
            }))
            .collect::<Vec<_>>(),
        "hotspots": report.hotspots.iter()
            .map(|h| json!({ "file": h.file, "count": h.count, "by_rule": map(&h.by_rule) }))
            .collect::<Vec<_>>(),
        "hints": report.hints.iter().map(|h| json!({
            "id": h.id,
            "confidence": h.confidence,
            "diagnostic_count": h.diagnostic_count,
            "summary": h.summary,
            "action": h.action,
        })).collect::<Vec<_>>(),
        "include_info": report.include_info,
    });
    serde_json::to_string_pretty(&payload).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_receiver_folds_literals() {
        assert_eq!(normalize_receiver("\"hello\"").as_deref(), Some("String"));
        assert_eq!(normalize_receiver("'x'").as_deref(), Some("String"));
        assert_eq!(normalize_receiver("5").as_deref(), Some("Integer"));
        assert_eq!(normalize_receiver("-42").as_deref(), Some("Integer"));
        assert_eq!(normalize_receiver("3.14").as_deref(), Some("Float"));
        assert_eq!(normalize_receiver(":sym").as_deref(), Some("Symbol"));
        assert_eq!(normalize_receiver("singleton(Time)").as_deref(), Some("Time"));
        assert_eq!(normalize_receiver("Array[String]").as_deref(), Some("Array[String]"));
        assert_eq!(normalize_receiver("Hash[Symbol, Integer]").as_deref(), Some("Hash"));
        assert_eq!(normalize_receiver("String").as_deref(), Some("String"));
        assert_eq!(normalize_receiver("Foo::Bar").as_deref(), Some("Foo::Bar"));
        // A display it cannot reduce → None (caller keeps the raw string).
        assert_eq!(normalize_receiver("[1, 2, 3]"), None);
        assert_eq!(normalize_receiver("String | nil"), None);
    }

    fn diag(rule: &'static str, sev: Severity, receiver: Option<&str>, method: Option<&str>) -> Diagnostic {
        Diagnostic {
            rule_id: rule,
            start_offset: 0,
            end_offset: 0,
            message: String::new(),
            severity: sev,
            source_family: "builtin",
            receiver_type: receiver.map(str::to_string),
            method_name: method.map(str::to_string),
        }
    }

    #[test]
    fn hint_recognisers_claim_in_precedence_order() {
        let td = diag("call.unresolved-toplevel", Severity::Error, None, Some("helper"));
        let u1 = diag("call.undefined-method", Severity::Error, Some("\"x\""), Some("a"));
        let u2 = diag("call.undefined-method", Severity::Error, Some("\"y\""), Some("b"));
        let diags = vec![("f.rb", &td), ("f.rb", &u1), ("g.rb", &u2)];
        let hints = recognise(&diags, false);
        // H7 (unresolved-toplevel) claims the toplevel call; H6 (genuine bugs)
        // then claims the two remaining undefined-methods. H5 needs >= 8, so no.
        assert_eq!(hints.len(), 2);
        assert_eq!(hints[0].id, "unresolved-toplevel");
        assert_eq!(hints[0].diagnostic_count, 1);
        assert_eq!(hints[1].id, "genuine-bugs");
        assert_eq!(hints[1].diagnostic_count, 2);
        assert_eq!(hints[1].summary, "low-count, scattered rules (call.undefined-method×2)");
    }

    #[test]
    fn systemic_cluster_fires_at_threshold() {
        // 8 undefined-methods in one file → H5 (not H6, which claims <= 5).
        let ds: Vec<Diagnostic> = (0..8)
            .map(|_| diag("call.undefined-method", Severity::Error, Some("\"x\""), Some("m")))
            .collect();
        let diags: Vec<DiagRef> = ds.iter().map(|d| ("big.rb", d)).collect();
        let hints = recognise(&diags, false);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].id, "systemic-file-cluster");
        assert_eq!(hints[0].summary, "8× `call.undefined-method` concentrated in big.rb");
    }

    #[test]
    fn distribution_and_summary() {
        let d1 = diag("call.undefined-method", Severity::Error, Some("\"x\""), Some("frist"));
        let d2 = diag("call.undefined-method", Severity::Error, Some("\"y\""), Some("lenght"));
        let d3 = diag("call.wrong-arity", Severity::Warning, Some("z"), Some("f"));
        let diags = vec![("a.rb", &d1), ("a.rb", &d2), ("b.rb", &d3)];
        let report = analyze(&diags, 10, false, false);
        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.error, 2);
        assert_eq!(report.summary.warning, 1);
        // distribution sorted by -count: undefined-method (2) before wrong-arity (1).
        assert_eq!(report.distribution[0].rule, "call.undefined-method");
        assert_eq!(report.distribution[0].count, 2);
        // selectors: two String literals fold to the same class but different
        // methods ⇒ two rows; hotspots: a.rb (2) before b.rb (1).
        assert_eq!(report.hotspots[0].file, "a.rb");
        assert_eq!(report.hotspots[0].count, 2);
        let str_frist = report.selectors.iter().find(|s| s.method == "frist").unwrap();
        assert_eq!(str_frist.receiver.as_deref(), Some("String"));
    }

    #[test]
    fn method_only_selector_has_null_receiver() {
        let d = diag("call.unresolved-toplevel", Severity::Error, None, Some("helper"));
        let diags = vec![("a.rb", &d)];
        let report = analyze(&diags, 10, false, false);
        assert_eq!(report.selectors.len(), 1);
        assert!(report.selectors[0].receiver.is_none());
        assert_eq!(report.selectors[0].method, "helper");
    }

    #[test]
    fn empty_stream_renders_none_placeholders() {
        let report = analyze(&[], 10, false, false);
        let text = render_text(&report, &[Section::Distribution, Section::Selectors, Section::Hotspots]);
        assert!(text.contains("Diagnostic distribution — 0 total (0 error / 0 warning)"));
        assert!(text.contains("Selectors — by class / method\n  (none)"));
        assert!(text.contains("Hotspot files\n  (none)"));
    }

    #[test]
    fn bar_scaling() {
        assert_eq!(bar(0, 4), "");
        assert_eq!(bar(1, 100), "█"); // rounds to 0 then bumped to 1
        assert_eq!(bar(4, 4), "█".repeat(24));
    }
}
