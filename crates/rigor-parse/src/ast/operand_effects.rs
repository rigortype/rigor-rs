//! `OperandEffects` (`reference/rigor/lib/rigor/inference/operand_effects.rb`)
//! plus the `thread_operand` disposition
//! (`reference/rigor/lib/rigor/inference/statement_evaluator.rb:2509-2535`):
//! whether an operand ELEMENT of a typed position — a call's receiver or
//! argument, a container element, a splat's contents, a `#{…}` part, a
//! range bound, a `rescue`-modifier operand — still gets *evaluated*.
//!
//! `thread_operand` gates each element on `OperandEffects.any?`: only a
//! subtree carrying an effect that OUTLIVES the operand is `evaluate`d —
//! a local write whose `depth` reaches the operand's own scope
//! (`CapturedLocals::LOCAL_WRITE_NODES`), an ivar/cvar/gvar write, a
//! compound index write (`[]=` widens its receiver), a `next`/`break`
//! that can still target its construct, or a `SHAPE_MUTATORS` call on an
//! outliving receiver (`outliving_mutation?`). An element with none is
//! `type_of`'d whole — no `eval_*` handler runs inside it — so a compound
//! ATTRIBUTE write's `widen_attribute_write` never lands there
//! (rigor-rs#361; `OperandEffects` deliberately excludes the
//! `Call*WriteNode`s themselves, rigor-rs#343).
//!
//! A gated-in element then dispatches like `thread_operand`: a CallNode
//! runs `call_effects` (its own operand positions re-gate), a value
//! container or sequence threads each child, anything else with an
//! `eval_*` handler is `operand.evaluate`d whole, and an effect-bearing
//! node with no handler (`super`, `yield`, `BEGIN`/`END`) is dropped.

use crate::ruby_prism::{self, Node as PrismNode, Visit};

use crate::mutators::is_shape_mutator;

/// How `thread_operand` reaches one operand element — the lowering-time
/// decision [`Builder::lower_typed_operand`] applies.
///
/// [`Builder::lower_typed_operand`]: super::Builder::lower_typed_operand
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperandMode {
    /// Never evaluated: `OperandEffects.any?` is false (`type_of` — the
    /// subtree only types) or the node carries an effect but has no
    /// `eval_*` handler and is not a container/sequence/call
    /// (`thread_operand`'s `return entry`). A compound attribute write
    /// inside keeps `evaluated: false`.
    Dead,
    /// An effect-bearing `CallNode` (`call_effects` → `call_operand_scope`
    /// threads receiver/arguments/block-pass) or a value container /
    /// sequence (`thread_operand_children`): each operand child is
    /// re-gated in turn.
    Gate,
    /// An effect-bearing node with an `eval_*` handler (`HANDLERS.key?`):
    /// `operand.evaluate` runs it whole, so its subtree is a normal
    /// evaluated context again.
    Eval,
}

/// `statement_evaluator.rb:2509` — the dispatch `thread_operand` applies to
/// one operand element once the enclosing position was taken.
pub(crate) fn operand_mode(node: &PrismNode<'_>) -> OperandMode {
    if !any(node) {
        return OperandMode::Dead;
    }
    // `operand.send(:call_effects, node, …)` — receiver, arguments and the
    // `&` block-pass thread; the literal block is `evaluate_block_if_present`'s.
    if node.as_call_node().is_some() {
        return OperandMode::Gate;
    }
    if is_operand_container(node) {
        return OperandMode::Gate;
    }
    if is_operand_evaluated(node) {
        return OperandMode::Eval;
    }
    OperandMode::Dead
}

/// `OperandEffects.any?` — `effect?(node, 0, jumps: true)`.
pub(crate) fn any(node: &PrismNode<'_>) -> bool {
    let mut scan = EffectScan {
        found: false,
        nesting: 0,
        jumps: true,
    };
    scan.visit(node);
    scan.found
}

