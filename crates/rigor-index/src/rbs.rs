//! Real RBS-backed core data: parse the Ruby `core/*.rbs` set with the
//! `ruby-rbs` crate (parser only — ADR-0004), extract per-class method tables
//! (return class + arity envelope) and the super/include graph, then flatten an
//! ancestor chain so method existence is decided over the full linearization.
//!
//! The signature set is **vendored and embedded at build time** (ADR-0007): the
//! whole `core/` ⊕ the `DEFAULT_LIBRARIES` stdlib closure is copied under
//! `vendor/rbs/`, `build.rs` emits `$OUT_DIR/embedded_rbs.rs` (the
//! [`EMBEDDED_RBS`] `(path, contents)` table), and [`CoreData::load`] ingests
//! those bytes by default — no runtime filesystem dependency on a local rbs gem.
//! `RIGOR_RBS_CORE_DIR` remains an override seam (ADR-0007 / audit-R2): when set,
//! the loader reads from that directory at runtime exactly as before, for
//! out-of-band stdlib-RBS refreshes.
//!
//! Falls back to a hardcoded stub only in the degenerate case (embedded set
//! empty / override dir absent or unparsable), so the crate never panics.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ruby_rbs::node::{
    parse, AliasKind, ClassNode, MethodDefinitionKind, ModuleNode, Node,
};

// The build-time-embedded RBS signature set: `EMBEDDED_RBS: &[(&str, &str)]`,
// one `(relative-path, file-contents)` entry per vendored `.rbs`, in
// deterministic sorted-by-path order (see `build.rs`). The ingest is
// order-independent for class membership, so any total order yields the same
// index; sorted is chosen for reproducibility.
include!(concat!(env!("OUT_DIR"), "/embedded_rbs.rs"));

/// The stdlib libraries loaded on top of `core/` — the reference's
/// `DEFAULT_LIBRARIES` (`Rigor::Environment::DEFAULT_LIBRARIES`). Each name maps
/// to `<RBS_ROOT>/stdlib/<lib>/0/*.rbs`. A lib whose dir is absent is skipped
/// silently (e.g. `prism` / `rbs` ship RBS with their own gems, not in the rbs
/// stdlib tree). Loading these matches the reference's default RBS universe so a
/// stdlib reopen like `class Hash ... def to_json` is in scope (no false
/// `call.undefined-method` on `h.to_json`).
const DEFAULT_LIBRARIES: &[&str] = &[
    "pathname", "optparse", "json", "yaml", "fileutils", "tempfile", "tmpdir",
    "stringio", "forwardable", "digest", "securerandom",
    "uri", "logger", "date",
    "pp", "delegate", "observable", "abbrev", "find", "tsort", "singleton",
    "shellwords", "benchmark", "base64", "did_you_mean",
    "monitor", "mutex_m", "timeout",
    "open3", "erb", "etc", "ipaddr", "bigdecimal", "bigdecimal-math",
    "prettyprint", "random-formatter", "time", "open-uri", "resolv",
    "csv", "pstore", "objspace", "io-console", "cgi", "cgi-escape",
    "strscan",
    "prism", "rbs",
];

/// The original runtime core-RBS directory (rbs-4.0.3 gem under mise). ADR-0007
/// replaced this default with the vendored, build-time-[`EMBEDDED_RBS`] set, so
/// it is no longer on the default load path — kept only as documentation of the
/// source the vendored tree was generated from. `RIGOR_RBS_CORE_DIR` (any dir)
/// is the live override seam; this constant is not read at runtime.
#[allow(dead_code)]
const DEFAULT_CORE_DIR: &str = "/Users/megurine/.local/share/mise/installs/ruby/4.0.5/\
lib/ruby/gems/4.0.0/gems/rbs-4.0.3/core";

/// An arity envelope `(min, max)`: `min` is the smallest required-positional
/// count across overloads; `max` is `None` (variadic) when any overload takes a
/// positional rest, else the largest required+optional count.
type Arity = (usize, Option<usize>);

/// The subtyping relation of two class names, mirroring the reference's
/// `class_ordering` result atoms (`:equal` / `:subclass` / `:superclass` /
/// `:disjoint` / `:unknown`). See [`CoreData::class_ordering`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassOrdering {
    /// The two names denote the same class.
    Equal,
    /// `lhs` is a proper descendant of `rhs` (its ancestry includes `rhs`).
    Subclass,
    /// `lhs` is a proper ancestor of `rhs` (`rhs`'s ancestry includes `lhs`).
    Superclass,
    /// Neither is an ancestor of the other (both chains fully known).
    Disjoint,
    /// The relation cannot be proven (an unloaded class or an incomplete chain).
    Unknown,
}

/// Sentinel stored in [`ClassEntry::block_returns`] for a block overload whose
/// RBS return type is `self` (e.g. `Array#each { } -> self`, `Kernel#tap { } ->
/// self`). At lookup time it resolves to the RECEIVER's own class name (the
/// value `method_return_with_block` was queried with), so `x.tap { } : x` and
/// `arr.each { } : arr`. A distinct value (not a real class name) so it can
/// never collide with an actual `ClassInstanceType` return.
const SELF_RETURN: &str = "\0self";

/// Per-class data extracted from RBS: its instance methods (name -> resolved
/// return class + arity), its direct superclass, and its included modules.
#[derive(Default, Clone)]
struct ClassEntry {
    /// `method name -> (return class name if resolvable, arity envelope,
    /// nilable)`. `nilable` is `true` iff the RBS return is an `Optional`
    /// (`String?`) over a resolvable `ClassInstanceType` — i.e. the method
    /// yields `C | nil`. It is `false` for a plain non-optional return, and
    /// is meaningful ONLY when the return class name is `Some` (a `None`
    /// return collapses to Dynamic and carries no nilability). Consumed solely
    /// by `call.possible-nil-receiver` via [`CoreData::method_return_nilable`];
    /// no existing rule reads it (return-class / arity stay as before).
    methods: HashMap<&'static str, (Option<&'static str>, Arity, bool)>,
    /// `method name -> block-overload return class name`, populated ONLY for
    /// methods that declare a block-bearing overload whose return is a
    /// resolvable concrete class (a `ClassInstanceType` like `Hash#filter { }
    /// -> ::Hash[K,V]` / `Enumerable#map { } -> ::Array[U]`) or the literal
    /// receiver itself (a `self` return like `Array#each { } -> self` /
    /// `Kernel#tap { } -> self`). The latter is stored as the sentinel
    /// [`SELF_RETURN`] and resolved to the receiver's own class at lookup time.
    /// Mirrors the reference's `block_required: true` overload selection
    /// (`rbs_dispatch.rb`): a block at the call site picks the block overload,
    /// and ITS return type is what the call yields. Methods with no block
    /// overload, or whose block overload returns a generic/union/void/unknown
    /// shape, are simply absent here (⇒ the block call stays Dynamic / silent).
    block_returns: HashMap<&'static str, &'static str>,
    /// Singleton (class-level) methods `def self.x` (and the singleton half of
    /// `def self?.x`). Keyed by name -> `(resolved return class, arity envelope)`.
    /// The singleton class inherits down the SUPERCLASS chain, so resolving a
    /// class method walks these maps up `superclass`. The return-class slot
    /// mirrors the instance `methods` table's resolution discipline (a single
    /// bare concrete `ClassInstanceType` ⇒ `Some(name)`, else `None`) and is read
    /// ONLY by the sig-gen-only [`Self::declared_singleton_return`]; the existence
    /// check ([`Self::class_has_singleton_method`]) uses just the key set.
    singleton_methods: HashMap<&'static str, (Option<&'static str>, Arity)>,
    /// Instance-method aliases `new_name -> old_name` (RBS `alias size length`).
    /// The alias target is resolved at lookup time so `new_name` inherits
    /// `old_name`'s existence / return type / arity (the old name may live on
    /// the same class or anywhere up the ancestor chain).
    aliases: HashMap<&'static str, &'static str>,
    /// Singleton (class-method) aliases `new -> old` (RBS `alias self.pwd
    /// self.getwd`, `alias self.escape self.shellescape`). Resolved at singleton
    /// lookup time over the singleton chain. These are COMMON in core/stdlib
    /// (File/Dir/Shellwords/…); omitting them makes the singleton surface look
    /// complete-but-missing-`new` and witnesses a real class method as absent.
    singleton_aliases: HashMap<&'static str, &'static str>,
    /// Direct superclass name, if any (`None` ⇒ implicit `Object`, except the
    /// roots which are seeded explicitly).
    superclass: Option<&'static str>,
    /// `true` when this name was declared as a `module` (not a `class`) in RBS —
    /// the analogue of the reference's `Environment#rbs_module?`. Read ONLY by
    /// `call.raise-non-exception`'s instance path (a value typed as a module
    /// includer could be an Exception at runtime, so it must stay silent). Set on
    /// the module ingest, OR-merged across reopens.
    is_module: bool,
    /// Included module names (in source order).
    includes: Vec<&'static str>,
    /// `extend`ed module names (in source order). An `extend M` directive folds
    /// `M`'s INSTANCE methods into THIS class/module's SINGLETON surface (the
    /// class object gains them as class methods — e.g. `SecureRandom` does
    /// `extend Random::Formatter`, so `SecureRandom.hex` is a real class method).
    extends: Vec<&'static str>,
}

/// Which signature source the loaded [`CoreData`] was built from. Surfaced by
/// `rigor doctor` so the embedded-vs-override coverage state is observable
/// (audit-R1 / ADR-0007): the standalone default is [`Embedded`](RbsSource::Embedded),
/// the out-of-band refresh seam is [`Override`](RbsSource::Override), and
/// [`Stub`](RbsSource::Stub) is the degenerate "nothing parsed" fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RbsSource {
    /// The build-time-embedded vendored RBS set (the standalone default — no
    /// runtime filesystem dependency).
    Embedded,
    /// The `RIGOR_RBS_CORE_DIR` override directory was set AND usable; the path
    /// carried is the dir that was ingested.
    Override(String),
    /// Neither the override nor the embedded set yielded any classes — the
    /// hardcoded conservative stub.
    Stub,
}

