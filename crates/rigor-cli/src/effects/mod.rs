//! `rigor effects [--full] [--format text|json] [PATHS…]` — the effect-summary
//! REPORT (ADR-0043 slice 2).
//!
//! # Why the report ships in slice 2 and not slice 5
//!
//! ADR-0043's table gives `rigor effects` to slice 5, but slice 2's gate is "0
//! OVER on the fixture set" and the only instrument that computes it —
//! `harness/effects_diff.py` — invokes `rigor effects --full --format=json`.
//! With no such command the differential reports NOT-IMPLEMENTED, every oracle
//! method counts UNDER, and `OVER` is **0 by construction**: a slice whose gate
//! cannot fail has not been gated. So the report surface comes forward and the
//! snapshot family (`update` / `check` / `diff` / `explain`), the judgment half
//! of `--no-tolerated-effects` and the polished text renderer stay in slice 5.
//!
//! # What this reports, and how honest it is about the rest
//!
//! The JSON's `effects` key is upstream's TRANSITIVE lane; this slice computes
//! the DIRECT one and prints it there, which is a subset (transitive is direct
//! joined with every project method reached) and therefore an UNDER.
//!
//! `exhaustive` and `causes` are slice 3's ([`collect`]): the bit is the
//! TRANSITIVE reading, tainted by every producer the collector cannot rule out
//! and by every call that could carry taint in along a project edge. `causes`
//! is `[[cause, detail], …]` with `cause` drawn from upstream's closed
//! `TaintCause::ALL` enum — the out-of-enum `port-incomplete` marker slice 2
//! shipped is RETIRED, because it broke the port's own
//! `causes.empty? == exhaustive` invariant.
//!
//! Two self-defenses sit beside them, both of them under-claims by construction:
//!
//! - **`"declared": []` on every method.** The declared lane is copied from the
//!   author's annotation, and it is the one lane graded as an EXACT match — so
//!   a port that cannot compute it must not report a method that HAS one. The
//!   self-defense is lexical and total: a project carrying an effect annotation
//!   (or an `effects.attribution:` table) reports `methods: {}`, which is
//!   always an under-claim. [`carries_effect_annotations`].
//! - **No `"exhaustive": true` from a plugin-bearing project.** A plugin's rows
//!   move the bit the UNSAFE way: a non-discharging row taints
//!   (`unit_scan.rb:262`), and plugins additionally synthesise framework units
//!   (`scanner.rb:163`) that widen the selector set the edge taint reads. The
//!   port has no plugin effect stratum at all, so a project whose `.rigor.yml`
//!   configures `plugins:` gets every bit withheld. [`configures_plugins`].
//!   Deliberately NARROWER than the annotation self-defense — the rows are still
//!   reported, and only the one lane the plugin could move is withheld.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod collect;
mod narrowing;
mod ownership;

use collect::Summary;

const USAGE: &str = "Usage: rigor effects [options] [paths]";

/// Upstream's `SignatureSources::ANNOTATION_HINT`
/// (`lib/rigor/effects/signature_sources.rb:27`), spelled as a scan rather than
/// a regex: `%a{` then optional space then either `pure` + optional space + `}`
/// or `rigor:v1:effect` at a word boundary. It is a ROUTING test upstream and
/// the whole test here — slice 2 never parses an envelope, it only refuses to
/// report a project that has one.
fn annotation_hint(line: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find("%a{") {
        let tail = rest[at + 3..].trim_start_matches([' ', '\t']);
        if let Some(after) = tail.strip_prefix("pure") {
            if after.trim_start_matches([' ', '\t']).starts_with('}') {
                return true;
            }
        }
        if let Some(after) = tail.strip_prefix("rigor:v1:effect") {
            // `\b` — the directive must not run into a longer word.
            if !after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                return true;
            }
        }
        rest = &rest[at + 3..];
    }
    false
}

