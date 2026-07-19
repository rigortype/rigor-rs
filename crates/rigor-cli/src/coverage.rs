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

    let mut scanner = FileScanner::new(&ast, &typer, &source_index, index, &mut interner);
    Ok(scanner.scan(&parse_result.node()))
}

// ---------------------------------------------------------------------------
// The per-file precision scan (reproduces PrecisionScanner + ExpressionTyper)
// ---------------------------------------------------------------------------

/// Collects every Prism node in DFS pre-order (the reference's `NodeWalker`
/// order; order is irrelevant to the tier histogram, only the node SET matters).
struct NodeCollector<'pr> {
    nodes: Vec<Node<'pr>>,
}

impl<'pr> Visit<'pr> for NodeCollector<'pr> {
    fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
        self.nodes.push(node);
    }
    fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
        self.nodes.push(node);
    }
}

struct FileScanner<'a> {
    ast: &'a LoweredAst,
    typer: &'a Typer<'a>,
    source: &'a SourceIndex,
    index: &'a CoreIndex,
    interner: &'a mut Interner,
    /// Exact `(start, end)` span → arena `NodeId`, for routing value-leaves to
    /// the arena typer. Last-writer (outermost / largest id) wins on collision.
    span_to_arena: HashMap<(usize, usize), rigor_parse::NodeId>,
    /// The top-level env (binds program-body local writes in source order).
    toplevel_env: TypeEnv,
    /// `def` spans (sorted None), each with its method-body env (fresh scope +
    /// def-local writes). Used to type a body-local read in its own scope.
    def_envs: Vec<((usize, usize), TypeEnv)>,
    /// `class`/`module`/`class << self` spans — a `self` inside one types
    /// nominal (the reference's injected class/instance `self_type`).
    class_spans: Vec<(usize, usize)>,
}

impl<'a> FileScanner<'a> {
    fn new(
        ast: &'a LoweredAst,
        typer: &'a Typer<'a>,
        source: &'a SourceIndex,
        index: &'a CoreIndex,
        interner: &'a mut Interner,
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
        let toplevel_env = typer.build_toplevel_env(ast, interner);
        FileScanner {
            ast,
            typer,
            source,
            index,
            interner,
            span_to_arena,
            toplevel_env,
            def_envs: Vec::new(),
            class_spans: Vec::new(),
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
                Node::DefNode { .. } => {
                    let span = span_of(node);
                    let env = self.build_def_env(span);
                    self.def_envs.push((span, env));
                }
                Node::ClassNode { .. }
                | Node::ModuleNode { .. }
                | Node::SingletonClassNode { .. } => {
                    self.class_spans.push(span_of(node));
                }
                _ => {}
            }
        }

