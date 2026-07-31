//! The RBS surface of a class the runtime BUILDS from `Data.define(...)` /
//! `Struct.new(...)` — its ancestry, its synthesised member accessors, and its
//! constructors (upstream `sig_gen/meta_class_shape.rb`, rigor#227).
//!
//! ## Why the members are not a completeness nicety
//!
//! Declaring the class without them is WORSE than not declaring it at all.
//! `::Data`'s own RBS declares `def self.new: () -> bot` and `::Struct`'s
//! declares only the `Struct.new("Name", :a, :b)` factory, so a subclass that
//! carries no `.new` of its own turns every `Point.new(1, 2)` into a
//! false-positive arity error — where the UNdeclared class it replaced merely
//! typed `Dynamic` and reported nothing. The same reasoning covers the member
//! readers and `.[]` (`Point[1, 2]`, which `::Data`'s RBS does not declare at
//! all): narrowing dispatch from `Dynamic` to a nominal class means everything
//! the runtime synthesises must be declared, or it reads as missing.
//!
//! ## Deliberate divergence from upstream: the layout source
//!
//! Upstream reads the ADR-48 member layouts the analyser's `ScopeIndexer`
//! already builds, so sig-gen's view of a value class cannot drift from the
//! checker's. rigor-rs has no ported equivalent — neither `SourceIndex` nor
//! `CoreIndex` carries a member layout — so this module keeps sig-gen's own
//! recogniser and walks Prism directly. Its RULES are ported one-for-one from
//! `ScopeIndexer#build_data_member_layouts` / `#build_struct_member_layouts`
//! (both spellings, a `::Data` receiver, literal-Symbol members only, the
//! `keyword_init:` flag) so the two agree TODAY; when a layout table lands in
//! `rigor-infer`, [`collect`] is the single call site to re-point.

use rigor_parse::ruby_prism::{
    self, CallNode, ClassNode, ConstantWriteNode, ModuleNode, Node as PrismNode, ParseResult, Visit,
};

/// Which runtime factory built the class. Drives the ancestry, the writers, and
/// the constructor's required-vs-optional positions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MetaKind {
    Data,
    Struct,
}

impl MetaKind {
    /// The emitted superclass. `::Struct` is generic (`class Struct[E]`) and RBS
    /// rejects a bare `< ::Struct` with `InvalidTypeApplicationError`; `untyped`
    /// is the only element type a member layout can justify.
    pub fn superclass(self) -> &'static str {
        match self {
            MetaKind::Data => "::Data",
            MetaKind::Struct => "::Struct[untyped]",
        }
    }
}

/// One class's member layout: the ordered member names plus the flag that
/// decides which constructor forms the class actually accepts.
#[derive(Debug)]
pub struct MetaLayout {
    pub kind: MetaKind,
    pub members: Vec<String>,
    /// The `Struct.new(..., keyword_init: true)` flag; always `false` for `Data`.
    pub keyword_init: bool,
}

/// The file's layouts keyed by fully-qualified class name, in EMISSION order.
///
/// Upstream merges two separate `ScopeIndexer` tables — every `Data.define`
/// class, then every `Struct.new` one — into one Ruby Hash, so the emitted
/// order is data-then-struct with each group in source order, and a re-assigned
/// constant keeps its FIRST position with its LAST value. This ordered map
/// reproduces exactly that (`insert` replaces in place).
#[derive(Default, Debug)]
pub struct MetaLayouts(Vec<(String, MetaLayout)>);

impl MetaLayouts {
    fn insert(&mut self, name: String, layout: MetaLayout) {
        match self.0.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = layout,
            None => self.0.push((name, layout)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &MetaLayout)> {
        self.0.iter().map(|(n, l)| (n.as_str(), l))
    }

    pub fn get(&self, name: &str) -> Option<&MetaLayout> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, l)| l)
    }
}

/// Every `Data.define` / `Struct.new` class in the parsed file, qualified by its
/// lexical namespace. Two passes (data, then struct) so the emission order
/// matches upstream's two-table merge.
pub fn collect(result: &ParseResult<'_>) -> MetaLayouts {
    let mut out = MetaLayouts::default();
    for kind in [MetaKind::Data, MetaKind::Struct] {
        let mut walker = LayoutWalker { kind, prefix: Vec::new(), out: &mut out };
        walker.visit(&result.node());
    }
    out
}

/// Walks the Prism tree recording every layout of ONE kind. Mirrors
/// `ScopeIndexer#walk_data_member_layouts`: a class records its SUPERCLASS
/// expression (the `class Point < Data.define(:x, :y)` spelling) then descends
/// into its body only, while a constant write records its rvalue AND still
/// recurses generically — a `Data.define` nested inside a block or a method body
/// belongs to the ENCLOSING namespace, exactly as the runtime binds it.
struct LayoutWalker<'a> {
    kind: MetaKind,
    prefix: Vec<String>,
    out: &'a mut MetaLayouts,
}

