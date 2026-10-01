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
use rigor_parse::{lower_with_key, parse, FileKey};
use rigor_rules::{analyze_with_source_and_folder, catalog, Diagnostic, Severity};
use rigor_types::Interner;

mod config;
use config::Config;
mod annotate;
mod coverage;
mod bundler;
mod bleeding_edge;
mod config_audit;
mod conformance_gate;
mod diff;
mod triage;
mod type_display;
mod ci_detector;
mod diagnostic_formats;
use diagnostic_formats::Rendered;
mod baseline;
use baseline::{Baseline, Bucket, DriftStatus, MatchMode, DEFAULT_BASELINE_PATH};
mod optparse;
use optparse::{Item, Value};
mod docs;
mod doctor;
mod effects;
mod explain;
mod init;
mod lsp;
mod mcp;
mod outline;
mod plugins_cmd;
mod rbs_collection;
mod ruby_mode;
mod severity;
mod sidecar;
mod sig_gen;
mod type_of;

/// The reference's full subcommand surface (ADR-0015).
const COMMANDS: &[&str] = &[
    "check", "annotate", "type-of", "trace", "type-scan", "explain", "diff",
    "sig-gen", "baseline", "triage", "coverage", "plugins", "plugin", "lsp",
    "mcp", "skill", "docs", "init", "doctor", "version", "show-bleedingedge",
    "effects", "unused", "describe", "playground", "upgrade",
];

/// The reference's `CLI#help` heredoc (`lib/rigor/cli.rb`), verbatim — the
/// text `rigor`, `rigor help`, `rigor -h` and `rigor --help` print to stdout
/// and the tail of the `Unknown command:` stderr surface.
const TOP_HELP: &str = "\
Usage: rigor <command> [options]