/// The loaded core data backing [`crate::CoreIndex`] and the free
/// `method_return` / `method_arity` functions.
pub struct CoreData {
    /// Which signature source this data was built from (embedded / override /
    /// stub). Informational only — used by `rigor doctor` to report the active
    /// RBS coverage source.
    source: RbsSource,
    /// `class name -> entry`. Keys are `&'static str` (leaked once at load) so
    /// resolved return-class names can flow out as `&'static str`.
    classes: HashMap<&'static str, ClassEntry>,
    /// Short names that were declared at GENUINE top level (empty namespace) in
    /// at least one declaration — e.g. `class Time` ⇒ `"Time"`, but a name that
    /// only ever appears namespaced (`class Process::Status`) is NOT here. Used
    /// by [`Self::knows_toplevel_class`] so an ambiguous short name shared by a
    /// project class and a namespaced stdlib class is not falsely treated as a
    /// known top-level core class (defect 2).
    toplevel_classes: HashSet<&'static str>,
    /// Class names INTRODUCED by project-`sig/` ingestion (ADR-0033) — those the
    /// project's own signatures declared that no bundled (core/stdlib/plugin) RBS
    /// already carried. The dispatch rules treat these as AUTHORITATIVE for
    /// witnessing an `X.new` instance method typo (the reference witnesses a
    /// project-sig class but stays lenient on a bundled stdlib/gem class like
    /// `Pathname`), so this set is the provenance gate that keeps the two apart.
    /// Empty when no `sig/` was ingested.
    project_sig_classes: HashSet<&'static str>,
}

impl CoreData {
    /// Build from the real RBS universe (the reference's default: ALL of
    /// `core/*.rbs` ⊕ the `DEFAULT_LIBRARIES` stdlib set). Never panics: any
    /// per-file parse error is skipped, and stdlib reopens of core classes
    /// (`class Hash ...`) merge into the existing entry (see [`Builder::merge`]).
    ///
    /// **Default:** ingest the build-time-[`EMBEDDED_RBS`] vendored set — no
    /// runtime filesystem dependency (ADR-0007). **Override:** if
    /// `RIGOR_RBS_CORE_DIR` is set, read from that directory at runtime exactly
    /// as before (the out-of-band stdlib-RBS refresh seam, audit-R2): the WHOLE
    /// dir plus the `DEFAULT_LIBRARIES` stdlib closure rooted at `<dir>/../stdlib`.
    /// The embedded path feeds the SAME bytes to the SAME parser as the override
    /// path ([`ingest_rbs_source`]), so the resulting index is byte-identical to
    /// what the override dir produced when it holds the same signatures.
    pub fn load() -> Self {
        Self::load_with_plugins(&[])
    }

    /// Build the core data, THEN ingest each bundled plugin's RBS on top
    /// (ADR-25, config-gated). With `plugins` empty this is byte-identical to
    /// [`Self::load`] — the default no-config path is unchanged.
    ///
    /// The core source is resolved exactly as [`Self::load`] does (the
    /// `RIGOR_RBS_CORE_DIR` override if set, else the embedded vendored set),
    /// and each plugin's `(name, contents)` entries are then fed to the SAME
    /// [`ingest_rbs_source`] / `ruby-rbs` parser. The existing [`Builder::merge`]
    /// reopen-union folds each plugin's reopened `class String ... def squish`
    /// into the EXISTING `String` entry, so the plugin selectors join the core
    /// surface — byte-identical to feeding the reference's bundled RBS through
    /// the core path (the zero-FP keystone). Unknown plugin ids are filtered out
    /// by the caller ([`crate::CoreIndex::with_plugins`]); here every entry is a
    /// real bundled payload.
    pub fn load_with_plugins(plugins: &[&crate::plugins::BundledPlugin]) -> Self {
        Self::load_for_project(plugins, &[])
    }

    /// Build the core data + bundled plugins (as [`Self::load_with_plugins`]),
    /// THEN ingest each project signature directory's `*.rbs` on top (ADR-0033).
    /// With `sig_dirs` empty this is byte-identical to [`Self::load_with_plugins`],
    /// so the no-config / no-`sig/` path is unchanged.
    ///
    /// The project sig is folded through the SAME native `ruby-rbs` parser and
    /// the SAME reopen-union [`Builder::merge`] as core + plugin RBS — no Ruby
    /// runtime, no new format (the ADR-0007 project-signature leg). A project's
    /// own classes thereby join the loaded set, so [`Self::knows_class`] (the
    /// dispatch-rule gate, the analogue of the reference's `rbs_class_known?`)
    /// witnesses them. A `sig_dir` that doesn't exist on disk is inert
    /// ([`ingest_rbs_dir`] skips a non-directory). User-authored RBS degrades
    /// soundly: a per-file parse failure drops only that file's declarations
    /// (ADR-0016), and there is no global resolve pass that a malformed file
    /// could collapse (ADR-0033).
    pub fn load_for_project(
        plugins: &[&crate::plugins::BundledPlugin],
        sig_dirs: &[PathBuf],
    ) -> Self {
        // 1) Resolve the core source (override dir, else embedded), folding into a
        //    fresh builder — the SAME logic [`Self::load`] previously inlined.
        let mut builder = Builder::default();
        let mut source = RbsSource::Embedded;
        if let Ok(dir) = std::env::var("RIGOR_RBS_CORE_DIR") {
            let dir = PathBuf::from(dir);
            if Self::ingest_dir_into(&mut builder, &dir) {
                source = RbsSource::Override(dir.display().to_string());
            }
            // Override set but unusable (absent / nothing parsed): fall through
            // to the embedded default rather than failing.
        }
        if source == RbsSource::Embedded {
            ingest_embedded(&mut builder);
        }

        // 2) Ingest each bundled plugin's RBS on top of whichever core source was
        //    used. The reopen-union merge handles classes already present.
        for plugin in plugins {
            for (name, contents) in plugin.rbs {
                ingest_rbs_source(&mut builder, name, contents);
            }
        }

        // 3) Ingest the project's own `sig/` RBS on top of core + plugin
        //    (ADR-0033). Same parser, same reopen-union merge. Snapshot the
        //    class keyset first so the names the project sig INTRODUCES (vs
        //    reopens of an already-bundled class) are recorded as project-sig
        //    provenance — the witnessing gate for `X.new` typos.
        let pre_sig: HashSet<&'static str> = builder.classes.keys().copied().collect();
        for dir in sig_dirs {
            ingest_rbs_dir(&mut builder, dir);
        }
        let project_sig_classes: HashSet<&'static str> = builder
            .classes
            .keys()
            .copied()
            .filter(|k| !pre_sig.contains(k))
            .collect();

        let (classes, toplevel_classes) = builder.finish();
        if !classes.is_empty() {
            return Self { source, classes, toplevel_classes, project_sig_classes };
        }
        // Fallback: nothing parsed (shouldn't happen) ⇒ hardcoded stub. The stub
        // carries no plugin selectors, which stays conservative (zero-FP).
        Self::stub()
    }

    /// The runtime-filesystem ingest path (the `RIGOR_RBS_CORE_DIR` override):
    /// fold the WHOLE `dir` plus the `DEFAULT_LIBRARIES` stdlib closure rooted at
    /// `<dir>/../stdlib` INTO `builder`. Returns `true` when the dir exists and
    /// something parsed (so the caller knows the override is usable), `false` when
    /// the dir is absent or nothing parsed (caller then falls back to the embedded
    /// default). Folding into a passed-in builder (rather than building `Self`)
    /// lets [`Self::load_with_plugins`] ingest plugin RBS on top of the SAME
    /// builder. This is the same core/stdlib logic the default previously ran.
    fn ingest_dir_into(builder: &mut Builder, dir: &std::path::Path) -> bool {
        if !dir.is_dir() {
            return false;
        }

        // 1) The WHOLE core dir (~62 files), not a curated subset — so every
        //    core class + its full ancestor chain is loaded.
        ingest_rbs_dir(builder, dir);

        // 2) The DEFAULT_LIBRARIES stdlib set, rooted at `<core>/../stdlib`,
        //    transitively closed over each lib's `manifest.yaml` deps (the
        //    reference resolves these — e.g. `yaml` ⇒ `psych` ships the
        //    `Object#to_yaml` reopen, `csv` ⇒ `stringio`). Each lib is
        //    `stdlib/<lib>/0/*.rbs`; an absent lib (e.g. `prism`/`rbs`, or a
        //    dep like `socket` not in this tree) is skipped silently.
        if let Some(root) = dir.parent() {
            let stdlib = root.join("stdlib");
            let mut loaded: HashSet<String> = HashSet::new();
            let mut queue: Vec<String> =
                DEFAULT_LIBRARIES.iter().map(|s| s.to_string()).collect();
            while let Some(lib) = queue.pop() {
                if !loaded.insert(lib.clone()) {
                    continue;
                }
                let lib_dir = stdlib.join(&lib).join("0");
                if !lib_dir.is_dir() {
                    continue; // ships RBS elsewhere / not in this tree ⇒ skip.
                }
                ingest_rbs_dir(builder, &lib_dir);
                // Enqueue manifest dependencies (transitive closure).
                for dep in manifest_deps(&lib_dir.join("manifest.yaml")) {
                    if !loaded.contains(&dep) {
                        queue.push(dep);
                    }
                }
            }
        }

        // "Usable" means the core dir yielded at least one class. The builder may
        // already hold classes from a prior fold, but the override is only ever
        // ingested into a FRESH builder, so non-empty ⇒ this dir parsed.
        !builder.classes.is_empty()
    }

    /// Whether the class is in the loaded set.
    pub fn knows_class(&self, class_name: &str) -> bool {
        self.classes.contains_key(class_name)
    }

    /// Whether `class_name` was declared at GENUINE top level (empty namespace)
    /// in at least one RBS declaration. Conservative companion to
    /// [`Self::knows_class`]: returns `true` ONLY for names that genuinely have a
    /// top-level declaration. A name that exists in the index solely because a
    /// namespaced/nested decl (`class Process::Status`) was registered by its
    /// short key (`"Status"`) returns `false`, so a project class sharing that
    /// short name is not falsely resolved to the namespaced stdlib class
    /// (defect 2). Instance-method behavior and `knows_class` are unchanged.
    pub fn knows_toplevel_class(&self, class_name: &str) -> bool {
        self.toplevel_classes.contains(class_name)
    }