impl LayoutWalker<'_> {
    /// Record `expr` as this walker's kind of layout under the current prefix.
    fn record(&mut self, expr: &PrismNode<'_>) {
        let Some(call) = expr.as_call_node() else { return };
        let matches = match self.kind {
            MetaKind::Data => is_data_define(&call),
            MetaKind::Struct => is_struct_new(&call),
        };
        if !matches {
            return;
        }
        let members = member_names(&call, self.kind);
        // A degenerate `Data.define` / `Struct.new("Name", :a)` (no literal-Symbol
        // members) declares nothing this module can describe — upstream records no
        // layout, so no class is emitted for it at all.
        if members.is_empty() {
            return;
        }
        let keyword_init = self.kind == MetaKind::Struct && struct_keyword_init(&call);
        self.out.insert(self.prefix.join("::"), MetaLayout { kind: self.kind, members, keyword_init });
    }
}

impl<'pr> Visit<'pr> for LayoutWalker<'_> {
    fn visit_class_node(&mut self, node: &ClassNode<'pr>) {
        let Some(name) = constant_path_name(&node.constant_path()) else {
            ruby_prism::visit_class_node(self, node);
            return;
        };
        self.prefix.push(name);
        if let Some(superclass) = node.superclass() {
            self.record(&superclass);
        }
        if let Some(body) = node.body() {
            self.visit(&body);
        }
        self.prefix.pop();
    }

    fn visit_module_node(&mut self, node: &ModuleNode<'pr>) {
        let Some(name) = constant_path_name(&node.constant_path()) else {
            ruby_prism::visit_module_node(self, node);
            return;
        };
        self.prefix.push(name);
        if let Some(body) = node.body() {
            self.visit(&body);
        }
        self.prefix.pop();
    }

    fn visit_constant_write_node(&mut self, node: &ConstantWriteNode<'pr>) {
        self.prefix.push(String::from_utf8_lossy(node.name().as_slice()).into_owned());
        self.record(&node.value());
        self.prefix.pop();
        ruby_prism::visit_constant_write_node(self, node);
    }
}

/// `Data.define(*Symbol)` / `::Data.define(*Symbol)`. EVERY argument must be a
/// literal Symbol — a splat or a keyword hash disqualifies the whole call, since
/// the member list would then be a guess.
fn is_data_define(call: &CallNode<'_>) -> bool {
    call_name(call) == "define"
        && meta_constant_receiver(call, "Data")
        && call_args(call).iter().all(|a| a.as_symbol_node().is_some())
}

/// `Struct.new(*Symbol [, keyword_init: <expr>])`. The trailing keyword hash is
/// accepted but contributes no member; every positional MUST be a literal Symbol,
/// which is what rejects the `Struct.new("Legacy", :a)` named-factory form.
fn is_struct_new(call: &CallNode<'_>) -> bool {
    if call_name(call) != "new" || !meta_constant_receiver(call, "Struct") {
        return false;
    }
    let args = call_args(call);
    let positional = struct_positionals(&args);
    !positional.is_empty() && positional.iter().all(|a| a.as_symbol_node().is_some())
}

/// True only for a LITERAL `keyword_init: true`. Anything else — absent, a
/// literal `false`, a non-literal expression — reads `false`; see
/// [`constructor_overloads`] for why that reading is the safe one.
fn struct_keyword_init(call: &CallNode<'_>) -> bool {
    let args = call_args(call);
    let Some(hash) = args.last().and_then(|a| a.as_keyword_hash_node()) else {
        return false;
    };
    hash.elements().iter().any(|element| {
        element.as_assoc_node().is_some_and(|assoc| {
            assoc.key().as_symbol_node().is_some_and(|k| k.unescaped() == b"keyword_init")
                && assoc.value().as_true_node().is_some()
        })
    })
}

/// The ordered literal-Symbol member names of a recognised call.
fn member_names(call: &CallNode<'_>, kind: MetaKind) -> Vec<String> {
    let args = call_args(call);
    let symbols = match kind {
        MetaKind::Data => args.as_slice(),
        MetaKind::Struct => struct_positionals(&args),
    };
    symbols
        .iter()
        .filter_map(|a| {
            a.as_symbol_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
        })
        .collect()
}

