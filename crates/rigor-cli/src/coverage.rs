//! `rigor coverage PATH...` — type-precision coverage scan.
//!
//! A port of the reference `CoverageCommand` + `Inference::PrecisionScanner` +
//! `CoverageRenderer` (`lib/rigor/cli/coverage_command.rb`,
//! `lib/rigor/inference/precision_scanner.rb`, `lib/rigor/cli/coverage_renderer.rb`).
//!
//! # What it does
//!
//! Walks every Prism node in each file, classifies the inferred type of each
//! *expression* node into one of eight precision tiers (constant / nominal /
//! shaped / refined / bot / dynamic_specific / dynamic_top / top), and reports
//! the aggregate + per-file precise-vs-Dynamic ratio. `--format=text|json`,
//! `--threshold=R` (exit 1 below), exit 64 on usage errors.
//!
//! # Parity model (see `docs/notes/20260719-coverage-command-scoping.md`)
//!
//! The scan walks the SAME Prism node set as the reference (the `ruby_prism`
//! binding's `Visit` trait yields the identical `compact_child_nodes` set the
//! reference's `NodeWalker` does), excluding the SAME non-expression node
//! classes, so the DENOMINATOR is byte-identical by construction. The per-node
//! TIER is computed by reproducing the reference's `ExpressionTyper#type_of`
//! dispatch, delegating value-producing *leaves* (calls, constant reads,
//! literals, arrays, hashes, ranges, local/ivar reads) to rigor-rs's own
//! [`rigor_infer::Typer`] and composing the structural handlers (if / and-or /
//! case / begin / class) at the tier level. Where rigor-rs's inference is a
//! sound subset of the reference (call dispatch, constant resolution), the tier
//! is coarser — a documented, enumerable divergence (the v0.3.0-RC precision
//! deltas), never a wrong-direction claim.
//!
//! The scan runs on rayon's file-parallel pool (per-file results merged in
//! input order — byte-identical to sequential by construction, the same
//! contract as the `check` pipeline). `--workers=N` maps to the pool size;
//! output is byte-identical regardless of N.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use rayon::prelude::*;
use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, TypeEnv, Typer};
use rigor_parse::ruby_prism::{self, Node, Visit};
use rigor_parse::{lower, LoweredAst};
use rigor_types::{ruby_float_to_s, Interner, Scalar, Type, TypeId};

use crate::config::Config;

const USAGE: &str = "Usage: rigor coverage [options] PATH...";

/// One scope's positional binding events: `(stmt end offset, name, type)` in
/// bind order (see `FileScanner::toplevel_env`).
type BindEvents = Vec<(usize, String, TypeId)>;

// ---------------------------------------------------------------------------
// Tiers
// ---------------------------------------------------------------------------

/// The eight precision tiers, in the reference's `TIERS` order (which is also
/// the rank order used by union `worst_of` / intersection `best_of`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    Constant,
    Nominal,
    Shaped,
    Refined,
    Bot,
    DynamicSpecific,
    DynamicTop,
    Top,
}

/// Tier order == `PrecisionScanner::TIERS` == the `by_tier` / tier-table order.
const TIERS: [Tier; 8] = [
    Tier::Constant,
    Tier::Nominal,
    Tier::Shaped,
    Tier::Refined,
    Tier::Bot,
    Tier::DynamicSpecific,
    Tier::DynamicTop,
    Tier::Top,
];

impl Tier {
    /// Index into a `[u64; 8]` tier-count array (== `TIER_RANK`).
    fn idx(self) -> usize {
        match self {
            Tier::Constant => 0,
            Tier::Nominal => 1,
            Tier::Shaped => 2,
            Tier::Refined => 3,
            Tier::Bot => 4,
            Tier::DynamicSpecific => 5,
            Tier::DynamicTop => 6,
            Tier::Top => 7,
        }
    }

    /// The JSON `by_tier` key.
    fn json_key(self) -> &'static str {
        match self {
            Tier::Constant => "constant",
            Tier::Nominal => "nominal",
            Tier::Shaped => "shaped",
            Tier::Refined => "refined",
            Tier::Bot => "bot",
            Tier::DynamicSpecific => "dynamic_specific",
            Tier::DynamicTop => "dynamic_top",
            Tier::Top => "top",
        }
    }

    /// The text tier-table label (reference `CoverageRenderer::TIER_LABELS`).
    fn text_label(self) -> &'static str {
        match self {
            Tier::Constant => "constant",
            Tier::Nominal => "nominal",
            Tier::Shaped => "shaped (Tuple/Hash/Range/generic)",
            Tier::Refined => "refined",
            Tier::Bot => "bot (unreachable)",
            Tier::DynamicSpecific => "dynamic — partial info",
            Tier::DynamicTop => "dynamic — opaque (untyped)",
            Tier::Top => "top",
        }
    }

    /// Precise tiers (numerator of `precision_ratio`): constant/nominal/shaped/
    /// refined/bot.
    fn is_precise(self) -> bool {
        matches!(
            self,
            Tier::Constant | Tier::Nominal | Tier::Shaped | Tier::Refined | Tier::Bot
        )
    }
}

// ---------------------------------------------------------------------------
// Per-file result & report
// ---------------------------------------------------------------------------

/// The per-file tier breakdown (reference `PrecisionScanner::FileResult`).
#[derive(Clone, Default)]
struct FileResult {
    total: u64,
    counts: [u64; 8],
}

impl FileResult {
    fn tier(&self, t: Tier) -> u64 {
        self.counts[t.idx()]
    }

    fn precise_count(&self) -> u64 {
        TIERS
            .iter()
            .filter(|t| t.is_precise())
            .map(|t| self.tier(*t))
            .sum()
    }

    fn dynamic_specific_count(&self) -> u64 {
        self.tier(Tier::DynamicSpecific)
    }

    fn opaque_count(&self) -> u64 {
        self.tier(Tier::DynamicTop) + self.tier(Tier::Top)
    }

    /// `precise / total`, or `1.0` when the file typed nothing.
    fn precision_ratio(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            self.precise_count() as f64 / self.total as f64
        }
    }

    /// `opaque / total`, or `0.0` when the file typed nothing.
    fn opaque_ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.opaque_count() as f64 / self.total as f64
        }
    }

    fn add(&mut self, other: &FileResult) {
        self.total += other.total;
        for i in 0..8 {
            self.counts[i] += other.counts[i];
        }
    }
}

/// The whole-run report (reference `CoverageReport`).
struct Report {
    /// The resolved file list, in `collect_paths` order (header + input order).
    files: Vec<String>,
    /// `(file, error-messages)` for each file Prism failed to parse.
    parse_errors: Vec<(String, Vec<String>)>,
    /// `(file, result)` for each successfully-scanned file, in scan order.
    per_file: Vec<(String, FileResult)>,
    /// The accumulated total across all scanned files.
    total: FileResult,
}

impl Report {
    fn grand_total(&self) -> u64 {
        self.total.total
    }
    fn precise_count(&self) -> u64 {
        self.total.precise_count()
    }
    fn opaque_count(&self) -> u64 {
        self.total.opaque_count()
    }
    fn precision_ratio(&self) -> f64 {
        self.total.precision_ratio()
    }
    fn opaque_ratio(&self) -> f64 {
        self.total.opaque_ratio()
    }
    fn tier_count(&self, t: Tier) -> u64 {
        self.total.tier(t)
    }
}

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------

/// Parsed command options.
struct Options {
    format: String,
    threshold: Option<f64>,
    config: Option<String>,
    workers: Option<usize>,
}

/// `rigor coverage [options] PATH...`.
///
/// Exit codes: 0 (scan complete, ratio >= threshold or none), 1 (ratio below
/// threshold, or parse errors), 64 (usage error), 2 (a deferred `--protection`/
/// `--mutation` mode was requested).
pub fn cmd_coverage(args: &[String]) -> ExitCode {
    let (options, positional) = match parse_options(args) {
        Ok(v) => v,
        Err(code) => return code,
    };

    // Config drives the fallback `paths:` and the RBS signature environment.
    let config_path = options.config.as_deref().map(Path::new);
    let cfg = Config::load(config_path);

    // Resolve paths: explicit args, else config `paths:` (reference
    // `@argv.empty? ? configuration.paths : @argv`).
    let args_for_paths: Vec<String> = if positional.is_empty() {
        cfg.paths.clone()
    } else {
        positional
    };
    let paths = match collect_paths(&args_for_paths) {
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(64);
        }
        Ok(p) => p,
    };
    if paths.is_empty() {
        eprintln!("coverage: at least one path is required");
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    }

    let index = build_index(&cfg);
    let report = scan_paths(&paths, &index, options.workers);

    let out = match options.format.as_str() {
        "json" => render_json(&report),
        "text" => render_text(&report),
        other => {
            eprintln!("coverage: unsupported format: {other}");
            return ExitCode::from(64);
        }
    };
    print!("{out}");

    determine_exit(&report, &options)
}

/// Parse the option flags, returning `(options, positional_args)` or an exit
/// code. `--protection` / `--mutation` / `--with-tests` and their sub-flags are
/// the deferred mutation-machinery track (ADR-63/70): parsed and rejected with
/// exit 2 (the stub convention).
fn parse_options(args: &[String]) -> Result<(Options, Vec<String>), ExitCode> {
    let mut format = "text".to_string();
    let mut threshold: Option<f64> = None;
    let mut config: Option<String> = None;
    let mut workers: Option<usize> = None;
    let mut positional: Vec<String> = Vec::new();

    // Split `--flag=value` on the first `=`; otherwise consume the next arg.
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        // A helper to fetch the flag's value from `--flag=v` or the next token.
        macro_rules! value_for {
            ($name:expr) => {
                match inline {
                    Some(v) => v,
                    None => match it.next() {
                        Some(v) => v.clone(),
                        None => {
                            eprintln!("coverage: {} requires an argument", $name);
                            return Err(ExitCode::from(64));
                        }
                    },
                }
            };
        }
        match flag {
            "--format" => format = value_for!("--format"),
            "--config" => config = Some(value_for!("--config")),
            "--threshold" => {
                let v = value_for!("--threshold");
                match v.parse::<f64>() {
                    Ok(r) => threshold = Some(r),
                    Err(_) => {
                        eprintln!("coverage: invalid argument for --threshold: {v}");
                        return Err(ExitCode::from(64));
                    }
                }
            }
            "--workers" => {
                let v = value_for!("--workers");
                match v.parse::<i64>() {
                    // Absent/0/negative → default pool (None). N>0 → pool size.
                    Ok(n) => workers = if n > 0 { Some(n as usize) } else { None },
                    Err(_) => {
                        eprintln!("coverage: invalid argument for --workers: {v}");
                        return Err(ExitCode::from(64));
                    }
                }
            }
            // The deferred mutation-machinery track (ADR-63/70).
            "--protection" | "--mutation" | "--with-tests" | "--test-command"
            | "--include-dynamic" | "--limit" | "--seed" => {
                eprintln!(
                    "coverage: {flag} (type-protection / mutation coverage) is not yet implemented in this port"
                );
                return Err(ExitCode::from(2));
            }
            other if other.starts_with('-') => {
                eprintln!("coverage: unknown option: {other}");
                eprintln!("{USAGE}");
                return Err(ExitCode::from(64));
            }
            _ => positional.push(arg.clone()),
        }
    }

    Ok((
        Options {
            format,
            threshold,
            config,
            workers,
        },
        positional,
    ))
}

/// Exit code (reference `determine_exit`): 1 when any file failed to parse; else
/// 1 when a `--threshold` is set and the precision ratio is below it; else 0.
fn determine_exit(report: &Report, options: &Options) -> ExitCode {
    if !report.parse_errors.is_empty() {
        return ExitCode::from(1);
    }
    match options.threshold {
        Some(t) if report.precision_ratio() < t => ExitCode::from(1),
        _ => ExitCode::SUCCESS,
    }
}

/// The RBS environment for the precision scan: libraries + `signature_paths`
/// only (no plugins — those are `--protection`-mode-only in the reference; the
/// default-mode `CoverageScan.project_environment` is plugin-free). With no
/// config this reduces to the core+stdlib index.
fn build_index(cfg: &Config) -> CoreIndex {
    let root = Path::new(".");
    let sig_dirs = cfg.all_signature_dirs(root);
    if sig_dirs.is_empty() {
        CoreIndex::new()
    } else {
        CoreIndex::for_project(&[], &sig_dirs)
    }
}

/// The `coverage --format=json` report for `path_args` (files/dirs; empty →
/// the config `paths:`), as the MCP `coverage` tool returns it — byte-identical
/// to the CLI output. `Err` carries the usage-error message.
pub fn mcp_coverage_json(
    path_args: &[String],
    config: Option<&Path>,
) -> Result<String, String> {
    let cfg = Config::load(config);
    let args_for_paths: Vec<String> = if path_args.is_empty() {
        cfg.paths.clone()
    } else {
        path_args.to_vec()
    };
    let paths = collect_paths(&args_for_paths)?;
    if paths.is_empty() {
        return Err("coverage: at least one path is required".to_string());
    }
    let index = build_index(&cfg);
    let report = scan_paths(&paths, &index, None);
    Ok(render_json(&report))
}

