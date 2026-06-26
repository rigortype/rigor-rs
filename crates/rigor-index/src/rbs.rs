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
    /// `method name -> (return class name if resolvable, arity envelope)`.
    methods: HashMap<&'static str, (Option<&'static str>, Arity)>,
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
    /// `def self?.x`). Keyed by name -> arity envelope. The singleton class
    /// inherits down the SUPERCLASS chain, so resolving a class method walks
    /// these maps up `superclass`. Return types are not modeled here (the
    /// existence check is all the singleton surface needs).
    singleton_methods: HashMap<&'static str, Arity>,
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
    /// Included module names (in source order).
    includes: Vec<&'static str>,
    /// `extend`ed module names (in source order). An `extend M` directive folds
    /// `M`'s INSTANCE methods into THIS class/module's SINGLETON surface (the
    /// class object gains them as class methods — e.g. `SecureRandom` does
    /// `extend Random::Formatter`, so `SecureRandom.hex` is a real class method).
    extends: Vec<&'static str>,
}

/// The loaded core data backing [`crate::CoreIndex`] and the free
/// `method_return` / `method_arity` functions.
pub struct CoreData {
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
        // Override seam: a runtime RBS dir (ADR-0007 / audit-R2). KEEP this path
        // exactly as before — when present it fully replaces the embedded set.
        if let Ok(dir) = std::env::var("RIGOR_RBS_CORE_DIR") {
            let dir = PathBuf::from(dir);
            if let Some(index) = Self::load_from_dir(&dir) {
                return index;
            }
            // Override set but unusable (absent / nothing parsed): fall through
            // to the embedded default rather than failing.
        }

        // Default: the vendored, build-time-embedded set.
        let mut builder = Builder::default();
        ingest_embedded(&mut builder);
        let (classes, toplevel_classes) = builder.finish();
        if !classes.is_empty() {
            return Self { classes, toplevel_classes };
        }
        // Fallback: embedded empty (shouldn't happen) ⇒ hardcoded stub.
        Self::stub()
    }

    /// The runtime-filesystem ingest path (the `RIGOR_RBS_CORE_DIR` override):
    /// ingest the WHOLE `dir` plus the `DEFAULT_LIBRARIES` stdlib closure rooted
    /// at `<dir>/../stdlib`. Returns `None` when the dir is absent or nothing
    /// parsed (caller then falls back to the embedded default / stub). This is
    /// the same logic the default previously ran against the hardcoded path.
    fn load_from_dir(dir: &std::path::Path) -> Option<Self> {
        if dir.is_dir() {
            let mut builder = Builder::default();

            // 1) The WHOLE core dir (~62 files), not a curated subset — so every
            //    core class + its full ancestor chain is loaded.
            ingest_rbs_dir(&mut builder, &dir);

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
                    ingest_rbs_dir(&mut builder, &lib_dir);
                    // Enqueue manifest dependencies (transitive closure).
                    for dep in manifest_deps(&lib_dir.join("manifest.yaml")) {
                        if !loaded.contains(&dep) {
                            queue.push(dep);
                        }
                    }
                }
            }

            let (classes, toplevel_classes) = builder.finish();
            if !classes.is_empty() {
                return Some(Self { classes, toplevel_classes });
            }
        }
        None
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
        //     `Class`). Reuse the conservative `class_has_method` over the five
        //     base classes. Their presence is also a completeness precondition:
        //     if any of the five is missing the surface is unknown ⇒ silent.
        const BASES: [&str; 5] =
            ["Class", "Module", "Object", "Kernel", "BasicObject"];
        let mut bases_loaded = true;
        for base in BASES {
            if !self.classes.contains_key(base) {
                bases_loaded = false;
                continue;
            }
            // `class_has_method` walks the base's own ancestor chain; a hit on
            // any of the five means the class object responds. We test each
            // base directly so a hit short-circuits to PRESENT.
            let (chain, _) = self.ancestors(base);
            if self.lookup_on_chain(&chain, method).is_some() {
                return true;
            }
        }
        // Not found anywhere. Witness absence ONLY when the whole surface is
        // known: the singleton superclass chain is complete AND all five base
        // classes are loaded. Otherwise stay silent.
        if complete && bases_loaded {
            return false;
        }
        true
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
                    if self.singleton_on_chain(chain, old, depth + 1) {
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
        self.lookup_on_chain(&chain, method).and_then(|(ret, _)| ret)
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
        self.lookup_on_chain(&chain, method).map(|(_, arity)| arity)
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
    ) -> Option<(Option<&'static str>, Arity)> {
        self.lookup_on_chain_depth(chain, method, 0)
    }

    fn lookup_on_chain_depth(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<(Option<&'static str>, Arity)> {
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
            let mut m: HashMap<&'static str, (Option<&'static str>, Arity)> = HashMap::new();
            for (n, ret, ar) in methods {
                m.insert(n, (*ret, *ar));
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
        Self { classes, toplevel_classes }
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
        if !nested && is_toplevel_name(&tn) {
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
        self.merge(name, entry);
    }

    fn ingest_module(&mut self, m: &ModuleNode, nested: bool) {
        let tn = m.name();
        let Some(name) = type_name_str(&tn) else {
            return;
        };
        if !nested && is_toplevel_name(&tn) {
            self.toplevel_classes.insert(name);
        }
        let mut entry = ClassEntry::default();
        self.collect_members(m.members().iter(), &mut entry);
        self.merge(name, entry);
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
                    let (ret, arity) = method_signature(&md);
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
                        entry.methods.entry(mname).or_insert((ret, arity));
                        if let Some(br) = block_ret {
                            entry.block_returns.entry(mname).or_insert(br);
                        }
                    }
                    if matches!(
                        kind,
                        MethodDefinitionKind::Singleton
                            | MethodDefinitionKind::SingletonInstance
                    ) {
                        entry.singleton_methods.entry(mname).or_insert(arity);
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
    fn merge(&mut self, name: &'static str, entry: ClassEntry) {
        let slot = self.classes.entry(name).or_default();
        if slot.superclass.is_none() {
            slot.superclass = entry.superclass;
        }
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
fn method_signature(
    md: &ruby_rbs::node::MethodDefinitionNode,
) -> (Option<&'static str>, Arity) {
    let mut min: Option<usize> = None;
    let mut max: Option<usize> = Some(0);
    let mut variadic = false;
    let mut ret: Option<&'static str> = None;
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

        // Return type: only resolve a concrete ClassInstanceType. Across
        // overloads, only adopt a return if ALL resolvable overloads agree;
        // any disagreement ⇒ leave None (never guess).
        let this_ret = match ft.return_type() {
            Node::ClassInstanceType(ci) => type_name_str(&ci.name()),
            _ => None,
        };
        if !ret_seen {
            ret = this_ret;
            ret_seen = true;
        } else if ret != this_ret {
            ret = None;
        }
    }

    let arity = (min.unwrap_or(0), if variadic { None } else { max });
    (ret, arity)
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
}
