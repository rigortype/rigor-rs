//! Guard narrowing of CONSTANT receivers and `respond_to?` guards on locals
//! (upstream `1be2fd0b`, rigor#1453): a suppression-only walk.
//!
//! Since `1be2fd0b` the reference narrows a constant read under the same
//! guards it narrows a local under (truthiness, `nil?`, `!`, the class guards,
//! `C === x`, `respond_to?`), keyed by the constant's spelling, and
//! `respond_to?(:m)` admits `m` on its truthy edge: a receiver whose class RBS
//! knows to lack `m` reads `Dynamic[top]` there. `if ::File.respond_to?
//! (:makedirs); ::File.makedirs "x"; end` is therefore silent on the
//! reference, and so is `v.respond_to?(:nan?) && v.nan?` on a `Numeric`-
//! narrowed local.
//!
//! The port types a constant receiver (`singleton(File)`, `FOO = "x"`) with no
//! flow at all, and its local engines never read `respond_to?`. Rather than
//! thread a new fact through every engine, this pass marks the call sites
//! where the reference's narrowing provably changes the receiver — the rules
//! layer skips them like the class-narrowing `dead` set. It never mints a
//! diagnostic, so every over-approximation below is a coverage loss:
//!
//! * a guard edge that does not KEEP the subject's carrier
//!   ([`Typer::subject_edge`]) suppresses the subject's receiver calls in that
//!   edge's region: an `if`/`unless`/ternary arm, the right operand of `&&` /
//!   `||`, a `while`/`until` body (loop-entry edge);
//! * a widening (`Other`) edge on a LOCAL also suppresses past the join that
//!   follows it, to the end of the enclosing scope or the local's next rebind
//!   — the reference's `Dynamic[top] | T` join reads untyped. A constant's
//!   narrowing, and an unreachable (`Empty`) edge, survive a join only when
//!   every live path holds them (a constant's narrowing is a scope-table
//!   entry; `Bot` is the join's identity). An arm that ends in `return`/
//!   `raise`/`next`/`break` is not a live path (the early-return guard);
//! * the reference RESTORES a constant's binding after a call that may run
//!   project or unresolved code (`GuardRebinding`); the port keeps the
//!   suppression instead;
//! * locals are only suppressed for `respond_to?` — their other guards are the
//!   existing engines' (`class_narrowing`, `loop_edges`).

use std::collections::{HashMap, HashSet};

use rigor_parse::{LoopKind, LoweredAst, Node, NodeId, Span, StatementsKind};
use rigor_types::{Interner, Type, TypeId};

use crate::loop_edges::{Edge, Subject};
use crate::{TypeEnv, Typer};

/// The subjects suppressed at one program point.
#[derive(Clone, Default, Debug)]
struct Sup {
    /// Widened by some live path: survives joins.
    widened: HashSet<Subject>,
    /// `Bot` on every live path: survives a join only when all paths agree.
    bot: HashSet<Subject>,
}

impl Sup {
    fn has(&self, s: &Subject) -> bool {
        self.widened.contains(s) || self.bot.contains(s)
    }

    fn kill(&mut self, s: &Subject) {
        self.widened.remove(s);
        self.bot.remove(s);
    }

    fn apply(&self, effects: &[(Subject, Edge)]) -> Sup {
        let mut out = self.clone();
        for (s, e) in effects {
            match e {
                Edge::Keep => {}
                Edge::Empty => {
                    out.bot.insert(s.clone());
                }
                Edge::Other => {
                    out.widened.insert(s.clone());
                }
            }
        }
        out
    }

    /// The join of the live paths' states; no live path (every arm
    /// terminates) leaves `fallback`.
    fn join(paths: Vec<Sup>, fallback: &Sup) -> Sup {
        let mut it = paths.into_iter();
        let Some(mut acc) = it.next() else {
            return fallback.clone();
        };
        for p in it {
            // A LOCAL's widened arm poisons the join (`Dynamic[top] | T`); a
            // CONSTANT's narrowing is a scope-table entry the join keeps only
            // when every path holds it (`if File.respond_to?(:x); end;
            // File.x` fires on the reference).
            acc.widened.retain(|s| matches!(s, Subject::Local(_)) || p.widened.contains(s));
            acc.widened.extend(p.widened.into_iter().filter(|s| matches!(s, Subject::Local(_))));
            acc.bot.retain(|s| p.bot.contains(s));
        }
        acc
    }
}

