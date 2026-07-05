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

use rayon::prelude::*;
use rigor_index::CoreIndex;
use rigor_parse::{lower, parse};
use rigor_rules::{analyze_with_source_and_folder, catalog, Diagnostic, Severity};
use rigor_types::Interner;

mod config;
use config::Config;
mod ci_detector;
mod diagnostic_formats;
use diagnostic_formats::Rendered;
mod baseline;
use baseline::{Baseline, MatchMode, DEFAULT_BASELINE_PATH};
mod docs;
mod doctor;
mod explain;
mod init;
mod lsp;
mod mcp;
mod outline;
mod plugins_cmd;
mod rbs_collection;
mod ruby_mode;
mod sidecar;
mod type_of;

/// The reference's full subcommand surface (ADR-0015).
const COMMANDS: &[&str] = &[
    "check", "annotate", "type-of", "trace", "type-scan", "explain", "diff",
    "sig-gen", "baseline", "triage", "coverage", "plugins", "plugin", "lsp",
    "mcp", "skill", "docs", "init", "doctor", "version",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // `version` / `--version` / `-v` / `-V` — print `rigor <version>` and
        // exit 0 (§13). Mirrors the reference's `rigor #{Rigor::VERSION}` output
        // (`lib/rigor/cli.rb`, which accepts `version`/`-v`/`--version`). The
        // version is the crate version baked in at compile time, so it tracks
        // the workspace `version` automatically. `-V` is the conventional Rust
        // short flag; accepted alongside the reference's `-v`.
        Some("version" | "--version" | "-v" | "-V") => {
            println!("rigor {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("check") => cmd_check(&args[1..]),
        Some("baseline") => cmd_baseline(&args[1..]),
        Some("type-of") => type_of::cmd_type_of(&args[1..]),
        Some("explain") => explain::cmd_explain(&args[1..]),
        Some("init") => init::cmd_init(&args[1..]),
        Some("doctor") => doctor::cmd_doctor(&args[1..]),
        Some("plugins") => plugins_cmd::cmd_plugins(&args[1..]),
        Some("docs") => docs::cmd_docs(&args[1..]),
        Some("lsp") => lsp::cmd_lsp(&args[1..]),
        Some("mcp") => mcp::cmd_mcp(&args[1..]),
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
    // ADR-22 baseline resolution (mirrors the reference's precedence in
    // `apply_baseline_filter`): `--no-baseline` (Off) > `--baseline PATH`
    // (Path) > `.rigor.yml`'s `baseline:` (Unset → config).
    let mut baseline_arg = BaselineArg::Unset;
    // ADR-0036 coverage-posture axis (CLI layer). `--ruby` and `--no-ruby` are
    // mutually exclusive; the effective mode is resolved (CLI > env > config >
    // default `require`) after config load.
    let mut ruby_cli: Option<ruby_mode::RubyMode> = None;
    let mut no_ruby_flag = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some("text") => format = OutputFormat::Text,
                Some("json") => format = OutputFormat::Json,
                Some("github") => format = OutputFormat::Github,
                Some("sarif") => format = OutputFormat::Sarif,
                Some("gitlab") => format = OutputFormat::Gitlab,
                Some("checkstyle") => format = OutputFormat::Checkstyle,
                Some("junit") => format = OutputFormat::Junit,
                Some("teamcity") => format = OutputFormat::Teamcity,
                other => {
                    eprintln!(
                        "rigor check: --format expects `text`, `json`, `github`, `sarif`, \
                         `gitlab`, `checkstyle`, `junit`, or `teamcity`, got {other:?}"
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
            "--baseline" => match it.next() {
                Some(path) => baseline_arg = BaselineArg::Path(path.clone()),
                None => {
                    eprintln!("rigor check: --baseline expects a path");
                    return ExitCode::from(64);
                }
            },
            "--no-baseline" => baseline_arg = BaselineArg::Off,
            "--ruby" => match it.next() {
                Some(v) => ruby_cli = Some(ruby_mode::parse_value(v)),
                None => {
                    eprintln!("rigor check: --ruby expects require|auto|off|<path>");
                    return ExitCode::from(64);
                }
            },
            "--no-ruby" => no_ruby_flag = true,
            other if other.starts_with("--ruby=") => {
                ruby_cli = Some(ruby_mode::parse_value(&other["--ruby=".len()..]));
            }
            other => files.push(other),
        }
    }

    if files.is_empty() {
        eprintln!("rigor check: expected at least one file");
        return ExitCode::from(64);
    }

    // ADR-0036 same-layer mutual exclusion: `--ruby` and `--no-ruby` together is
    // a usage error, redundant or not.
    if ruby_cli.is_some() && no_ruby_flag {
        eprintln!("rigor check: --ruby and --no-ruby are mutually exclusive (specify at most one)");
        return ExitCode::from(64);
    }
    let ruby_cli = ruby_cli.or(no_ruby_flag.then_some(ruby_mode::RubyMode::Off));

    // Load `.rigor.yml` (explicit `--config` path, else cwd auto-discovery).
    // Config ONLY suppresses/scopes diagnostics; it never changes analysis.
    // Degrades to default (= inert) on any error, so the differential harness —
    // which runs from a directory with no `.rigor.yml` — is unaffected.
    let cfg = Config::load(explicit_config.map(Path::new));

    // ADR-0036 coverage posture (ADR-0008 sidecar). Resolve the mode, then bring
    // up the Ruby sidecar accordingly: `require`/`<path>` MUST have it (exit 69 on
    // failure — full fidelity was demanded and cannot be delivered); `auto` uses
    // it when reachable and otherwise discloses + degrades to the sound subset;
    // `off` never spawns it. A wired folder lets `sidecar_foldable` literal calls
    // resolve to `Constant` (full fidelity); its absence is the sound subset.
    let sidecar_folder = match build_sidecar_folder(&cfg, ruby_cli) {
        Ok(f) => f,
        Err(code) => return code,
    };
    let folder_ref =
        sidecar_folder.as_ref().map(|f| f as &(dyn rigor_infer::RubyFolder + Sync));

    // Run the analysis pipeline (config `exclude:`/`disable:` + inline
    // `# rigor:disable` applied). Shared with `baseline generate`.
    let (mut findings, had_io_error) = analyze_files(&files, &cfg, "check", folder_ref);

    // ADR-22 — baseline filter, applied LAST (after inline `# rigor:disable`
    // and config `disable:`, per reference WD6). With no resolved baseline this
    // is a no-op, so the no-baseline path stays byte-identical (harness-gated).
    if let Some(path) = resolve_baseline_path(&baseline_arg, &cfg) {
        findings = apply_baseline(findings, &path);
    }

    match format {
        OutputFormat::Text => print_text(&findings),
        OutputFormat::Json => print_json(&findings),
        OutputFormat::Github => print_github(&findings),
        OutputFormat::Sarif => print_sarif(&findings),
        OutputFormat::Gitlab => print_rendered(&findings, diagnostic_formats::render_gitlab),
        OutputFormat::Checkstyle => print_rendered(&findings, diagnostic_formats::render_checkstyle),
        OutputFormat::Junit => print_rendered(&findings, diagnostic_formats::render_junit),
        OutputFormat::Teamcity => print_rendered(&findings, diagnostic_formats::render_teamcity),
    }

    // CI auto-detection (ADR-51 WD7): only augments the default human (`text`)
    // output — an explicit `--format` means the caller is in control and is left
    // untouched. For a first-class stdout-native CI (GitHub Actions / TeamCity)
    // the platform's annotations are emitted on top of the text output; for
    // GitLab (native but artifact-based) and the reviewdog-routed CIs a one-line
    // hint goes to stderr, but only when there are diagnostics so a clean run
    // stays quiet. `RIGOR_CI_DETECT=0`/`false`/`no`/`off` disables it (and so the
    // differential harness, which runs without those CI vars, is never affected).
    if matches!(format, OutputFormat::Text) {
        emit_ci_detected_output(&findings);
    }

    if had_io_error {
        ExitCode::from(1)
    } else if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The analysis pipeline shared by `check` and `baseline generate`: read +
/// parse + lower every file (project pass), then analyze each against the
/// shared project source, applying config `exclude:`/`disable:` and inline
/// `# rigor:disable` suppression. Returns `(findings, had_io_error)` with
/// findings in input order. The baseline filter is NOT applied here — that is
/// the LAST stage, applied only by `check` (reference WD6). `verb` labels the
/// command in error messages (`check` / `baseline`).
/// Resolve the coverage-posture mode (ADR-0036) and bring up the Ruby sidecar
/// folder (ADR-0008) accordingly. `Ok(Some)` = full fidelity; `Ok(None)` = the
/// sound subset (`off`, or `auto` with no reachable sidecar); `Err(code)` = a
/// usage error (64, conflicting env) or the require-but-unavailable hard error
/// (69) — the caller returns it as its exit code. Shared by `check` and
/// `baseline generate` so a baseline records exactly what `check` witnesses.
fn build_sidecar_folder(
    cfg: &Config,
    cli_ruby: Option<ruby_mode::RubyMode>,
) -> Result<Option<sidecar::SidecarFolder>, ExitCode> {
    let ruby = ruby_mode::resolve(cli_ruby, cfg.ruby_config_value(), ruby_mode::RubyMode::Require)
        .map_err(|e| {
            eprintln!("rigor: {e}");
            ExitCode::from(64)
        })?;
    match &ruby {
        ruby_mode::RubyMode::Off => Ok(None),
        mode => {
            let bin = sidecar::ruby_bin_for(mode).expect("a non-off mode names a ruby binary");
            match sidecar::Sidecar::spawn(&bin) {
                Ok(sc) => Ok(Some(sidecar::SidecarFolder::new(sc))),
                Err(e) => {
                    if matches!(mode, ruby_mode::RubyMode::Require | ruby_mode::RubyMode::Path(_)) {
                        eprintln!("rigor: full-fidelity Ruby sidecar required but unavailable — {e}.");
                        eprintln!("  Pass --ruby=off (or set RIGOR_NO_RUBY=1) to run the Ruby-free sound subset.");
                        return Err(ExitCode::from(69));
                    }
                    // `auto`: disclose the reduced posture and run the sound subset.
                    eprintln!(
                        "rigor: Ruby sidecar unavailable ({e}) — running the sound subset (coverage posture: subset)."
                    );
                    Ok(None)
                }
            }
        }
    }
}

fn analyze_files(
    files: &[&str],
    cfg: &Config,
    verb: &str,
    folder: Option<&(dyn rigor_infer::RubyFolder + Sync)>,
) -> (Vec<(usize, String, String, Diagnostic)>, bool) {
    let disable_matcher = cfg.disable_matcher();
    // ADR-25 — config-gated plugins. With no `plugins:` in `.rigor.yml` this is
    // byte-identical to `CoreIndex::new()` (empty list ⇒ default no-config path),
    // so the differential harness + default corpus run are unaffected. A named,
    // bundled plugin (e.g. `activesupport-core-ext`) reopens core classes with
    // its RBS selectors, suppressing the direct calls and enabling chained
    // witnesses — matching the reference, which loads plugins only from config.
    // Optional stage-timing breakdown (§9, "performance prototype" positioning).
    // `RIGOR_TIMING` (any value) prints a one-line per-stage breakdown to stderr
    // — invisible by default, so the differential harness (which never sets it)
    // and the byte-exact output are unaffected. `Instant::now()` is cheap; the
    // markers are unconditional but only formatted/emitted under the env gate.
    let timing = std::env::var_os("RIGOR_TIMING").is_some();
    let t_start = std::time::Instant::now();

    let index = CoreIndex::for_project(&cfg.plugins, &cfg.all_signature_dirs(std::path::Path::new(".")));
    let t_index = std::time::Instant::now();
    // Each entry: (input_order_key, path, source_or_empty, diagnostic).
    let mut findings: Vec<(usize, String, String, Diagnostic)> = Vec::new();
    let mut had_io_error = false;

    // PROJECT PASS (ADR-0023 cross-file): parse+lower EVERY file first, build
    // ONE project-wide SourceIndex, then analyze each file against it. Stages 1
    // (parse+lower) and 3 (analyze) are file-INDEPENDENT and run on a rayon pool
    // (§9, ADR-0006/0028); stage 2 (the project-index build) is the serial
    // barrier between them. Per-file panic isolation (ADR-0016) is preserved at
    // both parallel stages — each closure `catch_unwind`s its own file.
    //
    // Determinism (the parity keystone): each parallel stage collects its
    // outcomes IN INPUT ORDER (`par_iter().map().collect()` preserves the source
    // order into the result Vec), and side effects — the stderr lines and the
    // findings pushes — are replayed by a SEQUENTIAL drain of that ordered Vec.
    // So the stderr stream, the findings order, and the final `sort_by_key` are
    // all byte-identical to the old serial loop; the pool is invisible in output.
    struct Prepared {
        order: usize,
        path: String,
        source: String,
        ast: rigor_parse::LoweredAst,
        comments: Vec<(usize, String)>,
    }

    // STAGE 1 (file-parallel): read + parse + lower. A closure never mutates
    // shared state — it returns a self-contained outcome that the serial drain
    // below turns into the same eprintln / push the serial loop did.
    enum Stage1 {
        Excluded,
        Prepared(Prepared),
        IoError { path: String, msg: String },
        Panic { order: usize, path: String, msg: String },
    }
    let stage1: Vec<Stage1> = files
        .par_iter()
        .enumerate()
        .map(|(order, path)| {
            // Config `exclude:` — skip the file entirely before reading it.
            if cfg.is_excluded(path) {
                return Stage1::Excluded;
            }
            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    return Stage1::IoError { path: path.to_string(), msg: e.to_string() };
                }
            };
            let source_bytes = source.as_bytes().to_vec();
            let lowered = panic::catch_unwind(AssertUnwindSafe(|| {
                let result = parse(&source_bytes);
                let comments = rigor_parse::comment_lines(&result, &source_bytes);
                (lower(&result), comments)
            }));
            match lowered {
                Ok((ast, comments)) => Stage1::Prepared(Prepared {
                    order,
                    path: path.to_string(),
                    source,
                    ast,
                    comments,
                }),
                Err(panic_val) => Stage1::Panic {
                    order,
                    path: path.to_string(),
                    msg: panic_message(&panic_val),
                },
            }
        })
        .collect();

    // Drain stage-1 outcomes in input order: deterministic stderr + findings.
    let mut prepared: Vec<Prepared> = Vec::new();
    for outcome in stage1 {
        match outcome {
            Stage1::Excluded => {}
            Stage1::Prepared(p) => prepared.push(p),
            Stage1::IoError { path, msg } => {
                eprintln!("rigor {verb}: cannot read {path}: {msg}");
                had_io_error = true;
            }
            Stage1::Panic { order, path, msg } => {
                eprintln!("rigor {verb}: internal panic on {path}: {msg}");
                findings.push((order, path, String::new(), internal_error_diag(msg)));
            }
        }
    }

    let t_stage1 = std::time::Instant::now();

    // STAGE 2 (serial barrier): build ONE project-wide source index from all
    // cleanly-lowered ASTs. This is the cross-file join — it must see every AST.
    let asts: Vec<&rigor_parse::LoweredAst> = prepared.iter().map(|p| &p.ast).collect();
    let project_source = rigor_infer::SourceIndex::build_project(&asts, &index);
    let t_stage2 = std::time::Instant::now();

    // STAGE 3 (file-parallel): analyze each file against the shared, now-frozen
    // `index` + `project_source` (read-only, `Sync`) with a FRESH per-file
    // `Interner`. Each closure produces its file's post-suppression findings —
    // and, on a panic, the synthetic internal-error finding plus a DEFERRED
    // stderr line — all order-keyed, so the serial drain replays them in order.
    struct Stage3 {
        findings: Vec<(usize, String, String, Diagnostic)>,
        /// `(path, msg)` for a panic's deferred (in-order) stderr line.
        panic: Option<(String, String)>,
    }
    let stage3: Vec<Stage3> = prepared
        .par_iter()
        .map(|p| {
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                let mut interner = Interner::new();
                analyze_with_source_and_folder(&p.ast, &mut interner, &index, &project_source, folder)
            }));
            match result {
                Ok(diags) => {
                    let with_lines: Vec<(usize, Diagnostic)> = diags
                        .into_iter()
                        .map(|diag| (line_col(&p.source, diag.start_offset).0, diag))
                        .collect();
                    let mut local = Vec::new();
                    for (_line, diag) in rigor_rules::filter_suppressed(with_lines, &p.comments) {
                        // Config `disable:` — drop diagnostics whose rule matches the
                        // expanded disable set (the internal-error sentinel never matches).
                        if disable_matcher.suppresses(diag.rule_id) {
                            continue;
                        }
                        local.push((p.order, p.path.clone(), p.source.clone(), diag));
                    }
                    Stage3 { findings: local, panic: None }
                }
                Err(panic_val) => {
                    let msg = panic_message(&panic_val);
                    let finding =
                        (p.order, p.path.clone(), String::new(), internal_error_diag(msg.clone()));
                    Stage3 { findings: vec![finding], panic: Some((p.path.clone(), msg)) }
                }
            }
        })
        .collect();

    // Drain stage-3 outcomes in input order: deterministic stderr + findings.
    for s3 in stage3 {
        if let Some((path, msg)) = &s3.panic {
            eprintln!("rigor {verb}: internal panic on {path}: {msg}");
        }
        findings.extend(s3.findings);
    }

    let t_stage3 = std::time::Instant::now();

    // Restore input order (stage-1 panics and stage-3 findings interleave by order).
    findings.sort_by_key(|(order, _, _, _)| *order);

    if timing {
        let t_end = std::time::Instant::now();
        eprintln!(
            "rigor timing: index-load={:.3?} stage1(parse+lower)={:.3?} \
             stage2(build_project)={:.3?} stage3(analyze)={:.3?} sort={:.3?} \
             total={:.3?} files={} threads={}",
            t_index - t_start,
            t_stage1 - t_index,
            t_stage2 - t_stage1,
            t_stage3 - t_stage2,
            t_end - t_stage3,
            t_end - t_start,
            prepared.len(),
            rayon::current_num_threads(),
        );
    }
    (findings, had_io_error)
}

