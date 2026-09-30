//! The owned AST (`LoweredAst`), its file identity (`FileKey`), and the `lower`
//! / `lower_with_key` entry points (ADR-0012).

use crate::ruby_prism::ParseResult;

use super::{collect_const_mutations, Builder, ConstMutation, Node, NodeId, Span, StatementsKind};

/// Monotonic source of [`FileKey::Anonymous`]. One `lower()` call == one
/// pathless input, so a process-global counter gives every such lowering a
/// distinct identity — which is all a pathless caller (a test, a synthesized
/// buffer, stdin) can be given, and enough for two DIFFERENT pathless inputs
/// never to compare equal.
static NEXT_ANONYMOUS_FILE_KEY: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Stable identity of the FILE an AST was lowered from (issue #102).
///
/// Some analysis facts are per-FILE — the reference's in-source CONSTANT VALUE
/// table is rebuilt per file (`ScopeIndexer#build_in_source_constants` walks one
/// file's root), so a constant's harvested value may only be consumed at use
/// sites in the same file (`SourceIndex::literal_constant`, the C5 gate). That
/// gate needs an identity for "the same file", and the identity has to survive a
/// RE-LOWERING of the same bytes: the LSP lowers a buffer once on the diagnostics
/// worker and again on the hover path, and a per-`lower()`-call counter made
/// those two disagree (`FOO : 5` degraded to `FOO : Dynamic[top]` on a cache
/// hit). A persisted harvest would have the same problem across processes.
///
/// So the identity is the file's CANONICAL PATH wherever the caller has one, and
/// a fresh counter value only where it genuinely has none.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FileKey {
    /// A file with a filesystem identity, keyed by its canonical path — equal
    /// across lowerings, across threads, and (once harvests are persisted)
    /// across processes. `Arc` so the key is cheap to clone into every harvested
    /// constant.
    Path(std::sync::Arc<std::path::Path>),
    /// A lowering with no filesystem identity at all. Distinct for every such
    /// lowering, so two different pathless inputs never share a key.
    Anonymous(u64),
}

impl FileKey {
    /// The key for a file on disk: its canonical path, so `./a.rb`, `a.rb` and a
    /// symlink to it are ONE file. A path that cannot be canonicalized (the file
    /// was deleted between discovery and lowering, or the caller already resolved
    /// it against a parent that no longer holds the name) is kept VERBATIM rather
    /// than degraded to an anonymous key: a verbatim path still compares equal to
    /// itself across lowerings, which is the property the gate needs, and callers
    /// that pre-canonicalize (the LSP's held table, which resolves a deleted
    /// buffer's path via its parent) get exactly the spelling they computed.
    pub fn for_path(path: &std::path::Path) -> Self {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        FileKey::Path(std::sync::Arc::from(resolved.as_path()))
    }

    /// A fresh pathless key. Never equal to any other key, including another
    /// `anonymous()`.
    pub fn anonymous() -> Self {
        FileKey::Anonymous(
            NEXT_ANONYMOUS_FILE_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        )
    }
}