    /// Walk the full flattened ancestor chain; a method is present if ANY
    /// ancestor defines it directly OR via an instance `alias`. Conservative
    /// gate: if the chain is not fully loaded (an ancestor missing from the
    /// set), return `true` ("assume present") so absence is never falsely
    /// witnessed. Absence (`false`) is only returned when every ancestor is
    /// loaded and none defines (or aliases) the method.
    pub fn class_has_method(&self, class_name: &str, method: &str) -> bool {
        if !self.classes.contains_key(class_name) {
            return false;
        }
        let (chain, complete) = self.ancestors(class_name);
        if self.lookup_on_chain(&chain, method).is_some() {
            return true;
        }
        // Not found across the chain. Only witness absence if the chain is
        // fully loaded; otherwise assume present (zero false positive).
        !complete
    }

    /// Whether `name` was declared as a `module` in RBS (the analogue of the
    /// reference `Environment#rbs_module?`). `false` for a class or an unknown
    /// name. Read only by `call.raise-non-exception`'s instance path.
    pub fn is_module(&self, name: &str) -> bool {
        self.classes.get(name).is_some_and(|e| e.is_module)
    }

    /// The subtyping relation of two RBS-known class names, a faithful port of
    /// the reference `Environment::RbsHierarchy#class_ordering`
    /// (`environment/rbs_hierarchy.rb`): `Equal` when the (namespace-stripped)
    /// names match; `Unknown` when either class is unloaded; else `Subclass` when
    /// `lhs`'s ancestry includes `rhs`, `Superclass` when `rhs`'s ancestry
    /// includes `lhs`, else `Disjoint`.
    ///
    /// One conservative deviation from the reference (whose RBS `ancestors`
    /// always yields the COMPLETE linearization): rigor-rs's ancestor walk can be
    /// incomplete when some referenced ancestor is not loaded. When the two
    /// classes are UNRELATED (neither contains the other) but a chain is
    /// incomplete, this returns `Unknown` rather than `Disjoint`, so the caller
    /// never proves disjointness from a partial chain. A positive
    /// `Subclass`/`Superclass` witness stands regardless of completeness (finding
    /// the target IS the proof). For the raise rule's Exception/String targets the
    /// vendored chains are complete, so this matches the reference exactly.
    pub fn class_ordering(&self, lhs: &str, rhs: &str) -> ClassOrdering {
        let lhs = lhs.strip_prefix("::").unwrap_or(lhs);
        let rhs = rhs.strip_prefix("::").unwrap_or(rhs);
        if lhs == rhs {
            return ClassOrdering::Equal;
        }
        if !self.classes.contains_key(lhs) || !self.classes.contains_key(rhs) {
            return ClassOrdering::Unknown;
        }
        let (lhs_anc, lhs_complete) = self.ancestors(lhs);
        let (rhs_anc, rhs_complete) = self.ancestors(rhs);
        if lhs_anc.contains(&rhs) {
            return ClassOrdering::Subclass;
        }
        if rhs_anc.contains(&lhs) {
            return ClassOrdering::Superclass;
        }
        if lhs_complete && rhs_complete {
            ClassOrdering::Disjoint
        } else {
            ClassOrdering::Unknown
        }
    }

