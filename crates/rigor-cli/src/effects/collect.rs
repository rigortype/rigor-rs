//! The DIRECT effect collector — a port of upstream's
//! `lib/rigor/effects/scanner.rb` (unit identity) and `unit_scan.rb` (one
//! method body) restricted to what SYNTAX settles (ADR-0043 slice 2).
//!
//! # Why this walks Prism and not the lowered AST
//!
//! The construct origins do not survive lowering: `@@x = v` and `$x = v` both
//! become a nameless `Node::VariableWrite` and carry OPPOSITE labels
//! (`mutate.static` vs `global.write`); all three variable READS collapse into
//! one `VariableRead`, which makes `FRAME_LOCAL_GLOBALS` impossible; and
//! backticks, `alias`, `undef` and `@x ||=` fall to `Node::Other`. Widening the
//! lowered node set to fix that would change what every structural rule walk
//! sees — a `rigor check` movement risk on a slice whose whole promise is zero
//! movement. Upstream walks Prism for the same reason, so the port is a port.
//!
//! # No typer — the load-bearing scoping
//!
//! Upstream's `catalog_target` (`unit_scan.rb:553`) has three arms: implicit
//! self, a constant-path receiver, and **the class the typer projected the
//! receiver to**. This collector implements the first two and declines the
//! third, so its handled-target set is a strict SUBSET of upstream's — every
//! answer it gives is upstream's own answer off the same syntax and the same
//! vendored bytes, and the excluded arm can only cost UNDER. That is what makes
//! ADR-0043 § 1 ("collection is observational") a dependency fact here: nothing
//! below reads inference state, and nothing in `rigor-infer` is asked anything.
//!
//! # No POSTURE tier either — and the typer-free argument does NOT save it
//!
//! `Catalog#lookup`'s third tier answers the class's POSTURE for a selector it
//! does not row. Slice 2 kept that tier for constant-path receivers on the
//! argument that the constant is spelled by the same syntax in both engines.
//! **That argument is false and shipped a live over-claim** (issue #106): the
//! posture is not gated on how the receiver is SPELLED, it is gated on how the
//! receiver was TYPED — `posture_allowed?` is `!implicit && !record&.dynamic &&
//! …` (`unit_scan.rb:429`) — and the reference cannot resolve every constant its
//! own catalogue names. Eight of the eighty listed classes type as `Dynamic`
//! there (`Net::HTTP`, `Net::SMTP`, `Net::FTP`, `OpenSSL::SSL::SSLSocket`,
//! `Fiddle::Handle`, `Fiddle::Function`, `PTY`, `SOCKSSocket` at the pin), so
//! upstream refuses the posture and proves nothing, while a typer-free port with
//! the tier on proves the class default — seven measured OVER rows on
//! `harness/effects-corpus/05_posture`.
//!
//! So the tier is OFF for every receiver, which is what upstream itself does
//! whenever the receiver is `Dynamic`. Two roads were rejected: an allow-list of
//! the constants the reference resolves (a function of ITS RBS environment,
//! which moves with the pin and with the machine's installed gems — the
//! `UNBUILDABLE_DEFINITIONS` hazard), and asking rigor-rs's own typer (the port
//! is deliberately MORE robust than the reference on shapes it degrades to
//! `untyped` — `sig_gen.rs:20-23` — and `untyped == Dynamic`, so its answer
//! moves in exactly the unsafe direction).
//!
//! Nothing else is lost: `Catalog#lookup` answers a ROW first and the 34-name
//! `universal:` list second, both BEFORE it consults `posture:`
//! (`catalog.rb:186`), so a row stays authoritative even on a class the oracle
//! cannot resolve. Measured cost: 0 on the graded corpus, 4 methods of 6,948 on
//! mastodon/app — all UNDER (`docs/notes/20260826-effects-s3-probe.md` § 3c).
//!
//! # The TAINT bit (slice 3) — exhaustive iff no producer can fire
//!
//! `exhaustive` in the graded JSON is upstream's **transitive** bit, not the
//! direct one: `causes.empty?` per unit (`unit_scan.rb:138`), joined across
//! reopenings (`summary.rb:89`), then ANDed along every resolved project edge to
//! a fixpoint (`propagator.rb:128`). Emitting the direct bit instead scores 10
//! OVER on the graded corpus and 986 on mastodon/app
//! (`docs/notes/20260826-effects-s3-probe.md` § 3a), so this collector computes
//! the transitive reading under one rule:
//!
//! > **A method is exhaustive iff the collector can see that no producer fires,
//! > counting every undecidable site as firing.**
//!
//! Three of upstream's producers are pure syntax and are ported EXACTLY —
//! `dynamic-send` (a reflective send whose selector is not a literal),
//! `opaque-callable`'s eval / `binding` / `&expr` arms, and `unknown-ownership`
//! wherever the ownership judgment is syntax-only. Three are the typer's and
//! cannot be decided here, so every site that reaches them taints:
//! `opaque-callable`'s `.call` arm (upstream's `record.nil?` is always true
//! without a typer), `dynamic-receiver` at every uncatalogued call with an
//! explicit receiver, and `unresolved-self-call` at every uncatalogued
//! receiver-less one.
//!
//! ## The transitive AND, priced as a selector-set test
//!
//! [`UnitScan::push_edge`] is the single funnel every project edge goes through
//! upstream — the claimed path when `keeps_project_edge?` says so
//! (`unit_scan.rb:409`), a reflective `send` with a literal selector
//! (`:476`), and the uncatalogued path (`:514`). A **catalogue-CLAIMED call
//! still keeps an edge** for implicit self, which is why a body the catalogue
//! answers completely can still be transitively tainted (`Kernel#format` beside
//! a project's own `format`; `harness/effects-corpus/06_edge`).
//!
//! So the port taints at `push_edge` whenever the selector names ANY unit the
//! run collected. `Propagator::Index#targets_for` can only resolve to keys the
//! collection holds (`propagator.rb:195`), so {real targets} ⊆ {units whose
//! selector matches} and ignoring `kind` and the ancestry scope makes this a
//! superset — sound, and measured at zero cost on the graded corpus. Slice 4
//! replaces it with the real closure, and the taint it manufactures goes away.
//!
//! The set is only known once every file is scanned, so a unit records its
//! candidate selectors and [`Summary::exhaustive`] resolves them at report time.
//!
//! # What this still does NOT collect
//!
//! The transitive LABEL lane and the `resolved` bit (slice 4), the declared lane
//! — attribution table, imported envelopes (slice 6) — and the plugin stratum
//! (out of ADR-0043 entirely). Envelopes and plugin rows both make upstream MORE
//! exhaustive at a site; ignoring them can only taint more, which is the safe
//! direction. A plugin's own `taint:` moves the other way, and the report
//! withholds `exhaustive: true` from a plugin-bearing project for that reason
//! (`mod.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use rigor_effects::catalog::Resolution;
use rigor_parse::ruby_prism::{self, CallNode, DefNode, Node, ParametersNode, Visit};

use super::narrowing;
use super::ownership::{name_of, owned_locals, MutationClassifier};

/// Mirrors `Inference::ScopeIndexer::TOP_LEVEL_DEF_KEY` — a `def` outside any
/// class body (`scanner.rb:43`).
const TOP_LEVEL_KEY: &str = "<toplevel>";

/// Receiver-less calls in a class / module body that declare units or ancestry
/// (`scanner.rb:48`). Everything else in a class body is out of scope in v1 —
/// its statements run at load time, which is a unit of its own that no slice
/// models yet.
const DECLARATION_MACROS: &[&str] =
    &["include", "prepend", "attr_reader", "attr_writer", "attr_accessor", "define_method"];

/// `$~` and friends are frame-local, not global state: a read of one is not
/// `global.read` (`unit_scan.rb:40`).
const FRAME_LOCAL_GLOBALS: &[&str] = &["$~", "$_", "$&", "$`", "$'", "$+", "$!"];

const REFLECTIVE_SEND: &[&str] = &["send", "public_send", "__send__"];

const EVAL_SELECTORS: &[&str] = &["eval", "instance_eval", "class_eval", "module_eval"];

// The construct origins, spelled once (`unit_scan.rb:60-71`). A construct origin
// is line-free and carries no per-site state.
const DEFINE_METHOD: &str = "construct:define-method";
const XSTRING: &str = "construct:xstring";
const GVAR_READ: &str = "construct:gvar-read";
const GVAR_WRITE: &str = "construct:gvar-write";
const CVAR_READ: &str = "construct:cvar-read";
const CVAR_WRITE: &str = "construct:cvar-write";
const IVAR_WRITE: &str = "construct:ivar-write";
const ALIAS: &str = "construct:alias";
const UNDEF: &str = "construct:undef";
const RECEIVER_MUTATION: &str = "construct:receiver-mutation";
const ATTR_WRITER: &str = "construct:attr-writer";