/// The owned AST: a flat arena of [`Node`]s plus the [`NodeId`] of the root
/// `Program`. Free of the Prism parse-buffer lifetime (ADR-0012).
#[derive(Clone)]
pub struct LoweredAst {
    nodes: Vec<Node>,
    root: NodeId,
    /// Identity of the FILE this AST was lowered from — see [`FileKey`]. Stable
    /// for the AST's lifetime and preserved by `Clone` (a clone is the same
    /// file).
    file_key: FileKey,
    /// Upstream #540 (`fc3b8b42`) — every site in this file that MUTATES a
    /// constant-shaped receiver. See [`ConstMutation`] for why this is a side
    /// table rather than owned nodes.
    const_mutations: Vec<ConstMutation>,
    /// Sorted start offsets of every `LocalVariableRead`, so
    /// [`LoweredAst::reads_local_within`] is a binary search, not an arena scan.
    local_read_starts: Vec<usize>,
    /// The spans of every [`StatementsKind::Inert`] carrier, for
    /// [`LoweredAst::in_inert_carrier`].
    inert_spans: Vec<Span>,
    /// The spans of every inert carrier whose operand an iterated body's
    /// content-writeback text scan covers — a subset of `inert_spans`
    /// (rigor-rs#312; [`LoweredAst::in_scanned_inert_carrier`]).
    scanned_inert_spans: Vec<Span>,
    /// Nodes a single-statement `(e)` parens was UNWRAPPED to. The reference
    /// reads the receiver's SYNTAX node — `(nil)` is a `ParenthesesNode`, not
    /// a `NilNode` — so a consumer that discriminates literal syntax (the
    /// `nil&.m` fold) must decline on these ids even though the inner node is
    /// a literal. Sorted for a binary-search read via
    /// [`LoweredAst::paren_unwrapped`].
    paren_unwrapped: Vec<u32>,
    /// `(arena id, bound names)` for recovered children a wrapper's recovery
    /// walk reached by CROSSING a `BlockNode`/`LambdaNode` — `super { |o| … }`
    /// lowers its `o` reads into a `Statements` carrier with no `Node::Call` to
    /// carry `block_locals`, so the closure's bound names are recorded here
    /// instead (rigor-rs#137, upstream rigor#1245). The names still shadow the
    /// enclosing scope for every node under the recovered child.
    closure_bindings: Vec<(u32, Vec<String>)>,
}

/// Hand-written so `{:?}` stays a CONTENT rendering: `file_key` is extrinsic
/// identity (a path, or a process-global counter), not content, and two
/// lowerings of the same bytes must still compare equal by `{:?}`. The LSP's
/// incremental-vs-full differential tests do exactly that comparison, and
/// including the key would make them fail on identical trees.
impl std::fmt::Debug for LoweredAst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoweredAst")
            .field("nodes", &self.nodes)
            .field("root", &self.root)
            .finish()
    }
}

impl LoweredAst {
    /// The identity of the file this AST was lowered from — see [`FileKey`]. Two
    /// ASTs compare equal here iff they name the same canonical path, or came
    /// from the same pathless `lower()` call (or a clone of it).
    pub fn file_key(&self) -> &FileKey {
        &self.file_key
    }

    /// Upstream #540's mutation census for this file, in walk order. See
    /// [`ConstMutation`].
    pub fn const_mutations(&self) -> &[ConstMutation] {
        &self.const_mutations
    }

    /// Whether some `LocalVariableRead` starts inside `span`. A read is a leaf,
    /// so starting inside a node's span means lying inside its subtree.
    pub fn reads_local_within(&self, (lo, hi): Span) -> bool {
        let i = self.local_read_starts.partition_point(|&s| s < lo);
        self.local_read_starts.get(i).is_some_and(|&s| s < hi)
    }

    /// Whether `span` lies inside a [`StatementsKind::Inert`] carrier (a
    /// `defined?` operand, an `END { }` / `BEGIN { }` body). A local write there
    /// does not exist for flow: the reference never evaluates it in sequence.
    pub fn in_inert_carrier(&self, span: Span) -> bool {
        self.inert_spans.iter().any(|s| s.0 <= span.0 && span.1 <= s.1)
    }

    /// Whether `span` lies inside an inert carrier that an iterated body's
    /// content-writeback TEXT SCAN covers (rigor-rs#312): `while w;
    /// super(h[:a] ||= 1); end` — the operand never evaluates (a local
    /// write there still binds nothing), but `loop_content_writeback`'s
    /// `NodeWalker` read finds the content mutation, so its `[]=`/mutator
    /// widening lands. `defined?` is never scanned — `NodeWalker` prunes
    /// the operand entirely.
    pub fn in_scanned_inert_carrier(&self, span: Span) -> bool {
        self.scanned_inert_spans
            .iter()
            .any(|s| s.0 <= span.0 && span.1 <= s.1)
    }

    /// Whether `id` is a node a `(e)` single-statement parens unwrapped to —
    /// the reference's syntax-level test (`ParenthesesNode` is not `NilNode`)
    /// a literal-discriminating consumer must honor. See
    /// [`Self::paren_unwrapped`].
    pub fn paren_unwrapped(&self, id: NodeId) -> bool {
        self.paren_unwrapped
            .binary_search(&id.0)
            .is_ok()
    }