    /// Enumerate every INSTANCE method name callable on `class_name` — its own
    /// methods plus those inherited over the flattened ancestor chain (superclass
    /// and included modules), plus instance `alias` names. Sorted and deduped.
    /// Empty when the class is unknown. Used by LSP completion (§12); unlike the
    /// diagnostic predicates this is advisory, so it enumerates the full known
    /// surface without a completeness gate.
    pub fn instance_method_names(&self, class_name: &str) -> Vec<&'static str> {
        if !self.classes.contains_key(class_name) {
            return Vec::new();
        }
        let (chain, _complete) = self.ancestors(class_name);
        let mut set: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                set.extend(entry.methods.keys().copied());
                set.extend(entry.aliases.keys().copied());
            }
        }
        set.into_iter().collect()
    }

    /// Enumerate every SINGLETON (class-object) method name callable on the class
    /// object `class_name`: the `def self.x` methods up its superclass chain, the
    /// instance methods of every `extend`ed module, singleton aliases, and the
    /// instance methods of the base classes the class object is itself an instance
    /// of (`Class`/`Module`/`Object`/`Kernel`/`BasicObject`). Sorted + deduped.
    /// Mirrors the surface of [`Self::class_has_singleton_method`]; advisory
    /// (no completeness gate), for LSP completion on a `Singleton` receiver.
    pub fn singleton_method_names(&self, class_name: &str) -> Vec<&'static str> {
        if !self.classes.contains_key(class_name) {
            return Vec::new();
        }
        let mut set: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();

        // The singleton superclass chain (the singleton class inherits down
        // `superclass`); on each, own `def self.x` + singleton aliases + the
        // instance methods of every `extend`ed module.
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else { break };
            if !seen.insert(key) {
                break; // cycle guard.
            }
            set.extend(entry.singleton_methods.keys().copied());
            set.extend(entry.singleton_aliases.keys().copied());
            for &module in &entry.extends {
                for m in self.instance_method_names(module) {
                    set.insert(m);
                }
            }
            cur = entry.superclass;
        }

        // The class object is itself an instance of `Class` (→ `Module` →
        // `Object` → `Kernel`/`BasicObject`), so those instance methods respond
        // on it (`.new`, `.name`, `.tap`, …).
        for base in ["Class", "Module", "Object", "Kernel", "BasicObject"] {
            for m in self.instance_method_names(base) {
                set.insert(m);
            }
        }
        set.into_iter().collect()
    }

    /// Whether the class OBJECT `class_name` responds to a singleton (class)
    /// method `method`. Conservative (zero false positive): returns `true`
    /// ("present ⇒ stay silent") unless the full singleton surface is known to
    /// lack it.
    ///
    /// The singleton surface is the union of:
    ///   (a) `class_name`'s own `def self.x` methods PLUS those of every
    ///       superclass up its chain (the singleton class inherits down the
    ///       superclass chain), AND the INSTANCE methods of every module each of
    ///       those classes `extend`s (`extend M` folds `M`'s instance methods
    ///       into the class object — e.g. `SecureRandom extend Random::Formatter`
    ///       makes `SecureRandom.hex` a class method); AND
    ///   (b) the INSTANCE methods of `Class`/`Module`/`Object`/`Kernel`/
    ///       `BasicObject` — the class object is itself an instance of `Class`,
    ///       so e.g. `Time.name`, `Time.new`, `Time.tap`, `Time.instance_methods`
    ///       are all present and must NOT be witnessed absent.
    ///
    /// Absence (`false`) is returned ONLY when the whole surface is known: the
    /// class is loaded, its superclass chain is COMPLETE, all five base classes
    /// are loaded, and none of (a)/(b) defines `method`. If the class is unknown,
    /// its chain is incomplete, or any base class is missing ⇒ `true`.
    pub fn class_has_singleton_method(&self, class_name: &str, method: &str) -> bool {
        // Unknown class ⇒ stay silent.
        if !self.classes.contains_key(class_name) {
            return true;
        }
        // (a) Own + inherited singleton methods, walking the superclass chain.
        let (found, complete) = self.singleton_lookup(class_name, method);
        if found {
            return true;
        }
        // (b) Instance methods of the class object's own ancestry (it is a
        //     `Class`) — `new`, `name`, etc. from Class/Module/Object/Kernel/
        //     BasicObject. Their presence is also a completeness precondition.
        let (bases_found, bases_loaded) = self.singleton_bases_lookup(method);
        if bases_found {
            return true;
        }
        // Not found anywhere. Witness absence ONLY when the whole surface is
        // known: the singleton superclass chain is complete AND all five base
        // classes are loaded. Otherwise stay silent.
        if complete && bases_loaded {
            return false;
        }
        true
    }

    /// Whether `method` is an instance method of the class object's own ancestry
    /// — the five base classes a class object is/inherits (Class, Module, Object,
    /// Kernel, BasicObject) — plus whether all five are loaded (a completeness
    /// precondition). Shared by [`Self::class_has_singleton_method`] and the
    /// singleton-alias resolution, so an alias whose TARGET is a base method
    /// (`alias self.compile self.new`, where `new` is `Class#new`) resolves.
    fn singleton_bases_lookup(&self, method: &str) -> (bool, bool) {
        const BASES: [&str; 5] = ["Class", "Module", "Object", "Kernel", "BasicObject"];
        let mut loaded = true;
        let mut found = false;
        for base in BASES {
            if !self.classes.contains_key(base) {
                loaded = false;
                continue;
            }
            let (chain, _) = self.ancestors(base);
            if self.lookup_on_chain(&chain, method).is_some() {
                found = true;
            }
        }
        (found, loaded)
    }

    /// Walk `class_name` and its superclass chain collecting OWN singleton
    /// methods AND the instance methods of every `extend`ed module on the way.
    /// Returns `(found, complete)`: `found` is whether `method` is on that
    /// surface; `complete` is `false` if a referenced superclass OR any
    /// `extend`ed module is not in the loaded set (surface truncated), mirroring
    /// the completeness notion of [`Self::ancestors`]. The incompleteness from a
    /// missing extended module is the critical no-false-positive guard: if e.g.
    /// `Random::Formatter` (extended by `SecureRandom`) is not loaded, the
    /// surface is unknown ⇒ caller stays silent.
    fn singleton_lookup(&self, class_name: &str, method: &str) -> (bool, bool) {
        // Gather the singleton superclass chain (the singleton class inherits
        // down `superclass`), tracking completeness: a referenced superclass or
        // `extend`ed module that isn't loaded truncates the surface.
        let mut chain: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut complete = true;
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else {
                complete = false; // referenced superclass not loaded.
                break;
            };
            if !seen.insert(key) {
                break; // Defensive: cycle guard.
            }
            chain.push(key);
            for &module in &entry.extends {
                if !self.classes.contains_key(module) {
                    complete = false;
                }
            }
            cur = entry.superclass;
        }
        let found = self.singleton_on_chain(&chain, method, 0);
        (found, complete)
    }

    /// Whether `method` is on the class object's surface across the singleton
    /// `chain`: a direct `def self.x` on any class, an INSTANCE method of any
    /// `extend`ed module, or a singleton ALIAS resolving (bounded) to one of
    /// those. Existence-only; completeness is computed by the caller.
    fn singleton_on_chain(&self, chain: &[&'static str], method: &str, depth: usize) -> bool {
        // (1) A direct singleton method on any class in the chain.
        for &anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if entry.singleton_methods.contains_key(method) {
                    return true;
                }
            }
        }
        // (2) An `extend`ed module's INSTANCE method (extend folds M's instance
        //     methods into the class object's singleton surface).
        for &anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                for &module in &entry.extends {
                    if self.classes.contains_key(module) {
                        let (mod_chain, _) = self.ancestors(module);
                        if self.lookup_on_chain(&mod_chain, method).is_some() {
                            return true;
                        }
                    }
                }
            }
        }
        // (3) A singleton alias `method -> old`, resolved over the same chain.
        //     Bounded to defend against a pathological alias cycle.
        if depth >= 16 {
            return false;
        }
        for &anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&old) = entry.singleton_aliases.get(method) {
                    // The alias target may live on the singleton chain OR on the
                    // base-class surface (`alias self.compile self.new`, where
                    // `new` is `Class#new`), so check both.
                    if self.singleton_on_chain(chain, old, depth + 1)
                        || self.singleton_bases_lookup(old).0
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Resolve a method's return class over the ancestor chain (first defining
    /// ancestor wins), resolving through `alias` definitions. `None` if the
    /// return is not a known concrete class (or the method is unknown).
    pub fn method_return(&self, class_name: &str, method: &str) -> Option<&'static str> {
        let (chain, _) = self.ancestors(class_name);
        self.lookup_on_chain(&chain, method).and_then(|(ret, _, _)| ret)
    }

    /// Resolve `class_name#method` to `(return class, nilable)` over the
    /// ancestor chain — the nil-aware variant of [`Self::method_return`], used
    /// ONLY by `call.possible-nil-receiver`. `nilable` is `true` iff the RBS
    /// return is an `Optional` (`String?` ⇒ `(String, true)`); a plain return
    /// is `(C, false)`. `None` when the return is not a resolvable concrete
    /// class (so the nil-receiver pass never mints `T | nil` from a Dynamic /
    /// unknown return). Nilability rides the same alias resolution as the class
    /// (`String#size -> length` inherits `length`'s nilability).
    pub fn method_return_nilable(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<(&'static str, bool)> {
        let (chain, _) = self.ancestors(class_name);
        self.lookup_on_chain(&chain, method)
            .and_then(|(ret, _, nilable)| ret.map(|c| (c, nilable)))
    }

    /// Resolve the RETURN class of `class_name#method` **when called with a
    /// block**, over the flattened ancestor chain — the block-overload return
    /// the reference selects via `block_required: true` (`rbs_dispatch.rb`).
    ///
    /// Returns `Some(class)` only when the method (or an alias of it, e.g.
    /// `Hash#select -> filter`) declares a block-bearing overload whose return
    /// is a resolvable concrete class. A `self`-returning block overload
    /// (`Array#each { } -> self`, `Kernel#tap { } -> self`) resolves to
    /// `class_name` itself (the receiver's own class). `None` ⇒ the block form
    /// isn't precisely modeled (no block overload, or a generic/union/void/
    /// unknown return) ⇒ the caller declines to `Dynamic` (zero-FP).
    pub fn method_return_with_block(&self, class_name: &str, method: &str) -> Option<&'static str> {
        let (chain, _) = self.ancestors(class_name);
        let ret = self.lookup_block_return_on_chain(&chain, method, 0)?;
        if ret == SELF_RETURN {
            // `self` block return ⇒ the receiver type itself. Hand back the
            // receiver's interned `&'static` name (matching the stored key) only
            // when the index actually models the class, so the result
            // round-trips to a Nominal the rules can witness against.
            self.classes.get_key_value(class_name).map(|(&k, _)| k)
        } else {
            Some(ret)
        }
    }

    /// Walk the chain for `method`'s block-overload return, resolving instance
    /// `alias`es exactly like [`lookup_on_chain_depth`] (so `Hash#select`, an
    /// `alias select filter`, inherits `filter`'s block return). The first
    /// ancestor that records a block return for `method` wins.
    fn lookup_block_return_on_chain(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<&'static str> {
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&ret) = entry.block_returns.get(method) {
                    return Some(ret);
                }
            }
        }
        if depth >= 16 {
            return None;
        }
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&old) = entry.aliases.get(method) {
                    if let Some(found) = self.lookup_block_return_on_chain(chain, old, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// Resolve a method's arity envelope over the ancestor chain (first defining
    /// ancestor wins), resolving through `alias` definitions. `None` if the
    /// method is unknown on the chain.
    pub fn method_arity(&self, class_name: &str, method: &str) -> Option<Arity> {
        let (chain, _) = self.ancestors(class_name);
        self.lookup_on_chain(&chain, method).map(|(_, arity, _)| arity)
    }

    // -- Sig-gen-only precise declared-return accessors (ADR-14 slice 10) ------
    //
    // These are NOT diagnostic predicates. `class_has_method` /
    // `class_has_singleton_method` deliberately "assume present" on an incomplete
    // ancestor chain (a diagnostic must never witness false absence), which
    // conflates *not declared* with *declared, return unresolvable*. sig-gen's
    // generation-time classification needs those apart: NotDeclared ⇒ emit
    // `# [new]`, Declared(unresolvable) ⇒ silently DROP. The three-valued
    // `Option<Option<&str>>` encoding carries that distinction, and these
    // accessors NEVER assume-present — an incomplete chain with the method absent
    // yields `Some(None)` (the conservative DROP), never `None`.

    /// Whether the flattened ancestor chain of `class` is fully loaded (every
    /// referenced ancestor is in the RBS set). **Sig-gen only.**
    pub fn chain_complete(&self, class: &str) -> bool {
        self.ancestors(class).1
    }

    /// **Sig-gen only — NOT a diagnostic predicate.** Precise three-valued
    /// declared INSTANCE-return lookup over the ancestor chain:
    /// - `None` ⇒ the method is not declared anywhere on a COMPLETE chain
    ///   (⇒ sig-gen emits `# [new]`);
    /// - `Some(None)` ⇒ declared (or the chain is incomplete, so a declaration
    ///   may exist upstream) but the return is not a single bare concrete class
    ///   (⇒ sig-gen DROPs, conservatively);
    /// - `Some(Some(c))` ⇒ declared, resolvable return class `c`.
    ///
    /// The return-class resolution is exactly [`Self::method_return`]'s (a single
    /// bare concrete `ClassInstanceType` across all overloads, else `None`).
    pub fn declared_instance_return(&self, class: &str, method: &str) -> Option<Option<&'static str>> {
        if !self.classes.contains_key(class) {
            return None;
        }
        let (chain, complete) = self.ancestors(class);
        match self.lookup_on_chain(&chain, method) {
            Some((ret, _, _)) => Some(ret),
            None if complete => None,
            None => Some(None),
        }
    }

    /// **Sig-gen only — NOT a diagnostic predicate.** The singleton counterpart
    /// of [`Self::declared_instance_return`], over the same surface
    /// [`Self::class_has_singleton_method`] checks: own `def self.x` up the
    /// superclass chain, every `extend`ed module's INSTANCE methods, and the
    /// INSTANCE methods of the five base classes (`Class`/`Module`/`Object`/
    /// `Kernel`/`BasicObject`) the class object is itself an instance of. A
    /// singleton ALIAS resolves as `Some(None)` (declared, return unresolved ⇒
    /// DROP) rather than being missed.
    pub fn declared_singleton_return(&self, class: &str, method: &str) -> Option<Option<&'static str>> {
        if !self.classes.contains_key(class) {
            return None;
        }
        // (a) own singleton methods up the superclass chain + extends' instance.
        let (ret_a, found_a, complete_a) = self.singleton_return_lookup(class, method);
        if found_a {
            return Some(ret_a);
        }
        // (b) the class object's own ancestry (it is a `Class`): the instance
        //     surface of the five base classes.
        let (ret_b, found_b, bases_loaded) = self.singleton_bases_return(method);
        if found_b {
            return Some(ret_b);
        }
        // Not found anywhere: a precise NotDeclared only when the whole surface
        // is known; otherwise the conservative DROP.
        if complete_a && bases_loaded {
            None
        } else {
            Some(None)
        }
    }

    /// Walk `class_name`'s singleton superclass chain resolving the return of the
    /// first `def self.x` / extended-module-instance / singleton-alias match.
    /// Returns `(return, found, complete)`; `complete` is `false` when a
    /// referenced superclass or extended module is not loaded. An alias match
    /// yields `(None, true, _)` (declared, unresolved ⇒ DROP).
    fn singleton_return_lookup(
        &self,
        class_name: &str,
        method: &str,
    ) -> (Option<&'static str>, bool, bool) {
        let mut chain: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut complete = true;
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else {
                complete = false;
                break;
            };
            if !seen.insert(key) {
                break;
            }
            chain.push(key);
            for &module in &entry.extends {
                if !self.classes.contains_key(module) {
                    complete = false;
                }
            }
            cur = entry.superclass;
        }
        // (1) direct `def self.x` on any class in the chain.
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&(ret, _)) = entry.singleton_methods.get(method) {
                    return (ret, true, complete);
                }
            }
        }
        // (2) an `extend`ed module's INSTANCE method's return.
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                for &module in &entry.extends {
                    if self.classes.contains_key(module) {
                        let (mod_chain, _) = self.ancestors(module);
                        if let Some((ret, _, _)) = self.lookup_on_chain(&mod_chain, method) {
                            return (ret, true, complete);
                        }
                    }
                }
            }
        }
        // (3) a singleton alias ⇒ declared, but the return is not resolved here
        //     ⇒ DROP (safer than mis-emitting `# [new]`).
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                if entry.singleton_aliases.contains_key(method) {
                    return (None, true, complete);
                }
            }
        }
        (None, false, complete)
    }

    /// The INSTANCE-method return of `method` on the class object's own ancestry
    /// (the five base classes), plus whether all five are loaded — the
    /// return-resolving twin of [`Self::singleton_bases_lookup`].
    fn singleton_bases_return(&self, method: &str) -> (Option<&'static str>, bool, bool) {
        const BASES: [&str; 5] = ["Class", "Module", "Object", "Kernel", "BasicObject"];
        let mut loaded = true;
        for base in BASES {
            if !self.classes.contains_key(base) {
                loaded = false;
                continue;
            }
            let (chain, _) = self.ancestors(base);
            if let Some((ret, _, _)) = self.lookup_on_chain(&chain, method) {
                return (ret, true, loaded);
            }
        }
        (None, false, loaded)
    }

    /// Find `method`'s `(return, arity)` on the flattened ancestor chain,
    /// resolving instance `alias`es. The first ancestor that defines `method`
    /// directly wins; otherwise, if some ancestor aliases `method -> old`, the
    /// lookup re-runs on the **same chain** for `old` (which may itself be an
    /// alias or live on a different ancestor — `String#size -> length`, both on
    /// `String`; an inherited alias resolves to an inherited target too).
    fn lookup_on_chain(
        &self,
        chain: &[&'static str],
        method: &str,
    ) -> Option<(Option<&'static str>, Arity, bool)> {
        self.lookup_on_chain_depth(chain, method, 0)
    }

    fn lookup_on_chain_depth(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<(Option<&'static str>, Arity, bool)> {
        // A direct definition anywhere on the chain wins.
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&def) = entry.methods.get(method) {
                    return Some(def);
                }
            }
        }
        // Else: follow the first alias for `method` found on the chain. Bound
        // the recursion to defend against a pathological alias cycle in RBS.
        if depth >= 16 {
            return None;
        }
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&old) = entry.aliases.get(method) {
                    if let Some(found) = self.lookup_on_chain_depth(chain, old, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// Compute the flattened ancestor chain for `class_name`: the class itself,
    /// then its included modules, then its superclass's chain, recursively.
    /// Returns `(chain, complete)` where `complete` is `false` if any ancestor
    /// name referenced along the way is NOT in the loaded set (so absence must
    /// not be witnessed).
    fn ancestors(&self, class_name: &str) -> (Vec<&'static str>, bool) {
        let mut order: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut complete = true;
        self.collect(class_name, &mut order, &mut seen, &mut complete);
        (order, complete)
    }

    fn collect(
        &self,
        name: &str,
        order: &mut Vec<&'static str>,
        seen: &mut HashSet<&'static str>,
        complete: &mut bool,
    ) {
        let Some((&key, entry)) = self.classes.get_key_value(name) else {
            // Referenced ancestor not loaded ⇒ chain incomplete.
            *complete = false;
            return;
        };
        if !seen.insert(key) {
            return;
        }
        order.push(key);
        // Included modules sit between the class and its superclass in Ruby's
        // method resolution order; for *existence* the order doesn't matter.
        for inc in &entry.includes {
            self.collect(inc, order, seen, complete);
        }
        if let Some(sup) = entry.superclass {
            self.collect(sup, order, seen, complete);
        }
    }

    /// Hardcoded fallback used when the core RBS directory is unavailable. The
    /// chains here mirror the real ancestry (Object/BasicObject/Kernel/…) so the
    /// conservative gate behaves the same, just over fewer methods.
    fn stub() -> Self {
        let mut classes: HashMap<&'static str, ClassEntry> = HashMap::new();

        let mut put = |name: &'static str,
                       superclass: Option<&'static str>,
                       includes: Vec<&'static str>,
                       methods: &[(&'static str, Option<&'static str>, Arity)]| {
            let mut m: HashMap<&'static str, (Option<&'static str>, Arity, bool)> = HashMap::new();
            for (n, ret, ar) in methods {
                // The stub never models nilable returns (no `?` shapes here) ⇒
                // `false` keeps the fallback conservative: nothing mints
                // `T | nil`, so `possible-nil-receiver` stays silent under it.
                m.insert(n, (*ret, *ar, false));
            }
            classes.insert(
                name,
                ClassEntry {
                    methods: m,
                    // The stub doesn't model block-form returns (no block-call
                    // result typing under the fallback); empty keeps it
                    // conservative ⇒ a block-bearing call stays Dynamic/silent.
                    block_returns: HashMap::new(),
                    // The stub lists `size` directly alongside `length`, so it
                    // needs no alias table; real RBS uses `alias size length`.
                    aliases: HashMap::new(),
                    // The stub doesn't model singleton methods (no class-method
                    // typo detection under the fallback); the surface gate stays
                    // conservative because the five base classes' singleton
                    // surface is incomplete here ⇒ always silent.
                    singleton_methods: HashMap::new(),
                    singleton_aliases: HashMap::new(),
                    superclass,
                    // The stub does not distinguish modules from classes; the
                    // raise-non-exception module gate needs the real embedded RBS
                    // (Exception is absent under the stub, so the rule is silent
                    // regardless). `false` keeps it inert.
                    is_module: false,
                    includes,
                    // The stub models no `extend` directives (no class-method
                    // surface under the fallback); empty keeps it conservative.
                    extends: Vec::new(),
                },
            );
        };

        put("BasicObject", None, vec![], &[("==", Some("TrueClass"), (1, Some(1)))]);
        put(
            "Kernel",
            None,
            vec![],
            &[
                ("class", None, (0, Some(0))),
                ("frozen?", Some("TrueClass"), (0, Some(0))),
                ("tap", None, (0, Some(0))),
                ("inspect", Some("String"), (0, Some(0))),
                ("is_a?", Some("TrueClass"), (1, Some(1))),
                ("nil?", Some("FalseClass"), (0, Some(0))),
                ("to_s", Some("String"), (0, Some(0))),
            ],
        );
        put("Object", Some("BasicObject"), vec!["Kernel"], &[]);
        put(
            "Comparable",
            None,
            vec![],
            &[
                ("<", Some("TrueClass"), (1, Some(1))),
                (">", Some("TrueClass"), (1, Some(1))),
                ("<=", Some("TrueClass"), (1, Some(1))),
                (">=", Some("TrueClass"), (1, Some(1))),
            ],
        );
        put(
            "Numeric",
            Some("Object"),
            vec!["Comparable"],
            &[
                ("+", None, (1, Some(1))),
                ("-", None, (1, Some(1))),
                ("*", None, (1, Some(1))),
                ("abs", None, (0, Some(0))),
                ("zero?", Some("TrueClass"), (0, Some(0))),
            ],
        );
        put(
            "Integer",
            Some("Numeric"),
            vec![],
            &[
                ("+", Some("Integer"), (1, Some(1))),
                ("-", Some("Integer"), (1, Some(1))),
                ("*", Some("Integer"), (1, Some(1))),
                ("abs", Some("Integer"), (0, Some(0))),
                ("succ", Some("Integer"), (0, Some(0))),
                ("pred", Some("Integer"), (0, Some(0))),
                ("to_s", Some("String"), (0, Some(1))),
                ("even?", Some("TrueClass"), (0, Some(0))),
                ("odd?", Some("TrueClass"), (0, Some(0))),
                ("times", None, (0, Some(0))),
            ],
        );
        put(
            "Float",
            Some("Numeric"),
            vec![],
            &[
                ("+", Some("Float"), (1, Some(1))),
                ("-", Some("Float"), (1, Some(1))),
                ("*", Some("Float"), (1, Some(1))),
                ("abs", Some("Float"), (0, Some(0))),
                ("round", Some("Integer"), (0, Some(1))),
                ("ceil", Some("Integer"), (0, Some(1))),
                ("floor", Some("Integer"), (0, Some(1))),
                ("to_i", Some("Integer"), (0, Some(0))),
                ("to_s", Some("String"), (0, Some(0))),
                ("nan?", Some("TrueClass"), (0, Some(0))),
            ],
        );
        put(
            "String",
            Some("Object"),
            vec!["Comparable"],
            &[
                ("length", Some("Integer"), (0, Some(0))),
                ("size", Some("Integer"), (0, Some(0))),
                ("upcase", Some("String"), (0, Some(0))),
                ("downcase", Some("String"), (0, Some(0))),
                ("capitalize", Some("String"), (0, Some(0))),
                ("reverse", Some("String"), (0, Some(0))),
                ("strip", Some("String"), (0, Some(0))),
                ("chomp", Some("String"), (0, Some(1))),
                ("to_s", Some("String"), (0, Some(0))),
                ("to_str", Some("String"), (0, Some(0))),
                ("to_sym", Some("Symbol"), (0, Some(0))),
                ("to_i", Some("Integer"), (0, Some(1))),
                ("to_f", Some("Float"), (0, Some(0))),
                ("index", Some("Integer"), (1, Some(2))),
                ("gsub", Some("String"), (1, Some(2))),
                ("sub", Some("String"), (1, Some(2))),
                ("split", Some("Array"), (0, Some(2))),
                ("include?", Some("TrueClass"), (1, Some(1))),
                ("start_with?", Some("TrueClass"), (0, None)),
                ("end_with?", Some("TrueClass"), (0, None)),
                ("empty?", Some("TrueClass"), (0, Some(0))),
                ("rjust", Some("String"), (1, Some(2))),
                ("ljust", Some("String"), (1, Some(2))),
                ("center", Some("String"), (1, Some(2))),
                ("+", Some("String"), (1, Some(1))),
                ("*", Some("String"), (1, Some(1))),
            ],
        );
        put(
            "Symbol",
            Some("Object"),
            vec!["Comparable"],
            &[
                ("to_s", Some("String"), (0, Some(0))),
                ("to_sym", Some("Symbol"), (0, Some(0))),
                ("to_proc", None, (0, Some(0))),
                ("length", Some("Integer"), (0, Some(0))),
                ("size", Some("Integer"), (0, Some(0))),
                ("upcase", Some("Symbol"), (0, Some(0))),
                ("downcase", Some("Symbol"), (0, Some(0))),
            ],
        );
        put(
            "Enumerable",
            None,
            vec![],
            &[
                ("map", Some("Array"), (0, Some(0))),
                ("each", None, (0, Some(0))),
                ("select", Some("Array"), (0, Some(0))),
                ("reject", Some("Array"), (0, Some(0))),
                ("include?", Some("TrueClass"), (1, Some(1))),
                ("to_a", Some("Array"), (0, Some(0))),
                ("first", None, (0, Some(1))),
            ],
        );
        put(
            "Array",
            Some("Object"),
            vec!["Enumerable"],
            &[
                ("length", Some("Integer"), (0, Some(0))),
                ("size", Some("Integer"), (0, Some(0))),
                ("push", None, (0, None)),
                ("pop", None, (0, Some(1))),
                ("shift", None, (0, Some(1))),
                ("unshift", None, (0, None)),
                ("reverse", Some("Array"), (0, Some(0))),
                ("sort", Some("Array"), (0, Some(0))),
                ("join", Some("String"), (0, Some(1))),
                ("empty?", Some("TrueClass"), (0, Some(0))),
                ("+", Some("Array"), (1, Some(1))),
                ("<<", Some("Array"), (1, Some(1))),
            ],
        );
        put(
            "Hash",
            Some("Object"),
            vec!["Enumerable"],
            &[
                ("length", Some("Integer"), (0, Some(0))),
                ("size", Some("Integer"), (0, Some(0))),
                ("keys", Some("Array"), (0, Some(0))),
                ("values", Some("Array"), (0, Some(0))),
                ("fetch", None, (1, Some(2))),
                ("store", None, (2, Some(2))),
                ("merge", Some("Hash"), (0, None)),
                ("empty?", Some("TrueClass"), (0, Some(0))),
                ("key?", Some("TrueClass"), (1, Some(1))),
                ("to_h", Some("Hash"), (0, Some(0))),
            ],
        );
        put(
            "NilClass",
            Some("Object"),
            vec![],
            &[
                ("to_s", Some("String"), (0, Some(0))),
                ("to_a", Some("Array"), (0, Some(0))),
                ("to_h", Some("Hash"), (0, Some(0))),
                ("to_i", Some("Integer"), (0, Some(0))),
                ("nil?", Some("TrueClass"), (0, Some(0))),
                ("inspect", Some("String"), (0, Some(0))),
            ],
        );
        put(
            "TrueClass",
            Some("Object"),
            vec![],
            &[
                ("to_s", Some("String"), (0, Some(0))),
                ("&", Some("TrueClass"), (1, Some(1))),
                ("|", Some("TrueClass"), (1, Some(1))),
                ("^", Some("TrueClass"), (1, Some(1))),
            ],
        );
        put(
            "FalseClass",
            Some("Object"),
            vec![],
            &[
                ("to_s", Some("String"), (0, Some(0))),
                ("&", Some("FalseClass"), (1, Some(1))),
                ("|", Some("TrueClass"), (1, Some(1))),
                ("^", Some("TrueClass"), (1, Some(1))),
            ],
        );

        // Every curated stub name is a genuine top-level core class, so the
        // whole key set is the top-level set (defect 2: `knows_toplevel_class`
        // of a curated class is `true`, of an unknown name is `false`).
        let toplevel_classes: HashSet<&'static str> = classes.keys().copied().collect();
        Self {
            source: RbsSource::Stub,
            classes,
            toplevel_classes,
            project_sig_classes: HashSet::new(),
        }
    }

    /// The signature source this data was built from (embedded / override /
    /// stub) — surfaced by `rigor doctor` (audit-R1).
    pub fn source(&self) -> &RbsSource {
        &self.source
    }

    /// Whether `class_name` was INTRODUCED by project-`sig/` ingestion (ADR-0033)
    /// — declared in the project's own signatures and not already carried by a
    /// bundled (core/stdlib/plugin) RBS. The dispatch rules use this to witness an
    /// `X.new` instance-method typo on a project-authored class while staying
    /// lenient on a bundled stdlib/gem class. Always `false` when no `sig/` was
    /// ingested.
    pub fn is_project_sig_class(&self, class_name: &str) -> bool {
        self.project_sig_classes.contains(class_name)
    }

    /// How many distinct classes the loaded RBS surface registered. A coarse
    /// coverage signal for `rigor doctor`.
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }
}