// ---------------------------------------------------------------------------
// `rigor baseline` subcommand (ADR-22)
// ---------------------------------------------------------------------------

/// `rigor baseline <subcommand>` — record/inspect the suppression baseline.
///
/// Subcommands (mirroring the reference's surface where cheap):
/// - `generate [--match-mode rule|message] [--output PATH] [--force] <file...>`
///   — write a fresh baseline from a `check` run over the given files.
/// - `dump [--baseline PATH]` — print the contents of an existing baseline.
///
/// `regenerate`/`drift`/`prune` from the reference are NOT yet implemented in
/// this phase (they depend on `configuration.paths`, which rigor-rs's CLI does
/// not yet model); a clear message + exit 2 is reported for them.
fn cmd_baseline(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None | Some("help") | Some("-h") | Some("--help") => {
            print_baseline_help();
            ExitCode::SUCCESS
        }
        Some("generate") => baseline_generate(&args[1..]),
        Some("dump") => baseline_dump(&args[1..]),
        Some(sub @ ("regenerate" | "drift" | "prune")) => {
            eprintln!(
                "rigor baseline: `{sub}` is recognized but not yet implemented in this phase"
            );
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("rigor baseline: unknown subcommand `{other}`");
            print_baseline_help();
            ExitCode::from(64)
        }
    }
}