Commands:
  check      Analyze Ruby source files
  init       Create a starter .rigor.yml
  annotate   Print FILE with each line's last-expression type
  type-of    Print inferred types at FILE:LINE[:COL] positions
  trace      Replay how the engine typed FILE as a terminal animation
  type-scan  Report Scope#type_of coverage across PATHs
  effects    Report each method's effect labels, and the committed effect snapshot
             (opt-in; effects update/check/diff/explain)
  explain    Print the description of one or all CheckRules
  diff       Compare current diagnostics to a saved baseline JSON
  baseline   Manage the baseline file (baseline generate/regenerate/dump/drift/prune)
  sig-gen    Emit RBS skeletons inferred from .rb sources
  lsp        Run the Rigor Language Server (LSP) over stdio
  mcp        Run the Rigor MCP server over stdio
  triage     Summarise diagnostics: distribution, hotspots, hints
  coverage   Report type-precision coverage (precise vs Dynamic ratio)
  unused     Report unreferenced classes, modules and constants as removal candidates
  plugins    Report activation status of every configured plugin
  plugin     Browse bundled plugin source as worked examples (list/path/print/root)
  playground Start the browser playground (requires rigor-playground gem)
  describe   Recommend the next skill for this project (alias for `skill describe`)
  skill      Recommend the next skill + list/print bundled Agent Skills (skill describe, skill <name>)
  docs       Print the bundled docs offline (docs <name>, docs --list)
  show-bleedingedge  Show the bleeding-edge overlay + what your config adopts
  doctor     Classify setup problems vs clean run with routed next actions
  upgrade    Migration command skeleton (queued)
  version    Print the Rigor version
  help       Print this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // `CLI#run`: no command, `help`, `-h`, `--help` → the command list on
        // STDOUT, exit 0.
        None | Some("help" | "-h" | "--help") => {
            print!("{TOP_HELP}");
            ExitCode::SUCCESS
        }
        // `version` / `-v` / `--version` — print `rigor <version>` and exit 0
        // (`rigor #{Rigor::VERSION}` upstream). `-V` is the conventional Rust
        // short flag; accepted alongside the reference's `-v`.
        Some("version" | "--version" | "-v" | "-V") => {
            println!("rigor {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("check") => cmd_check(&args[1..]),
        Some("baseline") => cmd_baseline(&args[1..]),
        Some("type-of") => type_of::cmd_type_of(&args[1..]),
        Some("diff") => diff::cmd_diff(&args[1..]),
        Some("triage") => triage::cmd_triage(&args[1..]),
        Some("annotate") => annotate::cmd_annotate(&args[1..]),
        Some("sig-gen") => sig_gen::cmd_sig_gen(&args[1..]),
        Some("explain") => explain::cmd_explain(&args[1..]),
        Some("init") => init::cmd_init(&args[1..]),
        Some("doctor") => doctor::cmd_doctor(&args[1..]),
        Some("show-bleedingedge") => bleeding_edge::cmd_show_bleedingedge(&args[1..]),
        Some("plugins") => plugins_cmd::cmd_plugins(&args[1..]),
        Some("docs") => docs::cmd_docs(&args[1..]),
        Some("lsp") => lsp::cmd_lsp(&args[1..]),
        Some("mcp") => mcp::cmd_mcp(&args[1..]),
        Some("coverage") => coverage::cmd_coverage(&args[1..]),
        // ADR-0043 slice 2 — the effect-summary REPORT. Observational: it
        // shares no state with `check` and consults no inference.
        Some("effects") => effects::cmd_effects(&args[1..]),
        // Commands upstream owns an OptionParser table for but the port does
        // not implement: parse argv against the reference's table first — an
        // unknown flag is `invalid option:` + 64 and `--help` prints the
        // reference surface — before the deferred stub reports itself.
        Some("trace") => cmd_deferred("trace", &TRACE_PARSER, &args[1..]),
        Some("type-scan") => cmd_deferred("type-scan", &TYPE_SCAN_PARSER, &args[1..]),
        Some("unused") => cmd_unused(&args[1..]),
        // `run_upgrade`: ignores argv entirely — prints the queued notice and
        // exits 0 upstream.
        Some("upgrade") => {
            println!("rigor upgrade: No migration target available yet (ADR-50 WD7, queued).");
            println!("Current version: {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        // `run_playground`: the separate `rigor-playground` gem can never load
        // for a standalone binary — the reference's LoadError surface.
        Some("playground") => {
            eprintln!("rigor playground requires the rigor-playground gem.");
            eprintln!("Install it with: gem install rigor-playground");
            ExitCode::from(64)
        }
        // Manual-dispatch commands (no OptionParser upstream): reproduce the
        // reference's grammar-level errors — an unrecognised argv.first is a
        // subcommand / doc-name error, never a deferred stub and never a path.
        Some("plugin") => cmd_plugin_deferred(&args[1..]),
        Some("skill") => cmd_skill_deferred(&args[1..]),
        Some("describe") => cmd_describe_deferred(&args[1..]),
        Some(cmd) if COMMANDS.contains(&cmd) => {
            eprintln!("rigor-rs: `{cmd}` is recognized but not yet implemented in this phase");
            ExitCode::from(2)
        }
        // `CLI#dispatch`'s unknown-command path: the message and the full help
        // text on STDERR, exit 64 (`EXIT_USAGE`).
        Some(other) => {
            eprintln!("Unknown command: {other}");
            eprint!("{TOP_HELP}");
            ExitCode::from(64)
        }
    }
}

/// `rigor check [--format text|json] <path...>` — analyze each file or directory
/// (a directory expands to its `**/*.rb`, ADR-0040) and print
/// its diagnostics. Exit 1 if any ERROR-severity diagnostic is found (a
/// warning-only run exits 0, ADR-0040), 64 on a usage error (ADR-0030 exit codes).
/// The `check` switch table — `parse_check_options`' `opts.on` calls in
/// declaration order (reference `cli/check_command.rb`), which feeds
/// abbreviation resolution, `Did you mean?` and `--help`. `--ruby`/`--no-ruby`
/// are rigor-rs-only (ADR-0036): registered last and `hidden` so upstream
/// help/completion/candidate output stays byte-identical while they keep
/// parsing (`--ruby=MODE` and `--ruby MODE`).
const CHECK_SWITCHES: &[optparse::Switch] = &[
    optparse::Switch::new("config", &[("config", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
    optparse::Switch::new("format", &[("format", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--format", "=FORMAT", &["Output format: text, json, sarif, github, gitlab, checkstyle, junit, teamcity"]),
    optparse::Switch::new("explain", &[("explain", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--explain", "", &["Surface fail-soft fallback events as :info diagnostics"]),
    optparse::Switch::new("cache-stats", &[("cache-stats", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--cache-stats", "", &["Print on-disk cache inventory at end of run"]),
    optparse::Switch::new("coverage", &[("coverage", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--coverage", "", &["Add a type-precision coverage block (an extra precision pass over the analyzed files)"]),
    optparse::Switch::new("clear-cache", &[("clear-cache", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--clear-cache", "", &["Remove the .rigor/cache directory before running"]),
    optparse::Switch::new("no-cache", &[("no-cache", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--no-cache", "", &["Disable the persistent cache for this run"]),
    optparse::Switch::new("stats", &[("stats", false), ("no-stats", true)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--[no-]stats", "", &["Print run summary (files, classes, memory, wall time) to stderr (default: on)"]),
    optparse::Switch::new("workers", &[("workers", false)], optparse::ArgStyle::Required, optparse::ValueKind::Int, "--workers", "=N", &["Dispatch per-file analysis across N Ractor workers (default: 0; sequential)"]),
    optparse::Switch::new("tmp-file", &[("tmp-file", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--tmp-file", "=PATH", &["Editor mode: read source bytes from PATH instead of --instead-of (paired)"]),
    optparse::Switch::new("instead-of", &[("instead-of", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--instead-of", "=PATH", &["Editor mode: the logical project path the buffer represents (paired with --tmp-file)"]),
    optparse::Switch::new("baseline", &[("baseline", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--baseline", "=PATH", &["load baseline from PATH (overrides .rigor.yml `baseline:`)"]),
    optparse::Switch::new("no-baseline", &[("no-baseline", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--no-baseline", "", &["ignore any configured baseline for this run"]),
    optparse::Switch::new("baseline-strict", &[("baseline-strict", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--baseline-strict", "", &["fail the run on any baseline drift (CI gate)"]),
    optparse::Switch::new("fail-on", &[("fail-on", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--fail-on", "=SEVERITY", &["exit non-zero on a diagnostic at or above SEVERITY: error (default), warning, or info"]),
    optparse::Switch::new("treat-all-as-inline-rbs", &[("treat-all-as-inline-rbs", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--treat-all-as-inline-rbs", "", &["force-load rigor-rbs-inline with require_magic_comment: false"]),
    optparse::Switch::new("verify-incremental", &[("verify-incremental", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--verify-incremental", "", &["assert incremental analysis matches a full run, then exit"]),
    optparse::Switch::new("incremental", &[("incremental", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--incremental", "", &["re-analyze only files changed since the last run (cross-process cache)"]),
    optparse::Switch::new("no-ci-detect", &[("no-ci-detect", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--no-ci-detect", "", &["do not auto-emit CI-native output when a CI environment is detected"]),
    optparse::Switch::new("bleeding-edge", &[("bleeding-edge", false)], optparse::ArgStyle::Optional, optparse::ValueKind::Raw, "--bleeding-edge", "=[LIST]", &["adopt the bleeding-edge overlay for this run (all features, or a comma-separated feature-id list)"]),
    optparse::Switch::new("no-bleeding-edge", &[("no-bleeding-edge", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--no-bleeding-edge", "", &["ignore any configured bleeding_edge: selection for this run"]),
    optparse::Switch::new("no-tolerated-effects", &[("no-tolerated-effects", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--no-tolerated-effects", "", &["check effect envelopes as if effects.tolerated: were empty"]),
    // rigor-rs-only (ADR-0036): not in the reference table, so hidden from
    // `--help` / completion / `Did you mean?` — still parseable.
    optparse::Switch::hidden("ruby", &[("ruby", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw),
    optparse::Switch::hidden("no-ruby", &[("no-ruby", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw),
];

const CHECK_PARSER: optparse::OptParser =
    optparse::OptParser::new("Usage: rigor check [options] [paths]", CHECK_SWITCHES);

/// The `rigor: <flag> is not supported by rigor-rs` surface for reference-only
/// flags the port cannot reproduce — `ParseError`-shaped (stderr, exit 64) but
/// the message names the port gap, not a parser failure.
fn check_unsupported(flag: &str) -> ExitCode {
    eprintln!("rigor: {flag} is not supported by rigor-rs");
    ExitCode::from(64)
}

// ---------------------------------------------------------------------------
// Deferred commands (ADR-0015): real commands upstream, unimplemented here.
// Their OptionParser tables still run — parse errors surface identically
// (`invalid option:` + 64, abbreviations, `--`, POSIXLY_CORRECT) and `--help`
// prints the reference's table — before the stub's exit-2 message.
// ---------------------------------------------------------------------------

const TRACE_PARSER: optparse::OptParser = optparse::OptParser::new(
    "Usage: rigor trace [options] FILE",
    &[
        optparse::Switch::new("format", &[("format", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--format", "=FORMAT", &["Output format: text (animation) or json (raw event stream)"]),
        optparse::Switch::new("delay", &[("delay", false)], optparse::ArgStyle::Required, optparse::ValueKind::Float, "--delay", "=SECONDS", &["Autoplay with SECONDS between frames (default: step on key press)"]),
        optparse::Switch::new("line", &[("line", false)], optparse::ArgStyle::Required, optparse::ValueKind::Int, "--line", "=N", &["Only replay events whose source range starts on line N"]),
        optparse::Switch::new("verbose", &[("verbose", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--verbose", "", &["Include every expression enter/result frame"]),
        optparse::Switch::new("config", &[("config", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
    ],
);

const TYPE_SCAN_PARSER: optparse::OptParser = optparse::OptParser::new(
    "Usage: rigor type-scan [options] PATH...",
    &[
        optparse::Switch::new("format", &[("format", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--format", "=FORMAT", &["Output format: text or json"]),
        optparse::Switch::new("config", &[("config", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
        optparse::Switch::new("limit", &[("limit", false)], optparse::ArgStyle::Required, optparse::ValueKind::Int, "--limit", "=N", &["Max example events to print (text only)"]),
        optparse::Switch::new("show-recognized", &[("show-recognized", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--show-recognized", "", &["Include classes with 0 unrecognized in the table"]),
        optparse::Switch::new("threshold", &[("threshold", false)], optparse::ArgStyle::Required, optparse::ValueKind::Float, "--threshold", "=RATIO", &["Exit non-zero when unrecognized/visits > RATIO"]),
    ],
);

const UNUSED_PARSER: optparse::OptParser = optparse::OptParser::new(
    "Usage: rigor unused [options] [paths]",
    &[
        optparse::Switch::new("config", &[("config", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
        optparse::Switch::new("format", &[("format", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--format", "=FORMAT", &["Output format: text (default) or json"]),
        optparse::Switch::new("entry-point", &[("entry-point", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--entry-point", "=GLOB", &["Treat declarations in files matching GLOB as roots (repeatable)"]),
        optparse::Switch::new("limit", &[("limit", false)], optparse::ArgStyle::Required, optparse::ValueKind::Int, "--limit", "=N", &["Print at most N candidates (default: all)"]),
        optparse::Switch::new("incremental", &[("incremental", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--incremental", "", &["(unsupported)"]),
    ],
);

/// Parse `args` against the deferred command's reference table, then report
/// the command as unimplemented. Parse exits (--help, invalid option) are
/// honoured first.
fn cmd_deferred(name: &str, parser: &optparse::OptParser, args: &[String]) -> ExitCode {
    match parser.parse(args).items_or_exit() {
        Ok(_) => {
            eprintln!("rigor-rs: `{name}` is recognized but not yet implemented in this phase");
            ExitCode::from(2)
        }
        Err(code) => code,
    }
}

/// `rigor unused` — deferred, with the reference's one semantic refusal kept:
/// `--incremental` is a usage error upstream even though the command's run
/// body is unported.
fn cmd_unused(args: &[String]) -> ExitCode {
    let items = match UNUSED_PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };
    if items
        .iter()
        .any(|i| matches!(i, Item::Opt { key: "incremental", .. }))
    {
        // `usage_error` in UnusedCommand — verbatim message, exit 64.
        eprintln!(
            "rigor unused does not support --incremental: reachability is only sound over a \
             whole-project run, so an incremental pass would report constants as unused merely \
             because the files that reference them were served from cache. Re-run without \
             --incremental."
        );
        return ExitCode::from(64);
    }
    eprintln!("rigor-rs: `unused` is recognized but not yet implemented in this phase");
    ExitCode::from(2)
}

/// `PluginCommand::USAGE` — verbatim; `--help`/`help` print it on stdout.
const PLUGIN_USAGE: &str = r#"Usage: rigor plugin <subcommand> [args]

Browse the plugins bundled in the rigortype toolchain (worked
examples for authoring your own). For the activation status of
the plugins in your .rigor.yml, use `rigor plugins` (plural).

Subcommands:
  list                  List bundled + example plugins (default)
  path  <name>          Print the absolute directory path of <name>
  print <name>          Print <name>'s main lib source, with a header
  root                  Print the gem root + key subdirectories

Examples:
  rigor plugin list
  rigor plugin path  rigor-activerecord
  rigor plugin print rigor-activesupport-core-ext
  rigor plugin root
"#;

/// `SkillCommand::USAGE` — verbatim; `--help`/`help` print it on stdout.
const SKILL_USAGE: &str = r#"Usage: rigor skill [<name>] [--full <name>] [--path <name>] [--list] [--describe]

With no argument, lists the bundled skills.

  rigor skill                List bundled skills
  rigor skill <name>         Print the SKILL.md body for <name> (with a header)
  rigor skill --full <name>  Print the SKILL.md body AND its references/ inline
                             (the complete, version-current procedure in one call)
  rigor skill --path <name>  Print the absolute path of the SKILL.md file for <name>
  rigor skill --list         List bundled skills (name + absolute path)
  rigor skill --describe     Report project state + recommend the next skill to run
                             (presence-only probe — never runs `rigor check`)
  rigor skill describe --deep
                             Same, but run `rigor check` first and route the
                             recommendation on its result (slow; writes the cache)

Examples:
  rigor skill
  rigor skill rigor-project-init
  rigor skill --full rigor-baseline-reduce
  rigor skill --path rigor-baseline-reduce
  rigor skill --describe        (also: rigor describe)
  rigor skill describe --deep   (also: rigor describe --deep)
"#;

/// `rigor plugin` — deferred. `PluginCommand#run` is manual dispatch: argv[0]
/// is always a subcommand (default `list`), so an unrecognised one is
/// "Unknown subcommand: X" + USAGE on stderr, exit 64 — not an option error.
/// `path`/`print` then shift argv[1] as the plugin *name* — dashed tokens
/// included: a missing name is `usage_error` (exit 64), an unresolved one is
/// `name_error` (exit 1). The resolved bodies stay deferred (no bundled
/// plugin tree).
fn cmd_plugin_deferred(args: &[String]) -> ExitCode {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" | "root" => {
            eprintln!("rigor-rs: `plugin` is recognized but not yet implemented in this phase");
            ExitCode::from(2)
        }
        "path" | "print" => match args.get(1).map(String::as_str) {
            // `usage_error` — the message names the verb, then USAGE.
            None => {
                eprintln!("`{sub}` requires a plugin name");
                eprint!("{PLUGIN_USAGE}");
                ExitCode::from(64)
            }
            Some(name) if plugin_name_known(name) => {
                eprintln!("rigor-rs: `plugin` is recognized but not yet implemented in this phase");
                ExitCode::from(2)
            }
            // `name_error` — "Unknown plugin: X" + the list pointer.
            Some(name) => {
                eprintln!("Unknown plugin: {name}");
                eprintln!("Run `rigor plugin list` to see the bundled plugins.");
                ExitCode::from(1)
            }
        },
        "-h" | "--help" | "help" => {
            print!("{PLUGIN_USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("Unknown subcommand: {other}");
            eprint!("{PLUGIN_USAGE}");
            ExitCode::from(64)
        }
    }
}

/// `PluginCommand#find` over the reference's bundled `plugins/` + `examples/`
/// trees: a name resolves on an exact match or with the conventional `rigor-`
/// prefix dropped from either side. The plugin bodies are deferred; this list
/// exists only so the unknown-plugin error surface matches the reference.
fn plugin_name_known(name: &str) -> bool {
    let stripped = name.strip_prefix("rigor-").unwrap_or(name);
    BUNDLED_PLUGIN_NAMES
        .iter()
        .any(|p| p.strip_prefix("rigor-").unwrap_or(p) == stripped)
}

/// `discover(PLUGINS_ROOT) + discover(EXAMPLES_ROOT)` at the pinned ref —
/// production plugins first, then the tutorial examples.
const BUNDLED_PLUGIN_NAMES: &[&str] = &[
    "rigor-actioncable",
    "rigor-actionmailer",
    "rigor-actionpack",
    "rigor-active-model-serializers",
    "rigor-activejob",
    "rigor-activerecord",
    "rigor-activestorage",
    "rigor-activesupport-core-ext",
    "rigor-devise",
    "rigor-dry-monads",
    "rigor-dry-schema",
    "rigor-dry-struct",
    "rigor-dry-types",
    "rigor-dry-validation",
    "rigor-ethon",
    "rigor-factorybot",
    "rigor-ffi",
    "rigor-ffi-rzmq",
    "rigor-grape",
    "rigor-graphql",
    "rigor-hanami",
    "rigor-mangrove",
    "rigor-minitest",
    "rigor-pundit",
    "rigor-rails",
    "rigor-rails-i18n",
    "rigor-rails-routes",
    "rigor-railties",
    "rigor-rbnacl",
    "rigor-rbs-inline",
    "rigor-rspec",
    "rigor-rspec-rails",
    "rigor-sassc",
    "rigor-shoulda-matchers",
    "rigor-sidekiq",
    "rigor-sinatra",
    "rigor-sorbet",
    "rigor-statesman",
    "rigor-typescript-utility-types",
    "rigor-deprecations",
    "rigor-lisp-eval",
    "rigor-pattern",
    "rigor-routes",
    "rigor-units",
    "rigor-web",
];

/// `rigor skill` — deferred. `SkillCommand#run` is manual dispatch like
/// `docs`: argv[0] is a skill *name* unless it is one of the grammar words, so
/// an unrecognised token is `name_error` ("Unknown skill: X" + list, exit 1).
/// `--full`/`--path`/`--print` then shift argv[1] as the name — dashed tokens
/// included: a missing name is `usage_error` (message differs per flag,
/// exit 64). `describe`/`--describe` routes to `run_describe`, which refuses
/// any trailing token as `unknown option for \`describe\`: X` (usage_error,
/// exit 64).
fn cmd_skill_deferred(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None | Some("--list") => {
            eprintln!("rigor-rs: `skill` is recognized but not yet implemented in this phase");
            ExitCode::from(2)
        }
        Some("-h" | "--help" | "help") => {
            print!("{SKILL_USAGE}");
            ExitCode::SUCCESS
        }
        Some("describe" | "--describe") => cmd_describe_args(&args[1..]),
        Some(flag @ ("--full" | "--path" | "--print")) => {
            // `usage_error` — each flag names itself except `--print`, which
            // shares `run_print`'s positional-slot message.
            let missing = match flag {
                "--full" => "`--full` requires a skill name",
                "--path" => "`--path` requires a skill name",
                _ => "a skill name is required",
            };
            match args.get(1).map(String::as_str) {
                None => {
                    eprintln!("{missing}");
                    eprint!("{SKILL_USAGE}");
                    ExitCode::from(64)
                }
                Some(name) => cmd_skill_named(name),
            }
        }
        Some(name) => cmd_skill_named(name),
    }
}

/// The resolved-name slot shared by `run_print`/`run_path`/`run_full`: a
/// bundled skill's body is deferred; anything else is `name_error` —
/// "Unknown skill: X" + the bundled-skill list, exit 1.
fn cmd_skill_named(name: &str) -> ExitCode {
    if BUNDLED_SKILL_NAMES.contains(&name) {
        // `run_print` on a real skill — the SKILL.md bodies are the
        // deferred content, not the name resolution.
        eprintln!("rigor-rs: `skill` is recognized but not yet implemented in this phase");
        return ExitCode::from(2);
    }
    // The skills corpus itself is deferred (SKILL.md bodies aren't
    // shipped), but the name list is stable upstream data, so the
    // error surface stays byte-identical.
    eprintln!("Unknown skill: {name}");
    eprintln!("Available skills (try `rigor skill --list`):");
    for s in BUNDLED_SKILL_NAMES {
        eprintln!("  {s}");
    }
    ExitCode::from(1)
}

/// `SkillCommand#discover_skills` over the reference's bundled `skills/` tree —
/// the names `name_error` prints, in directory order. The SKILL.md bodies are
/// not shipped in the standalone build; this list exists only so the unknown-
/// skill error surface matches the reference byte-for-byte.
const BUNDLED_SKILL_NAMES: &[&str] = &[
    "rigor-ask",
    "rigor-baseline-reduce",
    "rigor-ci-setup",
    "rigor-doctor",
    "rigor-editor-setup",
    "rigor-mcp-setup",
    "rigor-monkeypatch-resolve",
    "rigor-next-steps",
    "rigor-plugin-author",
    "rigor-plugin-review",
    "rigor-plugin-tune",
    "rigor-project-init",
    "rigor-protection-uplift",
    "rigor-rbs-setup",
    "rigor-type-oracle",
    "rigor-unused-adjudicate",
    "rigor-upgrade",
];

/// `rigor describe` — `run_describe` upstream wraps `skill describe`: argv
/// becomes `["describe", *argv]`.
fn cmd_describe_deferred(args: &[String]) -> ExitCode {
    cmd_describe_args(args)
}

/// `SkillCommand#run_describe` argument handling: `--deep` tokens are deleted,
/// then any remaining argv is a usage error; an empty remainder is the
/// (unported) describe run itself.
fn cmd_describe_args(args: &[String]) -> ExitCode {
    let rest: Vec<&String> = args.iter().filter(|a| a.as_str() != "--deep").collect();
    if let Some(unknown) = rest.first() {
        eprintln!("unknown option for `describe`: {unknown}");
        eprint!("{SKILL_USAGE}");
        return ExitCode::from(64);
    }
    eprintln!("rigor-rs: `describe` is recognized but not yet implemented in this phase");
    ExitCode::from(2)
}

/// `rigor check [--format text|json] <path...>` — analyze each file or directory
/// (a directory expands to its `**/*.rb`, ADR-0040) and print
/// its diagnostics. Exit 1 if any ERROR-severity diagnostic is found (a
/// warning-only run exits 0, ADR-0040), 64 on a usage error (ADR-0030 exit codes).
fn cmd_check(args: &[String]) -> ExitCode {
    let items = match CHECK_PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };

    // `--format` stays a raw string until output time — the reference stores
    // it verbatim and only raises `invalid argument: unsupported format: X`
    // inside `write_result`, AFTER the analysis ran.
    let mut format = String::from("text");
    let mut files: Vec<String> = Vec::new();
    let mut explicit_config: Option<String> = None;
    // ADR-22 baseline resolution (mirrors the reference's precedence in
    // `apply_baseline_filter`): `--no-baseline` (Off) > `--baseline PATH`
    // (Path) > `.rigor.yml`'s `baseline:` (Unset → config).
    let mut baseline_arg = BaselineArg::Unset;
    // ADR-0036 coverage-posture axis (CLI layer). `--ruby` and `--no-ruby` are
    // mutually exclusive; the effective mode is resolved (CLI > env > config >
    // default `require`) after config load.
    let mut ruby_cli: Option<ruby_mode::RubyMode> = None;
    let mut no_ruby_flag = false;
    // ADR-22 slice 5 — the `--baseline-strict` CI gate.
    let mut baseline_strict = false;
    // ADR-50 WD2 — the `--bleeding-edge[=LIST]` / `--no-bleeding-edge` CLI
    // mirror of the `bleeding_edge:` config key.
    let mut bleeding_edge_cli: Option<config::BleedingEdgeSelector> = None;
    // Issue #812 — `--fail-on=SEVERITY` (`:error` default). Stored raw;
    // `valid_fail_on_option?` validates it post-parse, before config load.
    let mut fail_on: Option<String> = None;
    // ADR-51 WD7 — `--no-ci-detect` (the reference default is on).
    let mut ci_detect = true;
    // Editor mode (reference-only): collected for the upstream pairing check,
    // then rejected — the port has no buffer-binding path.
    let mut tmp_file: Option<String> = None;
    let mut instead_of: Option<String> = None;
    // Whether `--workers` was given — the env-var fallback's gate.
    let mut workers_cli = false;
    // The first unsupported flag in argv order, reported after the semantic
    // validations the reference runs post-parse.
    let mut unsupported: Option<&'static str> = None;

    for item in &items {
        match item {
            Item::Positional(p) => files.push(p.clone()),
            Item::Opt { key, value, .. } => match *key {
                "config" => explicit_config = Some(value.as_ref().unwrap().as_str().to_string()),
                "format" => format = value.as_ref().unwrap().as_str().to_string(),
                // Accepted no-ops — the semantics trivially hold for a port
                // with no persistent cache and no run-stats output.
                "no-cache" | "clear-cache" | "stats" => {}
                // Sequential-only: the worker pool is a performance axis with
                // identical output, so the (Integer-validated) value is a
                // no-op — but its presence suppresses the
                // `RIGOR_RACTOR_WORKERS` read (`resolve_workers`: flag wins).
                "workers" => workers_cli = true,
                "tmp-file" => tmp_file = Some(value.as_ref().unwrap().as_str().to_string()),
                "instead-of" => {
                    instead_of = Some(value.as_ref().unwrap().as_str().to_string())
                }
                "baseline" => {
                    baseline_arg = BaselineArg::Path(value.as_ref().unwrap().as_str().to_string())
                }
                "no-baseline" => baseline_arg = BaselineArg::Off,
                "baseline-strict" => baseline_strict = true,
                "fail-on" => {
                    fail_on = Some(value.as_ref().unwrap().as_str().to_lowercase())
                }
                // Reference-only features the port cannot reproduce — rejected
                // below, after the validations the reference runs first.
                "explain" | "cache-stats" | "coverage" | "treat-all-as-inline-rbs"
                | "verify-incremental" | "incremental" | "no-tolerated-effects" => {
                    if unsupported.is_none() {
                        unsupported = Some(*key);
                    }
                }
                "no-ci-detect" => ci_detect = false,
                "bleeding-edge" => {
                    bleeding_edge_cli = Some(match value {
                        // A bare `--bleeding-edge` (optional argument absent)
                        // adopts the whole overlay — the reference's
                        // `value.nil? || <list>` fold.
                        None => config::BleedingEdgeSelector::All { except: Vec::new() },
                        Some(Value::Str(list)) => config::BleedingEdgeSelector::List(
                            // `--bleeding-edge=` (empty) is `[]` upstream —
                            // "adopt only these ids" with none listed — NOT
                            // the bare-flag `true`. `List([])` activates
                            // nothing, matching.
                            list.split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                                .collect(),
                        ),
                        _ => unreachable!("Raw-valued switch"),
                    });
                }
                "no-bleeding-edge" => {
                    bleeding_edge_cli = Some(config::BleedingEdgeSelector::None);
                }
                "ruby" => {
                    ruby_cli = Some(ruby_mode::parse_value(value.as_ref().unwrap().as_str()))
                }
                "no-ruby" => no_ruby_flag = true,
                _ => unreachable!("the switch table is closed"),
            },
        }
    }

    // `--fail-on` is validated post-parse — BEFORE the buffer-binding check and
    // config load, exactly where `valid_fail_on_option?` sits in `run`.
    let fail_on = match fail_on.as_deref() {
        None => Severity::Error,
        Some("error") => Severity::Error,
        Some("warning") => Severity::Warning,
        Some("info") => Severity::Info,
        Some(v) => {
            eprintln!(
                "rigor: invalid --fail-on value: {v} (expected error, warning, or info)"
            );
            return ExitCode::from(64);
        }
    };

    // `Options.resolve_buffer_binding`: the pair must appear together; an
    // existing-but-unsupported pair is the port gap the reference never
    // reaches.
    match (tmp_file.is_some(), instead_of.is_some()) {
        (true, false) | (false, true) => {
            eprintln!("--tmp-file and --instead-of must appear together");
            return ExitCode::from(64);
        }
        (true, true) => {
            let tmp = tmp_file.as_deref().unwrap_or_default();
            if !Path::new(tmp).is_file() {
                eprintln!("--tmp-file {tmp:?}: no such file or not readable");
                return ExitCode::from(64);
            }
            return check_unsupported("--tmp-file/--instead-of");
        }
        (false, false) => {}
    }

    if let Some(flag) = unsupported {
        return check_unsupported(&format!("--{flag}"));
    }

    // ADR-0036 same-layer mutual exclusion: `--ruby` and `--no-ruby` together is
    // a usage error, redundant or not.
    if ruby_cli.is_some() && no_ruby_flag {
        eprintln!("rigor check: --ruby and --no-ruby are mutually exclusive (specify at most one)");
        return ExitCode::from(64);
    }
    let ruby_cli = ruby_cli.or(no_ruby_flag.then_some(ruby_mode::RubyMode::Off));

    // Load `.rigor.yml` (explicit `--config` path, else `.rigor.yml` →
    // `.rigor.dist.yml` cwd discovery). Config ONLY suppresses/scopes
    // diagnostics; it never changes analysis. A config the reference's
    // `Configuration.load` dies on (bad YAML, a non-mapping document, an
    // `includes:` miss or cycle) is fatal here too — `rigor: <msg>` +
    // exit 64 (the `rescue ConfigurationError` surface). An absent file
    // uses the defaults, so the differential harness — which runs from a
    // directory with no `.rigor.yml` — is unaffected.
    let cfg = match Config::load(explicit_config.as_deref().map(Path::new)) {
        Ok(c) => c,
        Err(f) => return f.report(),
    };

    // Config audit (reference `warn_unresolved_config`): surface configured
    // values that silently resolve to nothing — a typo'd `signature_paths:` dir
    // (which would manufacture hundreds of false `undefined-method`s), an inert
    // `disable:` rule token, a missing explicit `rbs_collection.lockfile`. Emitted
    // to stderr as `rigor: <message>` before analysis; the stdout diagnostic
    // stream is untouched (0-FP / harness-safe — the harness runs configless).
    // `project_root` is the process cwd, the base config discovery resolves against.
    config_audit::emit(&cfg, Path::new("."));

    // `CheckRunnerFactory.resolve_workers`: `--workers` wins outright, so the
    // env is only read when no flag was given. Upstream a non-`Integer()`
    // `RIGOR_RACTOR_WORKERS` dies on an uncaught `ArgumentError` (backtrace,
    // exit 1); the port surfaces the same rejection as a usage error instead —
    // `rigor: <msg>` + exit 64, and NO analysis rows (issue #157).
    if !workers_cli {
        if let Some(v) = std::env::var_os("RIGOR_RACTOR_WORKERS") {
            // `env_value && !env_value.empty?` upstream — an EMPTY value is
            // unset, not invalid (falls through to `parallel.workers`).
            let ok = v.is_empty()
                || v.to_str()
                    .is_some_and(|s| config::ruby_integer(s).is_some());
            if !ok {
                eprintln!(
                    "rigor: invalid RIGOR_RACTOR_WORKERS value: {v:?} (expected an Integer literal)"
                );
                return ExitCode::from(64);
            }
        }
    }

    // ADR-50 WD2 — the effective bleeding-edge selection: CLI flag > config.
    // Threaded into `analyze_files`, where it feeds the severity-resolution
    // pipeline (ADR-8 "Severity profile"): user `severity_overrides:` >
    // bleeding-edge overrides > `severity_profile:` table > authored.
    let bleeding_edge =
        bleeding_edge_cli.unwrap_or_else(|| cfg.bleeding_edge_selector());

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

    // ADR-0040 — the scan roots: explicit path args when given (verbatim —
    // `rigor check .` expands `./x.rb` spellings, which is why a `.`-led
    // `exclude:` like `**/*.rb` can never match them upstream), else the
    // config `paths:` in the reference's stored spelling — DECLARED entries
    // absolutized (`resolve_paths_in`), the `["lib"]` default verbatim.
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let config_path_strings: Vec<String>;
    let config_paths: Vec<&str>;
    let roots: &[&str] = if files.is_empty() {
        config_path_strings = effective_config_paths(&cfg);
        config_paths = config_path_strings.iter().map(String::as_str).collect();
        &config_paths
    } else {
        &file_refs
    };

    // `Runner#validate_target_ruby` — the FIRST thing `run_analysis` does, so
    // a format-valid `target_ruby:` this Prism build rejects returns a lone
    // run-level `configuration-error` row: no path expansion, no plugin
    // loading, no analysis, no expansion errors (issue #157). The row bypasses
    // `disable:` and the severity pipeline the same way it does upstream (the
    // early return renders `Result.diagnostics` verbatim); it still flows
    // through the baseline filter and `fail_on` below like any result.
    let (mut findings, had_io_error, expanded_owned, path_errors) = match cfg
        .target_ruby_failure()
    {
        Some(message) => (
            vec![(
                0usize,
                ".rigor.yml".to_string(),
                String::new(),
                Diagnostic {
                    rule_id: "configuration-error",
                    start_offset: 0,
                    end_offset: 0,
                    message,
                    severity: Severity::Error,
                    source_family: "builtin",
                    receiver_type: None,
                    method_name: None,
                },
            )],
            false,
            Vec::new(),
            Vec::new(),
        ),
        None => {
            // Expand roots into their `**/*.rb` files and collect bad-path
            // errors, matching the reference's `expand_paths`:
            // directory-expanded entries matching `BUILTIN_EXCLUDES +
            // exclude:` are pruned HERE (`reject_excluded` + `File.fnmatch?`
            // with no flags); explicit `.rb` file roots are kept verbatim
            // even when they match `exclude:` (issue #201 — the analyzed set
            // IS the rejected expansion; there is no second per-file gate).
            let excludes = exclude_patterns(&cfg);
            let (expanded_owned, path_errors) =
                expand_check_paths_excluding(roots, &excludes);
            let expanded: Vec<&str> =
                expanded_owned.iter().map(String::as_str).collect();

            // Run the analysis pipeline (config `disable:` + inline `#
            // rigor:disable` applied; `exclude:` already settled by the
            // expansion). Shared with `baseline generate`.
            // Issue #129 (ADR-0044 § "Environment-parity gate"): the CLI half
            // of the gate — every flag one the port parses exactly as the
            // reference does, the config path the file the reference reads,
            // no baseline in effect (the reference regroups its output by
            // (file, rule) bin under one, and matches paths the port renders
            // differently).
            let ref_has_files = reference_has_ruby_files(&cfg, &file_refs)
                && conformance_gate::check_args_ok(&items)
                && conformance_gate::config_path_ok(
                    explicit_config.as_deref().unwrap_or(".rigor.yml"),
                )
                && resolve_baseline_path(&baseline_arg, &cfg).is_none()
                && conformance_gate::process_env_ok(
                    std::env::var_os("POSIXLY_CORRECT").as_deref(),
                    std::env::var_os("RIGOR_RACTOR_WORKERS").as_deref(),
                );
            let (findings, had_io_error) = analyze_files(
                &expanded,
                // `None` on the `paths:` fallback ⇒ `paths ==
                // configuration.paths` ⇒ the reference never widens discovery
                // for a bare `check`.
                if files.is_empty() {
                    None
                } else {
                    Some(file_refs.as_slice())
                },
                &cfg,
                "check",
                folder_ref,
                &bleeding_edge,
                ref_has_files,
            );
            (findings, had_io_error, expanded_owned, path_errors)
        }
    };

    // ADR-22 slice 5 — snapshot the RAW (pre-baseline-filter) findings for the
    // `--baseline-strict` audit. The reference audits `raw_result.diagnostics`
    // (BEFORE `apply_baseline_filter`), so the gate sees deficit drift a bucket
    // would otherwise silence. Only snapshot when the flag is set; a clone
    // avoids threading a borrow through the mutate-in-place filter below.
    let raw_findings: Vec<(usize, String, String, Diagnostic)> =
        if baseline_strict { findings.clone() } else { Vec::new() };

    // ADR-22 — baseline filter, applied LAST (after inline `# rigor:disable`
    // and config `disable:`, per reference WD6). With no resolved baseline this
    // is a no-op, so the no-baseline path stays byte-identical (harness-gated).
    if let Some(path) = resolve_baseline_path(&baseline_arg, &cfg) {
        findings = apply_baseline(findings, &path);
    }

    // ADR-0040 — inject bad-path diagnostics AFTER the baseline filter (they are
    // not code findings and must never be baseline-suppressed). Severity follows
    // the reference: warn-and-skip when SOME files were analyzed, else error.
    prepend_path_errors(&mut findings, &path_errors, !expanded_owned.is_empty());

    // `write_result`'s `case format` — an unknown value raises
    // `OptionParser::InvalidArgument` HERE (after the run, instead of any
    // output), which `dispatch`'s `rescue ParseError` renders as
    // `invalid argument: unsupported format: X` on stderr, exit 64.
    match format.as_str() {
        "text" => print_text(&findings),
        "json" => print_json(&findings),
        "github" => print_rendered(&findings, diagnostic_formats::render_github),
        "sarif" => print_rendered(&findings, diagnostic_formats::render_sarif),
        "gitlab" => print_rendered(&findings, diagnostic_formats::render_gitlab),
        "checkstyle" => print_rendered(&findings, diagnostic_formats::render_checkstyle),
        "junit" => print_rendered(&findings, diagnostic_formats::render_junit),
        "teamcity" => print_rendered(&findings, diagnostic_formats::render_teamcity),
        _ => {
            eprintln!("invalid argument: unsupported format: {format}");
            return ExitCode::from(64);
        }
    }

    // CI auto-detection (ADR-51 WD7): only augments the default human (`text`)
    // output — an explicit `--format` means the caller is in control and is left
    // untouched. For a first-class stdout-native CI (GitHub Actions / TeamCity)
    // the platform's annotations are emitted on top of the text output; for
    // GitLab (native but artifact-based) and the reviewdog-routed CIs a one-line
    // hint goes to stderr, but only when there are diagnostics so a clean run
    // stays quiet. `RIGOR_CI_DETECT=0`/`false`/`no`/`off` or `--no-ci-detect`
    // disables it (and so the differential harness, which runs without those
    // CI vars, is never affected).
    if ci_detect && format == "text" {
        emit_ci_detected_output(&findings);
    }

    // ADR-0040 — exit 1 iff there is a genuine read I/O error OR any run-failing
    // finding (see `finding_fails_run`); a warning-only run — including a
    // warn-and-skip bad path alongside analyzed files — exits 0.
    let normal_fail =
        had_io_error || findings.iter().any(|(_, _, _, d)| finding_fails_run(d));

    // Issue #812 — `--fail-on=SEVERITY` (the reference's `fail_on_violation?`):
    // a diagnostic at or above the threshold — over the SAME baseline-filtered
    // list the output used — fails the run. `:error` is a no-op over
    // `normal_fail`'s severity half.
    let fail_on_hit = findings
        .iter()
        .any(|(_, _, _, d)| severity_rank(d.severity) >= severity_rank(fail_on));

    // ADR-22 slice 5 — the `--baseline-strict` gate runs LAST (after all normal
    // stdout diagnostics + stderr stats/silenced lines) so its report is the
    // final thing emitted, and OR's onto the exit code. It must run and print
    // even when `normal_fail` is already true (informational output + a flat OR;
    // no distinct exit code).
    let strict_violation =
        baseline_strict && baseline_strict_violation(&raw_findings, &cfg, &baseline_arg);

    if normal_fail || strict_violation || fail_on_hit {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Issue #812 — the `FAIL_ON_RANK` order: `info` (0) < `warning` (1) <
/// `error` (2).
fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Info => 0,
        Severity::Warning => 1,
        Severity::Error => 2,
    }
}

/// ADR-22 slice 5 — the `--baseline-strict` CI gate predicate. Audits the RAW
/// (pre-baseline-filter) findings against the resolved baseline and, on ANY
/// drift (`status != Within` — over, cleared, OR reducible), prints a report to
/// stderr and returns `true` (fail the run). Returns `false` (with an
/// appropriate stderr note, or silently) in every "nothing to gate" case. The
/// caller guards on the flag; this is only invoked when `--baseline-strict` is
/// set.
///
/// Faithful to the reference `baseline_strict_violation?`:
/// - no resolved baseline path → `... nothing to gate.` note, `false`.
/// - an ABSENT file → `Baseline::load` yields an empty baseline → `false`
///   SILENTLY (no message).
/// - a malformed file (`LoadError`) → `... gate skipped` note, `false`. This is
///   a SECOND, independent load — `apply_baseline` already loaded+warned
///   `... (continuing without baseline)`, and BOTH messages must appear in a run
///   with a malformed file, so we deliberately do not dedupe.
fn baseline_strict_violation(
    raw_findings: &[(usize, String, String, Diagnostic)],
    cfg: &Config,
    baseline_arg: &BaselineArg,
) -> bool {
    let Some(path) = resolve_baseline_path(baseline_arg, cfg) else {
        eprintln!("rigor: --baseline-strict given but no baseline is active; nothing to gate.");
        return false;
    };

    let baseline = match Baseline::load(Path::new(&path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rigor: baseline load failed: {e} (--baseline-strict gate skipped)");
            return false;
        }
    };
    // An absent file loads as empty → nothing to gate, silently.
    if baseline.is_empty() {
        return false;
    }

    let cwd = std::env::current_dir().ok();
    let entries: Vec<(String, &Diagnostic)> = raw_findings
        .iter()
        .map(|(_, p, _, d)| (relative_path(p, cwd.as_deref()), d))
        .collect();

    let rows = baseline.audit(&entries);
    let drifted: Vec<&baseline::DriftRow> =
        rows.iter().filter(|r| r.status != DriftStatus::Within).collect();
    if drifted.is_empty() {
        return false;
    }

    report_strict_drift(&drifted, &path);
    true
}

/// Print the `--baseline-strict` drift report to stderr (reference
/// `report_strict_drift`). Rows are sorted by `(bucket.file, bucket.rule)`; the
/// row format matches `baseline drift`'s EXCEPT for the trailing `, {status}`
/// inside the parens (verified byte-for-byte against the oracle). `delta_str`
/// is `+N` for positive delta, else Ruby's `Integer#to_s` (`0`, `-N`);
/// `status` renders as the lowercase status word (`over`/`cleared`/`reducible`).
fn report_strict_drift(drifted: &[&baseline::DriftRow], path: &str) {
    eprint!("{}", format_strict_drift(drifted, path));
}

/// Render the strict-drift report as one trailing-newline-terminated block, so
/// the exact bytes are unit-testable. `eprint!`ed verbatim by
/// `report_strict_drift`.
fn format_strict_drift(drifted: &[&baseline::DriftRow], path: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "rigor: --baseline-strict — {} bucket(s) drifted from {path}:\n",
        drifted.len()
    ));
    let mut rows: Vec<&&baseline::DriftRow> = drifted.iter().collect();
    rows.sort_by(|a, b| (&a.bucket.file, &a.bucket.rule).cmp(&(&b.bucket.file, &b.bucket.rule)));
    for row in rows {
        out.push_str(&format!(
            "  {}  [{}]  {} → {}  (Δ{}, {})\n",
            row.bucket.file,
            row.bucket.rule,
            row.bucket.count,
            row.actual,
            strict_delta_str(row.delta),
            drift_status_word(row.status),
        ));
    }
    out.push_str("rigor: run `rigor baseline regenerate` to refresh the baseline.\n");
    out
}

/// Ruby's `delta.positive? ? "+#{delta}" : delta.to_s` — `+N` for positive,
/// else the bare integer (`0`, `-N`).
fn strict_delta_str(delta: i64) -> String {
    match delta.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{delta}"),
        _ => delta.to_string(),
    }
}

/// The lowercase status word the strict report prints — the Ruby symbol's `to_s`
/// (`:over` → `over`). `Within` never reaches the report (drifted-only), but is
/// mapped for totality.
fn drift_status_word(status: DriftStatus) -> &'static str {
    match status {
        DriftStatus::Over => "over",
        DriftStatus::Cleared => "cleared",
        DriftStatus::Reducible => "reducible",
        DriftStatus::Within => "within",
    }
}

/// Whether a finding fails the `check` run (exit 1). ERROR severity matches the
/// reference's `error_count > 0`. The synthetic `internal-error` finding also
/// fails the run DESPITE being info-severity: its severity is info only to keep
/// it out of the differential harness's error/warning parity gate (see
/// [`internal_error_diag`]) — but a run whose analysis PANICKED must never exit 0
/// (the 2026-07-06 audit's regression: the error-severity-driven exit code would
/// otherwise silently green-light a crashed file in CI).
fn finding_fails_run(d: &Diagnostic) -> bool {
    d.severity == Severity::Error || d.rule_id == "internal-error"
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

/// A CLI path argument that could not be turned into an analyzable `.rb` file.
struct PathError {
    path: String,
    /// `true` — the path does not exist; `false` — it exists but is not a `.rb`
    /// file (a directory is expanded, never a `PathError`).
    not_found: bool,
}

/// The config `paths:` in the spelling the reference's `Configuration`
/// actually stores (`resolve_path_key!`): a DECLARED entry was
/// `File.expand_path(entry, config_dir)`'d at load time (issue #158) — an
/// ABSOLUTE string with `.`/`..` folded lexically (a cwd `.rigor.yml`
/// expands against the cwd itself). The absolute spelling is load-bearing
/// upstream, not cosmetic: `exclude:` is `File.fnmatch?`'d against the
/// expanded file list, so a project-relative pattern like `lib/ext.rb`
/// NEVER matches a declared `paths:` file there — a relative spelling would
/// over-exclude and manufacture FPs. The `["lib"]` DEFAULT is not in the
/// file, so it stays exactly as written (cwd-relative).
pub(crate) fn effective_config_paths(cfg: &Config) -> Vec<String> {
    cfg.paths.clone()
}

/// Expand raw `check`/`baseline` path arguments into the concrete `.rb` files
/// to analyze plus any bad-path errors — a faithful port of the reference's
/// `Runner#expand_paths` (`PathExpansion#call`, ADR-0040):
/// - a DIRECTORY → its `**/*.rb` (recursive; hidden dirs and symlinked dirs
///   skipped; `.gitignore` ignored), each directory's hits sorted and
///   concatenated in arg order — THEN `reject_excluded`: every directory hit
///   matching `excludes` is dropped.
/// - a FILE ending in `.rb` → kept as-is (`accept_as_ruby_file?` never
///   consults `exclude_patterns` — an explicit `check lib/ext.rb` analyzes
///   `ext.rb` even when `exclude: [lib/ext.rb]`).
/// - an existing non-`.rb` file → a `PathError { not_found: false }`.
/// - a missing path → a `PathError { not_found: true }`.
///
/// `excludes` is `Configuration#exclude_patterns` (`BUILTIN_EXCLUDES +
/// exclude:`) matched with `File.fnmatch?` and NO flags — `*` spans `/`, so
/// `*zz.rb` matches `lib/zz.rb`, but consecutive `*`s collapse so
/// `a/**/b` does NOT match `a/b` (unlike the retired `glob::Pattern` per-file
/// gate — see issue #201). There is no second exclusion stage: this
/// expansion IS the analyzed set.
pub(crate) fn expand_check_paths_excluding(
    raw: &[&str],
    excludes: &[String],
) -> (Vec<String>, Vec<PathError>) {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    // Decode the patterns once per expansion (not per root) — `reject_excluded`
    // runs patterns × files; `exclude_fnmatch` decodes each file path once.
    let mut compiled_excludes: Option<Vec<Vec<char>>> = None;
    for &p in raw {
        let path = Path::new(p);
        if path.is_dir() {
            let mut in_dir = Vec::new();
            collect_rb_files(path, &mut in_dir);
            in_dir.sort();
            let compiled = compiled_excludes.get_or_insert_with(|| {
                excludes.iter().map(|p| p.chars().collect()).collect()
            });
            in_dir.retain(|f| !exclude_fnmatch(compiled, f));
            files.extend(in_dir);
        } else if path.is_file() && p.ends_with(".rb") {
            files.push(p.to_string());
        } else if path.exists() {
            errors.push(PathError { path: p.to_string(), not_found: false });
        } else {
            errors.push(PathError { path: p.to_string(), not_found: true });
        }
    }
    (files, errors)
}

/// `Configuration#exclude_patterns` — `BUILTIN_EXCLUDES + exclude:` — the
/// one list `expand_paths`' `reject_excluded` applies to directory
/// expansions.
pub(crate) fn exclude_patterns(cfg: &Config) -> Vec<String> {
    conformance_gate::BUILTIN_EXCLUDES
        .iter()
        .map(|s| (*s).to_string())
        .chain(cfg.exclude.iter().cloned())
        .collect()
}

/// `File.fnmatch?(pattern, path)`-with-no-flags exclusion for one expanded
/// path — the exact matcher [`conformance_gate::fnmatch_chars`] ports from
/// MRI `dir.c`, including the leading-period rule: a `*`/`?`/`[` at pattern
/// position 0 never matches a `.` at path position 0 (`./app/gen.rb` is NOT
/// excluded by `*gen.rb`). Patterns arrive pre-decoded — the caller converts
/// them once per expansion, not once per file.
pub(crate) fn exclude_fnmatch(patterns: &[Vec<char>], path: &str) -> bool {
    let path: Vec<char> = path.chars().collect();
    patterns
        .iter()
        .any(|p| conformance_gate::fnmatch_chars(p, &path))
}

/// Recursively collect `*.rb` files under `dir`, mirroring Ruby's
/// `Dir.glob("**/*.rb")` exactly (probed): SKIP hidden entries (name starting
/// with `.`); do NOT traverse symlinked DIRECTORIES (`**` does not follow them);
/// but DO include symlinked `.rb` FILES (glob matches them — 2026-07-06 audit
/// correction; a symlink to a dir is skipped, a symlink to a file is a match).
/// Unreadable directories are silently skipped.
pub(crate) fn collect_rb_files(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let child = entry.path();
        if ft.is_symlink() {
            // Follow the link ONE step to classify it: a file target is matched
            // (like Dir.glob), a dir target is NOT traversed.
            if name.ends_with(".rb") {
                if let Ok(md) = std::fs::metadata(&child) {
                    if md.is_file() {
                        out.push(child.to_string_lossy().into_owned());
                    }
                }
            }
            continue;
        }
        if ft.is_dir() {
            collect_rb_files(&child, out);
        } else if ft.is_file() && name.ends_with(".rb") {
            out.push(child.to_string_lossy().into_owned());
        }
    }
}

/// Splice bad-path diagnostics into `findings` (ADR-0040): severity is
/// `warning` (` (skipped)`) when SOME files were found, else `error` — the
/// reference's "a bad path among valid ones warns; a bad path leaving nothing
/// to do errors, so a lone typo is not silently masked". Emitted with a
/// synthetic `rule_id` (rigor-rs's `Diagnostic.rule_id` is non-optional; the
/// reference uses `null`). They are `expansion.errors` upstream — the LAST of
/// the run-level rows — so they land after any leading `pre-eval.*` rows
/// `analyze_files` emitted but still ahead of every per-file finding.
fn prepend_path_errors(
    findings: &mut Vec<(usize, String, String, Diagnostic)>,
    errors: &[PathError],
    any_files: bool,
) {
    if errors.is_empty() {
        return;
    }
    let severity = if any_files { Severity::Warning } else { Severity::Error };
    let suffix = if any_files { " (skipped)" } else { "" };
    let injected: Vec<(usize, String, String, Diagnostic)> = errors
        .iter()
        .map(|e| {
            let (rule, base): (&'static str, &str) = if e.not_found {
                ("path.not-found", "no such file or directory")
            } else {
                ("path.not-ruby", "not a Ruby file (expected `.rb` or a directory)")
            };
            let diag = Diagnostic {
                rule_id: rule,
                start_offset: 0,
                end_offset: 0,
                message: format!("{base}{suffix}"),
                severity,
                source_family: "builtin",
                receiver_type: None,
                method_name: None,
            };
            (0usize, e.path.clone(), String::new(), diag)
        })
        .collect();
    // The leading run-level span is `plugin_loader.*` rows then `pre-eval.*`
    // rows (upstream `pre_file_diagnostics` order — `expansion.errors` come
    // last); the `plugin.<id>.*` emitted rows are NOT leading (they follow
    // the per-file stream).
    let n_run = findings
        .iter()
        .take_while(|(_, _, _, d)| {
            d.rule_id.starts_with("plugin_loader.") || d.rule_id.starts_with("pre-eval.")
        })
        .count();
    let tail = findings.split_off(n_run);
    findings.extend(injected);
    findings.extend(tail);
}

/// `files`: the expanded `.rb` file list to ANALYZE (one work item per entry,
/// duplicates kept). `argv_roots`: the path arguments as the user wrote them,
/// or `None` when the run's roots came from the config `paths:` fallback —
/// the reference's `widen_discovery_to_project?` gate (`paths !=
/// configuration.paths`) is then unconditionally false and discovery never
/// widens beyond the analyzed set (bare `check`, and `baseline` without
/// positionals — the reference's baseline always analyzes `paths:`).
fn analyze_files(
    files: &[&str],
    argv_roots: Option<&[&str]>,
    cfg: &Config,
    verb: &str,
    folder: Option<&(dyn rigor_infer::RubyFolder + Sync)>,
    bleeding_edge: &config::BleedingEdgeSelector,
    ref_has_files: bool,
) -> (Vec<(usize, String, String, Diagnostic)>, bool) {
    let disable_matcher = cfg.disable_matcher();
    // ADR-8 "Severity profile" — the resolution inputs, computed once: the
    // configured profile, the user's per-rule/family overrides, and the merged
    // overrides of the ACTIVE bleeding-edge features (reference
    // `Configuration#bleeding_edge_severity_overrides`).
    let profile = cfg.severity_profile();
    let user_overrides = cfg.severity_overrides();
    let bleeding_overrides = bleeding_edge::severity_overrides_for(bleeding_edge);
    // The reference's memoised rule-activation gate (`runner.rb`): the
    // `static.value-use.void` collector runs only when the rule's RESOLVED
    // severity is not `:off` — authored `:warning`, every shipped profile
    // `:off`, promoted by the `use-of-void-value` feature OR a user
    // `severity_overrides:` entry (the override alone resurrects it there,
    // and must here too). `disable:` still filters downstream as usual.
    let void_rule_active = severity::resolve(
        rigor_rules::STATIC_VALUE_USE_VOID,
        severity::ResolvedSeverity::Warning,
        profile,
        &user_overrides,
        &bleeding_overrides,
    ) != severity::ResolvedSeverity::Off;
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
    // The stage-1 line is further split into its `parse+lower` and `harvest`
    // components (issue #104) — those PER-FILE clock reads happen inside the
    // rayon closure and are therefore gated on this flag, not unconditional.
    // See `Stage1Times` for what each reported field means.
    let timing = std::env::var_os("RIGOR_TIMING").is_some();
    let t_start = std::time::Instant::now();

    // ADR-72: the effective plugin set = config `plugins:` + `Gemfile.lock`-gated
    // auto-detected overlays (bundler.auto_detect). Empty-Gemfile.lock projects
    // (incl. the config-less differential harness) get exactly the activating
    // `plugins:` entries. Issue #157: the resolution ALSO carries the loader's
    // run-level rows — `plugin_loader.load-error` for every entry whose
    // require/id/dup/config check fails (they lead the whole stream upstream,
    // ahead of `pre-eval.*`) — plus the plugins' own emitted rows (which land
    // after the per-file stream) and any stderr lines (`rigor-rbs-inline`'s
    // missing-`rbs/inline` warning).
    let root = std::path::Path::new(".");
    let plugin_resolution = cfg.plugin_resolution(root);
    for line in &plugin_resolution.stderr {
        eprintln!("{line}");
    }
    let effective_plugins = cfg.effective_plugins(root);
    // The rbs-collection gem dirs ride apart from `signature_paths:` so the
    // `conforms-to` scan can tell them apart (issue #129); every rule reads both.
    let index = CoreIndex::for_project_parts(
        &effective_plugins,
        &cfg.signature_dirs(),
        &cfg.collection_signature_dirs(root),
    );
    let t_index = std::time::Instant::now();
    // Each entry: (input_order_key, path, source_or_empty, diagnostic).
    let mut findings: Vec<(usize, String, String, Diagnostic)> = Vec::new();
    let mut had_io_error = false;

    // PROJECT PASS (ADR-0023 cross-file): parse+lower+HARVEST every file first,
    // MERGE the harvests into ONE project-wide SourceIndex, then analyze each
    // file against it. Stages 1 (parse+lower+harvest) and 3 (analyze) are
    // file-INDEPENDENT and run on a rayon pool (§9, ADR-0006/0028); stage 2 (the
    // merge) is the serial barrier between them. Per-file panic isolation
    // (ADR-0016) is preserved at both parallel stages — each closure
    // `catch_unwind`s its own file.
    //
    // Determinism (the parity keystone): each parallel stage collects its
    // outcomes IN INPUT ORDER (`par_iter().map().collect()` preserves the source
    // order into the result Vec), and side effects — the stderr lines and the
    // findings pushes — are replayed by a SEQUENTIAL drain of that ordered Vec.
    // So the stderr stream, the findings order, and the final `sort_by_key` are
    // all byte-identical to the old serial loop; the pool is invisible in output.
    // The harvests inherit that contract: they are frozen before the merge
    // starts and merged in the SAME input order, which is normative (issue #92
    // — `SourceIndex::merge` must never sort).
    struct Prepared {
        order: usize,
        path: String,
        source: String,
        ast: rigor_parse::LoweredAst,
        comments: Vec<(usize, usize, String)>,
    }

    /// Per-file stage-1 component costs, for the `RIGOR_TIMING` split of the
    /// `stage1(parse+lower+harvest)` label into its two halves (issue #104).
    ///
    /// **What is measured, and why it is reported the way it is.** Stage 1 runs
    /// on rayon, so a per-file duration is CPU time on one worker, NOT wall
    /// time — summing them across 4 675 files yields a number several times
    /// larger than the stage's own wall clock, and quoting that sum as "what a
    /// harvest cache would save" would be wrong by exactly the parallel speedup.
    /// The line therefore reports three different things and labels them as
    /// such: the **summed** per-file harvest CPU (`harvest-cpu`), the **single
    /// worst** file's harvest (`harvest-cpu-max`, a floor on any wall
    /// contribution — one file's harvest cannot be split across threads), and
    /// the **amortized wall estimate** `harvest-cpu / threads`
    /// (`harvest-wall-amortized`), which is a MODEL — it assumes the harvest
    /// work distributes as evenly over the pool as the rest of stage 1 does.
    /// `stage1-ex-harvest` is stage 1's measured wall minus that estimate.
    /// A true marginal wall cost needs an A/B against a build that does the
    /// harvest a different number of times; the model is the honest in-process
    /// approximation, not a substitute for it.
    ///
    /// Excluded / unreadable / unparseable files produce no `Prepared`, so they
    /// contribute to neither sum — which is the right domain, since those files
    /// have no harvest for a cache to serve either.
    #[derive(Clone, Copy, Default)]
    struct Stage1Times {
        /// Read + ERB sniff + parse + lower, i.e. everything a cache hit must
        /// still pay (the AST is required by merge M3 and by stage 3).
        parse_lower: std::time::Duration,
        /// `SourceIndex::harvest` alone — the one call a per-file harvest cache
        /// could skip.
        harvest: std::time::Duration,
    }

    // STAGE 1 (file-parallel): read + parse + lower + harvest. A closure never
    // mutates shared state — it returns a self-contained outcome that the serial
    // drain below turns into the same eprintln / push the serial loop did.
    enum Stage1 {
        Excluded,
        /// The lowered file plus its per-file [`rigor_infer::Harvest`], kept
        /// side by side so the serial drain can fill the two index-aligned Vecs.
        /// The harvest is BOXED to keep this variant close to the others in size
        /// (`clippy::large_enum_variant`); one allocation per file, in parallel.
        /// The third field is the `RIGOR_TIMING` component split (all-zero when
        /// the env gate is unset).
        Prepared(Prepared, Box<rigor_infer::Harvest>, Stage1Times),
        /// A discovery-ONLY file (upstream #684): parsed, lowered and
        /// harvested into the project index, but never analyzed — it
        /// contributes no findings.
        Discovery {
            ast: rigor_parse::LoweredAst,
            harvest: Box<rigor_infer::Harvest>,
            times: Stage1Times,
        },
        IoError { path: String, msg: String },
        /// A file Prism could not parse: NOT analysed (see the guard below),
        /// but its parse errors are reported, one diagnostic per raw Prism
        /// error. The source rides along so the serial drain can resolve each
        /// byte offset to a line/column exactly as every other finding does.
        ParseErrors { order: usize, path: String, source: String, diags: Vec<Diagnostic> },
        Panic { order: usize, path: String, msg: String },
    }

    /// The two outcomes of the panic-isolated parse+lower closure: a file that
    /// parsed (and is lowered), or one that did not (and carries its parse
    /// errors instead). Replaces the earlier `Option`, whose `None` conflated
    /// "unparseable" with "nothing to report".
    enum Lowered {
        Parsed(rigor_parse::LoweredAst, Vec<(usize, usize, String)>),
        Unparseable(Vec<Diagnostic>),
    }

    /// One stage-1 work item: `order`/`analyze` mark the analyzed files
    /// (keyed by their position in `files`); `analyze == false` items are
    /// discovery-only.
    struct WorkItem {
        order: usize,
        path: String,
        analyze: bool,
    }

    // Upstream #684 — `widen_discovery_to_project?` /
    // `project_discovery_expansion`: when the run targets an explicit file
    // list, the cross-file DISCOVERY pass still walks
    // `expand_paths(configuration.paths | argv)` — `rigor check a.rb`
    // sees `class String; attr_accessor :zz` in an unlisted `lib/ext.rb`,
    // so a project-declared method suppresses identically whether or not
    // its file was named.
    //
    // The ANALYZED side is the argv expansion VERBATIM — one work item per
    // `files` entry, each with its own `order` slot, exactly as the
    // reference's `expand_paths(argv)` keeps duplicates: `check a.rb a.rb`
    // analyzes twice, and an argv file that also sits under `paths:` still
    // reports at its argv position (`check lib lib/a.rb` → a,b,c,a).
    //
    // The widening gate is the reference's, verbatim:
    // `widened.files.size > expansion.files.size`, where `widened` expands
    // the ROOT-LEVEL union `configuration.paths | argv`. Repeated argv
    // strings dedup out of the union, so `check a.rb a.rb`'s widened set
    // ([lib…, a.rb]) is the same size as its expansion and discovery does
    // NOT widen — oracle-visible: the `Foo` decl in `lib/` stays unseen and
    // `Foo.new.bar.upcase` goes silent where a single `check a.rb` (widened
    // strictly larger) resolves `bar` and fires on `Integer#upcase`. And
    // `argv_roots == None` — the `paths:` fallback (bare `check`, bare
    // `baseline`) — means `paths == configuration.paths` upstream, which is
    // `widen_discovery_to_project?` false: no discovery items at all.
    //
    // Files the widened expansion adds BEYOND the analyzed set are appended
    // as discovery-only items: parsed + lowered + harvested into the
    // project index but producing no findings (the reference's discovery is
    // a parse pass, not an analysis).
    let analyzed_paths: std::collections::HashSet<&str> =
        files.iter().copied().collect();
    let mut worklist: Vec<WorkItem> = Vec::with_capacity(files.len());
    for (i, p) in files.iter().enumerate() {
        worklist.push(WorkItem {
            order: i,
            path: (*p).to_string(),
            analyze: true,
        });
    }
    if let Some(argv_roots) = argv_roots {
        // `configuration.paths | paths` — a ROOT-level ORDERED-SET union: the
        // receiver's own repeats dedup out too (`paths: [lib, lib]` and the
        // `File.expand_path`-identical `[./lib, lib/]` are ONE root upstream),
        // and so do repeat argv strings.
        let mut union_roots: Vec<String> = Vec::new();
        for root in effective_config_paths(cfg)
            .into_iter()
            .chain(argv_roots.iter().map(|s| (*s).to_string()))
        {
            if !union_roots.contains(&root) {
                union_roots.push(root);
            }
        }
        let union_root_refs: Vec<&str> =
            union_roots.iter().map(String::as_str).collect();
        // Both sides of `widened.files.size > expansion.files.size` are the
        // post-`reject_excluded` expansions — the same exclusion-aware
        // expansion `files` itself came through, so a directory hit matching
        // `BUILTIN_EXCLUDES + exclude:` (exact `File.fnmatch?`, no flags)
        // never counts and never becomes a discovery item. The `expansion`
        // side re-expands `argv_roots` rather than reading `files.len()`
        // because exclusion needs the dir-vs-file provenance only the roots
        // carry. Expansion errors on the discovery side are dropped, exactly
        // as `project_discovery_expansion` only reads `widened[:files]`.
        let excludes = exclude_patterns(cfg);
        let widened_files =
            expand_check_paths_excluding(&union_root_refs, &excludes).0;
        let expanded_files =
            expand_check_paths_excluding(argv_roots, &excludes).0;
        if widened_files.len() > expanded_files.len() {
            for path in widened_files {
                if analyzed_paths.contains(path.as_str()) {
                    continue;
                }
                worklist.push(WorkItem {
                    order: usize::MAX,
                    path,
                    analyze: false,
                });
            }
        }
    }

    let stage1: Vec<Stage1> = worklist
        .par_iter()
        .map(|item| {
            let order = item.order;
            let path = item.path.as_str();
            // `RIGOR_TIMING` component split: the clock is read ONLY under the
            // env gate, so an unset `RIGOR_TIMING` costs one already-hot
            // predicted branch per file and zero clock reads — the same
            // "invisible by default" contract the stage markers have.
            let t_file = timing.then(std::time::Instant::now);
            // No `exclude:` check here — exclusion is settled by
            // `expand_check_paths_excluding` before this worklist exists:
            // analyzed items arrive already past `reject_excluded` (directory
            // entries) or verbatim (explicit `.rb` roots the reference never
            // filters), and discovery items came through the same filtered
            // expansion. Re-matching patterns per-file here is how the old
            // `glob::Pattern` gate dropped `a/**/b`-adjacent and explicit
            // `exclude:`d roots the reference analyzes (issue #201).
            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    // A discovery-only file contributes nothing on a read
                    // failure — no finding, no stderr line.
                    return if item.analyze {
                        Stage1::IoError { path: path.to_string(), msg: e.to_string() }
                    } else {
                        Stage1::Excluded
                    };
                }
            };
            // Skip ERB templates (`.rb` generator templates using `<%= … %>`):
            // Prism's error recovery yields a garbage AST the structural rules
            // over-fire on. Matches the reference's ErbTemplateDetector (real-
            // corpus FP audit: jbuilder/redmine generator templates).
            if rigor_parse::looks_like_erb_template(source.as_bytes()) {
                return Stage1::Excluded;
            }
            let source_bytes = source.as_bytes().to_vec();
            let lowered = panic::catch_unwind(AssertUnwindSafe(|| {
                let result = parse(&source_bytes);
                // A file Prism could not parse is analysed by NEITHER tool. The
                // reference's `analyze_file_body` returns its parse diagnostics
                // and never reaches `ScopeIndexer.index`, so every semantic rule
                // is off for that file; Prism's error recovery invents bindings
                // (`def f int a, int b` recovers as a body referencing a
                // never-bound `b`) that the rules then over-fire on. Skipping
                // the file entirely — index included, matching the reference's
                // dependency walker — is the FP-safe match. rigor-rs emits no
                // parse diagnostics of its own, so the file falls silent: a
                // coverage gap against the reference's `rule: null` errors, not
                // a false positive.
                //
                // The REPORTING half is separable from that skip, and is
                // ported: the reference's `analyze_file_body` returns
                // `parse_diagnostics(path, parse_result)` here — one
                // `error`-severity, `rule: nil` diagnostic per raw Prism error,
                // 1:1, no filtering and no dedupe — and rigor-rs now does too.
                // Without them `rigor check` answered `[]` and exited 0 on a
                // file it could not read, which a CI gate reads as clean.
                if result.errors().next().is_some() {
                    return Lowered::Unparseable(parse_diagnostics(&result));
                }
                let comments = rigor_parse::comment_lines(&result, &source_bytes);
                // Issue #102: the AST carries the file's CANONICAL-path identity,
                // which is what the per-file constant gate compares. `check`
                // lowers each discovered path exactly once, so this is the same
                // partition the old per-`lower()` counter drew — but it is now a
                // property of the FILE, so a re-lowering (the LSP) and a
                // persisted harvest agree with it too.
                Lowered::Parsed(lower_with_key(&result, FileKey::for_path(Path::new(path))), comments)
            }));
            match lowered {
                // A discovery-only file that cannot be parsed contributes no
                // harvest and no findings — the reference's discovery pass
                // likewise only consumes parsed sources.
                Ok(Lowered::Unparseable(diags)) if item.analyze => Stage1::ParseErrors {
                    order,
                    path: path.to_string(),
                    source,
                    diags,
                },
                Ok(Lowered::Unparseable(_)) => Stage1::Excluded,
                Ok(Lowered::Parsed(ast, comments)) => {
                    // Issue #92: the per-file HARVEST — everything the project
                    // index derives from this file's AST + the already-frozen
                    // `CoreIndex` (ADR-0028 freezes it before any worker starts).
                    // It reads no other file and no accumulated index state, so
                    // it belongs here, in the parallel stage, and leaves stage 2
                    // with only the genuinely cross-file joins.
                    //
                    // Deliberately OUTSIDE the `catch_unwind` above: a harvest
                    // panic propagates out of the pool exactly as a
                    // `build_project` panic used to propagate out of stage 2,
                    // rather than silently degrading the file to an
                    // internal-error diagnostic and dropping it from the index.
                    let t_harvest = timing.then(std::time::Instant::now);
                    let harvest = Box::new(rigor_infer::SourceIndex::harvest(&ast, &index));
                    let times = Stage1Times {
                        parse_lower: match (t_file, t_harvest) {
                            (Some(a), Some(b)) => b - a,
                            _ => std::time::Duration::ZERO,
                        },
                        harvest: t_harvest.map_or(std::time::Duration::ZERO, |t| t.elapsed()),
                    };
                    if item.analyze {
                        Stage1::Prepared(
                            Prepared { order, path: path.to_string(), source, ast, comments },
                            harvest,
                            times,
                        )
                    } else {
                        Stage1::Discovery {
                            ast,
                            harvest,
                            times,
                        }
                    }
                }
                // A discovery-only file that panics the lowerer contributes
                // nothing — the same silent skip as an unparseable one.
                Err(panic_val) if item.analyze => Stage1::Panic {
                    order,
                    path: path.to_string(),
                    msg: panic_message(&panic_val),
                },
                Err(_) => Stage1::Excluded,
            }
        })
        .collect();

    // Drain stage-1 outcomes in input order: deterministic stderr + findings.
    // `prepared` and `harvests` stay INDEX-ALIGNED — both are pushed here, in
    // input order, and nothing reorders either afterwards.
    let mut prepared: Vec<Prepared> = Vec::new();
    // Discovery-only ASTs (upstream #684), kept alive through the stage-2
    // merge; `harvests` stays in WORKLIST order (argv files first, then the
    // config-`paths:` extras), so `merge_entries[i]` says which vec
    // `harvests[i]`'s AST lives in.
    let mut discovery_asts: Vec<rigor_parse::LoweredAst> = Vec::new();
    enum MergeAst {
        Prepared(usize),
        Discovery(usize),
    }
    let mut merge_entries: Vec<MergeAst> = Vec::new();
    let mut harvests: Vec<rigor_infer::Harvest> = Vec::new();
    // `RIGOR_TIMING` accumulators — folded into the drain that already runs, so
    // the split costs no extra pass and nothing inside the parallel region.
    let mut pl_cpu = std::time::Duration::ZERO;
    let mut hv_cpu = std::time::Duration::ZERO;
    let mut hv_cpu_max = std::time::Duration::ZERO;
    for outcome in stage1 {
        match outcome {
            Stage1::Excluded => {}
            Stage1::Prepared(p, h, t) => {
                pl_cpu += t.parse_lower;
                hv_cpu += t.harvest;
                hv_cpu_max = hv_cpu_max.max(t.harvest);
                prepared.push(p);
                harvests.push(*h);
                merge_entries.push(MergeAst::Prepared(prepared.len() - 1));
            }
            Stage1::Discovery { ast, harvest, times } => {
                pl_cpu += times.parse_lower;
                hv_cpu += times.harvest;
                hv_cpu_max = hv_cpu_max.max(times.harvest);
                discovery_asts.push(ast);
                harvests.push(*harvest);
                merge_entries.push(MergeAst::Discovery(discovery_asts.len() - 1));
            }
            Stage1::ParseErrors { order, path, source, diags } => {
                // Pushed straight into `findings`, bypassing stage 3 — which is
                // exactly what the reference does. Its `analyze_file_body`
                // RETURNS `parse_diagnostics` before any rule runs, and both
                // downstream filters short-circuit on a nil rule anyway:
                // `SeverityStamp.stamp` ("return diagnostic if
                // diagnostic.rule.nil?") and `filter_suppressed` ("Diagnostics
                // with `rule == nil` … are NEVER suppressed — they represent
                // failures the user cannot silence away"). So a parse error is
                // not re-stamped by `severity_profile:`, not silenced by
                // `disable:`, and not silenced by a `# rigor:disable` marker.
                for diag in diags {
                    findings.push((order, path.clone(), source.clone(), diag));
                }
            }
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

    // STAGE 2 (serial barrier): MERGE the per-file harvests into ONE
    // project-wide source index. This is the cross-file join — it must see every
    // file's harvest, and it still takes the ASTs because two merge-resident
    // passes walk them (tier-4b return typing and the interprocedural literal
    // fold; issue #92 §5). The pairing is positional in WORKLIST order:
    // `harvests[i]` was harvested from the AST `merge_entries[i]` names (a
    // `prepared` entry for analyzed files, a `discovery_asts` entry for the
    // discovery-only ones).
    let mut harvests_iter = harvests.into_iter();
    let files: Vec<(rigor_infer::Harvest, &rigor_parse::LoweredAst)> = merge_entries
        .iter()
        .map(|entry| {
            let harvest = harvests_iter.next().expect("merge_entries is harvest-aligned");
            let ast = match entry {
                MergeAst::Prepared(i) => &prepared[*i].ast,
                MergeAst::Discovery(i) => &discovery_asts[*i],
            };
            (harvest, ast)
        })
        .collect();
    let project_source = rigor_infer::SourceIndex::merge(&files, &index);
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
                let mut diags = analyze_with_source_and_folder(
                    &p.ast, &mut interner, &index, &project_source, folder,
                );
                // `flow.shadowed-rescue-clause` (v0.3.0): its own pass over the
                // begin/rescue clause chains, needing the raw source text (RAW
                // exception slices + earlier-clause line numbers). Produced BEFORE
                // suppression / `disable:` filtering, exactly like the call rules.
                diags.extend(rigor_rules::shadowed_rescue_diagnostics(
                    &p.ast, &index, &project_source, &p.source,
                ));
                // `static.value-use.void` (ADR-100) — behind the
                // `use-of-void-value` bleeding-edge feature; produced BEFORE
                // suppression filtering like every check rule.
                if void_rule_active {
                    diags.extend(rigor_rules::void_value_use_diagnostics(
                        &p.ast,
                        &mut interner,
                        &index,
                        &project_source,
                    ));
                }
                // ADR-47 WD5 (upstream #627) — the dead arm of a decidable
                // version guard reports nothing: it cannot run on the Ruby being
                // checked with, so a diagnostic there is a false positive.
                // Applied AFTER every type/flow rule and BEFORE `suppression.*`
                // joins the list (which stays reportable inside a dead arm).
                rigor_rules::filter_dead_version_guard_arms(diags, &p.ast)
            }));
            match result {
                Ok(mut diags) => {
                    // Suppression-marker surveillance (`suppression.unknown-rule` /
                    // `suppression.empty`) is produced into the SAME list BEFORE
                    // `filter_suppressed`, so a marker can suppress its own complaint.
                    diags.extend(rigor_rules::suppression_marker_diagnostics(&p.comments));
                    let with_lines: Vec<(usize, Diagnostic)> = diags
                        .into_iter()
                        .map(|diag| (line_col(&p.source, diag.start_offset).0, diag))
                        .collect();
                    let mut local = Vec::new();
                    for (_line, mut diag) in
                        rigor_rules::filter_suppressed(with_lines, &p.comments)
                    {
                        // Config `disable:` — drop diagnostics whose rule matches the
                        // expanded disable set (the internal-error sentinel never matches).
                        if disable_matcher.suppresses(diag.rule_id) {
                            continue;
                        }
                        // SeverityStamp (reference `severity_stamp.rb`, ADR-8
                        // "Severity profile"): re-stamp from the profile +
                        // overrides and DROP a `:off` resolution. The
                        // internal-error sentinel bypasses (the reference's
                        // `rule.nil?` short-circuit — a per-file panic must
                        // never be silenced by configuration). The stamp input
                        // is the emitted severity; every catalogued rule is in
                        // all three profile tables, so the authored-fallback
                        // arm is only ever consulted for ids outside them.
                        if diag.rule_id != "internal-error" {
                            let current = match diag.severity {
                                rigor_rules::Severity::Error => severity::ResolvedSeverity::Error,
                                rigor_rules::Severity::Warning => {
                                    severity::ResolvedSeverity::Warning
                                }
                                rigor_rules::Severity::Info => severity::ResolvedSeverity::Info,
                            };
                            match severity::resolve(
                                diag.rule_id,
                                current,
                                profile,
                                &user_overrides,
                                &bleeding_overrides,
                            ) {
                                severity::ResolvedSeverity::Off => continue,
                                severity::ResolvedSeverity::Error => {
                                    diag.severity = rigor_rules::Severity::Error;
                                }
                                severity::ResolvedSeverity::Warning => {
                                    diag.severity = rigor_rules::Severity::Warning;
                                }
                                severity::ResolvedSeverity::Info => {
                                    diag.severity = rigor_rules::Severity::Info;
                                }
                            }
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

    // ADR-17 slice 1 — `pre-eval.file-not-found`: a run-level `:error` row for
    // each `pre_eval:` entry that is not a FILE on disk (the reference's
    // `File.file?` — a directory earns the row too). Run-level, not per-file:
    // upstream these lead `pre_file_diagnostics` — AHEAD of the
    // `expansion.errors` rows `prepend_path_errors` splices in (which is why
    // that fn skips leading `pre-eval.*` entries) — and the `disable:` filter
    // never reaches them (oracle: `disable: [pre-eval.file-not-found]` leaves
    // the row standing), while `severity_overrides:` / `severity_profile:`
    // DO re-stamp them (`SeverityStamp.apply` covers the whole stream). The
    // `path:` is the literal ".rigor.yml" upstream even under `--config` or a
    // `.rigor.dist.yml` discovery — the aggregator hard-codes it. Glob-meta
    // entries (`*`, `?`, `[`) are `Dir.glob`'d into concrete files upstream
    // (`expand_pre_eval_entries`) and never earn this row, matched or not.
    // `pre_file_diagnostics` order upstream: `plugin_loader.load-error` rows
    // FIRST, then the pre-eval stream, then (streams the port does not carry)
    // and finally `expansion.errors` — which `prepend_path_errors` splices in
    // after the leading run-level rows. Load-error rows bypass `disable:`,
    // `severity_profile:` and `severity_overrides:` entirely upstream
    // (SeverityStamp is not applied to them — oracle: `plugin_loader: "off"`
    // leaves the row at error), so they carry their authored severity.
    let mut run_rows: Vec<(usize, String, String, Diagnostic)> = plugin_resolution
        .load_errors
        .iter()
        .map(|row| {
            (
                0usize,
                ".rigor.yml".to_string(),
                String::new(),
                Diagnostic {
                    rule_id: row.rule_id,
                    start_offset: 0,
                    end_offset: 0,
                    message: row.message.clone(),
                    severity: row.severity,
                    source_family: row.source_family,
                    receiver_type: None,
                    method_name: None,
                },
            )
        })
        .collect();

    if !cfg.pre_eval.is_empty() {
        // `expand_pre_eval_entries` ends in `.uniq` — a duplicated literal
        // entry earns ONE row upstream, not one per mention.
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for entry in &cfg.pre_eval {
            if !seen.insert(entry.as_str())
                || entry.contains(['*', '?', '['])
                || std::path::Path::new(entry).is_file()
            {
                continue;
            }
            let sev = match severity::resolve(
                "pre-eval.file-not-found",
                severity::ResolvedSeverity::Error,
                profile,
                &user_overrides,
                &bleeding_overrides,
            ) {
                severity::ResolvedSeverity::Off => continue,
                severity::ResolvedSeverity::Error => Severity::Error,
                severity::ResolvedSeverity::Warning => Severity::Warning,
                severity::ResolvedSeverity::Info => Severity::Info,
            };
            run_rows.push((
                0usize,
                ".rigor.yml".to_string(),
                String::new(),
                Diagnostic {
                    rule_id: "pre-eval.file-not-found",
                    start_offset: 0,
                    end_offset: 0,
                    // `{path.inspect}` upstream — `{:?}` renders the same
                    // double-quoted string for a path.
                    message: format!(
                        "pre_eval entry not found: {entry:?}. \
                         Pre-evaluation requires the file to exist on disk; \
                         remove the entry or create the file before \
                         re-running analysis."
                    ),
                    severity: sev,
                    source_family: "builtin",
                    receiver_type: None,
                    method_name: None,
                },
            ));
        }
    }
    run_rows.append(&mut findings);
    findings = run_rows;

    // `plugin_run_emission_diagnostics` upstream — the plugins' own
    // run-scoped rows land AFTER the per-file stream (and before the
    // `conforms-to` rows): `plugin.<id>.load-error` file-probe disclosures.
    for row in &plugin_resolution.emitted {
        findings.push((
            0usize,
            ".rigor.yml".to_string(),
            String::new(),
            Diagnostic {
                rule_id: row.rule_id,
                start_offset: 0,
                end_offset: 0,
                message: row.message.clone(),
                severity: row.severity,
                source_family: row.source_family,
                receiver_type: None,
                method_name: None,
            },
        ));
    }

    // Issue #129 — the `rigor:v1:conforms-to` rows. Run-level, not per-file:
    // the reference appends them AFTER the per-file stream, positioned at the
    // annotation in the project `.rbs`, re-stamped by the severity profile but
    // NOT filtered by `disable:` (its `disable:` filter only sees per-file rows;
    // oracle-measured, `disable: [all]` leaves both rows standing).
    // Round 5: the scan runs for `check` ONLY. `diff`, `triage` and the
    // `baseline` subcommands have their own option parsers upstream that the
    // gate does not model, so they never carry a conformance row.
    if verb == "check" && conformance_scan_active(cfg, root, ref_has_files) {
        findings.extend(conformance_rows(&index, profile, &user_overrides, &bleeding_overrides));
    }

    if timing {
        let t_end = std::time::Instant::now();
        let threads = rayon::current_num_threads();
        // See `Stage1Times`: `*-cpu` fields are SUMS of per-file worker time,
        // `harvest-wall-amortized` is `harvest-cpu / threads` (a model), and
        // `stage1-ex-harvest` is stage 1's measured wall minus that model.
        let hv_wall = hv_cpu / u32::try_from(threads.max(1)).unwrap_or(1);
        eprintln!(
            "rigor timing: index-load={:.3?} stage1(parse+lower+harvest)={:.3?} \
             stage1.parse+lower-cpu={:.3?} stage1.harvest-cpu={:.3?} \
             stage1.harvest-cpu-max={:.3?} stage1.harvest-wall-amortized={:.3?} \
             stage1-ex-harvest={:.3?} \
             stage2(merge)={:.3?} stage3(analyze)={:.3?} sort={:.3?} \
             total={:.3?} files={} threads={}",
            t_index - t_start,
            t_stage1 - t_index,
            pl_cpu,
            hv_cpu,
            hv_cpu_max,
            hv_wall,
            (t_stage1 - t_index).saturating_sub(hv_wall),
            t_stage2 - t_stage1,
            t_stage3 - t_stage2,
            t_end - t_stage3,
            t_end - t_start,
            prepared.len(),
            threads,
        );
    }
    (findings, had_io_error)
}

/// Issue #129: whether the reference's run over these positional roots (none
/// = the config's `paths:`, default `lib`) has a Ruby file at all — see
/// [`conformance_gate::reference_has_ruby_files`].
pub(crate) fn reference_has_ruby_files(cfg: &Config, positional: &[&str]) -> bool {
    conformance_gate::reference_has_ruby_files(
        positional,
        cfg.paths_explicitly_declared().then_some(cfg.paths.as_slice()),
        cfg.config_base_dir(),
        &cfg.exclude,
    )
}

/// Issue #129 — whether the `conforms-to` scan runs at all. The reference gates
/// it on the project CONFIGURING `signature_paths:` (`project_signature_paths?`:
/// non-nil and non-empty — a defaulted `sig/` is loaded but not scanned), and
/// rigor-rs additionally stands down whenever the reference's RBS environment
/// may hold a source rigor-rs does not load: a declaration that source makes
/// could resolve the interface ("not loaded" would then be a false positive)
/// or give the class a member (`libraries: [json]` reopens `Object`). Those are
/// `libraries:`, the bundler gem-`sig/` walk (`bundler:` / `.bundle/config` /
/// `vendor/bundle`), an unbundled plugin, and `includes:` (whose merged keys
/// rigor-rs does not read). It also stands down on a `target_ruby:` the
/// reference may reject: that run emits no row but its own error.
///
/// Round 3 (ADR-0044 § "Environment-parity gate") adds the clauses that prove
/// the reference runs in the environment the port models: the reference has
/// at least one Ruby file (else it builds no environment), the config text
/// is one it provably loads and reads the same way, every signature path is
/// the directory it reads, no rbs collection (its skip list is not
/// modelled), and a `Gemfile.lock` the port parses whole. The per-file
/// clauses (parse, NUL, `use`, `resolve-type-names`) are index-side.
fn conformance_scan_active(cfg: &Config, root: &Path, ref_has_files: bool) -> bool {
    let Some(entries) = cfg.explicit_signature_paths().filter(|e| !e.is_empty()) else {
        return false;
    };
    if !ref_has_files || !cfg.parity_text_ok() || !cfg.target_ruby_supported() {
        return false;
    }
    if !entries
        .iter()
        .all(|e| conformance_gate::signature_entry_ok(e, cfg.config_base_dir()))
    {
        return false;
    }
    if !conformance_gate::lockfile_ok(root)
        || root.join("rbs_collection.lock.yaml").exists()
        || !cfg.collection_signature_dirs(root).is_empty()
    {
        return false;
    }
    if ["libraries", "bundler", "includes"].iter().any(|k| cfg.declares_key(k)) {
        return false;
    }
    if conformance_gate::bundle_sources_present(root, std::env::var_os("HOME").as_deref()) {
        return false;
    }
    // Every enabled `plugins:` entry must activate a port-bundled plugin
    // cleanly — a load-error/emitted row or an unemulated activation means
    // the reference's environment diverges from the port's index (issue
    // #157: entries are gem names now; a bare manifest id is a load error).
    let resolution = cfg.plugin_resolution(root);
    resolution.load_errors.is_empty()
        && resolution.stderr.is_empty()
        && resolution.activated.len()
            == cfg
                .plugin_entries()
                .iter()
                .filter(|e| e.enabled)
                .count()
}

/// Issue #129 — the scan's findings as `(order, path, source, diagnostic)`
/// rows: the `.rbs` text rides along as the source so the annotation's byte
/// offset resolves to its line/column like every other row. Authored
/// `:warning`, re-stamped by the severity profile (`strict` makes the
/// unsatisfied row an error), dropped when it resolves `:off`.
fn conformance_rows(
    index: &CoreIndex,
    profile: severity::Profile,
    user_overrides: &[(String, severity::ResolvedSeverity)],
    bleeding_overrides: &[(&str, severity::ResolvedSeverity)],
) -> Vec<(usize, String, String, Diagnostic)> {
    let mut rows = Vec::new();
    for f in index.conformance_findings() {
        let resolved = severity::resolve(
            f.rule_id(),
            severity::ResolvedSeverity::Warning,
            profile,
            user_overrides,
            bleeding_overrides,
        );
        let severity = match resolved {
            severity::ResolvedSeverity::Off => continue,
            severity::ResolvedSeverity::Error => rigor_rules::Severity::Error,
            severity::ResolvedSeverity::Warning => rigor_rules::Severity::Warning,
            severity::ResolvedSeverity::Info => rigor_rules::Severity::Info,
        };
        // The bytes the index parsed — a re-read could see a file changed
        // since, and position the row against other text.
        let Some(source) = index.conformance_source(f.file).map(str::to_string) else {
            continue;
        };
        // RBS reports the annotation's column in CHARACTERS (Unicode scalar
        // values: oracle-measured with 2-, 3- and 4-byte characters and a
        // combining mark before it), where `line_col` counts bytes. Shift the
        // offset back by the difference so the shared renderer prints the
        // reference's column.
        let delta = char_column_delta(&source, f.start_offset);
        let diag = Diagnostic {
            rule_id: f.rule_id(),
            start_offset: f.start_offset - delta,
            end_offset: f.end_offset.saturating_sub(delta),
            message: f.message(),
            severity,
            source_family: "builtin",
            receiver_type: None,
            method_name: None,
        };
        rows.push((usize::MAX, f.file.to_string(), source, diag));
    }
    rows
}

/// Bytes minus characters between the start of `offset`'s line and `offset`
/// (0 on an ASCII line or an offset off a character boundary).
fn char_column_delta(source: &str, offset: usize) -> usize {
    let Some(before) = source.get(..offset) else {
        return 0;
    };
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    line.len() - line.chars().count()
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
/// `BaselineCommand#help`'s heredoc, verbatim — stdout for
/// `rigor baseline`/`--help`, stderr tail for the unknown-subcommand error.
const BASELINE_HELP: &str = "\
Usage: rigor baseline <subcommand> [options]

Subcommands:
  generate    Write a fresh baseline file from a `rigor check` run.
  regenerate  Rewrite the baseline unconditionally (post-fix refresh).
  dump        Print the contents of an existing baseline.
  drift       Compare baseline vs current diagnostics (reduction / regression hints).
  prune       Drop cleared buckets (`actual == 0`) from the baseline.

Run `rigor baseline <subcommand> --help` for subcommand options.
";

fn cmd_baseline(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None | Some("help") | Some("-h") | Some("--help") => {
            print!("{BASELINE_HELP}");
            ExitCode::SUCCESS
        }
        Some("generate") => baseline_generate(&args[1..]),
        Some("regenerate") => baseline_regenerate(&args[1..]),
        Some("dump") => baseline_dump(&args[1..]),
        Some("drift") => baseline_drift(&args[1..]),
        Some("prune") => baseline_prune(&args[1..]),
        Some(other) => {
            // `run`'s else: `subcommand.inspect` — the Ruby double-quoted
            // spelling Rust's `{:?}` reproduces for ordinary words.
            eprintln!("Unknown baseline subcommand: {other:?}");
            eprint!("{BASELINE_HELP}");
            ExitCode::from(64)
        }
    }
}

/// A run's findings: `(input-order, path, source, diagnostic)` tuples.
type Findings = Vec<(usize, String, String, Diagnostic)>;

/// The baseline subcommand switch tables — `parse_generate_options` /
/// `parse_dump_options` / `parse_drift_options` / `parse_prune_options` in
/// `cli/baseline_command.rb`, declaration order. `regenerate` shares
/// generate's table minus `--force`.
const BASELINE_GENERATE_SWITCHES: &[optparse::Switch] = &[
    optparse::Switch::new("config", &[("config", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
    optparse::Switch::new("output", &[("output", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--output", "=PATH", &["Write baseline to PATH (default: .rigor-baseline.yml)"]),
    optparse::Switch::new("match-mode", &[("match-mode", false)], optparse::ArgStyle::Required, optparse::ValueKind::Choice(&["rule", "message"]), "--match-mode", "=MODE", &["Row form: rule (default) or message"]),
    optparse::Switch::new("force", &[("force", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--force", "", &["Overwrite an existing baseline file"]),
];
const BASELINE_REGENERATE_SWITCHES: &[optparse::Switch] = &[
    optparse::Switch::new("config", &[("config", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
    optparse::Switch::new("output", &[("output", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--output", "=PATH", &["Write baseline to PATH (default: .rigor-baseline.yml)"]),
    optparse::Switch::new("match-mode", &[("match-mode", false)], optparse::ArgStyle::Required, optparse::ValueKind::Choice(&["rule", "message"]), "--match-mode", "=MODE", &["Row form: rule (default) or message"]),
];
const BASELINE_DUMP_SWITCHES: &[optparse::Switch] = &[
    optparse::Switch::new("baseline", &[("baseline", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--baseline", "=PATH", &["Path to the baseline file (default: .rigor-baseline.yml)"]),
    optparse::Switch::new("format", &[("format", false)], optparse::ArgStyle::Required, optparse::ValueKind::Choice(&["text", "json"]), "--format", "=FORMAT", &["Output format: text (default) or json"]),
    optparse::Switch::new("rule", &[("rule", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--rule", "=RULE", &["Filter rows by exact rule id"]),
    optparse::Switch::new("file", &[("file", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--file", "=GLOB", &["Filter rows by File.fnmatch? glob"]),
];
const BASELINE_DRIFT_SWITCHES: &[optparse::Switch] = &[
    optparse::Switch::new("config", &[("config", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
    optparse::Switch::new("baseline", &[("baseline", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--baseline", "=PATH", &["Path to the baseline file (default: .rigor-baseline.yml)"]),
    optparse::Switch::new("only", &[("only", false)], optparse::ArgStyle::Required, optparse::ValueKind::Choice(&["within", "over", "cleared", "reducible"]), "--only", "=STATUS", &["Show only buckets with the given status (within|over|cleared|reducible)"]),
];
const BASELINE_PRUNE_SWITCHES: &[optparse::Switch] = &[
    optparse::Switch::new("config", &[("config", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
    optparse::Switch::new("baseline", &[("baseline", false)], optparse::ArgStyle::Required, optparse::ValueKind::Raw, "--baseline", "=PATH", &["Path to the baseline file (default: .rigor-baseline.yml)"]),
    optparse::Switch::new("dry-run", &[("dry-run", false)], optparse::ArgStyle::Flag, optparse::ValueKind::Raw, "--dry-run", "", &["Show what would be dropped without writing the file"]),
];

const BASELINE_GENERATE_PARSER: optparse::OptParser =
    optparse::OptParser::new("Usage: rigor baseline generate [options]", BASELINE_GENERATE_SWITCHES);
const BASELINE_REGENERATE_PARSER: optparse::OptParser =
    optparse::OptParser::new("Usage: rigor baseline regenerate [options]", BASELINE_REGENERATE_SWITCHES);
const BASELINE_DUMP_PARSER: optparse::OptParser =
    optparse::OptParser::new("Usage: rigor baseline dump [options]", BASELINE_DUMP_SWITCHES);
const BASELINE_DRIFT_PARSER: optparse::OptParser =
    optparse::OptParser::new("Usage: rigor baseline drift [options]", BASELINE_DRIFT_SWITCHES);
const BASELINE_PRUNE_PARSER: optparse::OptParser =
    optparse::OptParser::new("Usage: rigor baseline prune [options]", BASELINE_PRUNE_SWITCHES);

/// Shared analysis path for generate/regenerate/drift/prune: load config,
/// build the sidecar folder, resolve roots, analyze, and return the findings
/// paired with their project-root-relative path (the baseline matcher key).
///
/// `roots`: positional path args (empty → config `paths:`). generate/regenerate
/// and (since ADR-22-followup) drift/prune pass their positionals — a deliberate
/// rigor-rs extension over the reference, which always analyzes config `paths:`.
/// A caller that passes an empty slice still falls back to config `paths:`, so
/// the no-positional invocation stays reference-faithful.
///
/// The returned `bool` is `scope_undeclared`: `true` when there is NO explicit
/// analysis scope — no positional roots were passed AND `.rigor.yml` did not
/// declare `paths:` (so the run falls back to the implicit `["lib"]` default).
/// drift/prune use it to refuse a scope-less audit against a non-empty baseline
/// (which would silently misjudge every out-of-`lib` bucket as cleared);
/// generate/regenerate ignore it (writing a `lib`-scoped baseline is fine).
///
/// The returned `usize` is the count of bad-path expansion diagnostics —
/// `expand_paths` errors the reference puts in the run's `diagnostics` list
/// (`rule: nil`, severity warn-or-error). They never carry a rule, so they
/// are not in `findings` and never land in a baseline bucket, but the
/// reference's `generate`/`regenerate` summary counts them
/// (`diagnostics.size` covers them): `covering 1 diagnostic(s)` for a
/// missing default `lib/` even when the baseline it writes is empty.
///
/// Returns `Err(code)` if the sidecar folder fails to build.
fn baseline_analysis(
    explicit_config: Option<&str>,
    roots: &[&str],
    verb: &'static str,
) -> Result<(Config, Findings, bool, usize), ExitCode> {
    let cfg = Config::load(explicit_config.map(Path::new)).map_err(|f| f.report())?;
    let sidecar_folder = build_sidecar_folder(&cfg, None)?;
    let folder_ref =
        sidecar_folder.as_ref().map(|f| f as &(dyn rigor_infer::RubyFolder + Sync));

    // "No positional roots AND no declared `paths:`" — the reference's default
    // `["lib"]` is a fallback, not a user-declared scope.
    let scope_undeclared = roots.is_empty() && !cfg.paths_explicitly_declared();
    let roots_given = !roots.is_empty();

    let config_path_strings: Vec<String>;
    let config_paths: Vec<&str>;
    let roots: &[&str] = if roots.is_empty() {
        // `runner.run(configuration.paths)` — declared `paths:` arrive
        // absolutized upstream (`resolve_paths_in`), so the exclusion match
        // and the rendered diagnostic path see the absolute spelling.
        config_path_strings = effective_config_paths(&cfg);
        config_paths = config_path_strings.iter().map(String::as_str).collect();
        &config_paths
    } else {
        roots
    };
    let ref_has_files = reference_has_ruby_files(&cfg, if roots_given { roots } else { &[] });
    // The reference's baseline generation runs `runner.run(paths)`, which
    // expands through the same `PathExpansion` as `check` —
    // `reject_excluded` on directory entries, `.rb` file roots verbatim.
    let (expanded_owned, path_errors) =
        expand_check_paths_excluding(roots, &exclude_patterns(&cfg));
    let expanded: Vec<&str> = expanded_owned.iter().map(String::as_str).collect();
    let (findings, _had_io_error) = analyze_files(
        &expanded,
        // `None` when the roots are the config `paths:` fallback — the
        // reference's baseline always analyzes `paths:` and never widens.
        if roots_given { Some(roots) } else { None },
        &cfg,
        verb,
        folder_ref,
        &cfg.bleeding_edge_selector(),
        ref_has_files,
    );
    Ok((cfg, findings, scope_undeclared, path_errors.len()))
}

/// Relativize findings against cwd, as the baseline matcher keys on
/// project-root-relative paths (the reference's `Dir.pwd`).
fn baseline_entries(
    findings: &[(usize, String, String, Diagnostic)],
) -> Vec<(String, &Diagnostic)> {
    let cwd = std::env::current_dir().ok();
    findings
        .iter()
        .map(|(_, p, _, d)| (relative_path(p, cwd.as_deref()), d))
        .collect()
}

/// Load a baseline for drift/prune, enforcing the reference's strict existence
/// + parse contract (unlike `Baseline::load`, a missing file is an error here).
///
/// `Err(ExitCode::from(64))` on missing/malformed; the message is on stderr.
fn load_baseline_strict(path: &str) -> Result<Baseline, ExitCode> {
    if !Path::new(path).exists() {
        eprintln!("rigor: baseline file not found: {path}");
        return Err(ExitCode::from(64));
    }
    Baseline::load(Path::new(path)).map_err(|e| {
        eprintln!("rigor: baseline load failed: {e}");
        ExitCode::from(64)
    })
}

/// `rigor baseline generate` — run `check` over the files and write a baseline.
fn baseline_generate(args: &[String]) -> ExitCode {
    let items = match BASELINE_GENERATE_PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut files: Vec<String> = Vec::new();
    let mut output = DEFAULT_BASELINE_PATH.to_string();
    let mut mode = MatchMode::Rule;
    let mut force = false;
    let mut explicit_config: Option<String> = None;
    for item in items {
        match item {
            // A rigor-rs generate-parity extension: positional roots override
            // config `paths:`. The reference accepts no positionals here.
            Item::Positional(p) => files.push(p),
            Item::Opt { key, value, .. } => match key {
                "config" => explicit_config = Some(value.unwrap().as_str().to_string()),
                "output" => output = value.unwrap().as_str().to_string(),
                // `Choice` already canonicalized/validated the value.
                "match-mode" => {
                    mode = if value.unwrap().as_str() == "message" {
                        MatchMode::Message
                    } else {
                        MatchMode::Rule
                    }
                }
                "force" => force = true,
                _ => unreachable!("the switch table is closed"),
            },
        }
    }
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();

    if Path::new(&output).exists() && !force {
        eprintln!(
            "rigor: {output} already exists. Re-run with --force to overwrite, \
             or use `rigor baseline regenerate`."
        );
        return ExitCode::from(64);
    }

    write_baseline(explicit_config.as_deref(), &file_refs, &output, mode, "wrote baseline to")
}

/// `rigor baseline regenerate` — `generate --force` with a different success
/// verb: unconditional overwrite (no existence check, no `--force` flag — its
/// table lacks `force`, so `--force` is `invalid option: --force`). Roots
/// follow the same rigor-rs generate-parity extension (positionals-if-given
/// else config `paths:`); the reference always analyzes config `paths:`.
fn baseline_regenerate(args: &[String]) -> ExitCode {
    let items = match BASELINE_REGENERATE_PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut files: Vec<String> = Vec::new();
    let mut output = DEFAULT_BASELINE_PATH.to_string();
    let mut mode = MatchMode::Rule;
    let mut explicit_config: Option<String> = None;
    for item in items {
        match item {
            Item::Positional(p) => files.push(p),
            Item::Opt { key, value, .. } => match key {
                "config" => explicit_config = Some(value.unwrap().as_str().to_string()),
                "output" => output = value.unwrap().as_str().to_string(),
                "match-mode" => {
                    mode = if value.unwrap().as_str() == "message" {
                        MatchMode::Message
                    } else {
                        MatchMode::Rule
                    }
                }
                _ => unreachable!("the switch table is closed"),
            },
        }
    }
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();

    write_baseline(explicit_config.as_deref(), &file_refs, &output, mode, "regenerated baseline")
}

/// Shared generate/regenerate writer: analyze, build the baseline, write it, and
/// emit the stderr summary + the `note` line when `.rigor.yml` lacks
/// `baseline:`. `verb` differs (`wrote baseline to` vs `regenerated baseline`).
fn write_baseline(
    explicit_config: Option<&str>,
    files: &[&str],
    output: &str,
    mode: MatchMode,
    verb: &str,
) -> ExitCode {
    // Same coverage posture as `check` (ADR-0008/0036) so the baseline records
    // exactly what `check` witnesses. IMPORTANT (reference parity): the baseline
    // records the UNFILTERED set — `analyze_files` never applies an existing
    // baseline, so the new file records live diagnostics, not the post-baseline
    // (empty) surface.
    // generate/regenerate ignore `analysis_set_empty`: an empty resolved set
    // legitimately writes an empty baseline (nothing to record).
    let (cfg, findings, _analysis_set_empty, path_error_count) =
        match baseline_analysis(explicit_config, files, "baseline") {
            Ok(v) => v,
            Err(code) => return code,
        };
    let entries = baseline_entries(&findings);
    let baseline = Baseline::from_diagnostics(&entries, mode);
    if let Err(e) = std::fs::write(output, baseline.to_yaml()) {
        eprintln!("rigor baseline: cannot write {output}: {e}");
        return ExitCode::from(1);
    }
    let mode_str = match mode {
        MatchMode::Rule => "rule",
        MatchMode::Message => "message",
    };
    // The reference's `diagnostics.size` counts every run diagnostic —
    // `rule: nil` path-expansion errors included — while the baseline FILE
    // buckets only ruled findings (`entries`), so the summary counts the
    // union even when the file stays empty (issue #201's `covering 0` row).
    eprintln!(
        "rigor: {verb} {output} ({} bucket(s) covering {} diagnostic(s); match-mode: {mode_str})",
        baseline.size(),
        entries.len() + path_error_count
    );
    if cfg.baseline_path().is_none() {
        // The reference names the config file actually read — the explicit
        // `--config` path when given, else `Configuration.discover`'s winner
        // (`.rigor.yml` before `.rigor.dist.yml`), else the bare name.
        let config_label = explicit_config.map(str::to_string).or_else(|| {
            Config::discover().map(|p| p.display().to_string())
        });
        let config_label = config_label.as_deref().unwrap_or(".rigor.yml");
        eprintln!(
            "rigor: note — `{config_label}` does not declare `baseline:`; \
             add `baseline: {output}` to activate the suppression."
        );
    }
    ExitCode::SUCCESS
}

/// `rigor baseline dump` — print an existing baseline's rows, honouring
/// `--format` (`text`|`json`), `--rule` (exact) and `--file` (`File.fnmatch?`
/// glob — `conformance_gate::fnmatch`, the no-flags port) filters.
fn baseline_dump(args: &[String]) -> ExitCode {
    let items = match BASELINE_DUMP_PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut path = DEFAULT_BASELINE_PATH.to_string();
    let mut format = String::from("text");
    let mut rule: Option<String> = None;
    let mut file_glob: Option<String> = None;
    for item in items {
        match item {
            Item::Positional(_) => {} // upstream leaves them in argv, unused
            Item::Opt { key, value, .. } => match key {
                "baseline" => path = value.unwrap().as_str().to_string(),
                "format" => format = value.unwrap().as_str().to_string(),
                "rule" => rule = Some(value.unwrap().as_str().to_string()),
                "file" => file_glob = Some(value.unwrap().as_str().to_string()),
                _ => unreachable!("the switch table is closed"),
            },
        }
    }
    let baseline = match load_baseline_strict(&path) {
        Ok(b) => b,
        Err(code) => return code,
    };
    // `filter_dump_rows`: exact rule match, `File.fnmatch?`-no-flags file glob.
    let rows: Vec<&Bucket> = baseline
        .buckets()
        .iter()
        .filter(|b| {
            if let Some(r) = &rule {
                if &b.rule != r {
                    return false;
                }
            }
            if let Some(g) = &file_glob {
                if !conformance_gate::fnmatch(g, &b.file) {
                    return false;
                }
            }
            true
        })
        .collect();
    if format == "json" {
        println!("{}", dump_json(&rows));
    } else {
        dump_text(&rows);
    }
    ExitCode::SUCCESS
}

/// `dump_text`: empty → `(no baseline rows matching the supplied filters)`;
/// else group by rule (first-seen order, most buckets first — `group_by` +
/// `sort_by -group.size`, stable), buckets by `[-count, file]`, a blank line
/// between groups, then the `Total:` line over the FILTERED rows.
fn dump_text(rows: &[&Bucket]) {
    if rows.is_empty() {
        println!("(no baseline rows matching the supplied filters)");
        return;
    }
    let mut groups: Vec<(&str, Vec<&Bucket>)> = Vec::new();
    for b in rows {
        if let Some(g) = groups.iter_mut().find(|(r, _)| *r == b.rule) {
            g.1.push(b);
        } else {
            groups.push((b.rule.as_str(), vec![b]));
        }
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
    let mut occurrences = 0usize;
    for (rule, group) in &groups {
        let total: usize = group.iter().map(|b| b.count).sum();
        occurrences += total;
        println!("{rule}  ({} bucket(s), {total} occurrence(s))", group.len());
        let mut sorted = group.clone();
        sorted.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.file.cmp(&b.file)));
        for bucket in sorted {
            match &bucket.message {
                Some(m) => println!("  {}: {}  ~/{m}/", bucket.file, bucket.count),
                None => println!("  {}: {}", bucket.file, bucket.count),
            }
        }
        println!();
    }
    println!("Total: {} bucket(s), {occurrences} occurrence(s)", rows.len());
}

/// `dump_to_json` + `JSON.pretty_generate` — the `{"version":…,"ignored":[…]}`
/// document at two-space indent. `message` rides `message_regex.source` — the
/// stored bucket `message` IS that source (it round-trips byte-for-byte).
fn dump_json(rows: &[&Bucket]) -> String {
    let mut out = String::from("{\n  \"version\": ");
    out.push_str(&baseline::CURRENT_VERSION.to_string());
    out.push_str(",\n  \"ignored\": ");
    if rows.is_empty() {
        out.push_str("[]\n}");
        return out;
    }
    out.push_str("[\n");
    for (i, b) in rows.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!("      \"file\": {},\n", json_string(&b.file)));
        out.push_str(&format!("      \"rule\": {},\n", json_string(&b.rule)));
        out.push_str(&format!("      \"count\": {}", b.count));
        if let Some(m) = &b.message {
            out.push_str(&format!(",\n      \"message\": {}", json_string(m)));
        }
        out.push_str("\n    }");
        if i + 1 < rows.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}");
    out
}

/// `rigor baseline drift` — audit current diagnostics against the baseline and
/// report per-bucket drift. Informational: exit 0 whether or not drift is
/// found; exit 64 only for usage / missing / malformed baseline.
fn baseline_drift(args: &[String]) -> ExitCode {
    let items = match BASELINE_DRIFT_PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut path = DEFAULT_BASELINE_PATH.to_string();
    let mut only: Option<DriftStatus> = None;
    let mut explicit_config: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    for item in items {
        match item {
            // A rigor-rs generate-parity extension: positional roots override
            // config `paths:`. The reference accepts no positionals here.
            Item::Positional(p) => files.push(p),
            Item::Opt { key, value, .. } => match key {
                "config" => explicit_config = Some(value.unwrap().as_str().to_string()),
                "baseline" => path = value.unwrap().as_str().to_string(),
                "only" => {
                    only = Some(match value.unwrap().as_str() {
                        "within" => DriftStatus::Within,
                        "over" => DriftStatus::Over,
                        "cleared" => DriftStatus::Cleared,
                        "reducible" => DriftStatus::Reducible,
                        _ => unreachable!("Choice canonicalizes"),
                    });
                }
                _ => unreachable!("the switch table is closed"),
            },
        }
    }
    let explicit_config = explicit_config.as_deref();
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();

    let baseline = match load_baseline_strict(&path) {
        Ok(b) => b,
        Err(code) => return code,
    };
    // Positionals-if-given, else config `paths:` (the reference-faithful path).
    let findings = match baseline_analysis(explicit_config, &file_refs, "baseline") {
        Ok((_cfg, f, scope_undeclared, _path_errors)) => {
            // Guard the scope-less audit: with no declared analysis scope, every
            // bucket outside the implicit `lib` default would falsely read as
            // "cleared". Refuse rather than mislead.
            if scope_undeclared && !baseline.is_empty() {
                eprintln!(
                    "rigor: baseline drift: nothing to analyze — pass a path \
                     (e.g. `rigor baseline drift .`) or declare `paths:` in .rigor.yml"
                );
                return ExitCode::from(64);
            }
            f
        }
        Err(code) => return code,
    };
    let entries = baseline_entries(&findings);
    let rows = baseline.audit(&entries);

    // Display filter: default = delta != 0; --only = status == S.
    let shown: Vec<&baseline::DriftRow> = match only {
        None => rows.iter().filter(|r| r.delta != 0).collect(),
        Some(s) => rows.iter().filter(|r| r.status == s).collect(),
    };

    if shown.is_empty() {
        println!("No drift detected.");
        return ExitCode::SUCCESS;
    }

    println!("Drift report against {path}:");
    println!();
    for status in [DriftStatus::Over, DriftStatus::Cleared, DriftStatus::Reducible, DriftStatus::Within] {
        let mut group: Vec<&&baseline::DriftRow> =
            shown.iter().filter(|r| r.status == status).collect();
        if group.is_empty() {
            continue;
        }
        group.sort_by(|a, b| (&a.bucket.file, &a.bucket.rule).cmp(&(&b.bucket.file, &b.bucket.rule)));
        let n = group.len();
        println!("{}", drift_section_header(status, n));
        for row in group {
            let delta_str = match row.delta.cmp(&0) {
                std::cmp::Ordering::Greater => format!("+{}", row.delta),
                _ => row.delta.to_string(),
            };
            println!(
                "  {}  [{}]  {} → {}  (Δ{delta_str})",
                row.bucket.file, row.bucket.rule, row.bucket.count, row.actual
            );
        }
        println!();
    }
    ExitCode::SUCCESS
}

fn drift_section_header(status: DriftStatus, n: usize) -> String {
    match status {
        DriftStatus::Over => {
            format!("## Over threshold ({n}) — bucket exceeded; check the regular diagnostic output.")
        }
        DriftStatus::Cleared => {
            format!("## Cleared ({n}) — `rigor baseline prune` can drop these.")
        }
        DriftStatus::Reducible => {
            format!("## Reducible ({n}) — tightening opportunity; run `rigor baseline regenerate`.")
        }
        DriftStatus::Within => format!("## Within threshold ({n})"),
    }
}

/// `rigor baseline prune` — drop cleared buckets (`actual == 0`) from the
/// baseline. Same missing/malformed handling as drift (exit 64).
fn baseline_prune(args: &[String]) -> ExitCode {
    let items = match BASELINE_PRUNE_PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut path = DEFAULT_BASELINE_PATH.to_string();
    let mut dry_run = false;
    let mut explicit_config: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    for item in items {
        match item {
            // A rigor-rs generate-parity extension: positional roots override
            // config `paths:`. The reference accepts no positionals here.
            Item::Positional(p) => files.push(p),
            Item::Opt { key, value, .. } => match key {
                "config" => explicit_config = Some(value.unwrap().as_str().to_string()),
                "baseline" => path = value.unwrap().as_str().to_string(),
                "dry-run" => dry_run = true,
                _ => unreachable!("the switch table is closed"),
            },
        }
    }
    let explicit_config = explicit_config.as_deref();
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();

    let baseline = match load_baseline_strict(&path) {
        Ok(b) => b,
        Err(code) => return code,
    };
    // Positionals-if-given, else config `paths:` (the reference-faithful path).
    let findings = match baseline_analysis(explicit_config, &file_refs, "baseline") {
        Ok((_cfg, f, scope_undeclared, _path_errors)) => {
            // Guard the scope-less audit: with no declared analysis scope, every
            // cleared-looking bucket outside the implicit `lib` default would be
            // dropped — emptying a live baseline. Refuse.
            if scope_undeclared && !baseline.is_empty() {
                eprintln!(
                    "rigor: baseline prune: nothing to analyze — pass a path \
                     (e.g. `rigor baseline prune .`) or declare `paths:` in .rigor.yml"
                );
                return ExitCode::from(64);
            }
            f
        }
        Err(code) => return code,
    };
    let entries = baseline_entries(&findings);
    let rows = baseline.audit(&entries);

    let mut cleared: Vec<&baseline::DriftRow> =
        rows.iter().filter(|r| r.status == DriftStatus::Cleared).collect();
    if cleared.is_empty() {
        println!("No cleared buckets to prune.");
        return ExitCode::SUCCESS;
    }
    cleared.sort_by(|a, b| (&a.bucket.file, &a.bucket.rule).cmp(&(&b.bucket.file, &b.bucket.rule)));

    println!("{} bucket(s) to prune from {path}:", cleared.len());
    for row in &cleared {
        println!("  - {}  [{}]  (was: {})", row.bucket.file, row.bucket.rule, row.bucket.count);
    }

    if dry_run {
        return ExitCode::SUCCESS;
    }

    let remove: Vec<&Bucket> = cleared.iter().map(|r| r.bucket).collect();
    let n = remove.len();
    let pruned = baseline.without(&remove);
    if let Err(e) = std::fs::write(&path, pruned.to_yaml()) {
        eprintln!("rigor baseline prune: cannot write {path}: {e}");
        return ExitCode::from(1);
    }
    eprintln!("rigor: pruned {n} bucket(s); baseline now has {} entries.", pruned.size());
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

    // Keep only the surfaced indices IN THE FILTER'S ORDER — the reference
    // regroups output by (file, rule) bin under a non-empty baseline
    // (`Baseline#filter` returns the regrouped diagnostics themselves).
    let mut slots: Vec<Option<(usize, String, String, Diagnostic)>> =
        findings.into_iter().map(Some).collect();
    surfaced_idx
        .into_iter()
        .map(|i| slots[i].take().expect("baseline filter returns each index at most once"))
        .collect()
}

/// Normalize a path to project-root-relative (against cwd), matching the
/// reference's `Pathname#relative_path_from(Dir.pwd)` — including a path
/// OUTSIDE the root, which relativizes through `..` segments (`check
/// /abs/sibling/o.rb` from `/proj` records `../sibling/o.rb`). Both sides are
/// `cleanpath`ed first, so `./` and `a/../` spellings fold. The reference's
/// `ArgumentError` fallbacks — mixed absolute/relative, or a `..` left over in
/// the base — return the original spelling unchanged, as does an unknown cwd.
fn relative_path(path: &str, cwd: Option<&Path>) -> String {
    let Some(cwd) = cwd else { return path.to_string() };
    let (dest_abs, dest) = cleanpath_components(path);
    let (base_abs, base) = cleanpath_components(&cwd.to_string_lossy());
    if dest_abs != base_abs {
        return path.to_string();
    }
    let common = dest.iter().zip(&base).take_while(|(d, b)| d == b).count();
    let base_rest = &base[common..];
    if base_rest.iter().any(|c| c == "..") {
        return path.to_string();
    }
    let mut parts: Vec<&str> = vec![".."; base_rest.len()];
    parts.extend(dest[common..].iter().map(String::as_str));
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// `Pathname#cleanpath` on components: drop `.` and empty segments, fold `..`
/// into the previous component. A `..` that reaches the root of an ABSOLUTE
/// path is dropped (`/a/../../b` → `/b`); a relative path keeps its leading
/// `..`s (`a/../../b` → `../b`). Returns `(absolute?, components)`.
fn cleanpath_components(path: &str) -> (bool, Vec<String>) {
    let abs = path.starts_with('/');
    let mut out: Vec<String> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => match out.last() {
                Some(last) if last != ".." => {
                    out.pop();
                }
                None if abs => {}
                _ => out.push("..".to_string()),
            },
            s => out.push(s.to_string()),
        }
    }
    (abs, out)
}

// ---------------------------------------------------------------------------
// Output formatters
// ---------------------------------------------------------------------------

/// `--format text`: the reference's `write_text_result` — `Diagnostic#to_s`
/// rows plus the `No diagnostics` / `N error(s) in M file(s)` summary, all on
/// stdout (see [`diagnostic_formats::render_text`]).
fn print_text(findings: &[(usize, String, String, Diagnostic)]) {
    print!("{}", diagnostic_formats::render_text(&to_rendered(findings)));
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
    println!("{}", json_document(findings));
}

/// The `--format json` payload as a string. Split out of [`print_json`] so a
/// test can assert on the exact bytes rather than re-implementing the writer.
fn json_document(findings: &[(usize, String, String, Diagnostic)]) -> String {
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
        // A ruleless diagnostic (a parse error) serialises as JSON `null`, the
        // reference's `"rule" => rule` with `rule` nil — NOT `""`, which is a
        // different `(rule, line, column)` key for `harness/lib.rb`'s `DiagKey`
        // and would score the row as a gap AND an unregistered extra at once.
        match diag.qualified_rule() {
            Some(rule) => push_kv_str(&mut buf, "rule", json_rule_name(diag, rule), false),
            None => push_kv_raw(&mut buf, "rule", "null"),
        }
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
    buf
}

/// The reference's `Diagnostic#rule` is the SHORT rule id — for a
/// non-`builtin` family it is the qualified id minus its `source_family.`
/// prefix (`plugin_loader.load-error` → `load-error`), while `builtin` rows
/// carry no prefix and pass through (`configuration-error`,
/// `pre-eval.file-not-found`).
fn json_rule_name<'a>(diag: &Diagnostic, qualified: &'a str) -> &'a str {
    if diag.source_family == "builtin" {
        return qualified;
    }
    qualified
        .strip_prefix(diag.source_family)
        .and_then(|s| s.strip_prefix('.'))
        .unwrap_or(qualified)
}

/// Flatten findings into `Rendered` rows (resolve each byte offset to a 1-based
/// line/column) for the CI formatters, then print the rendered document. Mirrors
/// the reference's `write_result` for the `DiagnosticFormats` cases: an empty
/// render (github / teamcity with no diagnostics) prints nothing; otherwise the
/// document is printed with a trailing newline (`@out.puts(output) unless
/// output.empty?`).
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
                rule_id: diag.qualified_rule(),
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
                Some("github") => diagnostic_formats::render_github(&rows),
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

/// Append `,"key":<raw>` with `raw` spliced in as a JSON *literal*, not a
/// string. Used for the `null` a ruleless diagnostic's `rule` field carries.
fn push_kv_raw(buf: &mut String, key: &str, raw: &str) {
    buf.push(',');
    buf.push_str(&json_string(key));
    buf.push(':');
    buf.push_str(raw);
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
/// Columns are counted in BYTES within the line, which is the reference's unit:
/// it reports `Prism::Location#start_column + 1`, and Prism's `start_column` is a
/// byte index into the line (`reference/rigor/lib/rigor/source/node_locator.rb`
/// documents the same convention for the inverse mapping). Counting Unicode
/// scalars instead shifted every column right of a multi-byte character, which on
/// real corpora reported the same diagnostic at a different column than the
/// reference — a parity break on 8 of the 24 survey FP candidates.
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
    // Column = byte count between the line start and the offset, plus 1.
    (line, clamped - line_start + 1)
}

/// One diagnostic per raw Prism parse ERROR, in Prism's own order.
///
/// Ports the reference's `Runner#parse_diagnostics`:
///
/// ```ruby
/// parse_result.errors.map do |error|
///   location = error.location
///   Diagnostic.new(path: path, line: location.start_line,
///                  column: location.start_column + 1,
///                  message: error.message, severity: :error)
/// end
/// ```
///
/// So: **1:1 with `errors`, no filtering and no dedupe** — two errors on one
/// line stay two rows; `severity: :error`; `rule` defaulted, i.e. `nil`, here
/// [`rigor_rules::NO_RULE`]; the message verbatim from Prism. `warnings` are
/// NOT reported (the reference never reads them here — measured, not assumed).
/// The location travels as the byte offsets it already is: `line_col` resolves
/// them to Prism's `start_line` / `start_column + 1`, because it counts BYTES
/// from the line start and adds one, which is exactly Prism's column.
fn parse_diagnostics(result: &rigor_parse::ruby_prism::ParseResult<'_>) -> Vec<Diagnostic> {
    result
        .errors()
        .map(|error| {
            let location = error.location();
            Diagnostic {
                rule_id: rigor_rules::NO_RULE,
                start_offset: location.start_offset(),
                end_offset: location.end_offset(),
                message: error.message().to_string(),
                severity: Severity::Error,
                source_family: "builtin",
                receiver_type: None,
                method_name: None,
            }
        })
        .collect()
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

// ---------------------------------------------------------------------------
// Tests for the additive CI output formats (github / sarif).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests;