/// `rigor effects` — the report. Exit 0 always; 64 on a usage error.
pub fn cmd_effects(args: &[String]) -> ExitCode {
    let mut format = "text";
    let mut full = false;
    let mut explicit_config: Option<&str> = None;
    let mut positional: Vec<&str> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "help" | "--help" | "-h" => {
                println!("{}", help());
                return ExitCode::SUCCESS;
            }
            // The snapshot family. Named explicitly so the message says which
            // slice owns them — and deliberately NOT phrased as an unknown
            // command, which `harness/effects_diff.py` reads as
            // "the port has no `effects` subcommand at all".
            verb @ ("update" | "check" | "diff" | "explain") => {
                eprintln!(
                    "rigor-rs: `effects {verb}` (the committed effect snapshot) is not yet \
                     implemented — ADR-0043 slice 5"
                );
                return ExitCode::from(2);
            }
            "--full" => full = true,
            // Accepted and deliberately inert, exactly as upstream accepts it on
            // the report: an observation is undischarged, and only a JUDGMENT
            // reads `effects.tolerated:`.
            "--no-tolerated-effects" => {}
            "--format" => match it.next().map(String::as_str) {
                Some(f @ ("text" | "json")) => format = f,
                other => {
                    eprintln!("effects: --format expects `text` or `json`, got {other:?}");
                    eprintln!("{USAGE}");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--format=") => {
                match other.trim_start_matches("--format=") {
                    f @ ("text" | "json") => format = f,
                    other => {
                        eprintln!("effects: unsupported format: {other}");
                        eprintln!("{USAGE}");
                        return ExitCode::from(64);
                    }
                }
            }
            "--config" => match it.next() {
                Some(path) => explicit_config = Some(path),
                None => {
                    eprintln!("effects: --config expects a path");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--") => {
                eprintln!("effects: unknown option `{other}`");
                eprintln!("{USAGE}");
                return ExitCode::from(64);
            }
            other => positional.push(other),
        }
    }

    let config_path = explicit_config.map_or_else(|| PathBuf::from(".rigor.yml"), PathBuf::from);
    let cfg = crate::Config::load(explicit_config.map(Path::new));
    // Paths: positional args, or config `paths:` when none are supplied —
    // upstream's `@argv.empty? ? configuration.paths : @argv`.
    let config_paths: Vec<&str>;
    let raw: &[&str] = if positional.is_empty() {
        config_paths = cfg.paths.iter().map(String::as_str).collect();
        &config_paths
    } else {
        &positional
    };
    let files = resolve_paths(raw, &cfg);

    let project_root =
        std::env::current_dir().and_then(|dir| dir.canonicalize()).unwrap_or_else(|_| ".".into());
    let rows = if carries_effect_annotations(&cfg, &config_path, &files, &project_root) {
        Vec::new()
    } else {
        report_rows(&files, full, configures_plugins(&cfg))
    };

    if format == "json" {
        println!("{}", render_json(&rows));
    } else {
        render_text(&rows);
    }
    ExitCode::SUCCESS
}

fn help() -> String {
    format!(
        "{USAGE}\n\n\
         With no subcommand, prints one line per method: its proven effect labels and whether\n\
         that list is exhaustive.\n\n\
         Options:\n    \
         --full                     List every method, including exhaustive ones with no\n                               \
         effects beyond mutate.local\n    \
         --format=FORMAT            Output format: text (default) or json\n    \
         --config=PATH              Read PATH instead of ./.rigor.yml\n    \
         --no-tolerated-effects     Accepted and inert on the report\n\n\
         The committed effect snapshot (`effects update` / `check` / `diff` / `explain`) is\n\
         ADR-0043 slice 5 and not implemented yet."
    )
}