fn print_baseline_help() {
    eprintln!(
        "Usage: rigor baseline <subcommand> [options]\n\n\
         Subcommands:\n\
         \x20 generate   Write a fresh baseline from a check run over the given files.\n\
         \x20 dump       Print the contents of an existing baseline.\n\n\
         generate options:\n\
         \x20 --match-mode rule|message   Row form: rule (default) or message\n\
         \x20 --output PATH               Write baseline to PATH (default: {DEFAULT_BASELINE_PATH})\n\
         \x20 --force                     Overwrite an existing baseline file\n\
         \x20 --config PATH               Path to .rigor.yml"
    );
}

/// `rigor baseline generate` — run `check` over the files and write a baseline.
fn baseline_generate(args: &[String]) -> ExitCode {
    let mut files: Vec<&str> = Vec::new();
    let mut output = DEFAULT_BASELINE_PATH.to_string();
    let mut mode = MatchMode::Rule;
    let mut force = false;
    let mut explicit_config: Option<&str> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--output" => match it.next() {
                Some(p) => output = p.clone(),
                None => {
                    eprintln!("rigor baseline generate: --output expects a path");
                    return ExitCode::from(64);
                }
            },
            "--match-mode" => match it.next().map(String::as_str) {
                Some("rule") => mode = MatchMode::Rule,
                Some("message") => mode = MatchMode::Message,
                other => {
                    eprintln!(
                        "rigor baseline generate: --match-mode expects `rule` or `message`, got {other:?}"
                    );
                    return ExitCode::from(64);
                }
            },
            "--config" => match it.next() {
                Some(p) => explicit_config = Some(p),
                None => {
                    eprintln!("rigor baseline generate: --config expects a path");
                    return ExitCode::from(64);
                }
            },
            "--force" => force = true,
            other => files.push(other),
        }
    }

    if files.is_empty() {
        eprintln!("rigor baseline generate: expected at least one file");
        return ExitCode::from(64);
    }
    if Path::new(&output).exists() && !force {
        eprintln!(
            "rigor: {output} already exists. Re-run with --force to overwrite."
        );
        return ExitCode::from(64);
    }

    let cfg = Config::load(explicit_config.map(Path::new));
    // Same coverage posture as `check` (ADR-0008/0036) so the baseline records
    // exactly what `check` witnesses — a subset-generated baseline would fail to
    // suppress full-fidelity findings. `baseline` has no `--ruby` flag, so the
    // mode comes from env / `.rigor.yml` / default.
    let sidecar_folder = match build_sidecar_folder(&cfg, None) {
        Ok(f) => f,
        Err(code) => return code,
    };
    let folder_ref =
        sidecar_folder.as_ref().map(|f| f as &(dyn rigor_infer::RubyFolder + Sync));
    // IMPORTANT (reference parity): generate must record the UNFILTERED set —
    // it never applies an existing baseline (`analyze_files` does not), so the
    // new file records the live diagnostics, not the post-baseline (empty) surface.
    let (findings, _had_io_error) = analyze_files(&files, &cfg, "baseline", folder_ref);

    // Key on project-root-relative paths, like the reference's `Dir.pwd`.
    let cwd = std::env::current_dir().ok();
    let entries: Vec<(String, &Diagnostic)> = findings
        .iter()
        .map(|(_, p, _, d)| (relative_path(p, cwd.as_deref()), d))
        .collect();

    let baseline = Baseline::from_diagnostics(&entries, mode);
    if let Err(e) = std::fs::write(&output, baseline.to_yaml()) {
        eprintln!("rigor baseline generate: cannot write {output}: {e}");
        return ExitCode::from(1);
    }
    let mode_str = match mode {
        MatchMode::Rule => "rule",
        MatchMode::Message => "message",
    };
    eprintln!(
        "rigor: wrote baseline to {output} ({} bucket(s) covering {} diagnostic(s); match-mode: {mode_str})",
        baseline.size(),
        entries.len()
    );
    ExitCode::SUCCESS
}