        let mut result = FileResult::default();
        for node in &nodes {
            if is_non_expression(node) {
                continue;
            }
            let tier = self.node_tier(node);
            result.counts[tier.idx()] += 1;
            result.total += 1;
        }
        result
    }

    /// Build the method-body env for one `def` span: a FRESH scope (Ruby method
    /// bodies do not see the enclosing locals) binding only the UNCONDITIONAL
    /// top-level statement writes of the body, in source order — the same
    /// straight-line discipline [`Typer::build_toplevel_env`] applies at the top
    /// level. A write nested in a branch / loop / block is NOT bound: binding it
    /// unconditionally (the old span-scan) over-claimed constants the reference's
    /// flow-sensitive scope correctly widens (a `x = "+inf" if x.blank?`
    /// modifier-write must leave the later read param-dependent). The
    /// conservative skip yields the same `Dynamic[top]` tier the reference's
    /// union-with-dynamic-param produces.
    fn build_def_env(&mut self, def_span: (usize, usize)) -> TypeEnv {
        let mut env = TypeEnv::new();
        let Some(&def_id) = self.span_to_arena.get(&def_span) else {
            return env;
        };
        let rigor_parse::Node::Definition { body, .. } = self.ast.get(def_id) else {
            return env;
        };
        let body = body.clone();
        for stmt in body {
            self.bind_stmt(stmt, &mut env);
        }
        env
    }

    /// Bind a single arena statement into `env` when it is a plain local write;
    /// recurse through a `Statements` wrapper. Any other statement (branch, loop,
    /// call-with-block, ...) has no binding effect — its interior writes stay
    /// conservative.
    fn bind_stmt(&mut self, id: rigor_parse::NodeId, env: &mut TypeEnv) {
        match self.ast.get(id) {
            rigor_parse::Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                let ty = self.typer.type_of(self.ast, value, env, self.interner);
                env.insert(name, ty);
            }
            rigor_parse::Node::Statements { body, .. } => {
                for s in body.clone() {
                    self.bind_stmt(s, env);
                }
            }
            _ => {}
        }
    }

    /// The env visible at a node: the innermost enclosing `def`'s method-body
    /// env, else the top-level env.
    fn env_at(&self, span: (usize, usize)) -> &TypeEnv {
        let mut best: Option<&((usize, usize), TypeEnv)> = None;
        for entry in &self.def_envs {
            let (ds, de) = entry.0;
            if ds <= span.0 && span.1 <= de {
                match best {
                    None => best = Some(entry),
                    Some(b) if (de - ds) < (b.0 .1 - b.0 .0) => best = Some(entry),
                    _ => {}
                }
            }
        }
        best.map(|e| &e.1).unwrap_or(&self.toplevel_env)
    }

    /// Whether a span sits inside any class/module body (→ `self` is nominal).
    fn inside_class(&self, span: (usize, usize)) -> bool {
        self.class_spans
            .iter()
            .any(|(s, e)| *s <= span.0 && span.1 <= *e)
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
            | Node::XStringNode { .. }
            | Node::SourceFileNode { .. }
            | Node::SourceLineNode { .. }
            | Node::TrueNode { .. }
            | Node::FalseNode { .. }
            | Node::NilNode { .. }
            | Node::RegularExpressionNode { .. } => Tier::Constant,

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

            // ---- Definitions that recurse into their body ------------------
            Node::ClassNode { .. } => match node.as_class_node().unwrap().body() {
                None => Tier::Constant,
                Some(b) => self.value_tier(b),
            },
            Node::ModuleNode { .. } => match node.as_module_node().unwrap().body() {
                None => Tier::Constant,
                Some(b) => self.value_tier(b),
            },
            Node::SingletonClassNode { .. } => {
                match node.as_singleton_class_node().unwrap().body() {
                    None => Tier::Constant,
                    Some(b) => self.value_tier(b),
                }
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
                if self.inside_class(span_of(node)) {
                    Tier::Nominal
                } else {
                    Tier::DynamicTop
                }
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

            // ---- Value-producing leaves → the arena typer ------------------
            Node::CallNode { .. }
            | Node::ArrayNode { .. }
            | Node::HashNode { .. }
            | Node::KeywordHashNode { .. }
            | Node::RangeNode { .. }
            | Node::LocalVariableReadNode { .. }
            | Node::ItLocalVariableReadNode { .. }
            | Node::InstanceVariableReadNode { .. }
            | Node::ClassVariableReadNode { .. }
            | Node::GlobalVariableReadNode { .. }
            | Node::EmbeddedStatementsNode { .. } => self.arena_tier(node),

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
        if self.source.knows_class(name) || self.index.knows_toplevel_class(name) {
            return Tier::Nominal;
        }
        self.arena_tier(node)
    }

    /// The arena `TypeId` for a Prism node with an exact-span arena counterpart,
    /// typed under the node's enclosing-scope env. `None` when no arena node has
    /// that exact span (a Prism node with no 1:1 lowered form).
    fn arena_type(&mut self, node: &Node<'_>) -> Option<TypeId> {
        let span = span_of(node);
        let id = *self.span_to_arena.get(&span)?;
        // Clone the env out to avoid borrowing `self` immutably + mutably.
        let env = self.env_at(span).clone();
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

/// The `(start, end)` byte span of a Prism node.
fn span_of(node: &Node<'_>) -> (usize, usize) {
    let loc = node.location();
    (loc.start_offset(), loc.end_offset())
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
        let mut scanner = FileScanner::new(&ast, &typer, &source_index, &index, &mut interner);
        scanner.scan(&parse_result.node())
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
