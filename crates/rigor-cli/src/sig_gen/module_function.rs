//! sig-gen's reading of a bare `module_function` (reference
//! `ModuleFunctionState.each_sig_gen_statement` / `.mode_after`, #1968).
//!
//! A bare `module_function` turns the body's module_function mode on for the
//! later statements of its own statement list. A later bare `public`,
//! `private` or `protected` (receiverless or on `self`) turns it off. One the
//! statement runs for certain (a direct statement, or inside a plain `begin`
//! or `ensure`) ends the mode. One that may not run (control flow, a block, an
//! argument) makes an on mode UNCERTAIN: the generator then cannot tell
//! whether a later instance `def` is a module function (`def self?.x`) or an
//! instance method only (`def x`), and skips it
//! (`sig.skipped.uncertain-module-function`).
//!
//! The answer is keyed by the `def`'s start offset, for the `def`s that are
//! direct statements of a `class` / `module` body or a block body (the bodies
//! the generator's namespace walk reads), or arguments of one.

use std::collections::HashMap;

use rigor_parse::ruby_prism::{
    self, BlockNode, CallNode, ClassNode, DefNode, ModuleNode, Node as PrismNode, ParseResult,
    PostExecutionNode, SingletonClassNode, Visit,
};

/// The module_function mode in force at a statement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Off,
    On,
    Uncertain,
    /// Off, but the method-visibility table still reads the def as private:
    /// it applies a bare `module_function` as a `private` call, and only a
    /// direct bare `public` / `private` / `protected` statement changes the
    /// running default again (a reset inside a `begin` ends the mode but is
    /// not read there).
    OffPrivate,
}

/// `def` start offset → the mode in force at it.
pub fn def_modes(result: &ParseResult<'_>) -> HashMap<usize, Mode> {
    let mut walker = Walker { out: HashMap::new() };
    walker.visit(&result.node());
    walker.out
}

struct Walker {
    out: HashMap<usize, Mode>,
}

impl Walker {
    /// A body as `walk_namespace_body` hands it on: a plain statement list,
    /// or a `begin` body's main list. Each starts with the mode off.
    fn body(&mut self, body: Option<PrismNode<'_>>) {
        let Some(body) = body else { return };
        let statements = if let Some(stmts) = body.as_statements_node() {
            stmts
        } else if let Some(begin) = body.as_begin_node() {
            let Some(stmts) = begin.statements() else { return };
            stmts
        } else {
            return;
        };
        let mut mode = Mode::Off;
        // Whether the visibility table's running default is the `private` a
        // bare `module_function` set.
        let mut private_default = false;
        for stmt in statements.body().iter() {
            if let Some(call) = stmt.as_call_node() {
                if is_module_function(&call) && bare(&call) {
                    mode = Mode::On;
                    private_default = true;
                    continue;
                }
                if visibility_reset(&call) {
                    private_default = false;
                }
            }
            let at = if mode == Mode::Off && private_default { Mode::OffPrivate } else { mode };
            if let Some(def) = stmt.as_def_node() {
                self.out.insert(def.location().start_offset(), at);
            }
            // A `def` passed to a call (`private def x`) is walked with the
            // statement's mode.
            if let Some(call) = stmt.as_call_node() {
                for arg in arguments(&call) {
                    if let Some(def) = arg.as_def_node() {
                        self.out.insert(def.location().start_offset(), at);
                    }
                }
            }
            let after = mode_after(mode, &stmt);
            mode = after;
        }
    }
}

impl<'pr> Visit<'pr> for Walker {
    fn visit_class_node(&mut self, node: &ClassNode<'pr>) {
        self.body(node.body());
        ruby_prism::visit_class_node(self, node);
    }

    fn visit_module_node(&mut self, node: &ModuleNode<'pr>) {
        self.body(node.body());
        ruby_prism::visit_module_node(self, node);
    }

    fn visit_block_node(&mut self, node: &BlockNode<'pr>) {
        self.body(node.body());
        ruby_prism::visit_block_node(self, node);
    }
}

fn name_of(call: &CallNode<'_>) -> String {
    String::from_utf8_lossy(call.name().as_slice()).into_owned()
}