/// `rigor baseline dump` — print an existing baseline's rows.
fn baseline_dump(args: &[String]) -> ExitCode {
    let mut path = DEFAULT_BASELINE_PATH.to_string();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--baseline" => match it.next() {
                Some(p) => path = p.clone(),
                None => {
                    eprintln!("rigor baseline dump: --baseline expects a path");
                    return ExitCode::from(64);
                }
            },
            other => {
                eprintln!("rigor baseline dump: unexpected argument `{other}`");
                return ExitCode::from(64);
            }
        }
    }
    if !Path::new(&path).exists() {
        eprintln!("rigor: baseline file not found: {path}");
        return ExitCode::from(64);
    }
    let baseline = match Baseline::load(Path::new(&path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rigor: baseline load failed: {e}");
            return ExitCode::from(64);
        }
    };
    let mut total = 0usize;
    for b in baseline.buckets() {
        total += b.count;
        match &b.message {
            Some(m) => println!("{}  [{}]  count={}  ~/{m}/", b.file, b.rule, b.count),
            None => println!("{}  [{}]  count={}", b.file, b.rule, b.count),
        }
    }
    println!("Total: {} bucket(s), {total} occurrence(s)", baseline.size());
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Baseline integration (ADR-22)
// ---------------------------------------------------------------------------