/// `OPERAND_CONTAINERS` + `OPERAND_SEQUENCES`
/// (`statement_evaluator.rb:2498-2505`) — the node classes `thread_operand`
/// descends child-by-child.
fn is_operand_container(node: &PrismNode<'_>) -> bool {
    matches!(
        node,
        // `OPERAND_CONTAINERS`.
        PrismNode::ArgumentsNode { .. }
        | PrismNode::KeywordHashNode { .. }
        | PrismNode::AssocNode { .. }
        | PrismNode::AssocSplatNode { .. }
        | PrismNode::SplatNode { .. }
        | PrismNode::BlockArgumentNode { .. }
        | PrismNode::ArrayNode { .. }
        | PrismNode::HashNode { .. }
        | PrismNode::InterpolatedStringNode { .. }
        | PrismNode::InterpolatedSymbolNode { .. }
        | PrismNode::InterpolatedXStringNode { .. }
        | PrismNode::EmbeddedStatementsNode { .. }
        | PrismNode::RangeNode { .. }
        // `OPERAND_SEQUENCES`.
        | PrismNode::StatementsNode { .. }
        | PrismNode::ParenthesesNode { .. }
    )
}

/// `thread_operand`'s `HANDLERS.key?(node.class)` (`statement_evaluator.rb`
/// `HANDLERS` table, pin `e59b7b89`) — every node class `operand.evaluate`
/// can run. The containers/sequences/CallNode above return before this is
/// consulted; the classes are listed anyway so the table stays a verbatim
/// census. (Constants'`AndWrite`/`Target` kinds and `DefinedNode`,
/// `SuperNode`, `YieldNode`, `PreExecutionNode`, `PostExecutionNode`, …
/// are NOT in it — an effect-bearing one returns `entry` unevaluated.)
fn is_operand_evaluated(node: &PrismNode<'_>) -> bool {
    matches!(
        node,
        PrismNode::StatementsNode { .. }
        | PrismNode::ProgramNode { .. }
        | PrismNode::LocalVariableWriteNode { .. }
        | PrismNode::LocalVariableOrWriteNode { .. }
        | PrismNode::LocalVariableAndWriteNode { .. }
        | PrismNode::LocalVariableOperatorWriteNode { .. }
        | PrismNode::InstanceVariableWriteNode { .. }
        | PrismNode::InstanceVariableOrWriteNode { .. }
        | PrismNode::InstanceVariableAndWriteNode { .. }
        | PrismNode::InstanceVariableOperatorWriteNode { .. }
        | PrismNode::ClassVariableWriteNode { .. }
        | PrismNode::ClassVariableOrWriteNode { .. }
        | PrismNode::ClassVariableAndWriteNode { .. }
        | PrismNode::ClassVariableOperatorWriteNode { .. }
        | PrismNode::GlobalVariableWriteNode { .. }
        | PrismNode::GlobalVariableOrWriteNode { .. }
        | PrismNode::GlobalVariableAndWriteNode { .. }
        | PrismNode::GlobalVariableOperatorWriteNode { .. }
        | PrismNode::IndexOrWriteNode { .. }
        | PrismNode::IndexAndWriteNode { .. }
        | PrismNode::IndexOperatorWriteNode { .. }
        | PrismNode::CallOrWriteNode { .. }
        | PrismNode::CallAndWriteNode { .. }
        | PrismNode::CallOperatorWriteNode { .. }
        | PrismNode::MultiWriteNode { .. }
        | PrismNode::ConstantWriteNode { .. }
        | PrismNode::ConstantPathWriteNode { .. }
        | PrismNode::ConstantOrWriteNode { .. }
        | PrismNode::ConstantPathOrWriteNode { .. }
        | PrismNode::IfNode { .. }
        | PrismNode::UnlessNode { .. }
        | PrismNode::ElseNode { .. }
        | PrismNode::CaseNode { .. }
        | PrismNode::CaseMatchNode { .. }
        | PrismNode::WhenNode { .. }
        | PrismNode::InNode { .. }
        | PrismNode::BeginNode { .. }
        | PrismNode::RescueNode { .. }
        | PrismNode::EnsureNode { .. }
        | PrismNode::WhileNode { .. }
        | PrismNode::UntilNode { .. }
        | PrismNode::ForNode { .. }
        | PrismNode::AndNode { .. }
        | PrismNode::OrNode { .. }
        | PrismNode::ParenthesesNode { .. }
        | PrismNode::DefNode { .. }
        | PrismNode::ClassNode { .. }
        | PrismNode::ModuleNode { .. }
        | PrismNode::SingletonClassNode { .. }
        | PrismNode::CallNode { .. }
        | PrismNode::BlockNode { .. }
        | PrismNode::LambdaNode { .. }
        | PrismNode::ReturnNode { .. }
        | PrismNode::NextNode { .. }
        | PrismNode::BreakNode { .. }
        | PrismNode::MatchWriteNode { .. }
        | PrismNode::MatchPredicateNode { .. }
        | PrismNode::MatchRequiredNode { .. }
        | PrismNode::RescueModifierNode { .. }
        | PrismNode::ArrayNode { .. }
        | PrismNode::HashNode { .. }
        | PrismNode::InterpolatedStringNode { .. }
        | PrismNode::InterpolatedSymbolNode { .. }
        | PrismNode::InterpolatedXStringNode { .. }
        | PrismNode::RangeNode { .. }
    )
}