/// Resolve path args to the `.rb` files to scan: a directory expands to its
/// sorted `**/*.rb`, a `.rb` file passes through, anything else is skipped, and
/// the config's `exclude:` prunes — the same file set `check` analyses, which is
/// what makes the two arms of the differential describe one project.
fn resolve_paths(raw: &[&str], cfg: &crate::Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &path in raw {
        let candidate = Path::new(path);
        if candidate.is_dir() {
            let mut in_dir = Vec::new();
            crate::collect_rb_files(candidate, &mut in_dir);
            in_dir.sort();
            out.extend(in_dir);
        } else if candidate.is_file() && path.ends_with(".rb") {
            out.push(path.to_string());
        }
    }
    out.dedup();
    out.retain(|path| !cfg.is_excluded(path));
    out
}

/// Whether this project carries an effect ANNOTATION or an
/// `effects.attribution:` table — in which case the report is `methods: {}`.
///
/// The declared lane is graded as an exact match (ADR-0043 § 2), a missing lane
/// reads as ∅, and slice 2 does not implement the lane at all; so the only safe
/// answer for a project that HAS one is to report nothing. An empty map is
/// always an under-claim, and this makes "do not point the differential at an
/// annotated project" a property of the binary rather than of the gate command.
///
/// The test is LEXICAL — upstream's own routing regex over the project's `.rbs`
/// and `.rb` surfaces — and never parses an envelope. Slice 6 replaces it with
/// the caller-lane join.
fn carries_effect_annotations(
    cfg: &crate::Config,
    config_path: &Path,
    files: &[String],
    project_root: &Path,
) -> bool {
    if config_declares_attribution(config_path) {
        return true;
    }
    let mut sources: Vec<PathBuf> = Vec::new();
    for dir in cfg.all_signature_dirs(project_root) {
        let mut found = Vec::new();
        collect_rbs_files(&dir, &mut found);
        sources.extend(found);
    }
    // rbs-inline writes its envelopes as `# @rbs %a{…}` comments in the Ruby
    // file, so the source surface counts too.
    sources.extend(files.iter().map(PathBuf::from));
    sources.iter().any(|path| {
        std::fs::read_to_string(path)
            .is_ok_and(|source| source.lines().any(annotation_hint))
    })
}

/// Whether the config carries an `effects: { attribution: … }` table — a
/// declared-lane producer (`unit_scan.rb:360`), and slice 6's. An absent config
/// declares nothing; a config that does not PARSE refuses to report at all,
/// because "I could not read it" is not "it has no table".
fn config_declares_attribution(config_path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(config_path) else { return false };
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&text) else { return true };
    value
        .get("effects")
        .and_then(|effects| effects.get("attribution"))
        .is_some_and(|table| !table.is_null())
}

fn collect_rbs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            collect_rbs_files(&entry.path(), out);
        } else if name.ends_with(".rbs") {
            out.push(entry.path());
        }
    }
}

/// Whether the project's config activates any plugin — the trigger for the
/// exhaustiveness self-defense above.
///
/// Keyed on the `plugins:` list ALONE, and deliberately not on
/// [`crate::Config::effective_plugins`]: the reference discovers effect-bearing
/// plugins only from that list. Its one auto-wire (ADR-93,
/// `configuration.rb:308`) is `rigor-rbs-inline`, which ships no
/// `effect_attributions:` and so contributes neither a row nor a framework unit
/// — verified against the pinned checkout. A bundler-detected RBS overlay is the
/// same shape. Were either to grow an effects stratum this predicate would have
/// to widen with it.
fn configures_plugins(cfg: &crate::Config) -> bool {
    !cfg.plugins.is_empty()
}

/// One method's row of the report.
struct Row {
    key: String,
    effects: Vec<String>,
    exhaustive: bool,
    causes: Vec<collect::Cause>,
    direct: std::collections::BTreeMap<String, BTreeSet<String>>,
}