/// Expand `args` into a unique list of `.rb` paths (reference `collect_paths`):
/// a directory expands to its `**/*.rb` (Ruby `Dir.glob` traversal order); a
/// file passes through; a non-existent path aborts the whole resolution with
/// the reference's error message (→ EXIT_USAGE at the CLI). The final list is
/// de-duplicated preserving first-seen order (`paths.uniq`).
fn collect_paths(args: &[String]) -> Result<Vec<String>, String> {
    let mut paths: Vec<String> = Vec::new();
    for arg in args {
        let p = Path::new(arg);
        if p.is_dir() {
            paths.extend(glob_rb(arg));
        } else if p.is_file() {
            paths.push(arg.clone());
        } else {
            return Err(format!("coverage: not a file or directory: {arg}"));
        }
    }
    // Dedup preserving first-seen order.
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    Ok(paths)
}

/// Collect `<dir>/**/*.rb` in Ruby `Dir.glob("**/*.rb")` order (skipping hidden
/// entries, which `Dir.glob` does not match without `File::FNM_DOTMATCH`).
///
/// Ruby's glob is NOT a flat lexicographic sort of the full paths: it descends
/// per directory, sorting each directory's entries (subdirectories AND files
/// together) by name and emitting in that order, recursing into a subdirectory
/// at its sorted position. So `controllers/admin/*.rb` (the `admin` dir sorts
/// before the `admin.rb` file, `"admin" < "admin.rb"`) emit before
/// `controllers/admin.rb`. Paths are returned prefixed with `dir` exactly as
/// `Dir.glob(File.join(dir, "**/*.rb"))` produces them.
fn glob_rb(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    glob_rb_walk(dir, &mut out);
    out
}

fn glob_rb_walk(dir: &str, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    // Collect (name, is_dir) for non-hidden entries, then sort by name — the
    // per-directory ordering Ruby's glob applies before descending.
    let mut items: Vec<(String, bool)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        items.push((name, is_dir));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, is_dir) in items {
        let path = format!("{dir}/{name}");
        if is_dir {
            glob_rb_walk(&path, out);
        } else if name.ends_with(".rb") {
            out.push(path);
        }
    }
}

/// Scan every path (file-parallel on rayon), merging per-file results in input
/// order — byte-identical to a sequential scan.
fn scan_paths(paths: &[String], index: &CoreIndex, workers: Option<usize>) -> Report {
    let scan_all = || -> Vec<Result<FileResult, Vec<String>>> {
        paths.par_iter().map(|p| scan_file(p, index)).collect()
    };

    // `--workers=N` sizes a scoped rayon pool; absent → the global pool. The
    // result is collected in input order either way, so N is invisible in output.
    let results = match workers {
        Some(n) => rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .map(|pool| pool.install(scan_all))
            .unwrap_or_else(|_| scan_all()),
        None => scan_all(),
    };

    let mut parse_errors = Vec::new();
    let mut per_file = Vec::new();
    let mut total = FileResult::default();
    for (path, result) in paths.iter().zip(results) {
        match result {
            Ok(fr) => {
                total.add(&fr);
                per_file.push((path.clone(), fr));
            }
            Err(errors) => parse_errors.push((path.clone(), errors)),
        }
    }

    Report {
        files: paths.to_vec(),
        parse_errors,
        per_file,
        total,
    }
}

/// Parse + scan one file, returning its tier breakdown or the parse-error
/// messages.
fn scan_file(path: &str, index: &CoreIndex) -> Result<FileResult, Vec<String>> {
    let source = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => return Err(vec![format!("cannot read {path}: {e}")]),
    };
    let parse_result = ruby_prism::parse(&source);
    let errors: Vec<String> = parse_result
        .errors()
        .map(|d| d.message().to_string())
        .collect();
    if !errors.is_empty() {
        return Err(errors);
    }

    let ast = lower(&parse_result);
    let source_index = SourceIndex::build(&ast, index);
    let typer = Typer::with_source(index, &source_index);
    let mut interner = Interner::new();

    // Parity-audit hook: a per-file section header preceding the node lines
    // (see the dump block in `FileScanner::scan`; audits run `--workers=1`).
    if std::env::var_os("RIGOR_COVERAGE_NODE_DUMP").is_some() {
        eprintln!("== {path}");
    }

    let root = parse_result.node();
    let mut scanner = FileScanner::new(&ast, &typer, index, &mut interner, &root);
    Ok(scanner.scan(&root))
}

// ---------------------------------------------------------------------------
// The per-file precision scan (reproduces PrecisionScanner + ExpressionTyper)
// ---------------------------------------------------------------------------

/// Collects every Prism node in DFS pre-order (the reference's `NodeWalker`
/// order; order is irrelevant to the tier histogram, only the node SET matters).
struct NodeCollector<'pr> {
    nodes: Vec<Node<'pr>>,
}

/// The node classes some parent reaches through a CONCRETELY-TYPED field
/// (`BeginNode#rescue_clause: RescueNode`, `MatchWriteNode#call: CallNode`, …).
/// The generated `Visit` walker dispatches those children DIRECTLY to their
/// `visit_x_node` method — `visit()` and the branch/leaf enter hooks never
/// fire — so a collector relying on the hooks alone silently misses them
/// (caught on the `rescue => x` taint: the RescueNode never surfaced). For
/// these classes the per-method overrides below do the push; the enter hooks
/// skip them to avoid double-counting when they arrive via a generic field.
fn reached_via_concrete_field(node: &Node<'_>) -> bool {
    matches!(
        node,
        Node::ArgumentsNode { .. }
            | Node::BlockArgumentNode { .. }
            | Node::BlockNode { .. }
            | Node::BlockParameterNode { .. }
            | Node::CallNode { .. }
            | Node::ConstantPathNode { .. }
            | Node::ElseNode { .. }
            | Node::EnsureNode { .. }
            | Node::LocalVariableTargetNode { .. }
            | Node::ParametersNode { .. }
            | Node::RescueNode { .. }
            | Node::SplatNode { .. }
            | Node::StatementsNode { .. }
    )
}

impl<'pr> Visit<'pr> for NodeCollector<'pr> {
    fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
        if !reached_via_concrete_field(&node) {
            self.nodes.push(node);
        }
    }
    fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
        if !reached_via_concrete_field(&node) {
            self.nodes.push(node);
        }
    }
    fn visit_arguments_node(&mut self, node: &ruby_prism::ArgumentsNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_arguments_node(self, node);
    }
    fn visit_block_argument_node(&mut self, node: &ruby_prism::BlockArgumentNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_block_argument_node(self, node);
    }
    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_block_node(self, node);
    }
    fn visit_block_parameter_node(&mut self, node: &ruby_prism::BlockParameterNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_block_parameter_node(self, node);
    }
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_call_node(self, node);
    }
    fn visit_constant_path_node(&mut self, node: &ruby_prism::ConstantPathNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_constant_path_node(self, node);
    }
    fn visit_else_node(&mut self, node: &ruby_prism::ElseNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_else_node(self, node);
    }
    fn visit_ensure_node(&mut self, node: &ruby_prism::EnsureNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_ensure_node(self, node);
    }
    fn visit_local_variable_target_node(
        &mut self,
        node: &ruby_prism::LocalVariableTargetNode<'pr>,
    ) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_local_variable_target_node(self, node);
    }
    fn visit_parameters_node(&mut self, node: &ruby_prism::ParametersNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_parameters_node(self, node);
    }
    fn visit_rescue_node(&mut self, node: &ruby_prism::RescueNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_rescue_node(self, node);
    }
    fn visit_splat_node(&mut self, node: &ruby_prism::SplatNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_splat_node(self, node);
    }
    fn visit_statements_node(&mut self, node: &ruby_prism::StatementsNode<'pr>) {
        self.nodes.push(node.as_node());
        ruby_prism::visit_statements_node(self, node);
    }
}