/// `ModuleFunctionState.call?`: a receiverless `module_function`.
fn is_module_function(call: &CallNode<'_>) -> bool {
    call.receiver().is_none() && name_of(call) == "module_function"
}

/// `bare?`: no arguments.
fn bare(call: &CallNode<'_>) -> bool {
    call.arguments().is_none_or(|a| a.arguments().iter().next().is_none())
}

fn self_or_none(receiver: Option<PrismNode<'_>>) -> bool {
    receiver.is_none_or(|r| r.as_self_node().is_some())
}

/// `visibility_reset?`: a bare `public` / `private` / `protected`,
/// receiverless or on `self`.
fn visibility_reset(call: &CallNode<'_>) -> bool {
    matches!(name_of(call).as_str(), "public" | "private" | "protected")
        && bare(call)
        && self_or_none(call.receiver())
}

/// `Source::Literals.symbol_or_string`, as the name it spells.
fn symbol_or_string(node: &PrismNode<'_>) -> Option<String> {
    if let Some(sym) = node.as_symbol_node() {
        return Some(String::from_utf8_lossy(sym.unescaped()).into_owned());
    }
    node.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
}

fn arguments<'pr>(call: &CallNode<'pr>) -> Vec<PrismNode<'pr>> {
    call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default()
}

/// `Spellings.arguments`: the arguments `self.module_function(...)` or
/// `send(:module_function, ...)` / `__send__(...)` (receiverless or on
/// `self`) passes `module_function`; `None` for any other call.
fn spelling_arguments<'pr>(call: &CallNode<'pr>) -> Option<Vec<PrismNode<'pr>>> {
    let args = arguments(call);
    let name = name_of(call);
    if name == "module_function" && call.receiver().is_some_and(|r| r.as_self_node().is_some()) {
        return Some(args);
    }
    if !matches!(name.as_str(), "send" | "__send__") || !self_or_none(call.receiver()) {
        return None;
    }
    if args.first().and_then(symbol_or_string).as_deref() != Some("module_function") {
        return None;
    }
    Some(args.into_iter().skip(1).collect())
}

/// `ScopeIndexer.meta_constant_receiver?`: `Name` or `::Name`.
fn meta_constant_receiver(receiver: Option<PrismNode<'_>>, name: &str) -> bool {
    let Some(r) = receiver else { return false };
    if let Some(read) = r.as_constant_read_node() {
        return read.name().as_slice() == name.as_bytes();
    }
    r.as_constant_path_node().is_some_and(|p| {
        p.parent().is_none() && p.name().is_some_and(|n| n.as_slice() == name.as_bytes())
    })
}

/// `ScopeIndexer.meta_new_constant_rvalue?`: `Class.new`, `Module.new`,
/// `Data.define(:a, …)` or `Struct.new(:a, …)`.
pub(super) fn meta_new_call(call: &CallNode<'_>) -> bool {
    let name = name_of(call);
    let receiver = || call.receiver();
    let symbols = |args: Vec<PrismNode<'_>>| args.iter().all(|a| a.as_symbol_node().is_some());
    match name.as_str() {
        "new" if meta_constant_receiver(receiver(), "Class") => true,
        "new" if meta_constant_receiver(receiver(), "Module") => true,
        "define" if meta_constant_receiver(receiver(), "Data") => symbols(arguments(call)),
        "new" if meta_constant_receiver(receiver(), "Struct") => {
            let positional: Vec<PrismNode<'_>> = arguments(call)
                .into_iter()
                .filter(|a| a.as_keyword_hash_node().is_none())
                .collect();
            !positional.is_empty() && symbols(positional)
        }
        _ => false,
    }
}

/// `ScopeIndexer.receiver_eval_call?`: a `*_eval` / `*_exec` call with a
/// literal block.
fn receiver_eval_call(call: &CallNode<'_>) -> bool {
    call.block().is_some_and(|b| b.as_block_node().is_some())
        && matches!(
            name_of(call).as_str(),
            "class_eval" | "module_eval" | "class_exec" | "module_exec" | "instance_eval" | "instance_exec"
        )
}