/// CLI baseline state for `check`, resolved against config in
/// `resolve_baseline_path` (reference `apply_baseline_filter` precedence).
enum BaselineArg {
    /// No `--baseline`/`--no-baseline` flag — fall through to `.rigor.yml`.
    Unset,
    /// `--baseline PATH` — overrides config.
    Path(String),
    /// `--no-baseline` — ignore any configured baseline for this run.
    Off,
}

/// Resolve the effective baseline path: `--no-baseline` wins (None),
/// then `--baseline PATH`, then `.rigor.yml`'s `baseline:` key.
fn resolve_baseline_path(arg: &BaselineArg, cfg: &Config) -> Option<String> {
    match arg {
        BaselineArg::Off => None,
        BaselineArg::Path(p) => Some(p.clone()),
        BaselineArg::Unset => cfg.baseline_path(),
    }
}

/// Apply the baseline filter to the sorted findings. Loads the baseline; on a
/// load error reports to stderr and continues WITHOUT a baseline (graceful
/// degradation, matching the reference's "continuing without baseline"). The
/// matcher keys each diagnostic on its project-root-relative path, exactly as
/// the reference normalizes `diag.path` against `Dir.pwd`.
fn apply_baseline(
    findings: Vec<(usize, String, String, Diagnostic)>,
    path: &str,
) -> Vec<(usize, String, String, Diagnostic)> {
    let baseline = match Baseline::load(Path::new(path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rigor: baseline load failed: {e} (continuing without baseline)");
            return findings;
        }
    };
    if baseline.is_empty() {
        return findings;
    }

    // Pair each finding with its relative path; internal-error diagnostics
    // (no rule the baseline can address — they have no catalog entry) bypass
    // the filter and always surface, like the reference's `unkeyable` set.
    let cwd = std::env::current_dir().ok();
    let entries: Vec<(String, &Diagnostic)> = findings
        .iter()
        .map(|(_, p, _, d)| (relative_path(p, cwd.as_deref()), d))
        .collect();

    let (surfaced_idx, silenced) = baseline.filter(&entries);
    if silenced > 0 {
        eprintln!("rigor: {silenced} diagnostic(s) silenced by baseline {path}");
    }

    // Keep only the surfaced indices, preserving order.
    let keep: std::collections::HashSet<usize> = surfaced_idx.into_iter().collect();
    findings
        .into_iter()
        .enumerate()
        .filter_map(|(i, f)| if keep.contains(&i) { Some(f) } else { None })
        .collect()
}