    /// Every `(arena id, bound names)` of a recovered child lowered under a
    /// crossed `BlockNode`/`LambdaNode` — see the `closure_bindings` field.
    /// Consumers needing "is `id` the root of a shadowed subtree" test these;
    /// the names apply to `id` and every node reachable below it.
    pub fn closure_bindings(&self) -> &[(u32, Vec<String>)] {
        &self.closure_bindings
    }

    /// The bound names recorded for recovered child `id` — empty unless `id`
    /// was lowered under a crossed block/lambda. A flow pass descending `id`
    /// must first drop these names from its env: the local they name inside is
    /// the closure's own, never the shadowed outer binding.
    pub fn closure_bound_names(&self, id: NodeId) -> &[String] {
        self.closure_bindings
            .binary_search_by_key(&id.0, |(k, _)| *k)
            .ok()
            .map(|i| self.closure_bindings[i].1.as_slice())
            .unwrap_or(&[])
    }

    /// Resolve a handle to its owned node.
    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    /// The root `Program` node id.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Number of owned nodes in the arena.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the arena is empty. Never true after a successful [`lower`].
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Iterate `(NodeId, &Node)` over the arena in id order. Rules use this to
    /// walk every node in a single converged pass (ADR-0005).
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (NodeId(i as u32), n))
    }
}

/// Lower a borrowed Prism [`ParseResult`] into the owned, `NodeId`-indexed AST
/// (ADR-0012), with NO filesystem identity — the AST gets a fresh
/// [`FileKey::anonymous`]. Use [`lower_with_key`] whenever the caller knows which
/// file the bytes came from; a pathless key cannot match a re-lowering of the
/// same file, which is what the per-file constant gate compares.
pub fn lower(result: &ParseResult<'_>) -> LoweredAst {
    lower_with_key(result, FileKey::anonymous())
}

/// Lower a borrowed Prism [`ParseResult`] under an explicit [`FileKey`]. Walks
/// the tree once; unhandled Prism nodes become [`Node::Other`] so the walk is
/// total and never panics on a novel construct (ADR-0016 never-crash posture).
pub fn lower_with_key(result: &ParseResult<'_>, file_key: FileKey) -> LoweredAst {
    let source = result.source();
    // Byte offset of every line start, so a node's 1-based line is a binary
    // search (used to precompute Hash-key lines for `flow.duplicate-hash-key`,
    // whose message embeds the first occurrence's line — the rule is source-free).
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in source.iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let mut builder = Builder {
        nodes: Vec::new(),
        source,
        line_starts,
        paren_unwrapped: Vec::new(),
        closure_bindings: Vec::new(),
        recovery_joined: 0,
        recovery_blocked: 0,
        recovery_iterative: 0,
        recovery_next_sink: 0,
        recovery_suppressed: 0,
        typed_depth: 0,
        closure_depth: 0,
        scanned_inert_spans: Vec::new(),
    };
    let root_prism = result.node();
    let root = builder.lower_node(&root_prism);
    let const_mutations = collect_const_mutations(&root_prism);
    let mut local_read_starts: Vec<usize> = builder
        .nodes
        .iter()
        .filter(|n| matches!(n, Node::LocalVariableRead { .. }))
        .map(|n| n.span().0)
        .collect();
    local_read_starts.sort_unstable();
    let inert_spans: Vec<Span> = builder
        .nodes
        .iter()
        .filter_map(|n| match n {
            Node::Statements { kind: StatementsKind::Inert, span, .. } => Some(*span),
            _ => None,
        })
        .collect();
    let scanned_inert_spans = builder.scanned_inert_spans;
    let mut paren_unwrapped = builder.paren_unwrapped;
    paren_unwrapped.sort_unstable();
    let mut closure_bindings: Vec<(u32, Vec<String>)> = builder
        .closure_bindings
        .into_iter()
        .map(|(id, bound)| (id.0, bound))
        .collect();
    closure_bindings.sort_unstable_by_key(|(id, _)| *id);
    LoweredAst {
        nodes: builder.nodes,
        root,
        file_key,
        const_mutations,
        local_read_starts,
        inert_spans,
        scanned_inert_spans,
        paren_unwrapped,
        closure_bindings,
    }
}