/// One `effect?` walk — `found` is the `||=` accumulator; `nesting` counts
/// the `BlockNode`/`LambdaNode` scopes crossed inside the operand
/// (`SCOPE_NODES`); `jumps` is `false` once a `JUMP_BOUNDARY_NODES` member
/// is crossed, disabling `next`/`break` under it (`JumpTargets.any?`
/// semantics folded into the same walk).
struct EffectScan {
    found: bool,
    nesting: u32,
    jumps: bool,
}

/// One `OUTLIVING_WRITE_NODES`/`LOCAL_WRITE_NODES` effect check plus the
/// `nesting`-relative `depth` test — shared by the five local write kinds.
macro_rules! local_write_effect {
    ($method:ident, $free:ident, $ty:ident) => {
        fn $method(&mut self, node: &ruby_prism::$ty<'pr>) {
            if self.found {
                return;
            }
            // `return true if LOCAL_WRITE_NODES.include?(klass) &&
            // node.depth >= nesting` — a write binding INSIDE a crossed
            // block/lambda (`depth < nesting`) is not outliving, but its
            // VALUE subtree is still scanned.
            if node.depth() >= self.nesting {
                self.found = true;
                return;
            }
            ruby_prism::$free(self, node);
        }
    };
}

/// The non-local writes — `OUTLIVING_WRITE_NODES` (ivar/cvar/gvar of every
/// write kind, targets included) and `OUTLIVING_INDEX_WRITE_NODES`
/// (`h[k] ||= …` etc. — their `[]=` store widens the receiver binding).
/// Once one is found the subtree no longer matters.
macro_rules! outliving_write_effect {
    ($method:ident, $ty:ident) => {
        fn $method(&mut self, _node: &ruby_prism::$ty<'pr>) {
            self.found = true;
        }
    };
}

impl<'pr> Visit<'pr> for EffectScan {
    local_write_effect!(
        visit_local_variable_write_node,
        visit_local_variable_write_node,
        LocalVariableWriteNode
    );
    local_write_effect!(
        visit_local_variable_or_write_node,
        visit_local_variable_or_write_node,
        LocalVariableOrWriteNode
    );
    local_write_effect!(
        visit_local_variable_and_write_node,
        visit_local_variable_and_write_node,
        LocalVariableAndWriteNode
    );
    local_write_effect!(
        visit_local_variable_operator_write_node,
        visit_local_variable_operator_write_node,
        LocalVariableOperatorWriteNode
    );
    local_write_effect!(
        visit_local_variable_target_node,
        visit_local_variable_target_node,
        LocalVariableTargetNode
    );

    outliving_write_effect!(visit_instance_variable_write_node, InstanceVariableWriteNode);
    outliving_write_effect!(visit_instance_variable_or_write_node, InstanceVariableOrWriteNode);
    outliving_write_effect!(
        visit_instance_variable_and_write_node,
        InstanceVariableAndWriteNode
    );
    outliving_write_effect!(
        visit_instance_variable_operator_write_node,
        InstanceVariableOperatorWriteNode
    );
    outliving_write_effect!(visit_instance_variable_target_node, InstanceVariableTargetNode);
    outliving_write_effect!(visit_class_variable_write_node, ClassVariableWriteNode);
    outliving_write_effect!(visit_class_variable_or_write_node, ClassVariableOrWriteNode);
    outliving_write_effect!(visit_class_variable_and_write_node, ClassVariableAndWriteNode);
    outliving_write_effect!(
        visit_class_variable_operator_write_node,
        ClassVariableOperatorWriteNode
    );
    outliving_write_effect!(visit_class_variable_target_node, ClassVariableTargetNode);
    outliving_write_effect!(visit_global_variable_write_node, GlobalVariableWriteNode);
    outliving_write_effect!(visit_global_variable_or_write_node, GlobalVariableOrWriteNode);
    outliving_write_effect!(visit_global_variable_and_write_node, GlobalVariableAndWriteNode);
    outliving_write_effect!(
        visit_global_variable_operator_write_node,
        GlobalVariableOperatorWriteNode
    );
    outliving_write_effect!(visit_global_variable_target_node, GlobalVariableTargetNode);
    outliving_write_effect!(visit_index_or_write_node, IndexOrWriteNode);
    outliving_write_effect!(visit_index_and_write_node, IndexAndWriteNode);
    outliving_write_effect!(visit_index_operator_write_node, IndexOperatorWriteNode);

