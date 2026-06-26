//! Real RBS-backed core data: parse a curated set of Ruby `core/*.rbs` with the
//! `ruby-rbs` crate (parser only — ADR-0004), extract per-class method tables
//! (return class + arity envelope) and the super/include graph, then flatten an
//! ancestor chain so method existence is decided over the full linearization.
//!
//! Falls back to a hardcoded stub when the core RBS directory is absent, so the
//! crate works without a Ruby install (CI, other machines) and never panics.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ruby_rbs::node::{
    parse, AliasKind, ClassNode, MethodDefinitionKind, ModuleNode, Node,
};

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

/// The default core-RBS directory (rbs gem under mise) used when
/// `RIGOR_RBS_CORE_DIR` is unset. ADR-0007 will replace this runtime path with
/// vendored, build-time-embedded RBS.
const DEFAULT_CORE_DIR: &str = "/Users/megurine/.local/share/mise/installs/ruby/4.0.5/\
lib/ruby/gems/4.0.0/gems/rbs-4.0.3/core";

/// An arity envelope `(min, max)`: `min` is the smallest required-positional
/// count across overloads; `max` is `None` (variadic) when any overload takes a
/// positional rest, else the largest required+optional count.
type Arity = (usize, Option<usize>);

/// Per-class data extracted from RBS: its instance methods (name -> resolved
/// return class + arity), its direct superclass, and its included modules.
#[derive(Default, Clone)]
struct ClassEntry {
    /// `method name -> (return class name if resolvable, arity envelope)`.
    methods: HashMap<&'static str, (Option<&'static str>, Arity)>,
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
    /// Direct superclass name, if any (`None` ⇒ implicit `Object`, except the
    /// roots which are seeded explicitly).
    superclass: Option<&'static str>,
    /// Included module names (in source order).
    includes: Vec<&'static str>,
}

/// The loaded core data backing [`crate::CoreIndex`] and the free
/// `method_return` / `method_arity` functions.
pub struct CoreData {
    /// `class name -> entry`. Keys are `&'static str` (leaked once at load) so
    /// resolved return-class names can flow out as `&'static str`.
    classes: HashMap<&'static str, ClassEntry>,
}

impl CoreData {
    /// Build from the real RBS universe (the reference's default: ALL of
    /// `core/*.rbs` ⊕ the `DEFAULT_LIBRARIES` stdlib set) if the core directory
    /// exists, else from the hardcoded stub. Never panics: any per-file parse
    /// error is skipped, and stdlib reopens of core classes (`class Hash ...`)
    /// merge into the existing entry (see [`Builder::merge`]).
    pub fn load() -> Self {
        let dir = std::env::var("RIGOR_RBS_CORE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_CORE_DIR));

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

            let classes = builder.finish();
            if !classes.is_empty() {
                return Self { classes };
            }
        }
        // Fallback: no RBS dir (or nothing parsed) ⇒ hardcoded stub.
        Self::stub()
    }

    /// Whether the class is in the loaded set.
    pub fn knows_class(&self, class_name: &str) -> bool {
        self.classes.contains_key(class_name)
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
    ///       superclass chain); AND
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
    /// methods. Returns `(found, complete)`: `found` is whether `method` is a
    /// singleton method on the class or any superclass; `complete` is `false`
    /// if a referenced superclass is not in the loaded set (chain truncated),
    /// mirroring the completeness notion of [`Self::ancestors`].
    fn singleton_lookup(&self, class_name: &str, method: &str) -> (bool, bool) {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else {
                // Referenced superclass not loaded ⇒ chain incomplete.
                return (false, false);
            };
            if !seen.insert(key) {
                break; // Defensive: cycle guard.
            }
            if entry.singleton_methods.contains_key(method) {
                return (true, true);
            }
            cur = entry.superclass;
        }
        // Walked the whole superclass chain to a root (no super) without a hit.
        (false, true)
    }

    /// Resolve a method's return class over the ancestor chain (first defining
    /// ancestor wins), resolving through `alias` definitions. `None` if the
    /// return is not a known concrete class (or the method is unknown).
    pub fn method_return(&self, class_name: &str, method: &str) -> Option<&'static str> {
        let (chain, _) = self.ancestors(class_name);
        self.lookup_on_chain(&chain, method).and_then(|(ret, _)| ret)
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
                    // The stub lists `size` directly alongside `length`, so it
                    // needs no alias table; real RBS uses `alias size length`.
                    aliases: HashMap::new(),
                    // The stub doesn't model singleton methods (no class-method
                    // typo detection under the fallback); the surface gate stays
                    // conservative because the five base classes' singleton
                    // surface is incomplete here ⇒ always silent.
                    singleton_methods: HashMap::new(),
                    superclass,
                    includes,
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

        Self { classes }
    }
}

/// Accumulates parsed RBS declarations into per-class entries before flattening.
#[derive(Default)]
struct Builder {
    classes: HashMap<&'static str, ClassEntry>,
}

impl Builder {
    /// Parse one RBS source and fold its top-level class/module declarations in.
    fn ingest(&mut self, code: &str) {
        let Ok(sig) = parse(code) else {
            return;
        };
        for decl in sig.declarations().iter() {
            match decl {
                Node::Class(c) => self.ingest_class(&c),
                Node::Module(m) => self.ingest_module(&m),
                _ => {}
            }
        }
    }

    fn ingest_class(&mut self, c: &ClassNode) {
        let Some(name) = type_name_str(&c.name()) else {
            return;
        };
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

    fn ingest_module(&mut self, m: &ModuleNode) {
        let Some(name) = type_name_str(&m.name()) else {
            return;
        };
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
                Node::Alias(a) => {
                    // `alias new old` makes `new` an instance method equivalent
                    // to `old` (RBS `alias size length`). Record only instance
                    // aliases — the existence check is instance dispatch.
                    if a.kind() != AliasKind::Instance {
                        continue;
                    }
                    let new_name = intern(a.new_name().as_str());
                    let old_name = intern(a.old_name().as_str());
                    entry.aliases.entry(new_name).or_insert(old_name);
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
                Node::Class(nested) => self.ingest_class(&nested),
                Node::Module(nested) => self.ingest_module(&nested),
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
        for (k, v) in entry.singleton_methods {
            slot.singleton_methods.entry(k).or_insert(v);
        }
        for (new_name, old_name) in entry.aliases {
            slot.aliases.entry(new_name).or_insert(old_name);
        }
        for inc in entry.includes {
            if !slot.includes.contains(&inc) {
                slot.includes.push(inc);
            }
        }
    }

    /// Finish: apply implicit-`Object` superclass defaulting (every class except
    /// `BasicObject` and modules implicitly inherits `Object` when no `< X` was
    /// given) and return the class map.
    fn finish(mut self) -> HashMap<&'static str, ClassEntry> {
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
        self.classes
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
                builder.ingest(&code);
            }
        }
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