impl Row {
    /// Upstream's `trivial?` — exhaustive, proving nothing beyond frame-local
    /// mutation, and claiming nothing the proven lane does not already admit
    /// (`effect_table.rb:50`). The declared half is vacuous while the port's
    /// declared lane is always ∅. Reachable from slice 3 on, so the DEFAULT
    /// report now omits rows; the differential always passes `--full`.
    fn trivial(&self) -> bool {
        self.exhaustive && self.effects.iter().all(|label| label == "mutate.local")
    }
}

fn report_rows(files: &[String], full: bool, plugins: bool) -> Vec<Row> {
    let mut merged: std::collections::BTreeMap<String, Summary> = std::collections::BTreeMap::new();
    for path in files {
        for (key, summary) in scan_file(path) {
            merged.entry(key).or_default().join(summary);
        }
    }
    // The run's own selector set, which the collector's edge taints resolve
    // against — the port's stand-in for the propagator's edge resolution. It is
    // knowable only once every file has been scanned, which is why the collector
    // records candidate selectors rather than deciding.
    let selectors: BTreeSet<String> =
        merged.keys().map(|key| collect::selector_of(key).to_string()).collect();
    merged
        .into_iter()
        .map(|(key, summary)| {
            let exhaustive = !plugins && summary.exhaustive(&selectors);
            let mut causes = summary.causes(&selectors);
            // The plugin self-defense keeps `causes.empty? == exhaustive` true
            // by naming its own reason, rather than emitting a bare `false` with
            // nothing behind it. `plugin-attribution` is upstream's spelling for
            // "a stratum claimed this and the analyzer did not read the body"
            // (`unit_scan.rb:262`), which is exactly what is being withheld; the
            // detail is the plugin row key upstream and unknown here.
            if plugins && causes.is_empty() {
                causes.push(("plugin-attribution".to_string(), None));
            }
            Row {
                key,
                effects: summary.proven(),
                exhaustive,
                causes,
                direct: summary.bundles().clone(),
            }
        })
        .filter(|row| full || !row.trivial())
        .collect()
}

/// One file's units, or nothing at all. Fail-soft per file, matching upstream's
/// fail-soft per unit: a file that does not read, does not parse, or is an ERB
/// template contributes no units — which can only cost UNDER.
fn scan_file(path: &str) -> std::collections::BTreeMap<String, Summary> {
    let empty = std::collections::BTreeMap::new();
    let Ok(source) = std::fs::read(path) else { return empty };
    if rigor_parse::looks_like_erb_template(&source) {
        return empty;
    }
    let result = rigor_parse::parse(&source);
    if result.errors().next().is_some() {
        return empty;
    }
    collect::scan(&result.node())
}

fn render_json(rows: &[Row]) -> String {
    let mut methods = serde_json::Map::new();
    for row in rows {
        let direct: serde_json::Map<String, serde_json::Value> = row
            .direct
            .iter()
            .map(|(origin, labels)| {
                (origin.clone(), serde_json::Value::from(labels.iter().cloned().collect::<Vec<_>>()))
            })
            .collect();
        // `[[cause, detail], …]` — upstream's spelling exactly: a JSON array of
        // two-element arrays, `detail` a string or `null`, the pairs
        // de-duplicated and sorted by `[cause, detail]` (`summary.rb:143`).
        let causes: Vec<serde_json::Value> = row
            .causes
            .iter()
            .map(|(cause, detail)| {
                serde_json::json!([cause, detail.as_ref().map(String::as_str)])
            })
            .collect();
        methods.insert(
            row.key.clone(),
            serde_json::json!({
                "effects": row.effects,
                "declared": Vec::<String>::new(),
                "exhaustive": row.exhaustive,
                "causes": causes,
                "direct": direct,
            }),
        );
    }
    serde_json::to_string_pretty(&serde_json::json!({ "methods": methods }))
        .unwrap_or_else(|_| "{\"methods\":{}}".to_string())
}