/// Normalize a path to project-root-relative (against cwd), matching the
/// reference's `Pathname#relative_path_from(Dir.pwd)`. A path outside the root
/// (or when cwd is unknown) is returned unchanged, as the reference falls back
/// to the original on `ArgumentError`.
fn relative_path(path: &str, cwd: Option<&Path>) -> String {
    let Some(cwd) = cwd else { return path.to_string() };
    let p = Path::new(path);
    let abs = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    match abs.strip_prefix(cwd) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string(),
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

/// Flatten findings into `Rendered` rows (resolve each byte offset to a 1-based
/// line/column) for the CI formatters, then print the rendered document. Mirrors
/// the reference's `write_result` for the `DiagnosticFormats` cases: an empty
/// render (teamcity with no diagnostics) prints nothing; otherwise the document
/// is printed with a trailing newline. ADDITIVE — does not touch text/json.
fn print_rendered(
    findings: &[(usize, String, String, Diagnostic)],
    render: fn(&[Rendered]) -> String,
) {
    let rows = to_rendered(findings);
    let output = render(&rows);
    if !output.is_empty() {
        println!("{output}");
    }
}

/// Resolve each finding's byte offset to a 1-based (line, column) and project
/// the fields the CI formatters read. `rule_id` is rigor-rs's qualified rule
/// (the `builtin` family is kept bare in `rule_id`).
fn to_rendered(findings: &[(usize, String, String, Diagnostic)]) -> Vec<Rendered<'_>> {
    findings
        .iter()
        .map(|(_order, path, source, diag)| {
            let (line, column) = line_col(source, diag.start_offset);
            Rendered {
                path,
                line,
                column,
                severity: diag.severity,
                rule_id: diag.rule_id,
                message: &diag.message,
            }
        })
        .collect()
}