struct Walk<'a, 'i> {
    typer: &'a Typer<'i>,
    ast: &'a LoweredAst,
    interner: &'a mut Interner,
    dead: HashSet<NodeId>,
    /// A constant subject's carrier, typed once per spelling.
    const_types: HashMap<Subject, TypeId>,
}

impl<'i> Typer<'i> {
    /// The call sites whose receiver a constant guard or a `respond_to?`
    /// guard narrows away from the carrier the port would witness against
    /// (see the module docs). The rules layer emits nothing at them.
    pub fn guard_suppressed_calls(&self, ast: &LoweredAst, interner: &mut Interner) -> HashSet<NodeId> {
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return HashSet::new(),
        };
        let mut w = Walk { typer: self, ast, interner, dead: HashSet::new(), const_types: HashMap::new() };
        w.list(&body, Sup::default());
        w.dead
    }
}

impl Walk<'_, '_> {
    fn list(&mut self, stmts: &[NodeId], mut sup: Sup) -> Sup {
        for &s in stmts {
            sup = self.node(s, sup);
        }
        sup
    }

    /// The truthy edge `cond` gives a `case`/`when` or `case`/`in` subject:
    /// `cond === subject`, where a `ConstantRead` condition maps to the class
    /// test (`C === x`). An `in` pattern also narrows on a literal
    /// (`lit === subject`); a `when` literal never does — `when "lit"` on
    /// `FOO = "x"` still fires `for "x"` (rigor-rs#452 review).
    fn case_condition_edge(
        &mut self,
        cond: NodeId,
        subject: &Subject,
        use_span: Span,
        in_arm: bool,
    ) -> Edge {
        let ty = match self.const_types.get(subject) {
            Some(&t) => t,
            None => self.interner.untyped(),
        };
        if !matches!(self.interner.get(ty), Type::Constant(_) | Type::Nominal { .. }) {
            return Edge::Keep;
        }
        match self.ast.get(cond) {
            Node::ConstantRead { .. } => self.typer.class_edge(
                self.ast,
                cond,
                use_span,
                false,
                ty,
                true,
                self.interner,
            ),
            // `when` conditions narrow only on a class constant.
            _ if !in_arm => Edge::Keep,
            // A literal `in` pattern narrows by `lit === subject` — only a
            // pinned constant subject has a decidable value; a Nominal keeps
            // the arm undecided.
            Node::NilLit { .. } => self.literal_edge(ty, &rigor_types::Scalar::Nil),
            Node::IntegerLit { value, digits, .. } => {
                let lit = value
                    .map(rigor_types::Scalar::Int)
                    .or_else(|| digits.clone().map(rigor_types::Scalar::BigInt));
                match lit {
                    Some(lit) => self.literal_edge(ty, &lit),
                    None => Edge::Keep,
                }
            }
            Node::FloatLit { value, .. } => {
                self.literal_edge(ty, &rigor_types::Scalar::Float(*value))
            }
            Node::StringLit { value, .. } => {
                self.literal_edge(ty, &rigor_types::Scalar::Str(value.clone()))
            }
            Node::SymbolLit { value, .. } => {
                self.literal_edge(ty, &rigor_types::Scalar::Sym(value.clone()))
            }
            Node::TrueLit { .. } => self.literal_edge(ty, &rigor_types::Scalar::Bool(true)),
            Node::FalseLit { .. } => self.literal_edge(ty, &rigor_types::Scalar::Bool(false)),
            _ => Edge::Keep,
        }
    }

    /// `lit === subject` on a pinned scalar — Keep iff the literal is the
    /// subject's own value (`in "x"` on `FOO = "x"` keeps the carrier and
    /// the arm's calls still fire `for "x"`); anything else is unreachable.
    /// A non-Constant carrier declines.
    fn literal_edge(&self, ty: TypeId, lit: &rigor_types::Scalar) -> Edge {
        match self.interner.get(ty) {
            Type::Constant(s) if s == lit => Edge::Keep,
            Type::Constant(_) => Edge::Empty,
            _ => Edge::Keep,
        }
    }