    /// `JUMP_NODES`: `next`/`break` count only while `jumps` still holds —
    /// a nested `JumpTargets`-boundary (block/lambda, `def`/class/module/
    /// `class <<`, `while`/`until`/`for`) retargets them, so they no longer
    /// leave the operand. Their arguments are still scanned for writes.
    fn visit_next_node(&mut self, node: &ruby_prism::NextNode<'pr>) {
        if self.found {
            return;
        }
        if self.jumps {
            self.found = true;
            return;
        }
        ruby_prism::visit_next_node(self, node);
    }

    fn visit_break_node(&mut self, node: &ruby_prism::BreakNode<'pr>) {
        if self.found {
            return;
        }
        if self.jumps {
            self.found = true;
            return;
        }
        ruby_prism::visit_break_node(self, node);
    }

    /// A call counts when it is a `SHAPE_MUTATORS` method on a receiver
    /// whose mutation outlives the operand (`outliving_mutation?`);
    /// otherwise its operand positions still get scanned for writes.
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if self.found {
            return;
        }
        if outliving_mutation(node, self.nesting) {
            self.found = true;
            return;
        }
        ruby_prism::visit_call_node(self, node);
    }

    /// `OPAQUE_NODES` — `defined?` inspects its operand statically, and a
    /// `def`/`class`/`module`/`class <<` body's effects are scoped to it.
    fn visit_defined_node(&mut self, _node: &ruby_prism::DefinedNode<'pr>) {}
    fn visit_def_node(&mut self, _node: &ruby_prism::DefNode<'pr>) {}
    fn visit_class_node(&mut self, _node: &ruby_prism::ClassNode<'pr>) {}
    fn visit_module_node(&mut self, _node: &ruby_prism::ModuleNode<'pr>) {}
    fn visit_singleton_class_node(&mut self, _node: &ruby_prism::SingletonClassNode<'pr>) {}

    /// `SCOPE_NODES` — a crossed literal block or lambda lifts `nesting`
    /// (only writes that reach past it outlive) and is a jump boundary.
    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        if self.found {
            return;
        }
        let (nesting, jumps) = (self.nesting, self.jumps);
        self.nesting += 1;
        self.jumps = false;
        ruby_prism::visit_block_node(self, node);
        self.nesting = nesting;
        self.jumps = jumps;
    }

    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        if self.found {
            return;
        }
        let (nesting, jumps) = (self.nesting, self.jumps);
        self.nesting += 1;
        self.jumps = false;
        ruby_prism::visit_lambda_node(self, node);
        self.nesting = nesting;
        self.jumps = jumps;
    }

    /// The remaining `JUMP_BOUNDARY_NODES` — `while`/`until`/`for` bodies
    /// retarget `next`/`break` without opening a local scope.
    /// (`def`/`class`/`module`/`class <<` are boundaries too, but they are
    /// opaque above — `effect?` never descends into one.)
    fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
        if self.found {
            return;
        }
        let jumps = self.jumps;
        self.jumps = false;
        ruby_prism::visit_while_node(self, node);
        self.jumps = jumps;
    }

    fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
        if self.found {
            return;
        }
        let jumps = self.jumps;
        self.jumps = false;
        ruby_prism::visit_until_node(self, node);
        self.jumps = jumps;
    }

    fn visit_for_node(&mut self, node: &ruby_prism::ForNode<'pr>) {
        if self.found {
            return;
        }
        let jumps = self.jumps;
        self.jumps = false;
        ruby_prism::visit_for_node(self, node);
        self.jumps = jumps;
    }
}