/// CI auto-detection augmentation (ADR-51 WD7), called only for `--format text`.
/// For a stdout-native CI (GitHub Actions → `github`, TeamCity → `teamcity`) the
/// platform's annotations are emitted on top of the human output; for GitLab
/// (artifact-based) and reviewdog-routed CIs a one-line hint goes to stderr when
/// there are diagnostics. No-op when no CI is detected or detection is disabled.
fn emit_ci_detected_output(findings: &[(usize, String, String, Diagnostic)]) {
    let Some(platform) = ci_detector::detect() else {
        return;
    };
    match platform.tier {
        ci_detector::Tier::NativeStdout => {
            // Render in the platform's native stdout format on top of the text.
            let rows = to_rendered(findings);
            let output = match platform.format {
                Some("github") => render_github_string(&rows),
                Some("teamcity") => diagnostic_formats::render_teamcity(&rows),
                _ => String::new(),
            };
            if !output.is_empty() {
                println!("{output}");
            }
        }
        ci_detector::Tier::NativeArtifact | ci_detector::Tier::Reviewdog => {
            if !findings.is_empty() {
                eprintln!("{}", ci_detected_hint(&platform));
            }
        }
    }
}

/// The stderr hint for a CI rigor can't auto-emit to stdout (GitLab artifact /
/// reviewdog-routed), mirroring the reference's `ci_detected_hint`.
fn ci_detected_hint(platform: &ci_detector::Platform) -> String {
    let tail = "see `rigor skill rigor-ci-setup`";
    match platform.tier {
        ci_detector::Tier::NativeArtifact => format!(
            "rigor: {} detected — for the inline report run \
             `rigor check --format {}` and publish it as the platform's report artifact ({tail}).",
            platform.name,
            platform.format.unwrap_or("gitlab"),
        ),
        _ => format!(
            "rigor: {} detected — Rigor has no native format for it; pipe \
             `rigor check --format checkstyle` through reviewdog, or use `--format junit` ({tail}).",
            platform.name,
        ),
    }
}

/// Build the GitHub Actions annotation block as a string (the same lines
/// `print_github` prints), so CI auto-detection can emit it on top of text.
fn render_github_string(rows: &[Rendered]) -> String {
    rows.iter()
        .map(|r| {
            let level = match r.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "notice",
            };
            format!(
                "::{level} file={},line={},col={}::{}",
                gh_escape_prop(r.path),
                r.line,
                r.column,
                gh_escape_data(r.message),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
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
/// `Github`/`Sarif`/`Gitlab`/`Checkstyle`/`Junit`/`Teamcity` are ADDITIVE
/// CI-oriented formats (reference ADR-51) — they do not affect the text/json
/// output the differential harness depends on.
#[derive(Clone, Copy)]
enum OutputFormat {
    Text,
    Json,
    Github,
    Sarif,
    Gitlab,
    Checkstyle,
    Junit,
    Teamcity,
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