/// PRISM-side local-rebind taints the arena cannot supply (PR #33 re-review
/// BLOCKING-1): `MultiWriteNode` local targets (multi-writes have no arena
/// lowering), `for x in …` index targets (the lowering drops the index), and
/// `rescue => x` captures. Each yields `(target span, name)` — same shape as
/// the substrate `collect_flow_writes` entries the taint scan consumes.
fn collect_prism_taints(root: &Node<'_>) -> Vec<((usize, usize), String)> {
    let mut collector = NodeCollector { nodes: Vec::new() };
    collector.visit(root);
    let mut out = Vec::new();
    for node in &collector.nodes {
        match node {
            Node::MultiWriteNode { .. } => {
                let n = node.as_multi_write_node().unwrap();
                for t in n.lefts().iter() {
                    collect_local_targets(&t, &mut out);
                }
                if let Some(t) = n.rest() {
                    collect_local_targets(&t, &mut out);
                }
                for t in n.rights().iter() {
                    collect_local_targets(&t, &mut out);
                }
            }
            Node::ForNode { .. } => {
                collect_local_targets(&node.as_for_node().unwrap().index(), &mut out);
            }
            Node::RescueNode { .. } => {
                if let Some(t) = node.as_rescue_node().unwrap().reference() {
                    collect_local_targets(&t, &mut out);
                }
            }
            // Index-writes on a bare-local receiver mutate its CONTENT
            // (`h[:k] += x` invalidates a HashShape/Tuple binding — the
            // reference's `[]=` mutator widening; PR #33 re-review, gitlab
            // metrics_interceptor.rb `response_size[:total]`).
            Node::IndexOperatorWriteNode { .. } => {
                if let Some(r) = node.as_index_operator_write_node().unwrap().receiver() {
                    collect_local_receiver(&r, node, &mut out);
                }
            }
            Node::IndexOrWriteNode { .. } => {
                if let Some(r) = node.as_index_or_write_node().unwrap().receiver() {
                    collect_local_receiver(&r, node, &mut out);
                }
            }
            Node::IndexAndWriteNode { .. } => {
                if let Some(r) = node.as_index_and_write_node().unwrap().receiver() {
                    collect_local_receiver(&r, node, &mut out);
                }
            }
            Node::IndexTargetNode { .. } => {
                let r = node.as_index_target_node().unwrap().receiver();
                collect_local_receiver(&r, node, &mut out);
            }
            // A block/lambda PARAMETER shadowing an enclosing local makes
            // reads inside the block refer to the parameter, not the binding
            // (Prism resolves depth statically). The flat name-keyed env
            // cannot model shadowing, so the shadowed name is dropped
            // entirely (gitmodules_parser.rb `with_object(iterator) do
            // |text, iterator|`).
            Node::BlockParametersNode { .. } => {
                let bp = node.as_block_parameters_node().unwrap();
                if let Some(params) = bp.parameters() {
                    collect_param_names(&params.as_node(), &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

/// Record a bare-local RECEIVER of an index-write as a taint entry.
fn collect_local_receiver(
    receiver: &Node<'_>,
    write: &Node<'_>,
    out: &mut Vec<((usize, usize), String)>,
) {
    if let Some(lvr) = receiver.as_local_variable_read_node() {
        let name = String::from_utf8_lossy(lvr.name().as_slice()).into_owned();
        out.push((span_of(write), name));
    }
}

/// Collect every plain parameter NAME under a block's `ParametersNode`
/// (required / optional / rest / keyword / block params — any of them shadows).
fn collect_param_names(node: &Node<'_>, out: &mut Vec<((usize, usize), String)>) {
    match node {
        Node::RequiredParameterNode { .. } => {
            let p = node.as_required_parameter_node().unwrap();
            let name = String::from_utf8_lossy(p.name().as_slice()).into_owned();
            out.push((span_of(node), name));
        }
        Node::OptionalParameterNode { .. } => {
            let p = node.as_optional_parameter_node().unwrap();
            let name = String::from_utf8_lossy(p.name().as_slice()).into_owned();
            out.push((span_of(node), name));
        }
        Node::RestParameterNode { .. } => {
            let p = node.as_rest_parameter_node().unwrap();
            if let Some(n) = p.name() {
                out.push((span_of(node), String::from_utf8_lossy(n.as_slice()).into_owned()));
            }
        }
        Node::RequiredKeywordParameterNode { .. } => {
            let p = node.as_required_keyword_parameter_node().unwrap();
            let name = String::from_utf8_lossy(p.name().as_slice()).into_owned();
            out.push((span_of(node), name));
        }
        Node::OptionalKeywordParameterNode { .. } => {
            let p = node.as_optional_keyword_parameter_node().unwrap();
            let name = String::from_utf8_lossy(p.name().as_slice()).into_owned();
            out.push((span_of(node), name));
        }
        Node::BlockParameterNode { .. } => {
            let p = node.as_block_parameter_node().unwrap();
            if let Some(n) = p.name() {
                out.push((span_of(node), String::from_utf8_lossy(n.as_slice()).into_owned()));
            }
        }
        Node::MultiTargetNode { .. } => {
            collect_local_targets(node, out);
        }
        Node::ParametersNode { .. } => {
            let p = node.as_parameters_node().unwrap();
            for c in p.requireds().iter() {
                collect_param_names(&c, out);
            }
            for c in p.optionals().iter() {
                collect_param_names(&c, out);
            }
            if let Some(r) = p.rest() {
                collect_param_names(&r, out);
            }
            for c in p.posts().iter() {
                collect_param_names(&c, out);
            }
            for c in p.keywords().iter() {
                collect_param_names(&c, out);
            }
            if let Some(kr) = p.keyword_rest() {
                collect_param_names(&kr, out);
            }
            if let Some(b) = p.block() {
                collect_param_names(&b.as_node(), out);
            }
        }
        _ => {}
    }
}

/// Collect the LOCAL-variable target names under a (possibly nested / splatted)
/// assignment-target node. Non-local targets (ivar / constant / call / index)
/// do not touch the local env and are skipped.
fn collect_local_targets(node: &Node<'_>, out: &mut Vec<((usize, usize), String)>) {
    match node {
        Node::LocalVariableTargetNode { .. } => {
            let t = node.as_local_variable_target_node().unwrap();
            let name = String::from_utf8_lossy(t.name().as_slice()).into_owned();
            out.push((span_of(node), name));
        }
        Node::SplatNode { .. } => {
            if let Some(e) = node.as_splat_node().unwrap().expression() {
                collect_local_targets(&e, out);
            }
        }
        Node::MultiTargetNode { .. } => {
            let n = node.as_multi_target_node().unwrap();
            for t in n.lefts().iter() {
                collect_local_targets(&t, out);
            }
            if let Some(t) = n.rest() {
                collect_local_targets(&t, out);
            }
            for t in n.rights().iter() {
                collect_local_targets(&t, out);
            }
        }
        _ => {}
    }
}

struct FileScanner<'a> {
    ast: &'a LoweredAst,
    typer: &'a Typer<'a>,
    index: &'a CoreIndex,
    interner: &'a mut Interner,
    /// Exact `(start, end)` span → arena `NodeId`, for routing value-leaves to
    /// the arena typer. Last-writer (outermost / largest id) wins on collision.
    span_to_arena: HashMap<(usize, usize), rigor_parse::NodeId>,
    /// The top-level scope's POSITIONAL binding events (Program-body
    /// straight-line writes, taint-widened): `(stmt end offset, name, type)`
    /// in bind order. A node sees only the bindings whose statement ENDS
    /// before it — the reference records the scope ENTERING each statement,
    /// and expression-interior nodes inherit it, so a read inside (or before)
    /// its own binding statement must not see that binding
    /// (`options = {...}.merge(options)`: the RHS `options` read is the
    /// param, not the fresh Hash — PR #33 re-review, gitlab api/helpers.rb).
    toplevel_env: BindEvents,
    /// Every FRESH-local-scope carrier's span (def / class / module /
    /// `class << self` bodies): a local write inside one is invisible to the
    /// enclosing scope's env AND to its taint scan (Ruby scoping).
    inner_scope_spans: Vec<(usize, usize)>,
    /// Every flow-write `(span, name)` in the file — local rebinds AND in-place
    /// mutator calls on a bare-local receiver (`x << y` invalidates a
    /// `Tuple[]`-pinned binding). The SAME substrate collector the Typer's flow
    /// passes widen from ([`rigor_infer::collect_flow_writes`]).
    flow_writes: Vec<(rigor_parse::Span, String)>,
    /// PRISM-side supplement to `flow_writes` (PR #33 re-review BLOCKING-1):
    /// local rebind forms the arena does not carry — `MultiWriteNode` targets
    /// (no arena lowering), `for x in …` index targets (lowering drops them),
    /// and `rescue => x` captures. Collected here in coverage.rs, NOT by
    /// widening `collect_flow_writes` itself (that would reach check-pipeline
    /// behavior and need its own parity run).
    extra_taints: Vec<((usize, usize), String)>,
    /// Per fresh scope (def/class/module), its straight-line + taint-widened
    /// POSITIONAL binding events (same shape as `toplevel_env`). `env_at`
    /// picks the innermost containing scope, then materializes the bindings
    /// visible at the query position.
    scope_envs: Vec<((usize, usize), BindEvents)>,
    /// The current file's lexical class/module scopes `(span, qualified
    /// segments)` — the use-site prefix source for the in-source constant
    /// visibility gate, and the declared-class-name table.
    lexical: Vec<(rigor_parse::Span, Vec<String>)>,
    /// `class`/`module`/`class << self` spans — a `self` inside one types
    /// nominal (the reference's injected class/instance `self_type`).
    class_spans: Vec<(usize, usize)>,
    /// Exact spans of `class`/`module` HEADER constant paths (`Foo` in
    /// `class Foo`): declaration positions the reference pre-fills with the
    /// qualified `Singleton` (`ScopeIndexer` declared_types) → nominal.
    header_spans: std::collections::HashSet<(usize, usize)>,
    /// The WALKED node's evaluation context, pinned for its entire tier
    /// recursion: `(env, lexical prefix, walked-node span)`. The reference
    /// types each walked node — and every interior node its handler recurses
    /// into — under the ONE scope recorded AT that node (`scope_index[node]`,
    /// the statement-entry scope). So the env is materialized at the walked
    /// node's START (a binding established INSIDE a composite is invisible to
    /// the composite's own tier: `@f ||= begin; present = …; present - w; end`
    /// must type dynamic even though `present - w`'s own later walk sees the
    /// binding), and a scope/lexical entry whose span EQUALS the walked node
    /// is excluded (a class/module wrapper types its body under the ENCLOSING
    /// scope — the reference's `type_of_class_or_module`).
    eval_ctx: Option<(TypeEnv, Vec<String>, (usize, usize))>,
}

impl<'a> FileScanner<'a> {
    fn new(
        ast: &'a LoweredAst,
        typer: &'a Typer<'a>,
        index: &'a CoreIndex,
        interner: &'a mut Interner,
        prism_root: &Node<'_>,
    ) -> Self {
        // Map every arena node's exact span to its id. `Program` / `Statements`
        // wrappers are EXCLUDED: a wrapper often shares its span with its sole
        // child (or with a `def` that is the whole program), and the last-writer
        // insert would shadow the real expression node at that span (the same
        // wrapper-tie hazard `type_of.rs`'s `locate_node` guards against). No
        // Prism-side handler routes a wrapper through the span map — statements
        // recurse structurally.
        let mut span_to_arena = HashMap::new();
        for (id, node) in ast.iter() {
            if matches!(
                node,
                rigor_parse::Node::Program { .. } | rigor_parse::Node::Statements { .. }
            ) {
                continue;
            }
            let (s, e) = node.span();
            span_to_arena.insert((s, e), id);
        }
        // Every fresh-local-scope carrier (def / class / module bodies).
        let inner_scope_spans: Vec<(usize, usize)> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                rigor_parse::Node::Definition { span, .. }
                | rigor_parse::Node::ClassDef { span, .. }
                | rigor_parse::Node::ModuleDef { span, .. } => Some(*span),
                _ => None,
            })
            .collect();
        let lexical = rigor_infer::lexical_scopes(ast);
        let flow_writes = rigor_infer::collect_flow_writes(ast);
        let extra_taints = collect_prism_taints(prism_root);

        let mut scanner = FileScanner {
            ast,
            typer,
            index,
            interner,
            span_to_arena,
            toplevel_env: Vec::new(),
            inner_scope_spans,
            flow_writes,
            extra_taints,
            scope_envs: Vec::new(),
            lexical,
            class_spans: Vec::new(),
            header_spans: std::collections::HashSet::new(),
            eval_ctx: None,
        };
        scanner.build_all_scope_envs();
        scanner
    }

    /// Build the toplevel env and one env per fresh scope, each from ITS OWN
    /// body's straight-line writes with taint widening (see
    /// [`Self::build_scope_env`]). Order-independent: every scope env starts
    /// fresh (Ruby def/class/module bodies do not see enclosing locals).
    fn build_all_scope_envs(&mut self) {
        // Toplevel: the Program body, scope span = the whole file.
        if let rigor_parse::Node::Program { body, .. } = self.ast.get(self.ast.root()) {
            let body = body.clone();
            self.toplevel_env = self.build_scope_env((0, usize::MAX), &body);
        }
        // Fresh scopes: def / class / module / `class << self` bodies.
        let scopes: Vec<((usize, usize), Vec<rigor_parse::NodeId>)> = self
            .ast
            .iter()
            .filter_map(|(_, n)| match n {
                rigor_parse::Node::Definition { span, body, .. }
                | rigor_parse::Node::ClassDef { span, body, .. }
                | rigor_parse::Node::ModuleDef { span, body, .. } => {
                    Some((*span, body.clone()))
                }
                _ => None,
            })
            .collect();
        for (span, body) in scopes {
            let env = self.build_scope_env(span, &body);
            self.scope_envs.push((span, env));
        }
    }

    /// Walk the Prism tree, classify each non-excluded expression node, and
    /// return the aggregated tier histogram.
    fn scan(&mut self, root: &Node<'_>) -> FileResult {
        let mut collector = NodeCollector { nodes: Vec::new() };
        collector.visit(root);
        let nodes = collector.nodes;

        // Pre-index def / class spans (for method-body envs + self typing).
        for node in &nodes {
            match node {
                Node::ClassNode { .. } => {
                    self.class_spans.push(span_of(node));
                    let path = node.as_class_node().unwrap().constant_path();
                    self.header_spans.insert(span_of(&path));
                }
                Node::ModuleNode { .. } => {
                    self.class_spans.push(span_of(node));
                    let path = node.as_module_node().unwrap().constant_path();
                    self.header_spans.insert(span_of(&path));
                }
                Node::SingletonClassNode { .. } => {
                    self.class_spans.push(span_of(node));
                }
                _ => {}
            }
        }

        // Internal parity-audit hook (same pattern as `RIGOR_TIMING`): dump one
        // `start end tier` line per counted node to stderr, in walk (DFS
        // pre-order) position — the reference `NodeWalker` order, so a Ruby-side
        // dump lines up positionally for node-level comparison. Invisible by
        // default; audits run `--workers=1` so files do not interleave.
        let dump = std::env::var_os("RIGOR_COVERAGE_NODE_DUMP").is_some();
        let mut dump_lines = String::new();

        let mut result = FileResult::default();
        for node in &nodes {
            if is_non_expression(node) {
                continue;
            }
            // Pin the walked node's evaluation context (see `eval_ctx`).
            let span = span_of(node);
            let env = self.env_for_walked(span);
            let prefix = self.prefix_outside(span).to_vec();
            self.eval_ctx = Some((env, prefix, span));
            let tier = self.node_tier(node);
            result.counts[tier.idx()] += 1;
            result.total += 1;
            if dump {
                let (s, e) = span_of(node);
                dump_lines.push_str(&format!("{s} {e} {}\n", tier.json_key()));
            }
        }
        if dump {
            eprint!("{dump_lines}");
        }
        result
    }

    /// Build one scope's env (toplevel Program, or a fresh def/class/module
    /// body): bind only the UNCONDITIONAL top-level statement writes of the
    /// body, in source order. Two widening rules keep the flat env from ever
    /// out-claiming the reference's flow-sensitive scope:
    ///
    /// 1. A write nested in a branch / loop / block is NOT bound (the old
    ///    span-scan bound it unconditionally — a `x = "+inf" if x.blank?`
    ///    modifier-write must leave the later read param-dependent; caught on
    ///    mastodon feed.rb).
    /// 2. Any name that is ALSO written outside the straight line — a write
    ///    inside a branch/loop/block, or a compound `x += / ||= / &&=` write
    ///    anywhere in the scope — is NEVER BOUND at all. A straight-line
    ///    binding alone is stale for every read after the conditional
    ///    reassignment (`x = 5; x = p if c; x` must read dynamic_top, not
    ///    Constant[5]); the reference joins the branches and widens. Never-bind
    ///    is strictly stronger than bind-then-drop: dropping AFTER the bind
    ///    pass would still leak the stale value through a straight-line RHS
    ///    typed in between (`x = 5; x = p if c; y = x + 1` must not pin `y` to
    ///    6). Reads BEFORE the reassignment also widen — an under-claim, never
    ///    an over-claim (PR #33 review; node-level witnesses: mastodon user.rb
    ///    `sign_up_email_requires_approval?`'s `records` in a def, harness
    ///    fixture 34's toplevel `na` — the toplevel env needs the SAME
    ///    discipline, `Typer::build_toplevel_env` does not widen either).
    ///
    /// Writes inside a NESTED fresh scope (a def/class/module strictly within
    /// this one) neither bind nor taint — Ruby scoping makes them invisible to
    /// this scope's locals.
    fn build_scope_env(
        &mut self,
        scope_span: (usize, usize),
        body: &[rigor_parse::NodeId],
    ) -> BindEvents {
        // The straight-line write SPANS this env may bind (top-level statements
        // of the body, through `Statements` wrappers).
        let mut straight_line: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();
        for &stmt in body {
            self.collect_straight_line_writes(stmt, &mut straight_line);
        }

        // Rule 2's taint set, over the substrate flow-write collector — which
        // also records in-place MUTATOR calls on a bare-local receiver
        // (`statuses_to_query << id` invalidates the straight-line `[]`
        // binding; the reference's MutationWidening does the same; PR #33
        // node-level audit, mastodon report.rb `history!`). Span-keyed —
        // orphan-proof, same discipline as the dead-assignment collector.
        let mut tainted: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (wspan, name) in self.flow_writes.iter().chain(self.extra_taints.iter()) {
            if straight_line.contains(wspan) {
                continue; // this scope's own straight-line binding
            }
            if !(scope_span.0 <= wspan.0 && wspan.1 <= scope_span.1) {
                continue; // outside this scope entirely
            }
            if self.in_nested_scope(scope_span, *wspan) {
                continue; // a nested def/class/module's own local
            }
            tainted.insert(name.clone());
        }

        // Positional bind pass: a running map feeds each RHS, and every
        // accepted binding is also recorded as a `(stmt end, name, type)`
        // EVENT so lookups can hide bindings from nodes positioned before
        // (or inside) the binding statement.
        let mut env = TypeEnv::new();
        let mut events: BindEvents = Vec::new();
        for &stmt in body {
            self.bind_stmt_untainted(stmt, &tainted, &mut env, &mut events);
        }
        events
    }

    /// Whether `wspan` sits inside a fresh scope STRICTLY nested within
    /// `scope_span` (a def/class/module of its own).
    fn in_nested_scope(&self, scope_span: (usize, usize), wspan: (usize, usize)) -> bool {
        self.inner_scope_spans.iter().any(|&(s, e)| {
            (s, e) != scope_span
                && scope_span.0 <= s
                && e <= scope_span.1
                && s <= wspan.0
                && wspan.1 <= e
        })
    }

    /// The straight-line bind pass, restricted to names outside `tainted`: a
    /// tainted name is never inserted, so no RHS in the pass can read its stale
    /// value. Recurses through `Statements` wrappers; any other statement has
    /// no binding effect.
    fn bind_stmt_untainted(
        &mut self,
        id: rigor_parse::NodeId,
        tainted: &std::collections::HashSet<String>,
        env: &mut TypeEnv,
        events: &mut BindEvents,
    ) {
        match self.ast.get(id) {
            rigor_parse::Node::LocalVariableWrite { name, value, span, .. } => {
                let (name, value, stmt_end) = (name.clone(), *value, span.1);
                // Every straight-line write emits an EVENT: a typed binding
                // when the value is trusted, else an UNTYPED INVALIDATION —
                // the write DID rebind the name at runtime, so a declined
                // bind must still hide any earlier binding from later reads
                // (`messages = errs.map(..); messages = messages.count == 1 ?
                // … : …; messages` — the ternary rebind must widen the read,
                // not leak the map result; PR #33 re-review, gitlab
                // api/helpers.rb).
                let ty = if tainted.contains(&name) {
                    self.interner.untyped()
                } else {
                    self.trusted_value_type(value, env)
                };
                env.insert(name.clone(), ty);
                events.push((stmt_end, name, ty));
            }
            rigor_parse::Node::Statements { body, .. } => {
                for s in body.clone() {
                    self.bind_stmt_untainted(s, tainted, env, events);
                }
            }
            _ => {}
        }
    }

    /// The bind-worthy type of a straight-line write's rvalue, or `untyped`
    /// when the Typer's answer is not trusted for a BINDING:
    ///
    /// - COMPOSITE values (if/case/loop/begin/logical/statements/other): the
    ///   arena Typer's composite arms type to the LAST-child constant, which
    ///   for an interpolated-symbol branch is a factually wrong pin
    ///   (`x = if c; :"a#{b}_z"; …` ⇒ Constant["_z"] — PR #33 re-review,
    ///   gitlab active_record.rb prometheus_key).
    /// - `X.new` on a bare-constant receiver that the reference-visible rules
    ///   do not resolve, or a CORE class without an RBS-declared `new`
    ///   (`Integer.new` is a NoMethodError): the Typer's `.new` interception
    ///   is unconditional (`Group.new` types `Group` with no Group anywhere)
    ///   — Ruby's actual semantics are the reference's PERMANENT behavior
    ///   (gitlab visibility_level.rb `subgroup`, duplicate_job.rb
    ///   `my_cookie`).
    /// - A CERTAIN nilable RBS return (`String#byteslice -> String?`) binds
    ///   the honest `C | nil` union, exactly as the reference scope does
    ///   (fixtures 27/28: the read stays nominal — worst member — while a
    ///   dispatch on the local declines to dynamic).
    fn trusted_value_type(&mut self, value: rigor_parse::NodeId, env: &TypeEnv) -> TypeId {
        if matches!(
            self.ast.get(value),
            rigor_parse::Node::If { .. }
                | rigor_parse::Node::Case { .. }
                | rigor_parse::Node::Loop { .. }
                | rigor_parse::Node::BeginRescue { .. }
                | rigor_parse::Node::Logical { .. }
                | rigor_parse::Node::Statements { .. }
                | rigor_parse::Node::Other { .. }
        ) {
            return self.interner.untyped();
        }

        if let rigor_parse::Node::Call { receiver: Some(r), method, .. } = self.ast.get(value) {
            let (r, method) = (*r, method.clone());

            // The `.new` receiver gate (mirrors the Prism-side CallNode arm).
            if method == "new" {
                if let rigor_parse::Node::ConstantRead { name, span, .. } = self.ast.get(r) {
                    let (name, rspan) = (name.clone(), *span);
                    if !self.new_receiver_allowed_arena(&name, rspan, r, env) {
                        return self.interner.untyped();
                    }
                }
            }

            let mut ty = self.typer.type_of(self.ast, value, env, self.interner);
            let rty = self.typer.type_of(self.ast, r, env, self.interner);
            if let Some(cls) = self.index.class_name_of(self.interner, rty) {
                if let Some((_, true)) = self.index.method_return_nilable(cls, &method) {
                    let nil = self.interner.nil();
                    ty = rigor_types::Algebra::join(self.interner, ty, nil);
                }
            }
            return ty;
        }

        self.typer.type_of(self.ast, value, env, self.interner)
    }

    /// Whether `.new` on the bare-constant arena receiver `name` resolves
    /// under the reference-visible rules: an in-source lexically-visible
    /// class, or a core class whose RBS actually declares a singleton `new`.
    fn new_receiver_allowed_arena(
        &mut self,
        name: &str,
        rspan: (usize, usize),
        r: rigor_parse::NodeId,
        env: &TypeEnv,
    ) -> bool {
        if self.declared_visible(name, rspan) {
            return true;
        }
        let rty = self.typer.type_of(self.ast, r, env, self.interner);
        if let Type::Singleton(cid) = self.interner.get(rty) {
            if let Some(cn) = self.index.class_name_for_id(*cid) {
                return self.index.class_has_singleton_method(cn, "new");
            }
        }
        false
    }

    /// Collect the SPANS of the plain local writes a straight-line bind pass
    /// reaches: top-level statements of the scope body, through `Statements`
    /// wrappers. Mirrors [`Self::bind_stmt_untainted`]'s traversal exactly.
    fn collect_straight_line_writes(
        &self,
        id: rigor_parse::NodeId,
        out: &mut std::collections::HashSet<(usize, usize)>,
    ) {
        match self.ast.get(id) {
            rigor_parse::Node::LocalVariableWrite { span, .. } => {
                out.insert(*span);
            }
            rigor_parse::Node::Statements { body, .. } => {
                for &s in body {
                    self.collect_straight_line_writes(s, out);
                }
            }
            _ => {}
        }
    }

    /// The env visible AT a walked node: the innermost enclosing fresh scope
    /// (excluding a scope whose span IS the node — a def/class/module wrapper
    /// is typed under the scope it is a statement in), materialized at the
    /// node's start (bindings whose statement ends before it; last wins).
    fn env_for_walked(&self, span: (usize, usize)) -> TypeEnv {
        let mut best: Option<&((usize, usize), BindEvents)> = None;
        for entry in &self.scope_envs {
            let (ds, de) = entry.0;
            if (ds, de) != span && ds <= span.0 && span.1 <= de {
                match best {
                    None => best = Some(entry),
                    Some(b) if (de - ds) < (b.0 .1 - b.0 .0) => best = Some(entry),
                    _ => {}
                }
            }
        }
        let events = best.map(|e| &e.1).unwrap_or(&self.toplevel_env);
        materialize_env(events, span.0)
    }

    /// The use-site lexical prefix: the innermost `lexical` scope containing
    /// `span`, or empty at toplevel. Mirrors the Typer's `enclosing_prefix`.
    fn prefix_at(&self, span: (usize, usize)) -> &[String] {
        let mut best: Option<&(rigor_parse::Span, Vec<String>)> = None;
        for sc in &self.lexical {
            if sc.0 .0 <= span.0 && span.1 <= sc.0 .1 {
                match best {
                    None => best = Some(sc),
                    Some(b) if (sc.0 .1 - sc.0 .0) < (b.0 .1 - b.0 .0) => best = Some(sc),
                    _ => {}
                }
            }
        }
        best.map(|b| b.1.as_slice()).unwrap_or(&[])
    }

    /// The lexical prefix OUTSIDE a class/module wrapper whose span is exactly
    /// `span` (skips the wrapper's own `lexical` entry).
    fn prefix_outside(&self, span: (usize, usize)) -> &[String] {
        let mut best: Option<&(rigor_parse::Span, Vec<String>)> = None;
        for sc in &self.lexical {
            if sc.0 != span && sc.0 .0 <= span.0 && span.1 <= sc.0 .1 {
                match best {
                    None => best = Some(sc),
                    Some(b) if (sc.0 .1 - sc.0 .0) < (b.0 .1 - b.0 .0) => best = Some(sc),
                    _ => {}
                }
            }
        }
        best.map(|b| b.1.as_slice()).unwrap_or(&[])
    }

    /// The tier of a class/module/singleton-class WRAPPER node: the body's
    /// value under the walked node's pinned `eval_ctx` — which, for a wrapper,
    /// resolves OUTSIDE it (own-span exclusion in `env_for_walked` /
    /// `prefix_outside`), matching the reference's `type_of_class_or_module`
    /// ("the body is typed in the surrounding scope"; PR #33 re-review
    /// BLOCKING-2: `module M; x = 5; x; end`'s wrapper must not read the
    /// body-scope binding). An empty body is `Constant[nil]`.
    fn wrapper_tier(&mut self, body: Option<Node<'_>>) -> Tier {
        match body {
            Some(body) => self.value_tier(body),
            None => Tier::Constant,
        }
    }

    /// Ruby lexical constant visibility for an in-source class/module: `name`
    /// read at a site with lexical prefix `P` resolves iff some declared
    /// class/module's qualified name equals `P[0..k] + [name]` for some k
    /// (innermost-out walk; ancestor lookup not modeled — a conservative miss).
    /// The declared-name table is the `lexical` scopes' own qualified names.
    fn declared_visible(&self, name: &str, span: (usize, usize)) -> bool {
        // The WALKED node's pinned lexical prefix (a wrapper resolves from
        // OUTSIDE itself: a reopened `module M`'s wrapper must not see an
        // `M::Foo` declared in the first block as a bare-`Foo` hit).
        let prefix: &[String] = match &self.eval_ctx {
            Some((_, p, _)) => p.as_slice(),
            None => self.prefix_at(span),
        };
        for k in (0..=prefix.len()).rev() {
            let candidate_len = k + 1;
            if self.lexical.iter().any(|(_, segs)| {
                segs.len() == candidate_len
                    && segs[..k] == prefix[..k]
                    && segs[k] == name
            }) {
                return true;
            }
        }
        false
    }

    /// Classify one Prism node into a tier, reproducing `ExpressionTyper#type_of`
    /// (`PRISM_DISPATCH`).
    fn node_tier(&mut self, node: &Node<'_>) -> Tier {
        match node {
            // ---- Value literals → Constant --------------------------------
            Node::IntegerNode { .. }
            | Node::FloatNode { .. }
            | Node::ImaginaryNode { .. }
            | Node::RationalNode { .. }
            | Node::SymbolNode { .. }
            | Node::StringNode { .. }
            | Node::TrueNode { .. }
            | Node::FalseNode { .. }
            | Node::NilNode { .. }
            | Node::RegularExpressionNode { .. } => Tier::Constant,

            // Backtick command output is a runtime String, never a pinned
            // value (reference `type_of_xstring` → Nominal[String]).
            Node::XStringNode { .. } | Node::InterpolatedXStringNode { .. } => Tier::Nominal,
            // `__FILE__` → `non-empty-string` = Difference[String, ""] —
            // classify(Difference) follows its BASE → nominal (PR #33
            // re-review gitlab audit: Constant here was an over-claim).
            Node::SourceFileNode { .. } => Tier::Nominal,
            // `__LINE__` → `positive-int` = IntegerRange[1, +inf) → shaped.
            Node::SourceLineNode { .. } => Tier::Shaped,

            // ---- Definitions ----------------------------------------------
            // `def foo` evaluates to `Constant[:foo]`.
            Node::DefNode { .. } => Tier::Constant,
            // `alias` / `undef` / `BEGIN{}` all evaluate to nil.
            Node::AliasMethodNode { .. }
            | Node::AliasGlobalVariableNode { .. }
            | Node::UndefNode { .. }
            | Node::PostExecutionNode { .. } => Tier::Constant,
            // `expr => pattern` evaluates to nil.
            Node::MatchRequiredNode { .. } => Tier::Constant,

            // ---- Loops evaluate to Constant[nil] --------------------------
            Node::WhileNode { .. } | Node::UntilNode { .. } | Node::ForNode { .. } => Tier::Constant,

            // ---- Jumps → Bot ----------------------------------------------
            Node::ReturnNode { .. }
            | Node::BreakNode { .. }
            | Node::NextNode { .. }
            | Node::RetryNode { .. }
            | Node::RedoNode { .. } => Tier::Bot,

            // ---- Fixed nominal --------------------------------------------
            Node::LambdaNode { .. } => Tier::Nominal, // Nominal[Proc]
            Node::InterpolatedStringNode { .. } => Tier::Nominal, // Nominal[String]
            Node::InterpolatedSymbolNode { .. } => Tier::Nominal, // Nominal[Symbol]
            Node::InterpolatedRegularExpressionNode { .. } => Tier::Nominal, // Nominal[Regexp]
            // `defined?` and back-references carry `String | nil` → nominal.
            Node::DefinedNode { .. }
            | Node::NumberedReferenceReadNode { .. }
            | Node::BackReferenceReadNode { .. } => Tier::Nominal,

            // `expr in pattern` → `true | false` → union of constants → constant.
            Node::MatchPredicateNode { .. } => Tier::Constant,

            // ---- Fixed dynamic_top ----------------------------------------
            Node::BlockNode { .. }
            | Node::YieldNode { .. }
            | Node::SuperNode { .. }
            | Node::ForwardingSuperNode { .. }
            | Node::EmbeddedVariableNode { .. }
            | Node::MatchWriteNode { .. } => Tier::DynamicTop,

            // ---- Assignment writes → type of the rvalue -------------------
            Node::LocalVariableWriteNode { .. } => {
                self.value_tier(node.as_local_variable_write_node().unwrap().value())
            }
            Node::LocalVariableOperatorWriteNode { .. } => {
                self.value_tier(node.as_local_variable_operator_write_node().unwrap().value())
            }
            Node::LocalVariableOrWriteNode { .. } => {
                self.value_tier(node.as_local_variable_or_write_node().unwrap().value())
            }
            Node::LocalVariableAndWriteNode { .. } => {
                self.value_tier(node.as_local_variable_and_write_node().unwrap().value())
            }
            Node::InstanceVariableWriteNode { .. } => {
                self.value_tier(node.as_instance_variable_write_node().unwrap().value())
            }
            Node::InstanceVariableOperatorWriteNode { .. } => self
                .value_tier(node.as_instance_variable_operator_write_node().unwrap().value()),
            Node::InstanceVariableOrWriteNode { .. } => {
                self.value_tier(node.as_instance_variable_or_write_node().unwrap().value())
            }
            Node::InstanceVariableAndWriteNode { .. } => {
                self.value_tier(node.as_instance_variable_and_write_node().unwrap().value())
            }
            Node::ConstantWriteNode { .. } => {
                self.value_tier(node.as_constant_write_node().unwrap().value())
            }
            Node::ConstantOperatorWriteNode { .. } => {
                self.value_tier(node.as_constant_operator_write_node().unwrap().value())
            }
            Node::ConstantOrWriteNode { .. } => {
                self.value_tier(node.as_constant_or_write_node().unwrap().value())
            }
            Node::ConstantAndWriteNode { .. } => {
                self.value_tier(node.as_constant_and_write_node().unwrap().value())
            }
            Node::ConstantPathWriteNode { .. } => {
                self.value_tier(node.as_constant_path_write_node().unwrap().value())
            }
            Node::ConstantPathOperatorWriteNode { .. } => {
                self.value_tier(node.as_constant_path_operator_write_node().unwrap().value())
            }
            Node::ConstantPathOrWriteNode { .. } => {
                self.value_tier(node.as_constant_path_or_write_node().unwrap().value())
            }
            Node::ConstantPathAndWriteNode { .. } => {
                self.value_tier(node.as_constant_path_and_write_node().unwrap().value())
            }
            Node::GlobalVariableWriteNode { .. } => {
                self.value_tier(node.as_global_variable_write_node().unwrap().value())
            }
            Node::GlobalVariableOperatorWriteNode { .. } => {
                self.value_tier(node.as_global_variable_operator_write_node().unwrap().value())
            }
            Node::GlobalVariableOrWriteNode { .. } => {
                self.value_tier(node.as_global_variable_or_write_node().unwrap().value())
            }
            Node::GlobalVariableAndWriteNode { .. } => {
                self.value_tier(node.as_global_variable_and_write_node().unwrap().value())
            }
            Node::ClassVariableWriteNode { .. } => {
                self.value_tier(node.as_class_variable_write_node().unwrap().value())
            }
            Node::ClassVariableOperatorWriteNode { .. } => {
                self.value_tier(node.as_class_variable_operator_write_node().unwrap().value())
            }
            Node::ClassVariableOrWriteNode { .. } => {
                self.value_tier(node.as_class_variable_or_write_node().unwrap().value())
            }
            Node::ClassVariableAndWriteNode { .. } => {
                self.value_tier(node.as_class_variable_and_write_node().unwrap().value())
            }
            Node::IndexOperatorWriteNode { .. } => {
                self.value_tier(node.as_index_operator_write_node().unwrap().value())
            }
            Node::IndexOrWriteNode { .. } => {
                self.value_tier(node.as_index_or_write_node().unwrap().value())
            }
            Node::IndexAndWriteNode { .. } => {
                self.value_tier(node.as_index_and_write_node().unwrap().value())
            }
            Node::MultiWriteNode { .. } => {
                self.value_tier(node.as_multi_write_node().unwrap().value())
            }
            Node::CallAndWriteNode { .. }
            | Node::CallOrWriteNode { .. }
            | Node::CallOperatorWriteNode { .. } => {
                // Attribute-assign writes (`a.b = c`); reference routes the
                // `.value` rvalue (also `type_of_assignment_write`-shaped).
                // Not in PRISM_DISPATCH explicitly → fall through to arena.
                self.arena_tier(node)
            }

            // ---- Definitions that recurse into their body (under the
            // ENCLOSING scope's context — see `wrapper_tier`) ----------------
            Node::ClassNode { .. } => {
                let body = node.as_class_node().unwrap().body();
                self.wrapper_tier(body)
            }
            Node::ModuleNode { .. } => {
                let body = node.as_module_node().unwrap().body();
                self.wrapper_tier(body)
            }
            Node::SingletonClassNode { .. } => {
                let body = node.as_singleton_class_node().unwrap().body();
                self.wrapper_tier(body)
            }

            // ---- Control-flow structural composers ------------------------
            Node::StatementsNode { .. } | Node::ProgramNode { .. } => self.statements_tier(node),
            Node::ParenthesesNode { .. } => match node.as_parentheses_node().unwrap().body() {
                None => Tier::Constant,
                Some(b) => self.value_tier(b),
            },
            Node::IfNode { .. } => {
                let n = node.as_if_node().unwrap();
                let then_t = self.opt_statements_tier(n.statements());
                let else_t = match n.subsequent() {
                    Some(s) => self.value_tier(s),
                    None => Tier::Constant, // implicit nil branch
                };
                self.elide_or_union(&n.predicate(), then_t, else_t)
            }
            Node::UnlessNode { .. } => {
                let n = node.as_unless_node().unwrap();
                let then_t = self.opt_statements_tier(n.statements());
                let else_t = match n.else_clause() {
                    Some(e) => self.value_tier(e.as_node()),
                    None => Tier::Constant,
                };
                // Inverted: a truthy predicate selects the else branch.
                self.elide_or_union(&n.predicate(), else_t, then_t)
            }
            Node::ElseNode { .. } => self.opt_statements_tier(node.as_else_node().unwrap().statements()),
            Node::AndNode { .. } => {
                let n = node.as_and_node().unwrap();
                self.and_or_tier(&n.left(), &n.right(), true)
            }
            Node::OrNode { .. } => {
                let n = node.as_or_node().unwrap();
                self.and_or_tier(&n.left(), &n.right(), false)
            }
            Node::CaseNode { .. } => {
                let n = node.as_case_node().unwrap();
                let mut tiers: Vec<Tier> = n
                    .conditions()
                    .iter()
                    .map(|c| self.value_tier(c))
                    .collect();
                tiers.push(match n.else_clause() {
                    Some(e) => self.value_tier(e.as_node()),
                    None => Tier::Constant,
                });
                union_tiers(tiers.into_iter())
            }
            Node::CaseMatchNode { .. } => {
                let n = node.as_case_match_node().unwrap();
                let mut tiers: Vec<Tier> = n
                    .conditions()
                    .iter()
                    .map(|c| self.value_tier(c))
                    .collect();
                tiers.push(match n.else_clause() {
                    Some(e) => self.value_tier(e.as_node()),
                    None => Tier::Constant,
                });
                union_tiers(tiers.into_iter())
            }
            Node::WhenNode { .. } => {
                self.opt_statements_tier(node.as_when_node().unwrap().statements())
            }
            Node::InNode { .. } => self.opt_statements_tier(node.as_in_node().unwrap().statements()),
            Node::BeginNode { .. } => self.begin_tier(&node.as_begin_node().unwrap()),
            Node::RescueNode { .. } => {
                self.opt_statements_tier(node.as_rescue_node().unwrap().statements())
            }
            Node::RescueModifierNode { .. } => {
                let n = node.as_rescue_modifier_node().unwrap();
                let a = self.value_tier(n.expression());
                let b = self.value_tier(n.rescue_expression());
                union_tiers([a, b].into_iter())
            }
            Node::EnsureNode { .. } => {
                self.opt_statements_tier(node.as_ensure_node().unwrap().statements())
            }

            // ---- `self` --------------------------------------------------
            Node::SelfNode { .. } => {
                // Judged at the WALKED node's position, excluding the walked
                // node's own class span — a wrapper's body-`self` is the
                // OUTER scope's self (`module M; self; end` at toplevel →
                // main → dynamic).
                let judge = match &self.eval_ctx {
                    Some((_, _, wspan)) => *wspan,
                    None => span_of(node),
                };
                let inside = self
                    .class_spans
                    .iter()
                    .any(|&(s, e)| (s, e) != judge && s <= judge.0 && judge.1 <= e);
                if inside { Tier::Nominal } else { Tier::DynamicTop }
            }

            // ---- Constant reads: resolve in-source / core → Singleton -----
            Node::ConstantReadNode { .. } => {
                let name = node.as_constant_read_node().unwrap().name().as_slice();
                let name = String::from_utf8_lossy(name).into_owned();
                self.constant_name_tier(&name, node)
            }
            Node::ConstantPathNode { .. } => {
                // A qualified constant path (`Foo::Bar`). Resolve via the arena
                // typer (which applies rigor-rs's own qualified-class gate).
                self.arena_tier(node)
            }

            // ---- Calls: the arena typer, with one Ruby-scoping guard -------
            Node::CallNode { .. } => {
                // `N.new` on a BARE constant receiver: the arena Typer's `.new`
                // interception resolves in-source classes by SHORT name, which
                // out-claims Ruby's lexical constant lookup (a toplevel
                // `Inner.new` with only `Outer::Inner` declared is a NameError,
                // and the reference types it Dynamic — its PERMANENT semantics,
                // not a gap; PR #33 node-level audit, fixtures 68/69). Accept
                // the arena result only when the receiver constant itself
                // resolves under the reference-visible rules.
                let call = node.as_call_node().unwrap();
                if call.name().as_slice() == b"new" {
                    if let Some(recv) = call.receiver() {
                        if let Some(cr) = recv.as_constant_read_node() {
                            let name =
                                String::from_utf8_lossy(cr.name().as_slice()).into_owned();
                            if self.constant_name_tier(&name, &recv) != Tier::Nominal {
                                return Tier::DynamicTop;
                            }
                            // A CORE class must actually declare a singleton
                            // `new` in RBS — `Integer.new` is a runtime
                            // NoMethodError the reference types Dynamic
                            // (gitlab template_parser/ast.rb). An in-source
                            // visible class inherits `Class#new` — allowed.
                            if !self.declared_visible(&name, span_of(&recv)) {
                                let sty = self.arena_type(&recv);
                                if let Some(sty) = sty {
                                    if let Type::Singleton(cid) = self.interner.get(sty) {
                                        let has_new = self
                                            .index
                                            .class_name_for_id(*cid)
                                            .is_some_and(|cn| {
                                                self.index.class_has_singleton_method(cn, "new")
                                            });
                                        if !has_new {
                                            return Tier::DynamicTop;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                self.arena_tier(node)
            }

            // ---- Value-producing leaves → the arena typer ------------------
            Node::ArrayNode { .. }
            | Node::HashNode { .. }
            | Node::KeywordHashNode { .. }
            | Node::RangeNode { .. }
            | Node::LocalVariableReadNode { .. }
            | Node::ItLocalVariableReadNode { .. }
            | Node::InstanceVariableReadNode { .. }
            | Node::ClassVariableReadNode { .. }
            | Node::GlobalVariableReadNode { .. } => self.arena_tier(node),

            // `"#{expr}"`'s embedded-statements part: the statements' value
            // (reference `type_of_embedded_statements`). STRUCTURAL — routing
            // it through the span map collided with same-span arena nodes and
            // minted Nominal[String] for `"#{dynamic_call}"` parts (PR #33
            // re-review gitlab audit, 23 nodes).
            Node::EmbeddedStatementsNode { .. } => {
                match node.as_embedded_statements_node().unwrap().statements() {
                    Some(s) => self.statements_tier(&s.as_node()),
                    None => Tier::Constant,
                }
            }

            // ---- Non-value positions & everything else → dynamic_top -------
            _ => Tier::DynamicTop,
        }
    }

    /// Type a Prism child node (recursing into the structural composers) — used
    /// for write rvalues, branch bodies, and the like.
    fn value_tier(&mut self, node: Node<'_>) -> Tier {
        self.node_tier(&node)
    }

    /// The type of a body `StatementsNode`: its last statement's tier, or
    /// `Constant[nil]` when empty (reference `statements_type_for`).
    fn statements_tier(&mut self, node: &Node<'_>) -> Tier {
        let body = match node {
            Node::StatementsNode { .. } => node.as_statements_node().unwrap().body(),
            Node::ProgramNode { .. } => {
                return self.statements_tier(&node.as_program_node().unwrap().statements().as_node())
            }
            _ => return self.node_tier(node),
        };
        match body.iter().last() {
            Some(last) => self.value_tier(last),
            None => Tier::Constant,
        }
    }

    fn opt_statements_tier(&mut self, stmts: Option<ruby_prism::StatementsNode<'_>>) -> Tier {
        match stmts {
            Some(s) => self.statements_tier(&s.as_node()),
            None => Tier::Constant,
        }
    }

    /// `begin` types to the union of every value-producing branch: the else
    /// clause (or the body when there is none), plus each rescue body.
    fn begin_tier(&mut self, n: &ruby_prism::BeginNode<'_>) -> Tier {
        let mut tiers = Vec::new();
        let primary = if let Some(e) = n.else_clause() {
            self.value_tier(e.as_node())
        } else if let Some(s) = n.statements() {
            self.statements_tier(&s.as_node())
        } else {
            Tier::Constant
        };
        tiers.push(primary);
        let mut cur = n.rescue_clause();
        while let Some(r) = cur {
            tiers.push(self.opt_statements_tier(r.statements()));
            cur = r.subsequent();
        }
        union_tiers(tiers.into_iter())
    }

    /// `a && b` / `a || b`: when the left operand folds to a constant, the
    /// short-circuit is known and one operand flows through; otherwise the union
    /// of both operands (a documented tier-level simplification of the
    /// reference's narrowed union).
    fn and_or_tier(&mut self, left: &Node<'_>, right: &Node<'_>, is_and: bool) -> Tier {
        match self.const_polarity(left) {
            Some(truthy) => {
                // `a && b` → b when a truthy, a when a falsey; `a || b` inverted.
                if is_and == truthy {
                    self.node_tier(right)
                } else {
                    self.node_tier(left)
                }
            }
            None => {
                let l = self.node_tier(left);
                let r = self.node_tier(right);
                union_tiers([l, r].into_iter())
            }
        }
    }

    /// Route a predicate through branch elision (reference `elide_or_union`):
    /// a constant-truthy predicate selects `live_truthy`, constant-falsey selects
    /// `live_falsey`, otherwise the union of both.
    fn elide_or_union(&mut self, predicate: &Node<'_>, live_truthy: Tier, live_falsey: Tier) -> Tier {
        match self.const_polarity(predicate) {
            Some(true) => live_truthy,
            Some(false) => live_falsey,
            None => union_tiers([live_truthy, live_falsey].into_iter()),
        }
    }

    /// `Some(truthy)` when `node` folds to a constant with known Ruby truthiness,
    /// else `None`. Literals resolve directly; other expressions consult the
    /// arena typer's constant fold.
    fn const_polarity(&mut self, node: &Node<'_>) -> Option<bool> {
        match node {
            Node::TrueNode { .. } => return Some(true),
            Node::FalseNode { .. } | Node::NilNode { .. } => return Some(false),
            Node::IntegerNode { .. }
            | Node::FloatNode { .. }
            | Node::SymbolNode { .. }
            | Node::StringNode { .. } => return Some(true),
            _ => {}
        }
        // Fall back to the arena fold: a `Type::Constant` pins truthiness.
        let ty = self.arena_type(node)?;
        match self.interner.get(ty) {
            Type::Constant(Scalar::Nil) | Type::Constant(Scalar::Bool(false)) => Some(false),
            Type::Constant(_) => Some(true),
            _ => None,
        }
    }

    /// Resolve a bare constant name: an in-source or core class object types to
    /// `Singleton` → nominal (reference `resolve_constant_name`). Otherwise defer
    /// to the arena typer (rigor-rs's own gate).
    fn constant_name_tier(&mut self, name: &str, node: &Node<'_>) -> Tier {
        let span = span_of(node);
        // Declaration position (`Foo` in `class Foo`): the reference's
        // `ScopeIndexer` declared_types pre-fill → Singleton → nominal.
        if self.header_spans.contains(&span) {
            return Tier::Nominal;
        }
        // An in-source class LEXICALLY VISIBLE from this use site → the
        // reference's discovered_classes resolution → Singleton → nominal.
        // NOTE: a bare `SourceIndex::knows_class(name)` gate is WRONG here —
        // it registers nested classes under their short name, over-claiming a
        // toplevel `Inner` read the reference leaves unresolved (PR #33
        // node-level audit, fixture 69).
        if self.declared_visible(name, span) {
            return Tier::Nominal;
        }
        // Core-RBS classes and everything else: the arena Typer's own gated
        // ConstantRead arm (toplevel/qualified registry + shadow gate).
        self.arena_tier(node)
    }

    /// The arena `TypeId` for a Prism node with an exact-span arena counterpart,
    /// typed under the node's enclosing-scope env. `None` when no arena node has
    /// that exact span (a Prism node with no 1:1 lowered form).
    fn arena_type(&mut self, node: &Node<'_>) -> Option<TypeId> {
        let span = span_of(node);
        let id = *self.span_to_arena.get(&span)?;
        // Kind-compatibility gate: distinct constructs CAN share an exact span
        // (a `where(domain:)` shorthand keyword-hash lowers to an arena
        // `HashLit` spanning exactly the pair — the same span as the Prism
        // value CallNode; PR #33 node-level audit, mastodon account.rb). A
        // span hit on an arena node of the WRONG kind must decline to the
        // Dynamic fallback, not adopt the collided node's type.
        if !arena_kind_compatible(node, self.ast.get(id)) {
            return None;
        }
        // The WALKED node's pinned env (see `eval_ctx`): interior nodes of a
        // composite are typed under the composite's entry scope, exactly as
        // the reference's single-scope `type_of` recursion does.
        let env = match &self.eval_ctx {
            Some((env, _, _)) => env.clone(),
            None => self.env_for_walked(span),
        };
        Some(self.typer.type_of(self.ast, id, &env, self.interner))
    }

    /// The tier of a value-leaf Prism node via the arena typer; `dynamic_top`
    /// when there is no exact-span arena node (the reference's `Dynamic[Top]`
    /// fallback).
    fn arena_tier(&mut self, node: &Node<'_>) -> Tier {
        match self.arena_type(node) {
            Some(ty) => classify_type(self.interner, ty),
            None => Tier::DynamicTop,
        }
    }
}

/// Materialize the bindings visible at byte position `pos` from a scope's
/// positional binding events: every event whose statement ends at or before
/// `pos`, last write wins.
fn materialize_env(events: &[(usize, String, TypeId)], pos: usize) -> TypeEnv {
    let mut env = TypeEnv::new();
    for (end, name, ty) in events {
        if *end <= pos {
            env.insert(name.clone(), *ty);
        }
    }
    env
}

/// The `(start, end)` byte span of a Prism node.
fn span_of(node: &Node<'_>) -> (usize, usize) {
    let loc = node.location();
    (loc.start_offset(), loc.end_offset())
}

/// Whether an exact-span arena hit is the SAME construct as the Prism node
/// being typed. Strict for the value-leaf kinds the scanner routes through the
/// span map; permissive for everything else (predicate probes on structural
/// nodes, where the arena kinds are many-to-one and a collision cannot mint a
/// wrong precise tier — an incompatible node types `Dynamic[top]` anyway).
fn arena_kind_compatible(prism: &Node<'_>, arena: &rigor_parse::Node) -> bool {
    use rigor_parse::Node as A;
    match prism {
        Node::CallNode { .. } => matches!(arena, A::Call { .. }),
        Node::LocalVariableReadNode { .. } | Node::ItLocalVariableReadNode { .. } => {
            matches!(arena, A::LocalVariableRead { .. })
        }
        Node::InstanceVariableReadNode { .. }
        | Node::ClassVariableReadNode { .. }
        | Node::GlobalVariableReadNode { .. } => matches!(arena, A::VariableRead { .. }),
        Node::ArrayNode { .. } => matches!(arena, A::ArrayLit { .. }),
        Node::HashNode { .. } => matches!(arena, A::HashLit { .. }),
        Node::RangeNode { .. } => matches!(arena, A::Range { .. }),
        Node::ConstantReadNode { .. } | Node::ConstantPathNode { .. } => {
            matches!(arena, A::ConstantRead { .. })
        }
        _ => true,
    }
}

/// The reference `PrecisionScanner::NON_EXPRESSION_NODE_TYPES` — nodes that do
/// not denote a value-producing expression, excluded from both numerator and
/// denominator.
fn is_non_expression(node: &Node<'_>) -> bool {
    matches!(
        node,
        Node::ProgramNode { .. }
            | Node::StatementsNode { .. }
            | Node::ArgumentsNode { .. }
            | Node::BlockArgumentNode { .. }
            | Node::ParametersNode { .. }
            | Node::BlockParametersNode { .. }
            | Node::NumberedParametersNode { .. }
            | Node::ItParametersNode { .. }
            | Node::KeywordHashNode { .. }
            | Node::RequiredParameterNode { .. }
            | Node::OptionalParameterNode { .. }
            | Node::RestParameterNode { .. }
            | Node::KeywordRestParameterNode { .. }
            | Node::BlockParameterNode { .. }
            | Node::RequiredKeywordParameterNode { .. }
            | Node::OptionalKeywordParameterNode { .. }
            | Node::ForwardingParameterNode { .. }
            | Node::NoKeywordsParameterNode { .. }
            | Node::ImplicitRestNode { .. }
            | Node::AssocNode { .. }
            | Node::AssocSplatNode { .. }
            | Node::WhenNode { .. }
            | Node::InNode { .. }
            | Node::ElseNode { .. }
            | Node::EnsureNode { .. }
            | Node::RescueNode { .. }
    )
}

/// Classify a rigor-rs `TypeId` into a precision tier (reference
/// `PrecisionScanner#classify`). Union → worst (max-rank) member; intersection →
/// best (min-rank); difference → its base.
fn classify_type(interner: &Interner, id: TypeId) -> Tier {
    match interner.get(id) {
        Type::Bottom => Tier::Bot,
        Type::Top => Tier::Top,
        Type::Constant(_) => Tier::Constant,
        Type::Nominal { .. } | Type::Singleton(_) => Tier::Nominal,
        Type::Tuple(_) | Type::HashShape(_) | Type::IntegerRange { .. } | Type::App { .. } => {
            Tier::Shaped
        }
        Type::Refined { .. } => Tier::Refined,
        Type::Dynamic(facet) => {
            if matches!(interner.get(*facet), Type::Top) {
                Tier::DynamicTop
            } else {
                Tier::DynamicSpecific
            }
        }
        Type::Union(members) => members
            .iter()
            .map(|m| classify_type(interner, *m))
            .max_by_key(|t| t.idx())
            .unwrap_or(Tier::DynamicTop),
        Type::Intersection(members) => members
            .iter()
            .map(|m| classify_type(interner, *m))
            .min_by_key(|t| t.idx())
            .unwrap_or(Tier::DynamicTop),
        Type::Difference { base, .. } => classify_type(interner, *base),
        // DataInstance / Void / SelfType / Instance / ClassType / Complement
        // are result-markers with no reference tier → dynamic_top (the else arm).
        _ => Tier::DynamicTop,
    }
}

/// The tier of a `Type::Combinator.union(members)` at the tier level: `Bot`
/// members are absorbed (`Constant | Bot == Constant`); an empty (all-Bot)
/// union is `Bot`; otherwise the worst (max-rank) surviving member.
fn union_tiers(tiers: impl Iterator<Item = Tier>) -> Tier {
    let mut surviving: Vec<Tier> = tiers.filter(|t| *t != Tier::Bot).collect();
    match surviving.iter().max_by_key(|t| t.idx()) {
        Some(t) => *t,
        None => {
            // All members were Bot (or none) → Bot.
            surviving.clear();
            Tier::Bot
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering (byte-exact ports of CoverageRenderer)
// ---------------------------------------------------------------------------

/// Ruby `Float#round(digits)` — round half away from zero to `digits` decimals.
fn ruby_round(f: f64, digits: i32) -> f64 {
    let p = 10f64.powi(digits);
    (f * p).round() / p
}

/// `round(digits)` then Ruby `Float#to_s` — the exact spelling the reference's
/// interpolated percentages / ratios produce.
fn round_to_s(f: f64, digits: i32) -> String {
    ruby_float_to_s(ruby_round(f, digits))
}

/// The ` (NN.N%)` suffix (reference `pct`): empty when the denominator is 0.
fn pct(numerator: u64, denominator: u64) -> String {
    if denominator == 0 {
        return String::new();
    }
    let v = numerator as f64 / denominator as f64 * 100.0;
    format!(" ({}%)", round_to_s(v, 1))
}

/// `ratio_f(val) = val.round(4)` (reference), for the JSON payload.
fn ratio_f(v: f64) -> String {
    round_to_s(v, 4)
}

fn render_text(report: &Report) -> String {
    let mut out = String::new();
    // Header.
    let n = report.files.len();
    let suffix = if n == 1 { "" } else { "s" };
    out.push_str(&format!("Type coverage: {n} file{suffix}\n"));
    for f in report.files.iter().take(5) {
        out.push_str(&format!("  - {f}\n"));
    }
    if n > 5 {
        out.push_str(&format!("  ... ({} more)\n", n - 5));
    }
    out.push('\n');

    // Summary.
    let g = report.grand_total();
    let p = report.precise_count();
    let o = report.opaque_count();
    out.push_str("Summary:\n");
    out.push_str(&format!(
        "  files processed:      {}\n",
        report.files.len() - report.parse_errors.len()
    ));
    out.push_str(&format!("  parse errors:         {}\n", report.parse_errors.len()));
    out.push_str(&format!("  expressions typed:    {g}\n"));
    out.push_str(&format!("  precise:              {p}{}\n", pct(p, g)));
    out.push_str(&format!("  dynamic (opaque):     {o}{}\n", pct(o, g)));
    out.push_str(&format!(
        "  precision ratio:      {}%\n",
        round_to_s(report.precision_ratio() * 100.0, 2)
    ));
    out.push('\n');

    // Tier breakdown (only non-zero tiers).
    out.push_str("Tier breakdown:\n");
    for tier in TIERS {
        let count = report.tier_count(tier);
        if count == 0 {
            continue;
        }
        let label = ljust(tier.text_label(), 36);
        out.push_str(&format!(
            "  {label} {}{}\n",
            rjust(&count.to_string(), 7),
            pct(count, g)
        ));
    }
    out.push('\n');

    // Per-file breakdown (only when more than one file).
    if report.per_file.len() > 1 {
        out.push_str("Per-file breakdown:\n");
        let width = report
            .per_file
            .iter()
            .map(|(f, _)| f.chars().count())
            .max()
            .unwrap_or(0);
        let mut rows: Vec<&(String, FileResult)> = report.per_file.iter().collect();
        // Ruby `sort_by` is a stable sort on the precision ratio.
        rows.sort_by(|a, b| {
            a.1.precision_ratio()
                .partial_cmp(&b.1.precision_ratio())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (file, r) in rows {
            if r.total == 0 {
                continue;
            }
            let ratio_str = rjust(&format!("{}%", round_to_s(r.precision_ratio() * 100.0, 1)), 6);
            out.push_str(&format!(
                "  {}  {ratio_str}  ({}/{})\n",
                ljust(file, width),
                r.precise_count(),
                r.total
            ));
        }
        out.push('\n');
    }

    // Parse errors.
    if !report.parse_errors.is_empty() {
        out.push_str("Parse errors:\n");
        for (file, errors) in &report.parse_errors {
            out.push_str(&format!("  {file}: {}\n", errors.join("; ")));
        }
    }

    out
}

/// Ruby `String#ljust(width)` on character count (pad right with spaces).
fn ljust(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// Ruby `String#rjust(width)` on character count (pad left with spaces).
fn rjust(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{s}", " ".repeat(width - len))
    }
}

// ---- JSON (a byte-exact port of JSON.pretty_generate over the payload) ------

fn render_json(report: &Report) -> String {
    let g = report.grand_total();
    let mut j = JsonWriter::new();
    j.begin_object();

    // summary
    j.key("summary");
    j.begin_object();
    j.int_field("files_processed", (report.files.len() - report.parse_errors.len()) as u64);
    j.int_field("parse_errors", report.parse_errors.len() as u64);
    j.int_field("expressions_typed", g);
    j.int_field("precise_count", report.precise_count());
    j.raw_field("precise_ratio", &ratio_f(report.precision_ratio()));
    j.int_field("dynamic_opaque_count", report.opaque_count());
    j.raw_field("dynamic_opaque_ratio", &ratio_f(report.opaque_ratio()));
    let dsc = report.total.dynamic_specific_count();
    j.int_field("dynamic_specific_count", dsc);
    j.raw_field(
        "dynamic_specific_ratio",
        &ratio_f(ratio_over(dsc, g)),
    );
    j.end_object();

    // by_tier
    j.key("by_tier");
    j.begin_object();
    for tier in TIERS {
        let n = report.tier_count(tier);
        j.key(tier.json_key());
        j.begin_object();
        j.int_field("count", n);
        j.raw_field("ratio", &ratio_f(ratio_over(n, g)));
        j.end_object();
    }
    j.end_object();

    // by_file
    j.key("by_file");
    j.begin_array();
    for (file, r) in &report.per_file {
        j.begin_object();
        j.str_field("file", file);
        j.int_field("expressions_typed", r.total);
        j.int_field("precise_count", r.precise_count());
        j.raw_field("precise_ratio", &ratio_f(r.precision_ratio()));
        j.int_field("dynamic_opaque_count", r.opaque_count());
        j.raw_field("dynamic_opaque_ratio", &ratio_f(r.opaque_ratio()));
        j.key("by_tier");
        j.begin_object();
        for tier in TIERS {
            let n = r.tier(tier);
            j.key(tier.json_key());
            j.begin_object();
            j.int_field("count", n);
            j.raw_field("ratio", &ratio_f(ratio_over(n, r.total)));
            j.end_object();
        }
        j.end_object();
        j.end_object();
    }
    j.end_array();

    // parse_errors
    j.key("parse_errors");
    j.begin_array();
    for (file, errors) in &report.parse_errors {
        j.begin_object();
        j.str_field("file", file);
        j.key("errors");
        j.begin_array();
        for e in errors {
            j.str_element(e);
        }
        j.end_array();
        j.end_object();
    }
    j.end_array();

    j.end_object();
    j.finish()
}

/// `n.fdiv(g.nonzero? || 1)` (reference): divide by `g`, or by 1 when `g` is 0.
fn ratio_over(n: u64, g: u64) -> f64 {
    if g == 0 {
        n as f64 // divided by 1
    } else {
        n as f64 / g as f64
    }
}

/// A minimal writer reproducing Ruby `JSON.pretty_generate`: 2-space indent,
/// `": "` after keys, `[]`/`{}` for empty containers, one member/element per line.
struct JsonWriter {
    buf: String,
    indent: usize,
    /// One `child-count` per open container (comma bookkeeping).
    stack: Vec<usize>,
    /// True after a `key(..)`: the next value follows `"k": ` directly, with no
    /// element separator of its own.
    pending_key: bool,
}

impl JsonWriter {
    fn new() -> Self {
        JsonWriter {
            buf: String::new(),
            indent: 0,
            stack: Vec::new(),
            pending_key: false,
        }
    }

    /// Emit the prefix before a value. A value after a `key` follows directly; an
    /// array element (or additional top-level value) gets a comma-if-not-first
    /// plus newline + indent; a top-level value gets nothing.
    fn value_prefix(&mut self) {
        if self.pending_key {
            self.pending_key = false;
            return;
        }
        if let Some(count) = self.stack.last_mut() {
            if *count > 0 {
                self.buf.push(',');
            }
            *count += 1;
            self.buf.push('\n');
            self.buf.push_str(&"  ".repeat(self.indent));
        }
    }

    fn begin_object(&mut self) {
        self.value_prefix();
        self.buf.push('{');
        self.stack.push(0);
        self.indent += 1;
    }

    fn end_object(&mut self) {
        let count = self.stack.pop().unwrap_or(0);
        self.indent -= 1;
        if count > 0 {
            self.buf.push('\n');
            self.buf.push_str(&"  ".repeat(self.indent));
        }
        self.buf.push('}');
    }

    fn begin_array(&mut self) {
        self.value_prefix();
        self.buf.push('[');
        self.stack.push(0);
        self.indent += 1;
    }

    fn end_array(&mut self) {
        let count = self.stack.pop().unwrap_or(0);
        self.indent -= 1;
        if count > 0 {
            self.buf.push('\n');
            self.buf.push_str(&"  ".repeat(self.indent));
        }
        self.buf.push(']');
    }

    /// Emit an object member key `"k": `; the value follows via a begin_* / field.
    fn key(&mut self, k: &str) {
        if let Some(count) = self.stack.last_mut() {
            if *count > 0 {
                self.buf.push(',');
            }
            *count += 1;
        }
        self.buf.push('\n');
        self.buf.push_str(&"  ".repeat(self.indent));
        self.buf.push_str(&escape_json_string(k));
        self.buf.push_str(": ");
        self.pending_key = true;
    }

    fn scalar(&mut self, s: &str) {
        self.value_prefix();
        self.buf.push_str(s);
    }

    fn int_field(&mut self, k: &str, v: u64) {
        self.key(k);
        self.scalar(&v.to_string());
    }

    fn raw_field(&mut self, k: &str, raw: &str) {
        self.key(k);
        self.scalar(raw);
    }

    fn str_field(&mut self, k: &str, v: &str) {
        self.key(k);
        self.scalar(&escape_json_string(v));
    }

    /// A bare array element (string).
    fn str_element(&mut self, v: &str) {
        self.scalar(&escape_json_string(v));
    }

    fn finish(self) -> String {
        // JSON.pretty_generate emits no trailing newline; the reference prints
        // via `@out.puts`, which appends exactly one "\n".
        let mut s = self.buf;
        s.push('\n');
        s
    }
}

/// JSON-escape a string the way Ruby's `JSON` generator does: escape `"`, `\`,
/// and the C0 control characters (`\b`/`\t`/`\n`/`\f`/`\r` by name, the rest as
/// `\u00XX`). Forward slash is NOT escaped (matching Ruby).
fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scan an inline source through the same path `scan_file` uses.
    fn scan_src(src: &str) -> FileResult {
        let parse_result = ruby_prism::parse(src.as_bytes());
        assert_eq!(parse_result.errors().count(), 0, "test source must parse");
        let ast = lower(&parse_result);
        let index = CoreIndex::new();
        let source_index = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source_index);
        let mut interner = Interner::new();
        let root = parse_result.node();
        let mut scanner = FileScanner::new(&ast, &typer, &index, &mut interner, &root);
        scanner.scan(&root)
    }

    #[test]
    fn literals_and_write_count_as_constant() {
        // `s = "Hello"; s.lenght` — 4 expressions: write (→ rvalue constant),
        // string, local read (bound constant), unresolvable call (dynamic_top).
        // The oracle-measured shape of harness fixture 01.
        let r = scan_src("s = \"Hello\"\ns.lenght\n");
        assert_eq!(r.total, 4);
        assert_eq!(r.tier(Tier::Constant), 3);
        assert_eq!(r.tier(Tier::DynamicTop), 1);
    }

    #[test]
    fn if_branches_union_and_def_locals_bind() {
        // Both branches constant → the if-expression unions to constant
        // (oracle-verified: 5 nodes, 4 constant, 1 dynamic_top for `c`).
        let r = scan_src("x = if c\n  1\nelse\n  2\nend\n");
        assert_eq!(r.total, 5);
        assert_eq!(r.tier(Tier::Constant), 4);
        assert_eq!(r.tier(Tier::DynamicTop), 1);

        // A def-body local binds in its own scope (oracle: 4 constant).
        let r = scan_src("def foo\n  z = 5\n  z\nend\n");
        assert_eq!(r.total, 4);
        assert_eq!(r.tier(Tier::Constant), 4);
    }

    #[test]
    fn conditional_def_write_stays_dynamic() {
        // `max_id = "+inf" if max_id.blank?` must NOT bind max_id for the later
        // read (flow-conservative def env; the over-claim caught on mastodon
        // feed.rb). Oracle: constant 4 / dynamic_top 3.
        let r = scan_src("def f(max_id)\n  max_id = \"+inf\" if max_id.blank?\n  max_id\nend\n");
        assert_eq!(r.tier(Tier::Constant), 4);
        assert_eq!(r.tier(Tier::DynamicTop), 3);
    }

    #[test]
    fn conditional_reassign_invalidates_the_straight_line_binding() {
        // PR #33 review blocker: a straight-line `x = 5` followed by a
        // CONDITIONAL reassignment anywhere in the body must not leave the
        // stale Constant binding for later reads — the reference joins the
        // branches and widens to dynamic. All expectations oracle-measured.

        // Modifier-if reassign: ref {constant 3, dynamic_top 5}.
        let r = scan_src("def f(p)\n  x = 5\n  x = p if c\n  x\nend\n");
        assert_eq!((r.tier(Tier::Constant), r.tier(Tier::DynamicTop)), (3, 5));

        // `||=` compound write: ref {constant 3, dynamic_top 3}.
        let r = scan_src("def f(p)\n  x = 5\n  x ||= p\n  x\nend\n");
        assert_eq!((r.tier(Tier::Constant), r.tier(Tier::DynamicTop)), (3, 3));

        // if/else both-branch reassign: ref {constant 5, dynamic_top 5}.
        let r = scan_src("def f(p)\n  x = 5\n  if c\n    x = p\n  else\n    x = 6\n  end\n  x\nend\n");
        assert_eq!((r.tier(Tier::Constant), r.tier(Tier::DynamicTop)), (5, 5));

        // case-branch reassign: ref {constant 4, dynamic_top 5}.
        let r = scan_src("def f(p)\n  x = 5\n  case p\n  when 1 then x = p\n  end\n  x\nend\n");
        assert_eq!((r.tier(Tier::Constant), r.tier(Tier::DynamicTop)), (4, 5));

        // while-body reassign: ref {constant 4, dynamic_top 4}.
        let r = scan_src("def f(p)\n  x = 5\n  while p\n    x = x2\n  end\n  x\nend\n");
        assert_eq!((r.tier(Tier::Constant), r.tier(Tier::DynamicTop)), (4, 4));

        // The stale value must not leak through a LATER straight-line RHS
        // either (`y = x + 1` after the conditional reassign must not pin 6):
        // ref {constant 4, dynamic_top 8}.
        let r = scan_src("def f(p)\n  x = 5\n  x = p if c\n  y = x + 1\n  y\nend\n");
        assert_eq!((r.tier(Tier::Constant), r.tier(Tier::DynamicTop)), (4, 8));
    }

    #[test]
    fn user_rb_extract_matches_reference_tiers() {
        // The real-corpus witness from the PR #33 review: mastodon
        // app/models/user.rb `sign_up_email_requires_approval?` — the
        // `records = []` seed reassigned by a modifier-unless. Oracle:
        // {constant 4, shaped 3, bot 1, dynamic_top 17} (rigor-rs used to
        // report shaped 4: the stale Tuple[] binding).
        let src = "def sign_up_email_requires_approval?\n\
                   \x20 return false if email_domain.blank?\n\n\
                   \x20 records = []\n\n\
                   \x20 records = DomainResource.new(email_domain).mx unless self.class.skip_mx_check?\n\n\
                   \x20 EmailDomainBlock.requires_approval?(records + [email_domain], attempt_ip: sign_up_ip)\nend\n";
        let r = scan_src(src);
        assert_eq!(r.tier(Tier::Constant), 4);
        assert_eq!(r.tier(Tier::Shaped), 3);
        assert_eq!(r.tier(Tier::Bot), 1);
        assert_eq!(r.tier(Tier::DynamicTop), 17);
    }

    #[test]
    fn jumps_are_bot_and_loops_are_constant_nil() {
        // `while c; break; end` — loop → Constant[nil], break → Bot, c → dyn.
        let r = scan_src("while c\n  break\nend\n");
        assert_eq!(r.tier(Tier::Bot), 1);
        assert_eq!(r.tier(Tier::Constant), 1);
        assert_eq!(r.tier(Tier::DynamicTop), 1);
    }

    #[test]
    fn non_expression_nodes_are_excluded() {
        // Params / argument-list / pair WRAPPERS are excluded from the
        // denominator, while a pair's key and value are themselves counted:
        // `def f(a, b); g(1, x: 2); end` → def(:f), g(...), 1, :x, 2
        // (oracle-verified: total 5, constant 4, dynamic_top 1).
        let r = scan_src("def f(a, b)\n  g(1, x: 2)\nend\n");
        assert_eq!(r.total, 5);
        assert_eq!(r.tier(Tier::Constant), 4);
        assert_eq!(r.tier(Tier::DynamicTop), 1);
    }

    #[test]
    fn union_tiers_absorbs_bot() {
        assert_eq!(union_tiers([Tier::Constant, Tier::Bot].into_iter()), Tier::Constant);
        assert_eq!(union_tiers([Tier::Bot, Tier::Bot].into_iter()), Tier::Bot);
        assert_eq!(
            union_tiers([Tier::Constant, Tier::DynamicTop].into_iter()),
            Tier::DynamicTop
        );
        assert_eq!(union_tiers(std::iter::empty()), Tier::Bot);
    }

    #[test]
    fn ratio_spelling_matches_ruby_round_and_to_s() {
        // Ruby: (3.0/4).round(4).to_s == "0.75"; (1.0).round(2) == "1.0";
        // 0.16666.. rounds half-away-from-zero to 0.1667.
        assert_eq!(ratio_f(0.75), "0.75");
        assert_eq!(ratio_f(1.0), "1.0");
        assert_eq!(ratio_f(1.0 / 6.0), "0.1667");
        assert_eq!(pct(1, 4), " (25.0%)");
        assert_eq!(pct(0, 0), "");
    }

    #[test]
    fn json_writer_matches_pretty_generate_shape() {
        let mut j = JsonWriter::new();
        j.begin_object();
        j.key("a");
        j.begin_object();
        j.int_field("n", 1);
        j.end_object();
        j.key("b");
        j.begin_array();
        j.end_array();
        j.end_object();
        assert_eq!(j.finish(), "{\n  \"a\": {\n    \"n\": 1\n  },\n  \"b\": []\n}\n");
    }

    #[test]
    fn collect_paths_globs_dirs_in_ruby_order_and_dedups() {
        let dir = std::env::temp_dir().join(format!("rigor-cov-glob-{}", std::process::id()));
        // Layout reproducing the Dir.glob ordering trap: `admin/` sorts before
        // `admin.rb`, so the subdir's files emit first.
        std::fs::create_dir_all(dir.join("admin")).unwrap();
        std::fs::write(dir.join("admin.rb"), "").unwrap();
        std::fs::write(dir.join("admin/a.rb"), "").unwrap();
        std::fs::write(dir.join("zz.rb"), "").unwrap();
        std::fs::write(dir.join(".hidden.rb"), "").unwrap();

        let root = dir.to_str().unwrap().to_string();
        let got = collect_paths(&[root.clone(), root.clone()]).unwrap();
        let rel: Vec<String> =
            got.iter().map(|p| p[root.len() + 1..].to_string()).collect();
        assert_eq!(rel, vec!["admin/a.rb", "admin.rb", "zz.rb"]);

        assert!(collect_paths(&["/nonexistent/xyz.rb".to_string()]).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod rebind_taint_regressions {
    //! PR #33 re-review BLOCKING-1/2: rebind forms the arena does not carry
    //! (multi-write / for-index / rescue-capture) must invalidate a
    //! straight-line binding, and a class/module WRAPPER's own tier must be
    //! computed under the ENCLOSING scope. Expectations oracle-verified
    //! (per-node dumps against the pinned reference).
    use super::*;

    fn scan_src(src: &str) -> FileResult {
        let parse_result = ruby_prism::parse(src.as_bytes());
        assert_eq!(parse_result.errors().count(), 0, "test source must parse");
        let ast = lower(&parse_result);
        let index = CoreIndex::new();
        let source_index = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source_index);
        let mut interner = Interner::new();
        let root = parse_result.node();
        let mut scanner = FileScanner::new(&ast, &typer, &index, &mut interner, &root);
        scanner.scan(&root)
    }

    #[test]
    fn rescue_capture_is_collected_as_taint() {
        let src = "x = 5\nbegin\n  g\nrescue => x\nend\nx\n";
        let pr = ruby_prism::parse(src.as_bytes());
        let root = pr.node();
        let taints = collect_prism_taints(&root);
        assert!(
            taints.iter().any(|(_, n)| n == "x"),
            "rescue => x must taint x; got {taints:?}"
        );
    }

    #[test]
    fn multi_write_if_invalidates_the_binding() {
        // `x = 5; x, y = frob, 2 if c; x` — the final read must NOT keep 5.
        // Oracle: constant 3 (5, 2, and the write) / dynamic_top 8; the ref's
        // three `shaped` nodes (if/multiwrite/rhs-array wrappers) are rs
        // dynamic — an under-claim.
        let r = scan_src("x = 5\nx, y = frob, 2 if c\nx\n");
        assert_eq!(r.tier(Tier::Constant), 3);
        assert_eq!(r.tier(Tier::DynamicTop), 8);
    }

    #[test]
    fn for_index_invalidates_the_binding() {
        // `x = 5; for x in p; end; x` — oracle-exact: constant 4 (incl. the
        // for-loop's Constant[nil]) / dynamic_top 2 (p and the final x).
        let r = scan_src("x = 5\nfor x in p\nend\nx\n");
        assert_eq!(r.tier(Tier::Constant), 4);
        assert_eq!(r.tier(Tier::DynamicTop), 2);
    }

    #[test]
    fn rescue_capture_invalidates_the_binding() {
        // `x = 5; begin; g; rescue => x; end; x` — the final read must NOT
        // keep Constant[5]. Oracle types it nominal (the Constant|StandardError
        // join); the taint widens to dynamic_top — an under-claim, never the
        // constant over-claim. constant 2 / dynamic_top 4.
        let r = scan_src("x = 5\nbegin\n  g\nrescue => x\nend\nx\n");
        assert_eq!(r.tier(Tier::Constant), 2);
        assert_eq!(r.tier(Tier::DynamicTop), 4);
    }

    #[test]
    fn module_wrapper_types_under_the_enclosing_scope() {
        // `module M; x = 5; x; end` — the WRAPPER's tier is the body value
        // typed OUTSIDE the module (where x is unbound) → dynamic_top, while
        // the interior write/read stay constant. Oracle-exact histogram:
        // constant 3, nominal 1 (header M), dynamic_top 1 (the wrapper).
        let r = scan_src("module M\n  x = 5\n  x\nend\n");
        assert_eq!(r.total, 5);
        assert_eq!(r.tier(Tier::Constant), 3);
        assert_eq!(r.tier(Tier::Nominal), 1);
        assert_eq!(r.tier(Tier::DynamicTop), 1);
    }

    #[test]
    fn reopened_module_wrapper_resolves_constants_outside() {
        // The second `module M` wrapper types `f = Foo` from OUTSIDE, where
        // bare `Foo` (declared only as M::Foo) does not resolve → the wrapper
        // is dynamic_top; the INTERIOR `Foo` read (prefix [M]) stays nominal.
        // Oracle-exact: constant 2, nominal 5, dynamic_top 1.
        let src = "module M\n  class Foo\n  end\nend\nmodule M\n  f = Foo\nend\n";
        let r = scan_src(src);
        assert_eq!(r.total, 8);
        assert_eq!(r.tier(Tier::Constant), 2);
        assert_eq!(r.tier(Tier::Nominal), 5);
        assert_eq!(r.tier(Tier::DynamicTop), 1);
    }
}