// The taint causes this collector can produce — four of upstream's closed
// ten-member `TaintCause::ALL` enum (`taint_cause.rb:16`). The other six have no
// producer here: `method-missing` and `budget` have none at the pin at all,
// `template-not-analysed` and `plugin-attribution` are the plugin stratum's,
// and `collector-error` is upstream's per-unit rescue where the port is
// per-FILE fail-soft (it omits the file's units instead, which is an UNDER).
pub(super) const DYNAMIC_RECEIVER: &str = "dynamic-receiver";
pub(super) const DYNAMIC_SEND: &str = "dynamic-send";
pub(super) const UNRESOLVED_SELF_CALL: &str = "unresolved-self-call";
pub(super) const OPAQUE_CALLABLE: &str = "opaque-callable";
pub(super) const UNKNOWN_OWNERSHIP: &str = "unknown-ownership";

/// One `[cause, detail]` pair. `detail` is the selector for
/// `unresolved-self-call` and None everywhere else: upstream's other details are
/// the `Inference::DynamicOrigin` name (there is no analogue here — `coverage
/// --protection` is unimplemented) and the plugin row key (out of scope), and
/// upstream itself emits `null` when it has neither.
pub(super) type Cause = (String, Option<String>);

/// Selectors that mutate EVERY receiver (`[]=`, an attribute writer) but which
/// some catalogued ROW answers as a NON-mutation — the one place declining the
/// typer arm could over-claim rather than under-claim, so it is the one place
/// the collector suppresses rather than mirrors.
///
/// Upstream reaches `ENV#[]=` / `Thread#[]=` through the typer for a receiver
/// this collector cannot name (`thread[:k] = v` on a local), and those rows say
/// `global.write` and *not* a receiver mutation. Falling to the uncatalogued
/// path there would answer `mutate.instance` / `mutate.local` where the oracle
/// proves neither — an OVER, and the subset argument does not cover it, because
/// the catalogued path is where upstream's answer gets NARROWER than the
/// uncatalogued one. Derived from the shipped catalogue rather than listed, so a
/// row upstream adds is covered without an edit.
static NON_MUTATING_ROWED_SELECTORS: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    let catalog = rigor_effects::catalog();
    let mut suppressed = BTreeSet::new();
    for class in catalog.class_names() {
        let entry = catalog.class_entry(class).expect("listed");
        let rows = entry.instance_methods().iter().chain(entry.singleton_methods().iter());
        for (selector, row) in rows {
            if MutationClassifier::universally_mutating(selector) && !row.mutates_receiver() {
                suppressed.insert(selector.clone());
            }
        }
    }
    for selector in catalog.universal() {
        // The universal `Entry` is never a mutation either (`catalog.rb:199`).
        if MutationClassifier::universally_mutating(selector) {
            suppressed.insert(selector.clone());
        }
    }
    suppressed
});

/// One effect unit's DIRECT summary: `{origin => labels}` and its flat join,
/// plus the two halves of the taint bit — the causes this unit's own walk
/// decided, and the selectors its calls could reach a project unit through.
#[derive(Debug, Default, Clone)]
pub(super) struct Summary {
    bundles: BTreeMap<String, BTreeSet<String>>,
    causes: BTreeSet<Cause>,
    /// Every selector this unit could contribute a project EDGE for. Resolved
    /// against the run's own unit set at report time — see the module docs.
    edge_selectors: BTreeSet<String>,
}

impl Summary {
    fn add<I, S>(&mut self, origin: &str, labels: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let labels: BTreeSet<String> = labels.into_iter().map(Into::into).collect();
        if labels.is_empty() {
            return;
        }
        self.bundles.entry(origin.to_string()).or_default().extend(labels);
    }

    /// Union per origin, and per cause and per pending edge. Reopenings of one
    /// method — in one file and across the several files that contribute to it —
    /// fold through this, exactly as upstream's `Summary#join` ANDs the bit and
    /// unions the causes (`summary.rb:89`).
    pub(super) fn join(&mut self, other: Self) {
        for (origin, labels) in other.bundles {
            self.bundles.entry(origin).or_default().extend(labels);
        }
        self.causes.extend(other.causes);
        self.edge_selectors.extend(other.edge_selectors);
    }

    /// The flat proven lane — every origin's labels, sorted and de-duplicated.
    pub(super) fn proven(&self) -> Vec<String> {
        let flat: BTreeSet<&String> = self.bundles.values().flatten().collect();
        flat.into_iter().cloned().collect()
    }

    /// `{origin => labels}`, for the report's ungraded `direct:` key.
    pub(super) fn bundles(&self) -> &BTreeMap<String, BTreeSet<String>> {
        &self.bundles
    }

    /// The TRANSITIVE bit: no producer fired, and no call could carry taint in
    /// along a project edge. `selectors` is the run's own unit-selector set.
    pub(super) fn exhaustive(&self, selectors: &BTreeSet<String>) -> bool {
        self.causes.is_empty() && self.edge_selectors.is_disjoint(selectors)
    }

    /// `[[cause, detail], …]` — de-duplicated and sorted by `[cause, detail]`,
    /// which is `BTreeSet` order with `None` reading as upstream's `nil.to_s`
    /// (`summary.rb:143`). The invariant `causes.empty? == exhaustive` holds by
    /// construction: the edge taints are materialised here too.
    pub(super) fn causes(&self, selectors: &BTreeSet<String>) -> Vec<Cause> {
        let mut causes = self.causes.clone();
        for selector in self.edge_selectors.intersection(selectors) {
            causes.insert((UNRESOLVED_SELF_CALL.to_string(), Some(selector.clone())));
        }
        causes.into_iter().collect()
    }
}

/// The selector half of a unit key — `C#m` / `C.m` / `Outer::Inner#m` /
/// `<toplevel>#m` all answer `m`. A class name carries neither `#` nor `.`, and
/// no method name does either, so the FIRST of the two separates them.
pub(super) fn selector_of(key: &str) -> &str {
    key.find(['#', '.']).map_or(key, |at| &key[at + 1..])
}

/// A synthesised `attr_writer` / `attr_accessor` setter's summary — the same
/// value at every accessor in the project (`scanner.rb:55`).
fn writer_summary() -> Summary {
    let mut summary = Summary::default();
    summary.add(ATTR_WRITER, ["mutate.self"]);
    summary
}

/// Scan one file's Prism tree into `{method key => direct summary}`.
///
/// Keys follow the existing symbol tables: `Class#m` for an instance method,
/// `Class.m` for a singleton one, `Outer::Inner#m` for a lexical join, and
/// `<toplevel>#m` for a `def` outside any class body.
pub(super) fn scan(root: &Node<'_>) -> BTreeMap<String, Summary> {
    let mut scanner = Scanner::default();
    scanner.walk(root, &[], false);
    scanner.summaries
}

#[derive(Default)]
struct Scanner {
    summaries: BTreeMap<String, Summary>,
}

impl Scanner {
    fn walk(&mut self, node: &Node<'_>, prefix: &[String], singleton: bool) {
        if let Some(class) = node.as_class_node() {
            return self.walk_namespace(node, &class.constant_path(), class.body().as_ref(), prefix);
        }
        if let Some(module) = node.as_module_node() {
            return self.walk_namespace(
                node,
                &module.constant_path(),
                module.body().as_ref(),
                prefix,
            );
        }
        if let Some(singleton_class) = node.as_singleton_class_node() {
            // `class << self` flips the axis that separates `mutate.self` from
            // `mutate.static`. An EMPTY one falls through to the ordinary
            // descent, exactly as upstream's `… if node.body` does.
            if let Some(body) = singleton_class.body() {
                self.walk(&body, prefix, true);
                return;
            }
        }
        if let Some(def) = node.as_def_node() {
            return self.enter_def(&def, prefix, singleton);
        }
        if let Some(call) = node.as_call_node() {
            if declaration(&call) {
                return self.record_declaration(&call, prefix);
            }
        }
        for child in children(node) {
            self.walk(&child, prefix, singleton);
        }
    }

    /// `node` is the whole `class` / `module` node; `path` and `body` are its
    /// own fields, read out by the caller because the two node types do not
    /// share a Rust trait.
    fn walk_namespace(
        &mut self,
        node: &Node<'_>,
        path: &Node<'_>,
        body: Option<&Node<'_>>,
        prefix: &[String],
    ) {
        let Some(name) = qualified_name(path) else {
            // A constant path the syntax cannot name — upstream descends into
            // the declaration's OWN children without entering a namespace.
            for child in children(node) {
                self.walk(&child, prefix, false);
            }
            return;
        };
        let mut nested = prefix.to_vec();
        nested.push(name);
        if let Some(body) = body {
            self.walk(body, &nested, false);
        }
    }