/// Accumulates parsed RBS declarations into per-class entries before flattening.
#[derive(Default)]
struct Builder {
    classes: HashMap<&'static str, ClassEntry>,
    /// Short names declared at GENUINE top level (empty namespace) in at least
    /// one declaration. Threaded out via [`Self::finish`] into
    /// [`CoreData::toplevel_classes`] (defect 2).
    toplevel_classes: HashSet<&'static str>,
    /// Short names whose superclass has been claimed by a GENUINE top-level
    /// declaration (`!nested && is_toplevel_name`). Because classes are keyed by
    /// SHORT name, a namespaced/nested class (`Psych::Exception < ::RuntimeError`)
    /// otherwise collapses onto a same-short-named top-level class (`Exception`)
    /// and its `< RuntimeError` wins first-write — a superclass CYCLE
    /// (`Exception → RuntimeError → StandardError → Exception`) that makes
    /// `class_ordering` return a spurious `Subclass` in BOTH directions. A
    /// top-level declaration's superclass (even an implicit `Object`, recorded as
    /// `None` here and defaulted in [`Self::finish`]) is authoritative for its
    /// short name; once claimed, a nested twin can no longer overwrite it. This
    /// mirrors the reference's namespace-aware RBS environment without giving up
    /// the deliberate short-name collapse used for method-existence leniency.
    super_claimed: HashSet<&'static str>,
}