    /// `pred`'s per-subject effects on its `edge`.
    fn effects(&mut self, pred: NodeId, edge: bool) -> Vec<(Subject, Edge)> {
        let mut out = Vec::new();
        for (subject, read) in self.typer.subjects_within(self.ast, pred) {
            let (ty, respond_only) = match &subject {
                Subject::Local(_) => (self.interner.untyped(), true),
                Subject::Const(..) => {
                    let ty = match self.const_types.get(&subject) {
                        Some(&t) => t,
                        None => {
                            let t = self.typer.type_of(self.ast, read, &TypeEnv::new(), self.interner);
                            self.const_types.insert(subject.clone(), t);
                            t
                        }
                    };
                    (ty, false)
                }
            };
            let e = self.typer.subject_edge(self.ast, pred, &subject, ty, edge, respond_only, self.interner);
            if e != Edge::Keep {
                out.push((subject, e));
            }
        }
        out
    }

    fn node(&mut self, id: NodeId, sup: Sup) -> Sup {
        let ast = self.ast;
        match ast.get(id) {
            Node::If { predicate, then_body, else_body, is_unless, .. } => {
                let (p, is_unless) = (*predicate, *is_unless);
                let then_body = then_body.clone();
                // An `else` clause lowers to a clause-less `BeginRescue`
                // carrier; unwrap it so its last statement decides liveness.
                let else_body = match else_body.as_slice() {
                    [only] => match ast.get(*only) {
                        Node::BeginRescue { body, ensure_body, clauses, .. }
                            if ensure_body.is_empty() && clauses.is_empty() =>
                        {
                            body.clone()
                        }
                        _ => else_body.clone(),
                    },
                    _ => else_body.clone(),
                };
                let sup = self.node(p, sup);
                let t_in = sup.apply(&self.effects(p, true));
                let f_in = sup.apply(&self.effects(p, false));
                let (truthy, falsey) =
                    if is_unless { (&else_body, &then_body) } else { (&then_body, &else_body) };
                let t_out = self.list(truthy, t_in);
                let f_out = self.list(falsey, f_in);
                let mut live = Vec::new();
                if !crate::class_narrowing::branch_terminates(ast, truthy) {
                    live.push(t_out);
                }
                if !crate::class_narrowing::branch_terminates(ast, falsey) {
                    live.push(f_out);
                }
                Sup::join(live, &sup)
            }
            Node::Logical { left, right, is_and, .. } => {
                let (l, r, is_and) = (*left, *right, *is_and);
                let sup = self.node(l, sup);
                let r_in = sup.apply(&self.effects(l, is_and));
                let short = sup.apply(&self.effects(l, !is_and));
                let r_out = self.node(r, r_in);
                Sup::join(vec![short, r_out], &sup)
            }
            Node::Loop { predicate: Some(p), body, kind, .. }
                if !matches!(kind, LoopKind::For) =>
            {
                let (p, body, kind) = (*p, body.clone(), *kind);
                let (entry, begin_modifier) = match kind {
                    LoopKind::While { begin_modifier } => (true, begin_modifier),
                    LoopKind::Until { begin_modifier } => (false, begin_modifier),
                    LoopKind::For => unreachable!(),
                };
                let sup = self.node(p, sup);
                let body_in = if begin_modifier { sup.clone() } else { sup.apply(&self.effects(p, entry)) };
                let body_out = self.list(&body, body_in);
                let joined = Sup::join(vec![sup.clone(), body_out], &sup);
                joined.apply(&self.effects(p, !entry))
            }
            Node::Call { receiver, args, block_body, .. } => {
                let (receiver, args, block_body) = (*receiver, args.clone(), block_body.clone());
                let mut sup = sup;
                if let Some(r) = receiver {
                    if Subject::of(ast, r).is_some_and(|s| sup.has(&s)) {
                        self.dead.insert(id);
                    }
                    sup = self.node(r, sup);
                }
                for a in args {
                    sup = self.node(a, sup);
                }
                // A block may run later or never: walk it from the current
                // state and keep the state (a rebind inside is ignored, which
                // only keeps a suppression longer).
                let _ = self.list(&block_body, sup.clone());
                sup
            }
            Node::LocalVariableWrite { name, value, .. }
            | Node::LocalVariableOpWrite { name, value, .. } => {
                let name = name.clone();
                let mut sup = self.node(*value, sup);
                sup.kill(&Subject::Local(name));
                sup
            }
            Node::MultiWrite { value, targets, target_exprs, .. } => {
                let (value, exprs) = (*value, target_exprs.clone());
                let names: Vec<String> = targets.bound_names().into_iter().map(|(n, _)| n).collect();
                let mut sup = self.node(value, sup);
                for e in exprs {
                    sup = self.node(e, sup);
                }
                for n in names {
                    sup.kill(&Subject::Local(n));
                }
                sup
            }
            Node::ConstantWrite { name, value, .. } => {
                let name = name.trim_start_matches("::").to_string();
                let mut sup = self.node(*value, sup);
                sup.kill(&Subject::Const(name.clone(), false));
                sup.kill(&Subject::Const(name, true));
                sup
            }
            // A fresh local scope; a constant narrowing does not enter a
            // method body either (the body runs later, under any binding).
            Node::Definition { body, .. } | Node::ClassDef { body, .. } | Node::ModuleDef { body, .. } => {
                let body = body.clone();
                let _ = self.list(&body, Sup::default());
                sup
            }
            Node::Lambda { body, .. } => {
                let body = body.clone();
                let _ = self.list(&body, sup.clone());
                sup
            }
            Node::Statements { body, kind, .. } => {
                let (body, kind) = (body.clone(), *kind);
                if kind == StatementsKind::Sequence {
                    self.list(&body, sup)
                } else {
                    let out = self.list(&body, sup.clone());
                    Sup::join(vec![sup.clone(), out], &sup)
                }
            }
            Node::Program { body, .. } => {
                let body = body.clone();
                self.list(&body, sup)
            }
            // `case`: every clause is an alternative entered from the
            // post-subject state; the no-match path keeps that state. A
            // CONSTANT subject narrows inside a `when`/`in` arm whose
            // patterns all disprove its carrier (`1be2fd0b`, rigor-rs#452):
            // `case FOO when Integer` enters the arm on the `Integer === FOO`
            // truthy edge. A mixed list keeps the carrier when ANY condition
            // could match it (`when String, Integer` on `FOO = "x"` still
            // fires `for "x"`), and a pattern the class test cannot decide
            // (`in [x]`) declines rather than suppress.
            Node::Case { predicate, branches, else_body, .. } => {
                let (p, branches, else_body) = (*predicate, branches.clone(), else_body.clone());
                let subject = p.and_then(|id| Subject::of(ast, id));
                // Prime the carrier — `effects()` types a Const subject at
                // its read site lazily; the case predicate IS the read.
                if let (Some(Subject::Const(..)), Some(p)) = (&subject, p) {
                    self.const_types
                        .entry(subject.clone().unwrap())
                        .or_insert_with(|| {
                            self.typer.type_of(ast, p, &TypeEnv::new(), self.interner)
                        });
                }
                let sup = match p {
                    Some(p) => self.node(p, sup),
                    None => sup,
                };
                let mut paths = vec![sup.clone()];
                for b in branches {
                    let b_sup = match (subject.clone(), ast.get(b)) {
                        (
                            Some(Subject::Const(name, rooted)),
                            Node::When { conditions, body, .. },
                        ) => {
                            let (conditions, body) = (conditions.clone(), body.clone());
                            let mut edge = Edge::Empty;
                            for c in &conditions {
                                edge = match edge {
                                    Edge::Keep => Edge::Keep,
                                    _ => self.case_condition_edge(
                                        *c,
                                        &Subject::Const(name.clone(), rooted),
                                        ast.get(*c).span(),
                                        false,
                                    ),
                                };
                            }
                            let arm =
                                sup.apply(&[(Subject::Const(name, rooted), edge)]);
                            let mut cur = arm;
                            for c in &conditions {
                                cur = self.node(*c, cur);
                            }
                            self.list(&body, cur)
                        }
                        (
                            Some(Subject::Const(name, rooted)),
                            Node::BeginRescue { main_body, body, .. },
                        ) => {
                            // A `case`/`in` arm: `main_body[0]` is the
                            // lowered PATTERN — a `ConstantRead` is the
                            // `in C` class test (`C === subject`), every
                            // other shape declines.
                            let (main_body, body) = (main_body.clone(), body.clone());
                            let (pattern, arm_body) = main_body
                                .split_first()
                                .map(|(p, r)| (*p, r))
                                .unwrap_or((b, &[][..]));
                            let edge = self.case_condition_edge(
                                pattern,
                                &Subject::Const(name.clone(), rooted),
                                ast.get(pattern).span(),
                                true,
                            );
                            let cur = self.node(
                                pattern,
                                sup.apply(&[(Subject::Const(name, rooted), edge)]),
                            );
                            let cur = self.list(arm_body, cur);
                            let covered: HashSet<NodeId> =
                                main_body.iter().copied().collect();
                            let rest: Vec<NodeId> =
                                body.into_iter().filter(|n| !covered.contains(n)).collect();
                            self.list(&rest, cur)
                        }
                        _ => self.node(b, sup.clone()),
                    };
                    paths.push(b_sup);
                }
                paths.push(self.list(&else_body, sup.clone()));
                Sup::join(paths, &sup)
            }
            // `begin`/`rescue`: the protected body (plus `else`/`ensure`,
            // flattened into `body`) is one path, each rescue clause another
            // entered from the `begin`'s entry state.
            Node::BeginRescue { main_body, body, clauses, .. } if !clauses.is_empty() => {
                let (main_body, body, clauses) = (main_body.clone(), body.clone(), clauses.clone());
                let mut paths = vec![sup.clone(), self.list(&main_body, sup.clone())];
                for c in &clauses {
                    let mut cur = self.list(&c.exceptions, sup.clone());
                    cur = self.list(&c.body, cur);
                    paths.push(cur);
                }
                let mut covered: HashSet<NodeId> = main_body.iter().copied().collect();
                for c in &clauses {
                    covered.extend(c.exceptions.iter().copied());
                    covered.extend(c.body.iter().copied());
                }
                let rest: Vec<NodeId> = body.into_iter().filter(|b| !covered.contains(b)).collect();
                let joined = Sup::join(paths, &sup);
                self.list(&rest, joined)
            }
            // Every other node: walk its children in order; anything that
            // may not run (`for`, a clause-less `begin` carrier, a `when`)
            // joins its walked state with the state it was entered with.
            other => {
                let branches = matches!(
                    other,
                    Node::When { .. } | Node::BeginRescue { .. } | Node::Loop { .. }
                );
                let mut kids = Vec::new();
                child_ids(other, &mut kids);
                let mut cur = sup.clone();
                for k in kids {
                    cur = self.node(k, cur);
                }
                if branches {
                    Sup::join(vec![sup.clone(), cur], &sup)
                } else {
                    cur
                }
            }
        }
    }
}