    fn enter_def(&mut self, node: &DefNode<'_>, prefix: &[String], singleton: bool) {
        self.add_unit(
            &class_name_for(prefix),
            &name_of(node.name().as_slice()),
            singleton || node.receiver().is_some(),
            node.body().as_ref(),
            node.parameters().as_ref(),
        );
    }

    /// Scans one unit and files its summary, then recurses into the units its
    /// body declared.
    fn add_unit(
        &mut self,
        class_name: &str,
        method_name: &str,
        singleton: bool,
        body: Option<&Node<'_>>,
        parameters: Option<&ParametersNode<'_>>,
    ) {
        let key = format!("{class_name}{}{method_name}", if singleton { "." } else { "#" });
        let names = parameter_names(parameters);
        let mut scan = UnitScan::new(
            singleton,
            names.clone(),
            block_parameter_name(parameters),
            owned_locals(body, &names),
        );
        if let Some(body) = body {
            scan.visit(body);
        }
        let (summary, nested) = scan.finish();
        self.merge_unit(&key, summary);
        for unit in nested {
            self.add_unit(
                class_name,
                &unit.name,
                singleton || unit.singleton,
                unit.body.as_ref(),
                unit.parameters.as_ref(),
            );
        }
    }

    fn record_declaration(&mut self, node: &CallNode<'_>, prefix: &[String]) {
        let class_name = class_name_for(prefix);
        match name_of(node.name().as_slice()).as_str() {
            // `include` / `prepend` record ancestry, which the transitive lane
            // (slice 4) reads and the direct one does not.
            "include" | "prepend" => {}
            "define_method" => {
                if let Some(unit) = define_method_unit(node) {
                    self.add_unit(
                        &class_name,
                        &unit.name,
                        unit.singleton,
                        unit.body.as_ref(),
                        unit.parameters.as_ref(),
                    );
                }
            }
            macro_name => self.synthesize_accessors(&class_name, macro_name, node),
        }
    }

    /// `attr_reader` / `attr_writer` / `attr_accessor` — synthesised: a reader
    /// is ∅, a writer is `mutate.self`. Without them a caller's edge into an
    /// accessor would read as unresolved.
    fn synthesize_accessors(&mut self, class_name: &str, macro_name: &str, node: &CallNode<'_>) {
        for name in symbol_arguments(node) {
            if macro_name != "attr_writer" {
                self.merge_unit(&format!("{class_name}#{name}"), Summary::default());
            }
            if macro_name == "attr_reader" {
                continue;
            }
            self.merge_unit(&format!("{class_name}#{name}="), writer_summary());
        }
    }

    fn merge_unit(&mut self, key: &str, summary: Summary) {
        self.summaries.entry(key.to_string()).or_default().join(summary);
    }
}

/// A unit discovered inside another — a nested `def`, or a `define_method` with
/// a literal name whose block becomes that method's body.
struct NestedUnit<'pr> {
    name: String,
    singleton: bool,
    body: Option<Node<'pr>>,
    parameters: Option<ParametersNode<'pr>>,
}

/// `define_method(:literal) { … }` — the block becomes `literal`'s body, so the
/// call is a unit declaration wherever it appears. A non-literal name has no key
/// to file the block under, so it stays contained in the enclosing method and
/// this answers None (`unit_scan.rb:91`).
fn define_method_unit<'pr>(node: &CallNode<'pr>) -> Option<NestedUnit<'pr>> {
    if node.name().as_slice() != b"define_method" || node.receiver().is_some() {
        return None;
    }
    let first = node.arguments()?.arguments().iter().next()?;
    let name = name_of(first.as_symbol_node()?.unescaped());
    let block = node.block()?.as_block_node()?;
    // A block's parameters are a `BlockParametersNode`, not a `ParametersNode`;
    // upstream's `parameter_names` takes only the latter, so a `define_method`
    // unit has no parameter names on either side.
    Some(NestedUnit { name, singleton: false, body: block.body(), parameters: None })
}

/// One method body, scanned into its direct summary.
struct UnitScan<'pr> {
    singleton: bool,
    /// The unit's own `&blk` parameter NAME. Read by the two `opaque-callable`
    /// producers that treat a call on it as FORWARDING rather than as an opaque
    /// callable (`unit_scan.rb:490`, `:528`); slice 2 dropped it because nothing
    /// but taints read it.
    block_parameter: Option<String>,
    mutation: MutationClassifier,
    summary: Summary,
    nested: Vec<NestedUnit<'pr>>,
}

impl<'pr> UnitScan<'pr> {
    fn new(
        singleton: bool,
        parameters: BTreeSet<String>,
        block_parameter: Option<String>,
        owned: BTreeSet<String>,
    ) -> Self {
        Self {
            singleton,
            block_parameter,
            mutation: MutationClassifier::new(singleton, parameters, owned),
            summary: Summary::default(),
            nested: Vec::new(),
        }
    }

    fn finish(self) -> (Summary, Vec<NestedUnit<'pr>>) {
        (self.summary, self.nested)
    }

    fn taint(&mut self, cause: &str, detail: Option<&str>) {
        self.summary.causes.insert((cause.to_string(), detail.map(str::to_string)));
    }

    /// Upstream's `push_edge` (`unit_scan.rb:514`) — the funnel every project
    /// edge goes through. Upstream records `(receiver class, kind, selector)`
    /// for the propagator; without a typer there is no receiver class, so the
    /// port records the SELECTOR and lets the report decide whether it names a
    /// project unit (module docs).
    fn push_edge(&mut self, selector: &str) {
        self.summary.edge_selectors.insert(selector.to_string());
    }

    /// Whether `node` is a read of this unit's own `&blk` parameter.
    fn is_block_parameter(&self, node: &Node<'pr>) -> bool {
        let Some(local) = node.as_local_variable_read_node() else { return false };
        self.block_parameter.as_deref() == Some(name_of(local.name().as_slice()).as_str())
    }

    /// The construct origins (`unit_scan.rb:192-218`), minus the `CallNode` arm,
    /// which `visit_call_node` owns because it also decides descent.
    fn visit_construct(&mut self, node: &Node<'pr>) {
        if node.as_x_string_node().is_some() || node.as_interpolated_x_string_node().is_some() {
            return self.summary.add(XSTRING, ["io.process"]);
        }
        if let Some(read) = node.as_global_variable_read_node() {
            let name = name_of(read.name().as_slice());
            if !FRAME_LOCAL_GLOBALS.contains(&name.as_str()) {
                self.summary.add(GVAR_READ, ["global.read"]);
            }
            return;
        }
        if node.as_global_variable_write_node().is_some()
            || node.as_global_variable_operator_write_node().is_some()
            || node.as_global_variable_or_write_node().is_some()
            || node.as_global_variable_and_write_node().is_some()
        {
            return self.summary.add(GVAR_WRITE, ["global.write"]);
        }
        if node.as_class_variable_read_node().is_some() {
            return self.summary.add(CVAR_READ, ["global.read"]);
        }
        if node.as_class_variable_write_node().is_some()
            || node.as_class_variable_operator_write_node().is_some()
            || node.as_class_variable_or_write_node().is_some()
            || node.as_class_variable_and_write_node().is_some()
        {
            return self.summary.add(CVAR_WRITE, ["mutate.static"]);
        }
        if node.as_instance_variable_write_node().is_some()
            || node.as_instance_variable_operator_write_node().is_some()
            || node.as_instance_variable_or_write_node().is_some()
            || node.as_instance_variable_and_write_node().is_some()
        {
            let label = if self.singleton { "mutate.static" } else { "mutate.self" };
            return self.summary.add(IVAR_WRITE, [label]);
        }
        if node.as_alias_method_node().is_some() || node.as_alias_global_variable_node().is_some() {
            return self.summary.add(ALIAS, ["mutate.static"]);
        }
        if node.as_undef_node().is_some() {
            return self.summary.add(UNDEF, ["mutate.static"]);
        }
        // The compound index / attribute writes. Unlike a plain `[]=` CALL these
        // never reach the catalogue at all, so the ownership judgment is the
        // whole reading and it is pure syntax on both sides.
        let receiver = if let Some(write) = node.as_index_operator_write_node() {
            write.receiver()
        } else if let Some(write) = node.as_index_or_write_node() {
            write.receiver()
        } else if let Some(write) = node.as_index_and_write_node() {
            write.receiver()
        } else if let Some(write) = node.as_call_operator_write_node() {
            write.receiver()
        } else if let Some(write) = node.as_call_or_write_node() {
            write.receiver()
        } else if let Some(write) = node.as_call_and_write_node() {
            write.receiver()
        } else {
            return;
        };
        self.classify_mutation(receiver.as_ref());
    }