impl Builder {
    /// Parse one RBS source and fold its top-level class/module declarations in.
    fn ingest(&mut self, code: &str) {
        let Ok(sig) = parse(code) else {
            return;
        };
        for decl in sig.declarations().iter() {
            // `false` = top-level (file-level) declaration: only these may enter
            // the `toplevel_classes` set.
            match decl {
                Node::Class(c) => self.ingest_class(&c, false),
                Node::Module(m) => self.ingest_module(&m, false),
                _ => {}
            }
        }
    }

    fn ingest_class(&mut self, c: &ClassNode, nested: bool) {
        let tn = c.name();
        let Some(name) = type_name_str(&tn) else {
            return;
        };
        // A genuine top-level decl (`class Time`) is a FILE-LEVEL declaration with
        // an EMPTY namespace. A LEXICALLY NESTED decl (`class Group` written
        // inside `class PrettyPrint`) ALSO has an empty namespace on its own node
        // (nesting is lexical, not embedded in the inner TypeName), so the
        // namespace check alone is insufficient — we must additionally know the
        // decl is file-level (`!nested`). Without this, `PrettyPrint::Group`,
        // `Benchmark::Report`, `Etc::Group` etc. would leak into the top-level set
        // and be wrongly singleton-witnessable (false positives on a project model
        // named `Group`/`Report`). Record only file-level, empty-namespace names.
        let authoritative = !nested && is_toplevel_name(&tn);
        if authoritative {
            self.toplevel_classes.insert(name);
        }
        let superclass = c
            .super_class()
            .and_then(|s| type_name_str(&s.name()));
        let mut entry = ClassEntry {
            superclass,
            ..Default::default()
        };
        self.collect_members(c.members().iter(), &mut entry);
        self.merge(name, entry, authoritative);
    }

    fn ingest_module(&mut self, m: &ModuleNode, nested: bool) {
        let tn = m.name();
        let Some(name) = type_name_str(&tn) else {
            return;
        };
        let authoritative = !nested && is_toplevel_name(&tn);
        if authoritative {
            self.toplevel_classes.insert(name);
        }
        let mut entry = ClassEntry {
            is_module: true,
            ..Default::default()
        };
        self.collect_members(m.members().iter(), &mut entry);
        self.merge(name, entry, authoritative);
    }

    /// Fold method definitions and `include` directives from a member list into
    /// `entry`. Only instance, public methods are recorded (the existence check
    /// is about instance dispatch; private/singleton are out of scope here).
    fn collect_members<'a>(
        &mut self,
        members: impl Iterator<Item = Node<'a>>,
        entry: &mut ClassEntry,
    ) {
        for member in members {
            match member {
                Node::MethodDefinition(md) => {
                    let mname = intern(md.name().as_str());
                    let (ret, arity, nilable) = method_signature(&md);
                    let block_ret = block_overload_return(&md);
                    let kind = md.kind();
                    // `def self.x` ⇒ Singleton; `def self?.x` ⇒ SingletonInstance
                    // (BOTH a class method AND an instance method); a plain
                    // `def x` ⇒ Instance. Record into the matching map(s).
                    if matches!(
                        kind,
                        MethodDefinitionKind::Instance
                            | MethodDefinitionKind::SingletonInstance
                    ) {
                        entry.methods.entry(mname).or_insert((ret, arity, nilable));
                        if let Some(br) = block_ret {
                            entry.block_returns.entry(mname).or_insert(br);
                        }
                    }
                    if matches!(
                        kind,
                        MethodDefinitionKind::Singleton
                            | MethodDefinitionKind::SingletonInstance
                    ) {
                        entry.singleton_methods.entry(mname).or_insert((ret, arity));
                    }
                }
                Node::Include(inc) => {
                    if let Some(modname) = type_name_str(&inc.name()) {
                        if !entry.includes.contains(&modname) {
                            entry.includes.push(modname);
                        }
                    }
                }
                Node::Extend(ext) => {
                    // `extend M` folds M's INSTANCE methods into this class
                    // object's SINGLETON surface (e.g. `SecureRandom extend
                    // Random::Formatter` ⇒ `SecureRandom.hex`). Record the
                    // module name; the singleton lookup resolves it conservatively
                    // (an unknown extended module ⇒ surface incomplete ⇒ silent).
                    if let Some(modname) = type_name_str(&ext.name()) {
                        if !entry.extends.contains(&modname) {
                            entry.extends.push(modname);
                        }
                    }
                }
                Node::Alias(a) => {
                    // `alias new old` aliases a method to another. An INSTANCE
                    // alias (`alias size length`) feeds instance dispatch; a
                    // SINGLETON alias (`alias self.pwd self.getwd`) feeds the
                    // class-object surface. Record each into its own map.
                    let new_name = intern(a.new_name().as_str());
                    let old_name = intern(a.old_name().as_str());
                    match a.kind() {
                        AliasKind::Instance => {
                            entry.aliases.entry(new_name).or_insert(old_name);
                        }
                        AliasKind::Singleton => {
                            entry.singleton_aliases.entry(new_name).or_insert(old_name);
                        }
                    }
                }
                // A NESTED class/module declaration (e.g. `module PP; module
                // ObjectMixin; end; end`) must be registered too, by its simple
                // name — otherwise an `include` that references it leaves the
                // ancestor chain "incomplete", and the conservative gate would
                // stop witnessing absence for EVERY class whose chain passes
                // through the reopened owner (e.g. `Object include PP::ObjectMixin`
                // ⇒ all typo detection silently disabled). Registering nested
                // types by simple name keeps chains complete. (Simple-name
                // collisions only ever ADD methods, never witness false absence.)
                Node::Class(inner) => self.ingest_class(&inner, true),
                Node::Module(inner) => self.ingest_module(&inner, true),
                _ => {}
            }
        }
    }

    /// Merge an entry into the map (the same class can be reopened across files,
    /// though core mostly isn't). Methods/includes union; an explicit superclass
    /// wins over none.
    ///
    /// `authoritative` is `true` for a GENUINE top-level declaration (empty
    /// namespace, file-level). Such a declaration owns the short name's superclass
    /// identity: the FIRST authoritative write wins and, once made, blocks a
    /// nested/namespaced same-short-name twin from overwriting it — preventing the
    /// short-name-collapse superclass cycles (see [`Builder::super_claimed`]). A
    /// non-authoritative (nested) entry may only fill a still-empty, unclaimed
    /// slot. Methods / includes / singletons still union unconditionally (the
    /// method-existence surface is deliberately the collapsed union).
    fn merge(&mut self, name: &'static str, entry: ClassEntry, authoritative: bool) {
        let claimed = self.super_claimed.contains(name);
        let slot = self.classes.entry(name).or_default();
        if authoritative {
            // The first top-level declaration's superclass (even implicit `Object`,
            // recorded `None` and defaulted in `finish`) is authoritative and may
            // overwrite a value a nested twin set earlier.
            if !claimed {
                slot.superclass = entry.superclass;
            }
        } else if slot.superclass.is_none() && !claimed {
            slot.superclass = entry.superclass;
        }
        slot.is_module |= entry.is_module;
        for (k, v) in entry.methods {
            slot.methods.entry(k).or_insert(v);
        }
        for (k, v) in entry.block_returns {
            slot.block_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.singleton_methods {
            slot.singleton_methods.entry(k).or_insert(v);
        }
        for (new_name, old_name) in entry.aliases {
            slot.aliases.entry(new_name).or_insert(old_name);
        }
        for (new_name, old_name) in entry.singleton_aliases {
            slot.singleton_aliases.entry(new_name).or_insert(old_name);
        }
        for inc in entry.includes {
            if !slot.includes.contains(&inc) {
                slot.includes.push(inc);
            }
        }
        for ext in entry.extends {
            if !slot.extends.contains(&ext) {
                slot.extends.push(ext);
            }
        }
        // Record the authoritative superclass claim AFTER the `slot` borrow of
        // `self.classes` is done (disjoint field).
        if authoritative {
            self.super_claimed.insert(name);
        }
    }

    /// Finish: apply implicit-`Object` superclass defaulting (every class except
    /// `BasicObject` and modules implicitly inherits `Object` when no `< X` was
    /// given) and return the class map plus the set of names declared at genuine
    /// top level (defect 2).
    fn finish(mut self) -> (HashMap<&'static str, ClassEntry>, HashSet<&'static str>) {
        let object = intern("Object");
        let basic = intern("BasicObject");
        // A module has no superclass; only *classes* default to Object. We can't
        // perfectly distinguish here, but the curated set's modules (Kernel,
        // Comparable, Enumerable) all legitimately have no class-superclass, and
        // giving a module an Object super would only *add* methods to its chain
        // (never falsely witness absence) — yet to stay precise we skip the
        // known modules.
        let modules: HashSet<&'static str> = ["Kernel", "Comparable", "Enumerable"]
            .into_iter()
            .map(intern)
            .collect();
        for (&name, entry) in self.classes.iter_mut() {
            if entry.superclass.is_none() && name != basic && !modules.contains(name) {
                entry.superclass = Some(object);
            }
        }
        (self.classes, self.toplevel_classes)
    }
}