/// A `Struct.new` argument list minus its trailing `keyword_init:` hash.
fn struct_positionals<'a, 'pr>(args: &'a [PrismNode<'pr>]) -> &'a [PrismNode<'pr>] {
    match args.last() {
        Some(last) if last.as_keyword_hash_node().is_some() => &args[..args.len() - 1],
        _ => args,
    }
}

/// The receiver must be the BARE constant (`Data`) or its top-level path
/// (`::Data`) — any other receiver's identity is not statically known.
fn meta_constant_receiver(call: &CallNode<'_>, expected: &str) -> bool {
    let Some(receiver) = call.receiver() else { return false };
    if let Some(read) = receiver.as_constant_read_node() {
        return read.name().as_slice() == expected.as_bytes();
    }
    if let Some(path) = receiver.as_constant_path_node() {
        return path.parent().is_none()
            && path.name().is_some_and(|n| n.as_slice() == expected.as_bytes());
    }
    false
}

fn call_name(call: &CallNode<'_>) -> String {
    String::from_utf8_lossy(call.name().as_slice()).into_owned()
}

fn call_args<'pr>(call: &CallNode<'pr>) -> Vec<PrismNode<'pr>> {
    call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default()
}

/// The written name of a `class` / `module` constant path (`Foo`, `Foo::Bar`), or
/// `None` for a dynamic path — mirrors `Source::ConstantPath.qualified_name`.
fn constant_path_name(node: &PrismNode<'_>) -> Option<String> {
    if let Some(read) = node.as_constant_read_node() {
        return Some(String::from_utf8_lossy(read.name().as_slice()).into_owned());
    }
    let path = node.as_constant_path_node()?;
    let last = String::from_utf8_lossy(path.name()?.as_slice()).into_owned();
    match path.parent() {
        Some(parent) => Some(format!("{}::{last}", constant_path_name(&parent)?)),
        None => Some(last),
    }
}

// ---------------------------------------------------------------------------
// The emitted shape (upstream `MetaClassShape`)
// ---------------------------------------------------------------------------

/// One RBS line the layout contributes, with the identity sig-gen needs to
/// classify it against the project's own signatures.
pub struct MetaMember {
    pub method_name: String,
    /// `"instance"` or `"singleton"`, matching [`super::Candidate::kind`].
    pub kind: &'static str,
    pub rbs: String,
}

/// Every accessor and constructor the runtime synthesises for `layout`.
///
/// Member types are always `untyped`: upstream types them from
/// `--params=observed` call sites, and that path is substrate-blocked in this
/// port (`docs/notes/20260711-siggen-params-observed-substrate-blocked.md`), so
/// the observed map is empty for BOTH tools here.
pub fn member_decls(layout: &MetaLayout) -> Vec<MetaMember> {
    let mut out: Vec<MetaMember> = layout
        .members
        .iter()
        .map(|member| MetaMember {
            method_name: member.clone(),
            kind: "instance",
            rbs: format!("def {member}: () -> untyped"),
        })
        .collect();
    // A Struct's members are mutable, so each one contributes a writer too. The
    // writer returns the assigned value's type — Ruby's assignment semantics.
    if layout.kind == MetaKind::Struct {
        out.extend(layout.members.iter().map(|member| MetaMember {
            method_name: format!("{member}="),
            kind: "instance",
            rbs: format!("def {member}=: (untyped) -> untyped"),
        }));
    }
    // `.new` and its `.[]` alias share one overload list.
    let overloads = constructor_overloads(layout).join(" | ");
    for name in ["new", "[]"] {
        out.push(MetaMember {
            method_name: name.to_string(),
            kind: "singleton",
            rbs: format!("def self.{name}: {overloads}"),
        });
    }
    out
}

/// `Data` requires every member; a Struct fills a missing one with `nil`, so its
/// positions are all optional.
///
/// `keyword_init: true` accepts keyword arguments ONLY. Every other layout gets
/// BOTH forms, because the flag reads `false` for an ABSENT `keyword_init:` as
/// well as for a literal `keyword_init: false` — and since Ruby 3.2 the absent
/// case, by far the dominant one, accepts both. Emitting both is the
/// false-positive-free reading of an ambiguity the layout cannot resolve.
fn constructor_overloads(layout: &MetaLayout) -> Vec<String> {
    let optional = if layout.kind == MetaKind::Struct { "?" } else { "" };
    let keyword = format!(
        "({}) -> instance",
        layout
            .members
            .iter()
            .map(|m| format!("{optional}{m}: untyped"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if layout.kind == MetaKind::Struct && layout.keyword_init {
        return vec![keyword];
    }
    let positional = format!(
        "({}) -> instance",
        layout
            .members
            .iter()
            .map(|m| format!("{optional}untyped {m}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    vec![keyword, positional]
}