    /// The catalogued path. Answers whether the catalogue claimed this call — a
    /// claim suppresses the uncatalogued reading, which is what an explicit ∅
    /// row is for (`unit_scan.rb:381`).
    fn claimed_by_catalogue(&mut self, node: &CallNode<'pr>) -> bool {
        let Some((owner, singleton, implicit)) = catalog_target(node) else { return false };
        let selector = name_of(node.name().as_slice());
        let catalog = rigor_effects::catalog();
        // `posture: false` — ALWAYS, for every receiver shape. The tier is gated
        // upstream on the typer's `dynamic` bit, which this collector does not
        // have and must not guess (module docs, issue #106). It is the answer
        // upstream itself gives for a `Dynamic` receiver, so what survives is a
        // strict subset of upstream's rows and universal answers.
        let Some(resolved) = catalog.resolve(&owner, &selector, singleton, false) else {
            return false;
        };
        let (labels, mutates) = match resolved {
            Resolution::Row(row) => match row.narrow() {
                // ADR-0043 § 2: the row's own `effects:` is the UNNARROWED
                // upper bound, and a coarser label than the oracle proves is an
                // OVER. A handler this port does not implement answers ∅ — never
                // the parent — and the row still CLAIMS the call.
                Some(handler) => {
                    (narrowing::apply(handler, node).unwrap_or_default(), row.mutates_receiver())
                }
                None => (row.labels().to_vec(), row.mutates_receiver()),
            },
            // The 34-name `universal:` list, which answers ∅ and claims the
            // call. `Posture` is unreachable with the tier off, and is spelled
            // here rather than `unreachable!()` because the two entries read
            // identically — a future tier would be a DECISION, not a panic.
            Resolution::Universal(entry) | Resolution::Posture(entry) => {
                (entry.labels().to_vec(), entry.mutates_receiver())
            }
        };
        if !labels.is_empty() {
            let separator = if singleton { "." } else { "#" };
            self.summary.add(&format!("catalogue:{owner}{separator}{selector}"), labels);
        }
        // `mutating_catalogued?` (`unit_scan.rb:413`) is
        // `mutates_receiver? || (posture? && mutating?(node, owner))`, and with
        // the posture tier off the second disjunct is constant-false: no entry
        // this lookup can return has `posture?` set. It was already inert —
        // that arm only ever fired on a CONSTANT receiver, which
        // `MutationClassifier::label_for` never classifies — so dropping it
        // moves nothing.
        if mutates {
            self.classify_mutation(node.receiver().as_ref());
        }
        // `keeps_project_edge?(entry, implicit)` is `entry.posture? || implicit`
        // (`unit_scan.rb:409`). With the posture tier off no entry this lookup
        // can return has `posture?` set, so the surviving disjunct is IMPLICIT
        // SELF — an unqualified name resolves against self's ancestry first, and
        // a project method of the same name wins at run time. `Kernel#format`
        // beside a project's own `format` is the measured case, and it is why a
        // fully catalogued body can still be transitively tainted.
        if implicit {
            self.push_edge(&selector);
        }
        true
    }

    /// What is left when the catalogue said nothing — `visit_uncatalogued`
    /// (`unit_scan.rb:433`), arm for arm and in upstream's order. The mutation
    /// judgment runs first and independently of the taints (`params[:x] = 1` on
    /// an untyped `params` is a proven `mutate.instance` *and* a
    /// `dynamic-receiver` taint), and the last two arms are where the port's
    /// blanket conservatism lives: upstream reads the typer's `dynamic` and
    /// `resolved` bits, this collector has neither, so every site that reaches
    /// them taints.
    fn visit_uncatalogued(&mut self, node: &CallNode<'pr>) {
        let selector = name_of(node.name().as_slice());
        // Upstream RETURNS before the mutation judgment for both of these.
        if REFLECTIVE_SEND.contains(&selector.as_str()) {
            return self.visit_reflective_send(node);
        }
        if opaque_eval(node, &selector) {
            return self.taint(OPAQUE_CALLABLE, None);
        }
        // No typer, so no receiver class: only the universally-mutating
        // selectors survive here, which is upstream's own answer whenever it
        // has no receiver class either. `NON_MUTATING_ROWED_SELECTORS` suppresses
        // the LABEL only — it is a port-side device against one over-claim, not
        // an upstream arm, so it may not swallow the taints below.
        if !NON_MUTATING_ROWED_SELECTORS.contains(&selector)
            && MutationClassifier::mutating(&selector, None)
        {
            self.classify_mutation(node.receiver().as_ref());
        }
        // `opaque-callable` is checked before `dynamic-receiver` because it is
        // the more specific reading of the same site. Upstream's last condition
        // is `record.nil? || record.receiver_class.nil? || receiver_class ∈
        // {Proc, Method}`, and `record.nil?` is always true without a typer — so
        // this arm is SOUND and over-taints (`.call` on a project object is an
        // ordinary edge upstream).
        if self.opaque_callable(node, &selector) {
            return self.taint(OPAQUE_CALLABLE, None);
        }
        // `record_edge` (`:497`). Upstream taints `dynamic-receiver` only when
        // the typer said `Dynamic`, and `unresolved-self-call` only when the
        // dispatcher declined; the port can prove NEITHER negative, so it taints
        // at every such site. Both are strict over-taints — the safe direction
        // (ADR-0043 § 2) — and they are what the residual UNDER column measures.
        if node.receiver().is_some() {
            return self.taint(DYNAMIC_RECEIVER, None);
        }
        self.push_edge(&selector);
        self.taint(UNRESOLVED_SELF_CALL, Some(&selector));
    }

    /// `send` / `public_send` / `__send__`: a LITERAL selector is an ordinary
    /// edge and must not taint, a computed one is `dynamic-send`
    /// (`unit_scan.rb:472`).
    fn visit_reflective_send(&mut self, node: &CallNode<'pr>) {
        match node.arguments().and_then(|args| args.arguments().iter().next()).and_then(
            |first| literal_selector(&first),
        ) {
            Some(selector) => self.push_edge(&selector),
            None => self.taint(DYNAMIC_SEND, None),
        }
    }

    /// A `.call` the analyzer cannot follow to a body (`unit_scan.rb:486`). A
    /// call on a lambda literal is the literal's own body by containment, and a
    /// call on the unit's `&blk` is forwarding — the block's effects are
    /// accounted at the caller's literal.
    fn opaque_callable(&self, node: &CallNode<'pr>, selector: &str) -> bool {
        if selector != "call" {
            return false;
        }
        let Some(receiver) = node.receiver() else { return false };
        receiver.as_lambda_node().is_none() && !self.is_block_parameter(&receiver)
    }

    /// `&expr` where `expr` is neither a symbol nor this unit's own `&blk`
    /// (`unit_scan.rb:522`) — a callable handed to a callee the analyzer will
    /// not read. Runs on EVERY call, catalogued or not. An anonymous forward
    /// (`foo(&)`) has no expression and is silent, exactly as upstream's
    /// `expression.nil?` guard.
    fn visit_block_argument(&mut self, node: &CallNode<'pr>) {
        let Some(block) = node.block() else { return };
        let Some(argument) = block.as_block_argument_node() else { return };
        let Some(expression) = argument.expression() else { return };
        if expression.as_symbol_node().is_some() || self.is_block_parameter(&expression) {
            return;
        }
        self.taint(OPAQUE_CALLABLE, None);
    }

    /// A mutation whose ownership is not provable is upstream's
    /// `unknown-ownership` TAINT and never a proven bare `mutate`
    /// (`unit_scan.rb:533`). `MutationClassifier::label_for` is pure syntax on
    /// both sides, so this producer is EXACT wherever the caller is — the six
    /// compound-write node types, and the claimed `mutates: receiver` path.
    fn classify_mutation(&mut self, receiver: Option<&Node<'pr>>) {
        match self.mutation.label_for(receiver) {
            Some(ownership) => self.summary.add(RECEIVER_MUTATION, [ownership.label()]),
            None => self.taint(UNKNOWN_OWNERSHIP, None),
        }
    }
}