fn render_text(rows: &[Row]) {
    for row in rows {
        let suffix = if row.exhaustive { "" } else { " …?" };
        println!("{}: [{}]{suffix}", row.key, row.effects.join(", "));
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use rigor_parse::ruby_prism::{CallNode, Node, Visit};

    /// The OUTERMOST call in a parsed fixture — pre-order, so `open("|#{cmd}")`
    /// answers the `open` and not the interpolated `cmd` vcall.
    pub(crate) fn first_call<'pr>(node: &Node<'pr>) -> Option<CallNode<'pr>> {
        struct FirstCall<'pr>(Option<CallNode<'pr>>);
        impl<'pr> Visit<'pr> for FirstCall<'pr> {
            fn visit_call_node(&mut self, node: &CallNode<'pr>) {
                if self.0.is_none() {
                    self.0 = node.as_node().as_call_node();
                }
            }
        }
        let mut found = FirstCall(None);
        found.visit(node);
        found.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_annotation_hint_matches_the_two_spellings_upstream_honours() {
        assert!(annotation_hint("  %a{pure}"));
        assert!(annotation_hint("%a{ pure }"));
        assert!(annotation_hint("  %a{rigor:v1:effect io.db}"));
        assert!(annotation_hint("%a{rigor:v1:effect}"));
        assert!(annotation_hint("  # @rbs %a{rigor:v1:effect io.db, nondet.time}"));
        // …and nothing else. A near-miss must NOT suppress the whole report.
        assert!(!annotation_hint("%a{purely}"));
        assert!(!annotation_hint("%a{rigor:v1:effects io.db}"));
        assert!(!annotation_hint("%a{assert Foo}"));
        assert!(!annotation_hint("def pure"));
        assert!(!annotation_hint("# talks about %a{ and effects"));
    }

    fn row(key: &str, effects: &[&str], exhaustive: bool, causes: &[(&str, Option<&str>)]) -> Row {
        Row {
            key: key.to_string(),
            effects: effects.iter().map(|l| (*l).to_string()).collect(),
            exhaustive,
            causes: causes
                .iter()
                .map(|(cause, detail)| ((*cause).to_string(), detail.map(str::to_string)))
                .collect(),
            direct: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn trivial_reads_the_taint_bit_and_is_reachable_from_slice_3() {
        assert!(!row("C#m", &["mutate.local"], false, &[("dynamic-receiver", None)]).trivial());
        assert!(row("C#m", &["mutate.local"], true, &[]).trivial());
        assert!(!row("C#m", &["io.fs.read"], true, &[]).trivial(), "a real label is never trivial");
    }

    #[test]
    fn the_json_is_the_shape_the_grader_consumes() {
        let rows = vec![Row {
            key: "C#m".to_string(),
            effects: vec!["io.fs.read".to_string()],
            exhaustive: false,
            causes: vec![("unresolved-self-call".to_string(), Some("helper".to_string()))],
            direct: [("catalogue:File.read".to_string(), ["io.fs.read".to_string()].into())]
                .into_iter()
                .collect(),
        }];
        let payload: serde_json::Value =
            serde_json::from_str(&render_json(&rows)).expect("valid JSON");
        let entry = &payload["methods"]["C#m"];
        assert_eq!(entry["effects"], serde_json::json!(["io.fs.read"]));
        assert_eq!(entry["declared"], serde_json::json!([]));
        assert_eq!(entry["exhaustive"], serde_json::json!(false));
        // Upstream's `causes` spelling exactly, `port-incomplete` retired.
        assert_eq!(entry["causes"], serde_json::json!([["unresolved-self-call", "helper"]]));
        assert_eq!(entry["direct"]["catalogue:File.read"], serde_json::json!(["io.fs.read"]));

        // A cause with no detail renders `null`, not the string "null".
        let bare = vec![row("C#n", &[], false, &[("dynamic-send", None)])];
        let payload: serde_json::Value =
            serde_json::from_str(&render_json(&bare)).expect("valid JSON");
        assert_eq!(payload["methods"]["C#n"]["causes"], serde_json::json!([["dynamic-send", null]]));

        // An exhaustive row carries NO causes — the port's own
        // `causes.empty? == exhaustive` invariant, which slice 2's out-of-enum
        // marker broke.
        let clean = vec![row("C#o", &[], true, &[])];
        let payload: serde_json::Value =
            serde_json::from_str(&render_json(&clean)).expect("valid JSON");
        assert_eq!(payload["methods"]["C#o"]["exhaustive"], serde_json::json!(true));
        assert_eq!(payload["methods"]["C#o"]["causes"], serde_json::json!([]));

        // An empty report is still a parseable object — the grader reads
        // `None` as INVALID, not as "0 methods".
        let empty: serde_json::Value =
            serde_json::from_str(&render_json(&[])).expect("valid JSON");
        assert_eq!(empty["methods"], serde_json::json!({}));
    }

    // -----------------------------------------------------------------------
    // The declared-lane self-defense, BOTH directions.
    //
    // A suppression without a must-still-fire control is how this repo got
    // burned three times (`docs/notes/…subset-arguments…`): a predicate that
    // silently answered "annotated" for every project would make the effects
    // gate green by construction and look exactly like a passing slice. So the
    // control is not "the negative case returns false" — it is "the negative
    // case returns false AND that project still produces real methods".
    // -----------------------------------------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("rigor_effects_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sig")).expect("mkdir sig");
        std::fs::create_dir_all(root.join("lib")).expect("mkdir lib");
        std::fs::write(root.join("lib/x.rb"), b"class X\n  def m\n    puts 1\n  end\nend\n")
            .expect("write rb");
        root
    }

    /// A `Config` whose signature dir is this scratch project's, so the scan
    /// does not depend on the test process's cwd.
    fn scratch_config(root: &Path) -> crate::Config {
        let mut cfg = crate::Config::default();
        cfg.signature_paths = vec![root.join("sig").to_string_lossy().into_owned()];
        cfg
    }

    #[test]
    fn an_annotation_free_project_reports_real_methods() {
        let root = scratch("plain");
        std::fs::write(root.join("sig/x.rbs"), b"class X\n  def m: () -> void\nend\n").unwrap();
        let cfg = scratch_config(&root);
        let files = vec![root.join("lib/x.rb").to_string_lossy().into_owned()];

        assert!(
            !carries_effect_annotations(&cfg, &root.join(".rigor.yml"), &files, &root),
            "an unannotated project must NOT be suppressed"
        );
        // THE CONTROL: the same file set, through the same report path, must
        // produce a method. Without this the negative case above proves nothing.
        let rows = report_rows(&files, true, false);
        assert_eq!(rows.len(), 1, "the control project must report its method");
        assert_eq!(rows[0].key, "X#m");
        assert_eq!(rows[0].effects, ["io.output.stdout"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // The PLUGIN self-defense, both directions.
    //
    // Same shape as the annotation one and for the same reason: a predicate
    // that answered "plugins" for every project would make the exhaustiveness
    // lane green by construction — 0 OVER with the bit never claimed is exactly
    // what slice 2 shipped — and would look like a passing slice. So the
    // negative case is not "the predicate returns false", it is "the predicate
    // returns false AND that project still REACHES exhaustiveness".
    // -----------------------------------------------------------------------

    #[test]
    fn a_plugin_free_project_still_reaches_exhaustiveness() {
        let root = scratch("plainplugins");
        let cfg = scratch_config(&root);
        assert!(!configures_plugins(&cfg), "a config with no `plugins:` must not suppress");

        // THE MUST-STILL-FIRE CONTROL. `puts` is a `Kernel` row and no project
        // unit is called `puts`, so nothing taints and nothing keeps an edge:
        // this method MUST come out exhaustive. If it does not, the taint bit is
        // unreachable and every 0-OVER verdict below is vacuous.
        let files = vec![root.join("lib/x.rb").to_string_lossy().into_owned()];
        let rows = report_rows(&files, true, false);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].exhaustive, "a plugin-free project must still reach exhaustiveness");
        assert!(rows[0].causes.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_plugin_bearing_project_never_claims_exhaustiveness() {
        let root = scratch("plugins");
        let mut cfg = scratch_config(&root);
        cfg.plugins = vec!["rigor-actionpack".to_string()];
        assert!(configures_plugins(&cfg));

        // The SAME file that is exhaustive above must now be withheld — and the
        // rows are still reported, which is what makes this narrower than the
        // annotation self-defense's `methods: {}`.
        let files = vec![root.join("lib/x.rb").to_string_lossy().into_owned()];
        let rows = report_rows(&files, true, true);
        assert_eq!(rows.len(), 1, "the rows themselves are NOT withheld");
        assert_eq!(rows[0].effects, ["io.output.stdout"]);
        assert!(!rows[0].exhaustive);
        // …and the bit is not bare: the invariant `causes.empty? == exhaustive`
        // holds on the port side.
        assert_eq!(rows[0].causes, [("plugin-attribution".to_string(), None)]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_annotated_signature_suppresses_the_whole_report() {
        let root = scratch("annotated");
        std::fs::write(
            root.join("sig/x.rbs"),
            b"class X\n  %a{rigor:v1:effect io.db}\n  def m: () -> void\nend\n",
        )
        .unwrap();
        let cfg = scratch_config(&root);
        let files = vec![root.join("lib/x.rb").to_string_lossy().into_owned()];

        assert!(
            carries_effect_annotations(&cfg, &root.join(".rigor.yml"), &files, &root),
            "an envelope in the project's own sig/ must suppress the report"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_rbs_inline_annotation_in_the_ruby_source_suppresses_too() {
        // rbs-inline writes envelopes as `# @rbs %a{…}` comments in the .rb
        // file, so the SOURCE surface counts and not only `sig/`.
        let root = scratch("inline");
        std::fs::write(
            root.join("lib/x.rb"),
            b"class X\n  # @rbs %a{pure}\n  def m\n    1\n  end\nend\n",
        )
        .unwrap();
        let cfg = scratch_config(&root);
        let files = vec![root.join("lib/x.rb").to_string_lossy().into_owned()];

        assert!(carries_effect_annotations(&cfg, &root.join(".rigor.yml"), &files, &root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_attribution_table_in_the_config_suppresses_and_a_plain_config_does_not() {
        let root = scratch("config");
        let cfg = scratch_config(&root);
        let files = vec![root.join("lib/x.rb").to_string_lossy().into_owned()];
        let config_path = root.join(".rigor.yml");

        std::fs::write(&config_path, b"paths:\n  - lib\n").unwrap();
        assert!(!carries_effect_annotations(&cfg, &config_path, &files, &root));

        std::fs::write(
            &config_path,
            b"paths:\n  - lib\neffects:\n  attribution:\n    \"Gem::Client#call\": [io.net]\n",
        )
        .unwrap();
        assert!(carries_effect_annotations(&cfg, &config_path, &files, &root));

        // An `effects:` block with no `attribution:` is not a declared-lane
        // producer and must not suppress.
        std::fs::write(&config_path, b"paths:\n  - lib\neffects:\n  tolerated: [io.fs]\n").unwrap();
        assert!(!carries_effect_annotations(&cfg, &config_path, &files, &root));

        // …and a config neither loader can read refuses to report at all:
        // "I could not read it" is not "it has no table".
        std::fs::write(&config_path, b"paths:\n  - lib\n  bad indent: [\n").unwrap();
        assert!(carries_effect_annotations(&cfg, &config_path, &files, &root));

        let _ = std::fs::remove_dir_all(&root);
    }
}