#[derive(Clone, Copy)]
enum Event {
    Off,
    MaybeOff,
}

/// `mode_after`: the mode after the direct statement `stmt`.
fn mode_after(mode: Mode, stmt: &PrismNode<'_>) -> Mode {
    if matches!(mode, Mode::Off | Mode::OffPrivate) {
        return mode;
    }
    let mut events: Vec<(usize, Event)> = Vec::new();
    prescan(stmt, false, &mut events);
    events.sort_by_key(|(offset, _)| *offset);
    let mut mode = mode;
    for (_, event) in events {
        match event {
            Event::Off => mode = Mode::Off,
            Event::MaybeOff if mode == Mode::On => mode = Mode::Uncertain,
            Event::MaybeOff => {}
        }
    }
    mode
}

/// `prescan_deferred`, reduced to the reset events `mode_after` reads.
/// `nested` says the node itself may not run where it is written.
fn prescan(node: &PrismNode<'_>, nested: bool, events: &mut Vec<(usize, Event)>) {
    Prescanner { stack: vec![nested], events }.visit(node);
}

/// The prescan as a visitor. `stack` holds, per node being visited, whether
/// its children may not run in place: a node's own reading is the entry below
/// its own (`nested ||= !RUN_IN_PLACE.include?(node.class)` for its children).
struct Prescanner<'e> {
    stack: Vec<bool>,
    events: &'e mut Vec<(usize, Event)>,
}

impl Prescanner<'_> {
    /// Whether the node whose `visit_*` is running may not run in place.
    fn own_nested(&self) -> bool {
        self.stack[self.stack.len().saturating_sub(2)]
    }
}

impl<'pr> Visit<'pr> for Prescanner<'_> {
    fn visit_branch_node_enter(&mut self, node: PrismNode<'pr>) {
        let own = *self.stack.last().unwrap_or(&false);
        let in_place = node.as_statements_node().is_some()
            || node.as_begin_node().is_some()
            || node.as_ensure_node().is_some();
        self.stack.push(own || !in_place);
    }

    fn visit_branch_node_leave(&mut self) {
        self.stack.pop();
    }

    // Another scope's body, or an `END` body that runs after every def.
    fn visit_class_node(&mut self, _node: &ClassNode<'pr>) {}
    fn visit_module_node(&mut self, _node: &ModuleNode<'pr>) {}
    fn visit_singleton_class_node(&mut self, _node: &SingletonClassNode<'pr>) {}
    fn visit_def_node(&mut self, _node: &DefNode<'pr>) {}
    fn visit_post_execution_node(&mut self, _node: &PostExecutionNode<'pr>) {}

    fn visit_call_node(&mut self, node: &CallNode<'pr>) {
        let nested = self.own_nested();
        if !prescanned_call(node, nested, self.events) {
            ruby_prism::visit_call_node(self, node);
        }
    }
}

/// `prescanned_call?`: true when the arm took the call and its subtree.
fn prescanned_call(call: &CallNode<'_>, nested: bool, events: &mut Vec<(usize, Event)>) -> bool {
    let module_function_args = if is_module_function(call) {
        Some(arguments(call))
    } else {
        spelling_arguments(call)
    };
    if let Some(args) = module_function_args {
        // A `def` argument and a literal name record a deferred-ranges row,
        // which `mode_after` does not read; anything else is prescanned.
        for arg in args {
            if arg.as_def_node().is_none() && symbol_or_string(&arg).is_none() {
                prescan(&arg, true, events);
            }
        }
        if let Some(block) = call.block() {
            prescan(&block, true, events);
        }
        return true;
    }
    if visibility_reset(call) {
        events.push((
            call.location().start_offset(),
            if nested { Event::MaybeOff } else { Event::Off },
        ));
        return true;
    }
    if receiver_eval_call(call) || (call.block().is_some() && meta_new_call(call)) {
        if let Some(receiver) = call.receiver() {
            prescan(&receiver, true, events);
        }
        for arg in arguments(call) {
            prescan(&arg, true, events);
        }
        return true;
    }
    false
}