impl<'pr> Visit<'pr> for UnitScan<'pr> {
    fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
        self.visit_construct(&node);
    }

    fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
        self.visit_construct(&node);
    }

    /// A nested `def` is recorded and NOT descended into: its body belongs to
    /// its own summary, and the enclosing method gets nothing — a nested def is
    /// deliberately not `mutate.static` (`unit_scan.rb:176`).
    fn visit_def_node(&mut self, node: &DefNode<'pr>) {
        self.nested.push(NestedUnit {
            name: name_of(node.name().as_slice()),
            singleton: node.receiver().is_some(),
            body: node.body(),
            parameters: node.parameters(),
        });
    }

    fn visit_call_node(&mut self, node: &CallNode<'pr>) {
        // A `define_method` with a literal name is a unit boundary: the block
        // is the new method's body, and the enclosing method gains only the
        // `mutate.static` of having defined it.
        if let Some(unit) = define_method_unit(node) {
            self.nested.push(unit);
            self.summary.add(DEFINE_METHOD, ["mutate.static"]);
            return;
        }
        if !self.claimed_by_catalogue(node) {
            self.visit_uncatalogued(node);
        }
        // Upstream runs this on every call, claimed or not (`unit_scan.rb:232`).
        self.visit_block_argument(node);
        // Containment: a block literal's origins always join the enclosing
        // method, so the walk descends into every child including the block.
        ruby_prism::visit_call_node(self, node);
    }
}

/// The class the catalogue would look this call up under, as
/// `(owner, singleton, implicit_self)` — spelled from the SYNTAX, or None.
///
/// Upstream's third arm (`record.receiver_class`) is deliberately absent; see
/// the module docs.
fn catalog_target(node: &CallNode<'_>) -> Option<(String, bool, bool)> {
    let Some(receiver) = node.receiver() else {
        return Some(("Kernel".to_string(), false, true));
    };
    if receiver.as_self_node().is_some() {
        return Some(("Kernel".to_string(), false, true));
    }
    let constant = qualified_name(&receiver)?;
    // `kind: object` constants (`ENV ARGF STDIN STDOUT STDERR`) flip the
    // singleton bit, so `ENV["HOME"]` keys as the INSTANCE row `ENV#[]` while
    // `File.read` keys as the singleton `File.read` (`unit_scan.rb:558`).
    let singleton = !rigor_effects::catalog().object_constant(&constant);
    Some((constant, singleton, false))
}

/// An `eval`-family call carrying code rather than a block, or a bare
/// `binding` — both hand the analyzer source it will not read
/// (`unit_scan.rb:543`). The BLOCK forms are containment and must not be
/// treated as opaque, which is why the test is a POSITIONAL argument.
fn opaque_eval(node: &CallNode<'_>, selector: &str) -> bool {
    if selector == "binding" {
        return node.receiver().is_none() && node.arguments().is_none();
    }
    if !EVAL_SELECTORS.contains(&selector) {
        return false;
    }
    positional_arity(node) > 0
}

/// Upstream's `literal_selector` — a `send` argument that settles the callee's
/// name (`unit_scan.rb:481`). An interpolated string is not a `StringNode` and
/// so answers None, which is the `dynamic-send` taint.
fn literal_selector(node: &Node<'_>) -> Option<String> {
    if let Some(symbol) = node.as_symbol_node() {
        return Some(name_of(symbol.unescaped()));
    }
    node.as_string_node().map(|string| name_of(string.unescaped()))
}

fn positional_arity(node: &CallNode<'_>) -> usize {
    node.arguments().map_or(0, |arguments| {
        arguments
            .arguments()
            .iter()
            .filter(|argument| argument.as_keyword_hash_node().is_none())
            .count()
    })
}

/// A receiver-less call in a class / module body that declares units or
/// ancestry (`scanner.rb:201`).
fn declaration(node: &CallNode<'_>) -> bool {
    node.receiver().is_none()
        && DECLARATION_MACROS.contains(&name_of(node.name().as_slice()).as_str())
}

fn symbol_arguments(node: &CallNode<'_>) -> Vec<String> {
    node.arguments().map_or_else(Vec::new, |arguments| {
        arguments
            .arguments()
            .iter()
            .filter_map(|argument| {
                argument.as_symbol_node().map(|symbol| name_of(symbol.unescaped()))
            })
            .collect()
    })
}

fn class_name_for(prefix: &[String]) -> String {
    if prefix.is_empty() { TOP_LEVEL_KEY.to_string() } else { prefix.join("::") }
}

fn parameter_names(parameters: Option<&ParametersNode<'_>>) -> BTreeSet<String> {
    let Some(parameters) = parameters else { return BTreeSet::new() };
    let mut names = BTreeSet::new();
    let groups = [
        parameters.requireds(),
        parameters.optionals(),
        parameters.posts(),
        parameters.keywords(),
    ];
    for group in &groups {
        for parameter in group.iter() {
            if let Some(name) = parameter_name(&parameter) {
                names.insert(name);
            }
        }
    }
    for parameter in [parameters.rest(), parameters.keyword_rest()].into_iter().flatten() {
        if let Some(name) = parameter_name(&parameter) {
            names.insert(name);
        }
    }
    names
}

/// The unit's `&blk` parameter name — upstream's `block_parameter_name`
/// (`scanner.rb:283`), which is `parameters.block&.name&.to_s` and deliberately
/// NOT part of `parameter_names`: a block parameter is not an ordinary one for
/// the ownership judgment. An anonymous `&` has no name and answers None.
fn block_parameter_name(parameters: Option<&ParametersNode<'_>>) -> Option<String> {
    let block = parameters?.block()?;
    block.name().map(|name| name_of(name.as_slice()))
}

/// The name of one parameter node, for every shape that has one. Upstream's
/// `respond_to?(:name)` guard, spelled out.
fn parameter_name(node: &Node<'_>) -> Option<String> {
    if let Some(required) = node.as_required_parameter_node() {
        return Some(name_of(required.name().as_slice()));
    }
    if let Some(optional) = node.as_optional_parameter_node() {
        return Some(name_of(optional.name().as_slice()));
    }
    if let Some(keyword) = node.as_required_keyword_parameter_node() {
        return Some(name_of(keyword.name().as_slice()));
    }
    if let Some(keyword) = node.as_optional_keyword_parameter_node() {
        return Some(name_of(keyword.name().as_slice()));
    }
    if let Some(rest) = node.as_rest_parameter_node() {
        return rest.name().map(|name| name_of(name.as_slice()));
    }
    if let Some(rest) = node.as_keyword_rest_parameter_node() {
        return rest.name().map(|name| name_of(name.as_slice()));
    }
    None
}

/// Upstream's `Source::ConstantPath.qualified_name` — LENIENT: a constant path
/// rooted in a dynamic base (`expr::Bar`) drops the dynamic segment and renders
/// the trailing constant names. A node that is neither a `ConstantReadNode` nor
/// a `ConstantPathNode` answers None.
fn qualified_name(node: &Node<'_>) -> Option<String> {
    if let Some(read) = node.as_constant_read_node() {
        return Some(name_of(read.name().as_slice()));
    }
    let path = node.as_constant_path_node()?;
    Some(render_constant_path(&path))
}

fn render_constant_path(path: &ruby_prism::ConstantPathNode<'_>) -> String {
    let name = path.name().map_or_else(String::new, |name| name_of(name.as_slice()));
    let prefix = match path.parent() {
        Some(parent) => {
            if let Some(read) = parent.as_constant_read_node() {
                format!("{}::", name_of(read.name().as_slice()))
            } else if let Some(inner) = parent.as_constant_path_node() {
                format!("{}::", render_constant_path(&inner))
            } else {
                String::new()
            }
        }
        None => String::new(),
    };
    format!("{prefix}{name}")
}