/// Every lowered child of `n` in evaluation order (the variants the walk's
/// own arms do not cover).
fn child_ids(n: &Node, out: &mut Vec<NodeId>) {
    match n {
        Node::IndexWrite { receiver, indices, value, .. } => {
            out.extend(receiver.iter().copied());
            out.extend_from_slice(indices);
            out.push(*value);
        }
        Node::AttrWrite { receiver, value, .. } => {
            out.extend(receiver.iter().copied());
            out.push(*value);
        }
        Node::VariableWrite { value, .. } | Node::InstanceVariableWrite { value, .. } => out.push(*value),
        Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
            out.extend_from_slice(parts);
        }
        Node::Case { predicate, branches, else_body, .. } => {
            out.extend(predicate.iter().copied());
            out.extend_from_slice(branches);
            out.extend_from_slice(else_body);
        }
        Node::When { conditions, body, .. } => {
            out.extend_from_slice(conditions);
            out.extend_from_slice(body);
        }
        Node::Loop { predicate, body, .. } => {
            out.extend(predicate.iter().copied());
            out.extend_from_slice(body);
        }
        Node::BeginRescue { body, .. } => out.extend_from_slice(body),
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => out.extend_from_slice(elements),
        Node::Range { left, right, .. } => {
            out.extend(left.iter().copied());
            out.extend(right.iter().copied());
        }
        Node::Return { values, .. } => out.extend_from_slice(values),
        _ => {}
    }
}