/// `outliving_mutation?` (`operand_effects.rb:39`) — a `SHAPE_MUTATORS`
/// call on a receiver whose mutation outlives the operand. `element_read_path`
/// first: an element-read chain (`arr[i]`, `arr.first`, `arr.last`,
/// `arr.dig(...)`) rooted at a local outlives when the root does
/// (`element_read_widening.rb`); otherwise `mutated_reads` asks the
/// receiver-alias walk whether the expression can mutate a readable local
/// or ivar — cvar/gvar reads and writes are unconditionally outliving.
fn outliving_mutation(call: &ruby_prism::CallNode<'_>, nesting: u32) -> bool {
    if !is_shape_mutator(&String::from_utf8_lossy(call.name().as_slice())) {
        return false;
    }
    let Some(receiver) = call.receiver() else {
        return false;
    };
    // `return false if !receiver || receiver.is_a?(Prism::SelfNode)`.
    if receiver.as_self_node().is_some() {
        return false;
    }
    if let Some(depth) = element_read_root(&receiver) {
        return depth >= nesting;
    }
    mutated_reads_outliving(receiver, nesting)
}

/// `element_read_path` reduced to the outliving question: walk the
/// `[]`/`first`/`last`/`dig` chain (`element_read_step` — a block or a
/// non-positional arity ends it, as does any non-element-read link) and,
/// when it bottoms out at a `LocalVariableReadNode`, answer that read's
/// `depth`. Anything else answers `None` — no path — and the caller falls
/// back to `mutated_reads`.
fn element_read_root(node: &PrismNode<'_>) -> Option<u32> {
    let mut call = node.as_call_node()?;
    let mut cursor;
    loop {
        if !element_read_step(&call) {
            return None;
        }
        // `element_read_step` guarantees a receiver.
        cursor = call.receiver()?;
        match cursor.as_call_node() {
            Some(next) => call = next,
            None => break,
        }
    }
    cursor
        .as_local_variable_read_node()
        .map(|read| read.depth())
}

/// `element_read_step` (`element_read_widening.rb:87`): `[]` with exactly
/// one positional argument, `first`/`last` with none, `dig` with at least
/// one — a block or a missing receiver makes the call something else.
fn element_read_step(call: &ruby_prism::CallNode<'_>) -> bool {
    if call.receiver().is_none() || call.block().is_some() {
        return false;
    }
    let args = call
        .arguments()
        .map(|a| a.arguments().len())
        .unwrap_or(0);
    match call.name().as_slice() {
        b"[]" => args == 1,
        b"first" | b"last" => args == 0,
        b"dig" => args >= 1,
        _ => false,
    }
}

/// `mutated_reads(receiver).any? { |read| outliving_read?(read, nesting) }`
/// (`receiver_alias.rb`) — the boolean question `outliving_mutation?` asks.
fn mutated_reads_outliving(receiver: PrismNode<'_>, nesting: u32) -> bool {
    // `direct` unwraps `ParenthesesNode`→`StatementsNode` shells to the last
    // statement — a cvar/gvar read there, or a cvar/gvar WRITE (which
    // `mutated_reads` answers with a non-local read of itself), always
    // outlives the operand. `None` tracks "still `receiver`"; `nil` (an
    // empty statements body — `NON_ALIASED_*` decline it either way) is
    // `direct_nil`.
    let mut direct: Option<PrismNode<'_>> = None;
    let mut direct_nil = false;
    loop {
        let cur: &PrismNode = direct.as_ref().unwrap_or(&receiver);
        let Some(parens) = cur.as_parentheses_node() else {
            break;
        };
        // `direct.body.is_a?(StatementsNode)` must hold to unwrap.
        let Some(statements) = parens.body().and_then(|b| b.as_statements_node()) else {
            break;
        };
        match statements.body().iter().last() {
            Some(last) => direct = Some(last),
            None => {
                direct_nil = true;
                break;
            }
        }
    }
    let direct = direct.as_ref().unwrap_or(&receiver);
    if !direct_nil
        && (direct.as_class_variable_read_node().is_some()
            || direct.as_global_variable_read_node().is_some()
            || direct.as_class_variable_write_node().is_some()
            || direct.as_class_variable_or_write_node().is_some()
            || direct.as_class_variable_and_write_node().is_some()
            || direct.as_class_variable_operator_write_node().is_some()
            || direct.as_global_variable_write_node().is_some()
            || direct.as_global_variable_or_write_node().is_some()
            || direct.as_global_variable_and_write_node().is_some()
            || direct.as_global_variable_operator_write_node().is_some())
    {
        return true;
    }
    candidates_outliving(Some(receiver), nesting, 0)
}