/// Every direct child node, in `compact_child_nodes` order — the `Scanner`'s
/// own descent, which stops at a `def` and at a declaration macro and so cannot
/// ride the `Visit` trait's.
fn children<'pr>(node: &Node<'pr>) -> Vec<Node<'pr>> {
    struct Children<'pr> {
        depth: usize,
        out: Vec<Node<'pr>>,
    }
    impl<'pr> Visit<'pr> for Children<'pr> {
        fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
            if self.depth == 1 {
                self.out.push(node);
            }
            self.depth += 1;
        }

        fn visit_branch_node_leave(&mut self) {
            self.depth -= 1;
        }

        fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
            if self.depth == 1 {
                self.out.push(node);
            }
            self.depth += 1;
        }

        fn visit_leaf_node_leave(&mut self) {
            self.depth -= 1;
        }
    }
    let mut children = Children { depth: 0, out: Vec::new() };
    children.visit(node);
    children.out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{key => sorted proven labels}` for one Ruby source.
    fn summaries(source: &str) -> BTreeMap<String, Vec<String>> {
        let result = rigor_parse::parse(source.as_bytes());
        assert!(result.errors().next().is_none(), "the fixture must parse");
        scan(&result.node())
            .into_iter()
            .map(|(key, summary)| (key, summary.proven()))
            .collect()
    }

    fn keys(source: &str) -> Vec<String> {
        summaries(source).into_keys().collect()
    }

    /// `{key => (exhaustive, causes)}` for one source, resolved against that
    /// source's OWN unit set — the same two-pass shape `report_rows` runs.
    fn bits(source: &str) -> BTreeMap<String, (bool, Vec<Cause>)> {
        let result = rigor_parse::parse(source.as_bytes());
        assert!(result.errors().next().is_none(), "the fixture must parse");
        let scanned = scan(&result.node());
        let selectors: BTreeSet<String> =
            scanned.keys().map(|key| selector_of(key).to_string()).collect();
        scanned
            .into_iter()
            .map(|(key, summary)| {
                let bit = summary.exhaustive(&selectors);
                (key, (bit, summary.causes(&selectors)))
            })
            .collect()
    }

    /// One method body's `(exhaustive, causes)`. The unit carries a `&blk`, so
    /// the forwarding arms of `opaque-callable` are reachable from here.
    fn taint(body: &str) -> (bool, Vec<Cause>) {
        bits(&format!("class C\n def m(list, path, payload, &blk)\n{body}\n end\nend"))
            .remove("C#m")
            .expect("the unit must exist")
    }

    fn cause(cause: &str, detail: Option<&str>) -> Cause {
        (cause.to_string(), detail.map(str::to_string))
    }

    // -----------------------------------------------------------------------
    // Unit identity — the probe's § 4a table, case for case.
    // -----------------------------------------------------------------------

    #[test]
    fn a_toplevel_def_keys_under_the_toplevel_marker() {
        assert_eq!(keys("def m\nend"), ["<toplevel>#m"]);
    }

    #[test]
    fn the_three_singleton_spellings_key_with_a_dot() {
        assert_eq!(keys("class C\n def i\n end\n def self.s\n end\nend"), ["C#i", "C.s"]);
        assert_eq!(keys("class C\n class << self\n  def s\n  end\n end\nend"), ["C.s"]);
    }

    #[test]
    fn a_nested_def_is_its_own_unit_and_the_encloser_gains_nothing() {
        let found = summaries("class C\n def outer\n  def inner\n   @x = 1\n  end\n end\nend");
        assert_eq!(found.keys().collect::<Vec<_>>(), ["C#inner", "C#outer"]);
        assert!(found["C#outer"].is_empty(), "a nested def is NOT mutate.static");
        assert_eq!(found["C#inner"], ["mutate.self"]);
    }

    #[test]
    fn define_method_in_a_class_body_is_a_unit_and_the_body_is_not() {
        let found = summaries("class C\n define_method(:lit) { @x = 1 }\nend");
        assert_eq!(found.keys().collect::<Vec<_>>(), ["C#lit"]);
        assert_eq!(found["C#lit"], ["mutate.self"]);
    }

    #[test]
    fn define_method_inside_a_method_is_a_unit_and_mutate_static_on_the_encloser() {
        let found = summaries("class C\n def build\n  define_method(:lit) { @x = 1 }\n end\nend");
        assert_eq!(found.keys().collect::<Vec<_>>(), ["C#build", "C#lit"]);
        assert_eq!(found["C#build"], ["mutate.static"]);
        assert_eq!(found["C#lit"], ["mutate.self"]);
    }

    #[test]
    fn a_computed_define_method_name_stays_contained() {
        // No key to file the block under, so the block's origins join the
        // enclosing method by containment.
        let found = summaries("class C\n def build(n)\n  define_method(n) { @x = 1 }\n end\nend");
        assert_eq!(found.keys().collect::<Vec<_>>(), ["C#build"]);
        assert_eq!(found["C#build"], ["mutate.self"]);
    }

    #[test]
    fn the_accessor_macros_synthesise_units() {
        let found = summaries("class C\n attr_reader :ro\n attr_writer :wo\n attr_accessor :rw\nend");
        assert_eq!(found.keys().collect::<Vec<_>>(), ["C#ro", "C#rw", "C#rw=", "C#wo="]);
        assert!(found["C#ro"].is_empty());
        assert!(found["C#rw"].is_empty());
        assert_eq!(found["C#wo="], ["mutate.self"]);
        assert_eq!(found["C#rw="], ["mutate.self"]);
    }

    #[test]
    fn a_nested_namespace_joins_lexically() {
        assert_eq!(keys("module W\n class I\n  def deep\n  end\n end\nend"), ["W::I#deep"]);
    }

    #[test]
    fn a_class_body_alias_is_silent() {
        // Class bodies are not effect units in v1, so an `alias` in one is
        // neither a unit nor a label.
        assert_eq!(keys("class C\n def original\n end\n alias aliased original\nend"), [
            "C#original"
        ]);
    }

    #[test]
    fn a_reopening_joins_rather_than_replacing() {
        let found = summaries("class C\n def m\n  @x = 1\n end\nend\nclass C\n def m\n  $g = 1\n end\nend");
        assert_eq!(found["C#m"], ["global.write", "mutate.self"]);
    }

    // -----------------------------------------------------------------------
    // Construct origins — the probe's § 4d table.
    // -----------------------------------------------------------------------

    fn one(body: &str) -> Vec<String> {
        summaries(&format!("class C\n def m(list, path, payload)\n{body}\n end\nend"))
            .remove("C#m")
            .expect("the unit must exist")
    }

    #[test]
    fn every_construct_origin_reads() {
        assert_eq!(one("  `echo hi`"), ["io.process"]);
        assert_eq!(one("  %x{echo hi}"), ["io.process"]);
        assert_eq!(one("  $LOAD_PATH"), ["global.read"]);
        assert_eq!(one("  $x = 1"), ["global.write"]);
        assert_eq!(one("  $x ||= 1"), ["global.write"]);
        assert_eq!(one("  $x += 1"), ["global.write"]);
        assert_eq!(one("  @@x"), ["global.read"]);
        assert_eq!(one("  @@x = 1"), ["mutate.static"]);
        assert_eq!(one("  @@x ||= 1"), ["mutate.static"]);
        assert_eq!(one("  @x = 1"), ["mutate.self"]);
        assert_eq!(one("  @x ||= 1"), ["mutate.self"]);
        assert_eq!(one("  @x += 1"), ["mutate.self"]);
        assert_eq!(one("  alias a b"), ["mutate.static"]);
        // MEASURED against the oracle: the alias TARGETS are GlobalVariableReadNodes
        // and the walk descends into them, so upstream proves the read too.
        assert_eq!(one("  alias $a $b"), ["global.read", "mutate.static"]);
        assert_eq!(one("  undef a"), ["mutate.static"]);
        // An ivar READ produces nothing; only writes do.
        assert!(one("  @x").is_empty());
    }

    #[test]
    fn the_frame_local_globals_are_not_global_state() {
        for special in ["$~", "$_", "$&", "$`", "$'", "$+", "$!"] {
            assert!(one(&format!("  {special}")).is_empty(), "{special} read as global state");
        }
        assert_eq!(one("  $stdout"), ["global.read"]);
    }

    #[test]
    fn an_ivar_write_in_a_singleton_unit_is_mutate_static() {
        let found = summaries("class C\n def self.m\n  @x = 1\n end\nend");
        assert_eq!(found["C.m"], ["mutate.static"]);
        let inner = summaries("class C\n class << self\n  def m\n   @x = 1\n  end\n end\nend");
        assert_eq!(inner["C.m"], ["mutate.static"]);
    }

    // -----------------------------------------------------------------------
    // The catalogued path.
    // -----------------------------------------------------------------------

    #[test]
    fn a_constant_path_receiver_reads_the_catalogue() {
        assert_eq!(one("  File.read(path)"), ["io.fs.read"]);
        assert_eq!(one("  File.write(path, payload)"), ["io.fs.write"]);
        assert_eq!(one("  Time.now"), ["nondet.time"]);
        // An `object` constant keys as the INSTANCE row.
        assert_eq!(one(r#"  ENV["HOME"]"#), ["global.read"]);
    }

    #[test]
    fn an_implicit_self_call_reads_a_kernel_row_and_never_the_posture() {
        assert_eq!(one(r#"  puts "hello""#), ["io.output.stdout"]);
        assert_eq!(one("  rand(10)"), ["nondet.random"]);
        // …but a project method of the same shape must stay ∅. Upstream refuses
        // `Kernel`'s `world` posture for implicit self (else every unqualified
        // call in a project body would read `io`); this port refuses the tier
        // outright (issue #106), so the two agree here for two reasons.
        assert!(one("  helper(1)").is_empty());
        assert!(one("  self.helper(1)").is_empty());
    }

    #[test]
    fn a_narrowed_row_narrows_rather_than_answering_its_parent() {
        // The probe's § 2c trap: `io.fs` where the oracle proves `io.fs.read`
        // is an OVER, not a coarser truth.
        assert_eq!(one("  File.open(path)"), ["io.fs.read"]);
        assert_eq!(one(r#"  File.open(path, "w")"#), ["io.fs.write"]);
        assert!(one("  Time.new(2020, 1, 1)").is_empty());
        assert_eq!(one("  Time.new"), ["nondet.time"]);
        assert!(one("  Random.new(42)").is_empty());
    }

    #[test]
    fn the_posture_tier_never_answers_however_the_receiver_is_spelled() {
        // Issue #106. Upstream gates the class default on the TYPER's `dynamic`
        // bit, not on the receiver's spelling, and it cannot resolve every
        // constant its own catalogue names — so this port, which has no such
        // bit, may not read the tier at all. `File.some_uncatalogued_thing`
        // proves `io.fs` upstream and ∅ here (a deliberate UNDER); the eight
        // classes below prove ∅ on BOTH sides, and reading the tier for them is
        // the over-claim this closes.
        assert!(one("  File.some_uncatalogued_thing").is_empty());
        for class in [
            "Net::HTTP",
            "Net::SMTP",
            "Net::FTP",
            "OpenSSL::SSL::SSLSocket",
            "Fiddle::Handle",
            "Fiddle::Function",
            "PTY",
            "SOCKSSocket",
        ] {
            assert!(one(&format!("  {class}.zz_uncatalogued_zz")).is_empty(), "{class}");
        }
        // A `kind: object` constant takes the same answer on the INSTANCE side.
        assert!(one("  ENV.zz_uncatalogued_zz").is_empty());
        // A class the catalogue does NOT list contributes nothing at all.
        assert!(one("  SomeUndeclaredGem::Client.new.fetch").is_empty());
    }

    #[test]
    fn the_row_and_universal_tiers_still_answer_with_the_posture_off() {
        // The must-still-fire control for the tier drop above: `Catalog#lookup`
        // reads a ROW first and the 34-name `universal:` list second, both
        // BEFORE `posture:` (`catalog.rb:186`), so switching the third tier off
        // may not cost either. `Net::HTTP.get` is the sharp case — a row on one
        // of the eight classes the oracle cannot resolve, which the oracle
        // still proves, so an over-corrected fix reads as a missing label here.
        assert_eq!(one("  Net::HTTP.get(uri)"), ["io.net.http"]);
        assert_eq!(one("  Net::HTTP.post_form(uri, data)"), ["io.net.http"]);
        assert_eq!(one("  File.read(path)"), ["io.fs.read"]);
        assert_eq!(one(r#"  ENV["HOME"]"#), ["global.read"]);
        // The universal list answers ∅ — and it is what keeps `File.class` at ∅
        // rather than `io.fs` for any tier consulted after it.
        assert!(one("  File.class").is_empty());
        assert!(one("  File.frozen?").is_empty());
    }

    // -----------------------------------------------------------------------
    // Mutations.
    // -----------------------------------------------------------------------

    #[test]
    fn ownership_decides_the_mutate_label_of_an_attribute_write() {
        assert_eq!(one("  self.name = 1"), ["mutate.self"]);
        assert_eq!(one("  @x.name = 1"), ["mutate.self"]);
        // The cvar READ is an origin of its own — measured against the oracle.
        assert_eq!(one("  @@x.name = 1"), ["global.read", "mutate.static"]);
        assert_eq!(one("  list.name = 1"), ["mutate.instance"]);
        assert_eq!(one("  b = []\n  b.name = 1\n  nil"), ["mutate.local"]);
    }

    #[test]
    fn an_unprovable_ownership_suppresses_the_label_rather_than_guessing() {
        // Upstream's `unknown-ownership` TAINT; slice 2 carries no taints, so
        // the label is simply absent. A bare `mutate` here would be an OVER.
        assert!(one("  other.name = 1").is_empty());
        assert!(one("  make_it.name = 1").is_empty());
    }

    #[test]
    fn a_compound_index_write_classifies_without_the_catalogue() {
        // These never reach the catalogue on either side — upstream's `visit`
        // calls `classify_mutation` from the node type itself — so the reading
        // is pure syntax and EXACTLY upstream's.
        assert_eq!(one("  @x[0] += 1"), ["mutate.self"]);
        assert_eq!(one("  list[0] ||= 1"), ["mutate.instance"]);
        assert_eq!(one("  @x.count += 1"), ["mutate.self"]);
        assert_eq!(one("  b = []\n  b[0] &&= 1\n  nil"), ["mutate.local"]);
    }

    #[test]
    fn a_typed_mutator_needs_a_constant_receiver_and_is_otherwise_silent() {
        // `n << 2` is a bit shift and `io << "x"` is output, so `<<` on an
        // untyped receiver claims nothing — the corpus's `mutates_its_argument`.
        assert!(one("  list << 1").is_empty());
        assert!(one("  b = []\n  b << 1\n  nil").is_empty());
    }

    #[test]
    fn a_plain_index_write_is_suppressed_because_two_rows_answer_it_as_a_non_mutation() {
        // MEASURED against the oracle (impl note § composition probes):
        // `t[:k] = 1` on a `Thread` proves `global.write` and NOT
        // `mutate.instance`, because `Thread#[]=` is a ROW and rows are
        // authoritative. With no typer this collector cannot tell a `Thread`
        // local from any other, so it declines `[]=` on the UNCATALOGUED path
        // entirely — an UNDER on every ordinary index write, and the only
        // alternative was an OVER on this one.
        assert_eq!(
            NON_MUTATING_ROWED_SELECTORS.iter().map(String::as_str).collect::<Vec<_>>(),
            ["[]=", "default_external=", "default_internal="],
            "the suppression is DERIVED from the catalogue; a new row changes it"
        );
        assert!(one("  list[0] = 1").is_empty());
        assert!(one("  @x[0] = 1").is_empty());
        assert!(one("  b = []\n  b[0] = 1\n  nil").is_empty());
        // …but a CONSTANT receiver reads the row itself, both its label and its
        // deliberate non-mutation.
        assert_eq!(one(r#"  ENV["k"] = "v""#), ["global.write"]);
    }

    // -----------------------------------------------------------------------
    // Containment, blocks and the shapes that must stay ∅.
    // -----------------------------------------------------------------------

    #[test]
    fn a_block_literals_origins_join_the_enclosing_method() {
        assert_eq!(one(r#"  later = proc { puts "ran" }"#), ["io.output.stdout"]);
        assert_eq!(one("  list.each { |i| puts i }"), ["io.output.stdout"]);
        assert_eq!(one(r#"  instance_eval { puts "block" }"#), ["io.output.stdout"]);
    }

    #[test]
    fn yield_and_block_forwarding_originate_nothing() {
        assert!(one("  yield 1").is_empty());
        assert!(summaries("class C\n def m(&blk)\n  other(&blk)\n end\nend")["C#m"].is_empty());
    }

    // -----------------------------------------------------------------------
    // The taint bit (slice 3). Every negative below needs its positive control:
    // "no producer fired" is only meaningful beside a body that DOES reach
    // exhaustiveness, and the whole lane reads 0 OVER when the bit is never
    // claimed at all — which is precisely what slice 2 shipped.
    // -----------------------------------------------------------------------

    #[test]
    fn a_body_whose_every_call_the_catalogue_settles_is_exhaustive() {
        // THE CONTROL for every taint test below.
        assert_eq!(taint(r#"  puts "hi""#), (true, vec![]));
        assert_eq!(taint("  File.read(path)"), (true, vec![]));
        assert_eq!(taint("  @x = 1"), (true, vec![]));
        assert_eq!(taint("  yield 1"), (true, vec![]), "yield is not a call node");
        assert_eq!(taint("  list.send(:known)"), (true, vec![]));

        // …and the honest limit of the control: a LITERAL receiver is a shape
        // both engines type identically and neither calls `Dynamic`, but this
        // collector reads no types at all, so `1 + 2` taints where upstream does
        // not. Probe § 6b prices that island; slice 3 does not spend it.
        assert_eq!(taint("  (1 + 2) * 3"), (false, vec![cause(DYNAMIC_RECEIVER, None)]));
    }

    #[test]
    fn an_uncatalogued_call_taints_by_its_receiver_shape() {
        // Upstream reads the typer's `dynamic` / `resolved` bits here; the port
        // can prove neither negative, so both sites taint (module docs).
        assert_eq!(taint("  list.each_slice(2)"), (false, vec![cause(DYNAMIC_RECEIVER, None)]));
        assert_eq!(
            taint("  helper(1)"),
            (false, vec![cause(UNRESOLVED_SELF_CALL, Some("helper"))]),
            "the detail is the selector, character for character with upstream"
        );
        // A constant receiver the catalogue does not list is the same shape.
        assert_eq!(
            taint("  SomeUndeclaredGem::Client.fetch"),
            (false, vec![cause(DYNAMIC_RECEIVER, None)])
        );
    }

    #[test]
    fn a_reflective_send_taints_only_on_a_computed_selector() {
        assert_eq!(taint("  list.send(:known)"), (true, vec![]), "a literal is an ordinary edge");
        assert_eq!(taint(r#"  list.send("known")"#), (true, vec![]));
        assert_eq!(taint("  list.send(path)"), (false, vec![cause(DYNAMIC_SEND, None)]));
        assert_eq!(taint("  public_send(path)"), (false, vec![cause(DYNAMIC_SEND, None)]));
        assert_eq!(taint(r#"  __send__("a#{path}")"#), (false, vec![cause(DYNAMIC_SEND, None)]));
    }

    #[test]
    fn the_three_opaque_callable_arms_fire_and_their_forwarding_shapes_do_not() {
        // (a) the eval family with a POSITIONAL argument, and a bare `binding`.
        assert_eq!(taint(r#"  eval("1")"#), (false, vec![cause(OPAQUE_CALLABLE, None)]));
        assert_eq!(taint(r#"  instance_eval("1")"#), (false, vec![cause(OPAQUE_CALLABLE, None)]));
        assert_eq!(taint("  binding"), (false, vec![cause(OPAQUE_CALLABLE, None)]));
        // The BLOCK form is containment and the keyword form is `grep_v`'d, so
        // neither is OPAQUE — both fall through to the ordinary receiver-less
        // reading, which is what upstream does with them too.
        assert_eq!(
            taint("  instance_eval { @x = 1 }"),
            (false, vec![cause(UNRESOLVED_SELF_CALL, Some("instance_eval"))])
        );
        assert_eq!(
            taint("  instance_eval(mode: 1)"),
            (false, vec![cause(UNRESOLVED_SELF_CALL, Some("instance_eval"))])
        );

        // (b) `&expr` that is neither a symbol nor this unit's own `&blk`.
        assert_eq!(taint("  list.each(&blk)"), (false, vec![cause(DYNAMIC_RECEIVER, None)]));
        assert_eq!(taint("  send(:each, &blk)"), (true, vec![]), "forwarding the unit's own block");
        assert_eq!(taint("  send(:each, &:to_s)"), (true, vec![]), "a symbol is not opaque");
        assert_eq!(
            taint("  send(:each, &payload)"),
            (false, vec![cause(OPAQUE_CALLABLE, None)]),
            "any other callable is"
        );

        // (c) `.call` on a receiver that is neither a lambda literal nor `&blk`.
        // Upstream's own last condition is `record.nil? || …`, which is always
        // true without a typer — so this arm is the SOUND, over-tainting one.
        assert_eq!(taint("  payload.call"), (false, vec![cause(OPAQUE_CALLABLE, None)]));
        // The two exemptions still fall through to the ordinary explicit-receiver
        // reading rather than to `opaque-callable`: a lambda literal's body is
        // already counted by containment, and `&blk` is forwarding. (Upstream
        // reaches `exhaustive: true` for both because it can see the receiver is
        // a Proc; the port cannot, so it keeps the weaker `dynamic-receiver` —
        // an over-taint, the safe direction.)
        assert_eq!(taint("  blk.call"), (false, vec![cause(DYNAMIC_RECEIVER, None)]));
        assert_eq!(taint("  ->(x) { x }.call(1)"), (false, vec![cause(DYNAMIC_RECEIVER, None)]));
    }

    #[test]
    fn an_unprovable_ownership_taints_where_slice_2_was_merely_silent() {
        // The label half is unchanged (`…suppresses_the_label_rather_than_guessing`);
        // this is the taint upstream records at the same site. `b = list` is a
        // non-allocating assignment, so `b` is neither a parameter nor
        // frame-owned and its ownership is genuinely unprovable.
        //
        // At the six COMPOUND-write node types the producer is exact and alone:
        // they never reach the catalogue and are not `CallNode`s, so the
        // ownership judgment is the whole reading on both sides.
        assert_eq!(
            taint("  b = list\n  b[0] += 1\n  nil"),
            (false, vec![cause(UNKNOWN_OWNERSHIP, None)])
        );
        assert_eq!(
            taint("  b = list\n  b.count ||= 1\n  nil"),
            (false, vec![cause(UNKNOWN_OWNERSHIP, None)])
        );
        // The CALL form taints twice — upstream classifies the mutation first
        // and independently, then reads the receiver (`unit_scan.rb:445`).
        assert_eq!(
            taint("  b = list\n  b.name = 1\n  nil"),
            (false, vec![cause(DYNAMIC_RECEIVER, None), cause(UNKNOWN_OWNERSHIP, None)])
        );
        // A provable owner produces the label and no ownership taint.
        assert_eq!(taint("  @x[0] += 1"), (true, vec![]));
        assert_eq!(taint("  list[0] += 1"), (true, vec![]), "a parameter is mutate.instance");
        assert_eq!(taint("  b = []\n  b[0] += 1\n  nil"), (true, vec![]), "a frame-owned local");
    }

    #[test]
    fn a_catalogue_claimed_implicit_call_still_taints_when_it_shadows_a_project_unit() {
        // The transitive trap, and the single reason this port's bit is not the
        // direct one: `Kernel#format` is a real row with `effects: []`, so the
        // site has no taint of its own — but an unqualified name resolves
        // against self's ancestry first, and the project's own `format` is not
        // exhaustive. `harness/effects-corpus/06_edge` is the corpus half.
        let shadowed = bits(
            "class C\n def calls\n  format(\"a\")\n end\n def format(s)\n  s.render\n end\nend",
        );
        assert!(!shadowed["C#calls"].0);
        assert_eq!(shadowed["C#calls"].1, vec![cause(UNRESOLVED_SELF_CALL, Some("format"))]);

        // …and the same call in a project that does NOT define `format` stays
        // exhaustive. Without this control the rule would read as "every
        // catalogued implicit call taints", which is the useless suppression.
        let plain = bits("class C\n def calls\n  format(\"a\")\n end\nend");
        assert_eq!(plain["C#calls"], (true, vec![]));

        // A LITERAL reflective send is the third edge source and reads the same
        // way (`unit_scan.rb:476`).
        let reflective =
            bits("class C\n def calls\n  send(:target)\n end\n def target\n  x.y\n end\nend");
        assert!(!reflective["C#calls"].0);
        assert_eq!(reflective["C#calls"].1, vec![cause(UNRESOLVED_SELF_CALL, Some("target"))]);

        // The edge is keyed on the SELECTOR alone, so a singleton unit of the
        // same name is a target too — deliberately a superset of what the
        // propagator would resolve, which is the sound direction.
        let singleton =
            bits("class C\n def calls\n  format(\"a\")\n end\nend\nclass D\n def self.format(s)\n  s.render\n end\nend");
        assert!(!singleton["C#calls"].0);
    }

    #[test]
    fn causes_are_deduplicated_and_sorted_and_stay_inside_upstreams_enum() {
        const ALL: &[&str] = &[
            "dynamic-receiver",
            "dynamic-send",
            "method-missing",
            "unresolved-self-call",
            "opaque-callable",
            "unknown-ownership",
            "plugin-attribution",
            "template-not-analysed",
            "collector-error",
            "budget",
        ];
        let (exhaustive, causes) = taint(
            "  list.one\n  payload.two\n  helper(1)\n  helper(2)\n  other(3)\n  \
             list.send(path)\n  binding",
        );
        assert!(!exhaustive);
        assert_eq!(
            causes,
            vec![
                cause(DYNAMIC_RECEIVER, None),
                cause(DYNAMIC_SEND, None),
                cause(OPAQUE_CALLABLE, None),
                cause(UNRESOLVED_SELF_CALL, Some("helper")),
                cause(UNRESOLVED_SELF_CALL, Some("other")),
            ],
            "de-duplicated, and sorted by [cause, detail] as `summary.rb:143` sorts"
        );
        for (name, _) in &causes {
            assert!(ALL.contains(&name.as_str()), "{name} is outside TaintCause::ALL");
        }
    }

    #[test]
    fn the_bit_is_joined_across_reopenings() {
        // Upstream's `Summary#join` ANDs it and unions the causes
        // (`summary.rb:89`), so one tainted reopening taints the key.
        let found = bits(
            "class C\n def m\n  puts 1\n end\nend\nclass C\n def m(thing)\n  thing.other\n end\nend",
        );
        assert_eq!(found["C#m"], (false, vec![cause(DYNAMIC_RECEIVER, None)]));
    }

    #[test]
    fn a_unit_key_splits_into_its_selector_at_the_first_separator() {
        assert_eq!(selector_of("C#m"), "m");
        assert_eq!(selector_of("C.m"), "m");
        assert_eq!(selector_of("Outer::Inner#deep"), "deep");
        assert_eq!(selector_of("<toplevel>#m"), "m");
        assert_eq!(selector_of("C#[]="), "[]=");
        assert_eq!(selector_of("C#respond_to_missing?"), "respond_to_missing?");
    }
}