/// Parse every `*.rbs` file under `dir` (recursively) and fold its declarations
/// into `builder`. Per-file isolation (ADR-0016 never-crash): a read or parse
/// failure on one file is skipped; the rest still load. Subdirectories are
/// walked because both `core/` (e.g. `core/io/*.rbs`, `core/rubygems/*.rbs`) and
/// some stdlib libs (`stdlib/<lib>/0/<sub>/*.rbs`) nest their signatures.
fn ingest_rbs_dir(builder: &mut Builder, dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            ingest_rbs_dir(builder, &path);
        } else if path.extension().is_some_and(|e| e == "rbs") {
            if let Ok(code) = std::fs::read_to_string(&path) {
                ingest_rbs_source(builder, &path.to_string_lossy(), &code);
            }
        }
    }
}

/// Fold one RBS source's declarations into `builder`. The single per-file ingest
/// point shared by BOTH the filesystem path ([`ingest_rbs_dir`], the
/// `RIGOR_RBS_CORE_DIR` override) and the embedded path ([`ingest_embedded`]):
/// both feed the SAME bytes to the SAME [`Builder::ingest`] / `ruby-rbs` parser,
/// which is what makes the embedded default byte-identical to the runtime path.
/// `name` is informational only (the path or embedded key) — the parser keys off
/// `contents` alone, so it never affects the resulting index.
fn ingest_rbs_source(builder: &mut Builder, _name: &str, contents: &str) {
    builder.ingest(contents);
}

/// Ingest the build-time-embedded vendored RBS set ([`EMBEDDED_RBS`]) — the
/// default (no `RIGOR_RBS_CORE_DIR`) load path. Each `(relative-path, contents)`
/// entry is fed to the SAME [`ingest_rbs_source`] the filesystem path uses, so
/// the index is identical to ingesting the vendored tree from disk. The embedded
/// set is the whole `core/` ⊕ the `DEFAULT_LIBRARIES` stdlib closure already
/// resolved at vendoring time, so no `manifest.yaml` walk is needed here.
fn ingest_embedded(builder: &mut Builder) {
    for (name, contents) in EMBEDDED_RBS {
        ingest_rbs_source(builder, name, contents);
    }
}

/// Parse the `dependencies:` list out of an RBS stdlib `manifest.yaml`, returning
/// the dependency lib names. Hand-rolled (no YAML crate): the manifests are a
/// trivial, fixed shape — a `dependencies:` key followed by `- name: <lib>`
/// items. A missing/garbled manifest yields no deps (never panics).
fn manifest_deps(path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut deps = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        // Leave the `dependencies:` block when a new unindented top-level key
        // appears (a line that isn't a list item and isn't indented).
        if in_deps && !line.starts_with(' ') && !line.starts_with('-') {
            in_deps = false;
        }
        if trimmed == "dependencies:" {
            in_deps = true;
            continue;
        }
        if in_deps {
            // Item shape: `- name: psych` (quotes optional).
            if let Some(rest) = trimmed.strip_prefix("- name:") {
                let name = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                if !name.is_empty() {
                    deps.push(name.to_string());
                }
            }
        }
    }
    deps
}

/// Resolve the **last** path component of a `TypeNameNode`'s namespace+name into
/// a `&'static str` (interned). RBS class refs are usually bare (`Object`,
/// `Comparable`), but a namespaced ref (`::Foo::Bar`) resolves to `Bar`.
fn type_name_str(tn: &ruby_rbs::node::TypeNameNode) -> Option<&'static str> {
    let sym = tn.name();
    let s = sym.as_str();
    if s.is_empty() {
        None
    } else {
        Some(intern(s))
    }
}

/// Whether a `TypeNameNode` names a GENUINE top-level declaration — i.e. its
/// namespace is empty. `class Time` ⇒ empty namespace ⇒ `true`; `class
/// Process::Status` ⇒ namespace path `[Process]` ⇒ `false`. Used to refuse
/// treating a namespaced stdlib class registered by its short key as a known
/// top-level class (defect 2).
fn is_toplevel_name(tn: &ruby_rbs::node::TypeNameNode) -> bool {
    tn.namespace().path().iter().next().is_none()
}

/// Extract `(return class, arity envelope)` from a method definition by reading
/// its overloads' function types. The return class is resolved only when it is a
/// plain `ClassInstanceType` (a concrete class), or `self`/`instance` mapped to
/// the receiver class by the caller (we record `self` returns as the receiver's
/// own name when known). A union (`bool`), generic, `void`, etc. ⇒ `None`.
///
/// An `Optional` return (`String?`) is unwrapped to its inner
/// `ClassInstanceType` (the class) PLUS a `nilable: true` bit — so a nilable
/// return is preserved, not discarded (it previously fell through to `None` ⇒
/// Dynamic, losing the optionality). The nilable bit obeys the SAME
/// all-overloads-agree discipline as the class: across overloads we adopt the
/// `(class, nilable)` pair only if every resolvable overload agrees on BOTH;
/// any disagreement ⇒ `None` (never guess, never invent nil — being
/// conservative here only loses recall in `possible-nil-receiver`, never an FP).
fn method_signature(
    md: &ruby_rbs::node::MethodDefinitionNode,
) -> (Option<&'static str>, Arity, bool) {
    let mut min: Option<usize> = None;
    let mut max: Option<usize> = Some(0);
    let mut variadic = false;
    // The agreed `(class, nilable)` pair across overloads. `ret` carries the
    // class (as before); `ret_nilable` carries the matching nil bit. They move
    // together so a disagreement on EITHER collapses the return to `None`.
    let mut ret: Option<&'static str> = None;
    let mut ret_nilable = false;
    let mut ret_seen = false;

    for overload in md.overloads().iter() {
        let Node::MethodDefinitionOverload(ov) = overload else {
            continue;
        };
        let Node::MethodType(mt) = ov.method_type() else {
            continue;
        };
        let Node::FunctionType(ft) = mt.type_() else {
            continue;
        };

        let required = ft.required_positionals().iter().count();
        let optional = ft.optional_positionals().iter().count();
        let has_rest = ft.rest_positionals().is_some();

        min = Some(min.map_or(required, |m| m.min(required)));
        if has_rest {
            variadic = true;
        } else {
            let hi = required + optional;
            max = max.map(|m| m.max(hi));
        }

        // Return type: resolve a concrete ClassInstanceType, OR an `Optional`
        // wrapping one (`String?` ⇒ class `String`, nilable). Across overloads,
        // only adopt a return if ALL resolvable overloads agree on BOTH the
        // class AND the nil bit; any disagreement ⇒ leave None (never guess).
        let (this_ret, this_nilable) = match ft.return_type() {
            Node::ClassInstanceType(ci) => (type_name_str(&ci.name()), false),
            // `String?` lowers to `OptionalType(ClassInstanceType String)`.
            // Recurse into the inner type; a nested optional/union/generic
            // inside the optional is not a single concrete class ⇒ None.
            Node::OptionalType(opt) => match opt.type_() {
                Node::ClassInstanceType(ci) => (type_name_str(&ci.name()), true),
                _ => (None, false),
            },
            _ => (None, false),
        };
        if !ret_seen {
            ret = this_ret;
            ret_nilable = this_nilable;
            ret_seen = true;
        } else if ret != this_ret || ret_nilable != this_nilable {
            // Disagreement on class or nilability ⇒ drop the return entirely
            // (and with it the nil bit), the conservative choice.
            ret = None;
            ret_nilable = false;
        }
    }

    let arity = (min.unwrap_or(0), if variadic { None } else { max });
    // `ret_nilable` is only meaningful when `ret` is Some; callers read it via
    // `method_return_nilable`, which gates on `ret` being present.
    (ret, arity, ret_nilable)
}