/// `candidates(node).any? { |read| outliving_read?(read, nesting) }`
/// folded into one walk (`receiver_alias.rb:36` `candidates` + `operand_effects.rb`
/// `outliving_read?`): the unbounded `WALK_DEPTH_CAP` recursion through
/// parens/statements/`else`/`if`/`unless`/`and`/`or`, where a local write
/// or read outlives when its `depth` reaches `nesting`, an `it` read only
/// at `nesting == 0`, and an ivar write or read always.
fn candidates_outliving(node: Option<PrismNode<'_>>, nesting: u32, depth: u32) -> bool {
    const WALK_DEPTH_CAP: u32 = 6;
    let Some(node) = node else {
        return false;
    };
    if depth > WALK_DEPTH_CAP {
        return false;
    }
    // `variable_write?` → `read_of` before the structural cases.
    if let Some(write_depth) = local_write_depth(&node) {
        return write_depth >= nesting;
    }
    if is_instance_variable_write(&node) {
        return true;
    }
    if let Some(read) = node.as_local_variable_read_node() {
        return read.depth() >= nesting;
    }
    if node.as_it_local_variable_read_node().is_some() {
        return nesting == 0;
    }
    if node.as_instance_variable_read_node().is_some() {
        return true;
    }
    if let Some(parens) = node.as_parentheses_node() {
        return candidates_outliving(parens.body(), nesting, depth + 1);
    }
    if let Some(statements) = node.as_statements_node() {
        return candidates_outliving(statements.body().iter().last(), nesting, depth + 1);
    }
    if let Some(else_node) = node.as_else_node() {
        return candidates_outliving(
            else_node.statements().map(|s| s.as_node()),
            nesting,
            depth + 1,
        );
    }
    if let Some(if_node) = node.as_if_node() {
        return candidates_outliving(if_node.statements().map(|s| s.as_node()), nesting, depth + 1)
            || candidates_outliving(if_node.subsequent(), nesting, depth + 1);
    }
    if let Some(unless_node) = node.as_unless_node() {
        return candidates_outliving(
            unless_node.statements().map(|s| s.as_node()),
            nesting,
            depth + 1,
        ) || candidates_outliving(
            unless_node.else_clause().map(|e| e.as_node()),
            nesting,
            depth + 1,
        );
    }
    if let Some(or_node) = node.as_or_node() {
        return candidates_outliving(Some(or_node.left()), nesting, depth + 1)
            || candidates_outliving(Some(or_node.right()), nesting, depth + 1);
    }
    if let Some(and_node) = node.as_and_node() {
        return candidates_outliving(Some(and_node.left()), nesting, depth + 1)
            || candidates_outliving(Some(and_node.right()), nesting, depth + 1);
    }
    false
}

/// `LOCAL_WRITE_NODES`' `write.depth` (`receiver_alias.rb`) — the four
/// local write kinds (no `LocalVariableTargetNode`).
fn local_write_depth(node: &PrismNode<'_>) -> Option<u32> {
    if let Some(w) = node.as_local_variable_write_node() {
        Some(w.depth())
    } else if let Some(w) = node.as_local_variable_or_write_node() {
        Some(w.depth())
    } else if let Some(w) = node.as_local_variable_and_write_node() {
        Some(w.depth())
    } else {
        node.as_local_variable_operator_write_node()
            .map(|w| w.depth())
    }
}

/// `INSTANCE_WRITE_NODES` (`receiver_alias.rb`) — the four ivar write kinds
/// (no `InstanceVariableTargetNode`).
fn is_instance_variable_write(node: &PrismNode<'_>) -> bool {
    node.as_instance_variable_write_node().is_some()
        || node.as_instance_variable_or_write_node().is_some()
        || node.as_instance_variable_and_write_node().is_some()
        || node.as_instance_variable_operator_write_node().is_some()
}