/// The RETURN class of the method's **block-bearing overload** — the overload
/// the reference picks when a block is supplied at the call site
/// (`OverloadSelector` with `block_required: true`). We scan the overloads for
/// one declaring a `block:` clause (`MethodTypeNode::block()`), and resolve ITS
/// function return type:
///
/// - a concrete `ClassInstanceType` (`Hash#filter { } -> ::Hash[K,V]`,
///   `Enumerable#map { } -> ::Array[U]`) ⇒ that class name;
/// - a `self` return (`Array#each { } -> self`, `Kernel#tap { } -> self`) ⇒
///   the [`SELF_RETURN`] sentinel, resolved to the receiver at lookup time.
///
/// Returns `None` (⇒ block form not modeled ⇒ caller stays Dynamic, zero-FP)
/// when no overload has a block, or when the block overload's return is a
/// union (`bool`), bare generic variable, `void`, nilable, or anything else we
/// can't pin to a single concrete class. When MULTIPLE block overloads exist
/// we require them to AGREE on the return (any disagreement ⇒ `None`), matching
/// the conservative discipline of [`method_signature`].
fn block_overload_return(md: &ruby_rbs::node::MethodDefinitionNode) -> Option<&'static str> {
    let mut found: Option<Option<&'static str>> = None;
    for overload in md.overloads().iter() {
        let Node::MethodDefinitionOverload(ov) = overload else {
            continue;
        };
        let Node::MethodType(mt) = ov.method_type() else {
            continue;
        };
        // Only the block-bearing overload(s) participate.
        if mt.block().is_none() {
            continue;
        }
        let Node::FunctionType(ft) = mt.type_() else {
            continue;
        };
        let this_ret = match ft.return_type() {
            Node::ClassInstanceType(ci) => type_name_str(&ci.name()),
            // A `self` block return (each/tap) ⇒ the receiver's own type.
            Node::SelfType(_) => Some(SELF_RETURN),
            _ => None,
        };
        match found {
            None => found = Some(this_ret),
            Some(prev) if prev != this_ret => return None,
            _ => {}
        }
    }
    found.flatten()
}

/// Intern a `&str` to a `&'static str` by leaking, deduplicated through a
/// process-global set so equal names share one allocation. The core class/method
/// vocabulary is small and bounded, so the leak is negligible and one-time.
fn intern(s: &str) -> &'static str {
    use std::sync::Mutex;
    use std::sync::OnceLock;
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = pool.lock().unwrap();
    if let Some(&existing) = guard.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    guard.insert(leaked);
    leaked
}

#[cfg(test)]
mod embedded_tests {
    use super::*;

    /// The build-time-embedded set is present and carries the core classes.
    #[test]
    fn embedded_rbs_non_empty_with_core_files() {
        assert!(!EMBEDDED_RBS.is_empty(), "EMBEDDED_RBS must not be empty");
        let has = |needle: &str| EMBEDDED_RBS.iter().any(|(p, _)| p.ends_with(needle));
        assert!(has("core/array.rbs"), "missing core/array.rbs");
        assert!(has("core/string.rbs"), "missing core/string.rbs");
        // A stdlib closure member.
        assert!(
            EMBEDDED_RBS.iter().any(|(p, _)| p.contains("stdlib/pathname/")),
            "missing stdlib/pathname"
        );
        // Entries are non-empty file contents.
        assert!(EMBEDDED_RBS.iter().all(|(_, c)| !c.is_empty()));
    }

    /// `CoreData::load()` with `RIGOR_RBS_CORE_DIR` UNSET ingests the embedded
    /// set (NOT a stub): it knows the core roots and stdlib-closure classes, and
    /// resolves instance methods with the same parity the runtime path gives.
    ///
    /// NB: this asserts on the global `load()` and so must not clobber the env
    /// var (other code/tests share the process). It relies on the harness/CI
    /// running with the var unset; if it happens to be set, the override path is
    /// equally valid for these assertions (same signatures), so we don't gate.
    #[test]
    fn embedded_load_is_non_stub_and_method_parity() {
        let idx = CoreData::load();
        // Core classes from the embedded core/ tree.
        assert!(idx.knows_class("String"));
        assert!(idx.knows_class("Array"));
        assert!(idx.knows_class("Hash"));
        // Stdlib-closure classes (proves the stdlib set embedded, not just core
        // / the small stub): `Pathname` (pathname) and `Set` (in core builtin).
        assert!(idx.knows_class("Pathname"));
        assert!(idx.knows_class("Set"));
        // Method existence parity for a known method, and absence for a typo.
        assert!(idx.class_has_method("String", "upcase"));
        assert!(!idx.class_has_method("String", "lenght"));
    }

    /// Step 1 (nilable-RBS-return): an `Optional` return (`String?`) is
    /// preserved as `(class, nilable=true)`; a plain return is `(class, false)`;
    /// and overloads that DISAGREE on nilability collapse to `None` (never
    /// invent nil). `byteslice` is uniformly `-> String?`; `upcase` is plainly
    /// `-> String`; `try_convert`'s overloads mix `String` and `String?`.
    #[test]
    fn nilable_return_preserved_and_conservative() {
        let idx = CoreData::load();
        // Nilable return: `String#byteslice : (...) -> String?` ⇒ (String, true).
        assert_eq!(
            idx.method_return_nilable("String", "byteslice"),
            Some(("String", true)),
            "byteslice's String? must surface nilable=true"
        );
        // Plain return: `String#upcase : () -> String` ⇒ (String, false).
        assert_eq!(
            idx.method_return_nilable("String", "upcase"),
            Some(("String", false)),
            "upcase's plain String must surface nilable=false"
        );
        // Disagreeing overloads (String vs String?) ⇒ conservative None.
        assert_eq!(
            idx.method_return_nilable("String", "try_convert"),
            None,
            "overloads disagreeing on nilability must collapse to None"
        );
        // The existing non-nil accessors are unchanged by the new bit.
        assert_eq!(idx.method_return("String", "upcase"), Some("String"));
        assert_eq!(idx.method_return("String", "byteslice"), Some("String"));
    }

    /// ADR-0033: an empty `sig_dirs` is byte-identical to `load_with_plugins`
    /// (the gating contract) — the no-`sig/` path must be unchanged.
    #[test]
    fn empty_project_sig_is_unchanged() {
        let base = CoreData::load_with_plugins(&[]);
        let with_sig = CoreData::load_for_project(&[], &[]);
        assert_eq!(base.class_count(), with_sig.class_count());
        // A named-but-absent dir is inert too (ingestion skips a non-directory).
        let absent = CoreData::load_for_project(
            &[],
            &[std::path::PathBuf::from("this-dir-does-not-exist-xyzzy")],
        );
        assert_eq!(base.class_count(), absent.class_count());
    }

    /// ADR-14 slice 10: the sig-gen-only precise declared-return accessors are
    /// three-valued and never "assume present". `Object#hash → Integer` resolves
    /// through the ancestor chain (instance AND — via the class object's own
    /// ancestry — singleton), an absent method on a complete chain is `None`
    /// (NotDeclared), and an unresolvable-return method is `Some(None)`.
    #[test]
    fn sig_gen_declared_return_accessors_are_three_valued() {
        let idx = CoreData::load();
        // Instance: declared, concrete return.
        assert_eq!(idx.declared_instance_return("String", "upcase"), Some(Some("String")));
        assert_eq!(idx.declared_instance_return("Object", "hash"), Some(Some("Integer")));
        // Instance: declared, but the return is not a single bare concrete class
        // (`Integer#times` returns an Enumerator/self union) ⇒ Some(None).
        assert_eq!(idx.declared_instance_return("Integer", "times"), Some(None));
        // Instance: not declared on a fully-loaded chain ⇒ None (NotDeclared).
        assert_eq!(idx.declared_instance_return("String", "definitely_absent_zzz"), None);
        // An unknown class ⇒ None (the SigEnv gates on presence first).
        assert_eq!(idx.declared_instance_return("NoSuchClassZzz", "foo"), None);

        // Singleton: the class object inherits `Object#hash` (Integer) through
        // its `Class`/`Module`/`Object` ancestry.
        assert_eq!(idx.declared_singleton_return("String", "hash"), Some(Some("Integer")));
        // Singleton: an absent class method on a complete surface ⇒ None.
        assert_eq!(idx.declared_singleton_return("String", "definitely_absent_zzz"), None);

        // Chain completeness: a fully-loaded core class is complete.
        assert!(idx.chain_complete("String"));
        assert!(!idx.chain_complete("NoSuchClassZzz"));
    }

    /// ADR-0033: a project `sig/` dir's classes join the known set (so the
    /// dispatch rules can witness them) and their methods resolve, while a typo
    /// on such a class is witnessed-absent — exactly the reference's
    /// `rbs_class_known?` behaviour. Uses a real temp dir since ingestion is
    /// filesystem-driven (`ingest_rbs_dir`).
    #[test]
    fn project_sig_widens_known_classes() {
        let base = std::env::temp_dir()
            .join(format!("rigor-sig-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("temp sig dir");
        std::fs::write(
            base.join("widget.rbs"),
            "class Widget\n  def spin: () -> Integer\nend\n",
        )
        .expect("write sig");

        let data = CoreData::load_for_project(&[], std::slice::from_ref(&base));
        assert!(data.knows_class("Widget"), "project class joins knows_class");
        assert!(data.knows_toplevel_class("Widget"), "declared at top level");
        assert!(data.class_has_method("Widget", "spin"), "declared method resolves");
        // A typo is witnessed-absent (the whole ancestor chain — Widget + Object
        // — is loaded, so absence is decidable), the coverage this leg unlocks.
        assert!(!data.class_has_method("Widget", "spni"));
        // Core classes are unaffected by the project ingest.
        assert!(data.knows_class("String"));
        assert!(data.class_has_method("String", "upcase"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
