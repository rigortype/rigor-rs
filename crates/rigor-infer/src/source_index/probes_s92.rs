use super::*;
use rigor_parse::{lower, parse, LoweredAst, Node, NodeId};
use rigor_types::Interner;
use rigor_index::CoreIndex;
use super::harvest::def_names;

/// A per-FIELD rendering of the whole index. `sorted = true` canonicalises
/// every collection (the SEMANTIC content); `sorted = false` renders the
/// `Vec` fields in their built order (exposing ORDER leakage).
fn fingerprint(idx: &SourceIndex, sorted: bool) -> Vec<(&'static str, String)> {
    fn sort_join(mut v: Vec<String>) -> String {
        v.sort();
        v.join(" | ")
    }
    let mut out: Vec<(&'static str, String)> = Vec::new();

    out.push((
        "classes",
        sort_join(
            idx.classes
                .iter()
                .map(|(k, c)| {
                    let mut ms: Vec<&str> = c.methods.iter().map(|s| s.as_str()).collect();
                    ms.sort();
                    format!("{k}<{:?}>{{{}}}", c.superclass, ms.join(","))
                })
                .collect(),
        ),
    ));
    // `names` IS the ClassId assignment order — never sorted for the
    // ordered fingerprint.
    out.push((
        "names",
        if sorted { sort_join(idx.names.clone()) } else { idx.names.join(" | ") },
    ));
    // `name_to_id` is the reverse half of the registry bijection. Rendered
    // name-sorted with the ID INCLUDED, so it pins the id assignment itself
    // in both modes (the 17th field; the probe module shipped 16).
    out.push((
        "name_to_id",
        sort_join(idx.name_to_id.iter().map(|(n, i)| format!("{n}={i}")).collect()),
    ));
    out.push((
        "declaration_only_classes",
        sort_join(idx.declaration_only_classes.iter().cloned().collect()),
    ));
    out.push((
        "method_returns",
        sort_join(
            idx.method_returns.iter().map(|((c, m), r)| format!("{c}#{m}->{r}")).collect(),
        ),
    ));
    out.push((
        "param_bound_returns",
        sort_join(
            idx.param_bound_returns
                .iter()
                .map(|((c, m), p)| format!("{c}#{m}->{p:?}"))
                .collect(),
        ),
    ));
    out.push((
        "override_classes",
        sort_join(
            idx.override_classes
                .iter()
                .map(|(k, c)| {
                    let mut ms: Vec<&str> = c.methods.iter().map(|s| s.as_str()).collect();
                    ms.sort();
                    let mut vis: Vec<String> = c
                        .method_visibilities
                        .iter()
                        .map(|(m, v)| format!("{m}={v:?}"))
                        .collect();
                    vis.sort();
                    // `includes` is ORDER-BEARING (MRO): keep source order
                    // in the ordered fingerprint.
                    let inc = if sorted {
                        sort_join(c.includes.clone())
                    } else {
                        c.includes.join(",")
                    };
                    format!(
                        "{k}<{:?}>inc[{inc}]m{{{}}}vis{{{}}}",
                        c.superclass,
                        ms.join(","),
                        vis.join(",")
                    )
                })
                .collect(),
        ),
    ));
    out.push(("toplevel_defs", sort_join(idx.toplevel_defs.iter().cloned().collect())));
    out.push((
        "literal_returns",
        sort_join(
            idx.literal_returns
                .iter()
                .map(|((o, m, k), s)| format!("{o}.{m}/{k:?}->{s:?}"))
                .collect(),
        ),
    ));
    out.push((
        "definers",
        sort_join(
            idx.definers
                .iter()
                .map(|((m, k), owners)| {
                    let o = if sorted { sort_join(owners.clone()) } else { owners.join(",") };
                    format!("{m}/{k:?}->[{o}]")
                })
                .collect(),
        ),
    ));
    out.push((
        "toplevel_constants",
        sort_join(idx.toplevel_constants.iter().cloned().collect()),
    ));
    out.push((
        "literal_constants",
        sort_join(
            idx.literal_constants
                .iter()
                .map(|(k, v)| {
                    let mut es: Vec<String> = v
                        .iter()
                        .map(|(ns, f, l)| format!("{}#{f:?}={l:?}", ns.join("::")))
                        .collect();
                    if sorted {
                        es.sort();
                    }
                    format!("{k}->[{}]", es.join(","))
                })
                .collect(),
        ),
    ));
    out.push((
        "qualified_literal_constants",
        sort_join(
            idx.qualified_literal_constants
                .iter()
                .map(|(k, (ns, f, l))| format!("{k}->{}#{f:?}={l:?}", ns.join("::")))
                .collect(),
        ),
    ));
    out.push((
        "project_constant_write_names",
        sort_join(idx.project_constant_write_names.iter().cloned().collect()),
    ));
    out.push((
        "nested_constant_namespaces",
        sort_join(
            idx.nested_constant_namespaces
                .iter()
                .map(|(k, v)| {
                    let mut nss: Vec<String> = v.iter().map(|ns| ns.join("::")).collect();
                    if sorted {
                        nss.sort();
                    }
                    format!("{k}->[{}]", nss.join(","))
                })
                .collect(),
        ),
    ));
    out.push((
        "discovered_methods",
        sort_join(
            idx.discovered_methods
                .iter()
                .map(|(k, v)| {
                    let mut ms: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
                    ms.sort();
                    format!("{k}->{{{}}}", ms.join(","))
                })
                .collect(),
        ),
    ));
    // `file_defs` is index-aligned with `files` (the per-file def overlay
    // the toplevel/owner suppression reads) — keep the FILE ORDER in the
    // ordered fingerprint, sort in the canonical one. Without it a merge
    // that drops or misorders a file's `def` attribution can still
    // fingerprint equal on every other field (issue #141).
    out.push((
        "file_defs",
        {
            let mut per_file: Vec<String> = idx
                .file_defs
                .iter()
                .enumerate()
                .map(|(i, fd)| {
                    let mut tl: Vec<&str> =
                        fd.toplevel.iter().map(|s| s.as_str()).collect();
                    tl.sort();
                    let mut owners: Vec<String> = fd
                        .methods
                        .iter()
                        .map(|(k, v)| {
                            let mut ms: Vec<&str> =
                                v.iter().map(|s| s.as_str()).collect();
                            ms.sort();
                            format!("{k}->{{{}}}", ms.join(","))
                        })
                        .collect();
                    owners.sort();
                    // Canonical mode drops the `f{i}` index so a file
                    // PERMUTATION renders the same content set; the
                    // ordered form keeps it to pin the alignment.
                    if sorted {
                        format!("tl[{}]m[{}]", tl.join(","), owners.join(","))
                    } else {
                        format!("f{i}tl[{}]m[{}]", tl.join(","), owners.join(","))
                    }
                })
                .collect();
            if sorted {
                per_file.sort();
            }
            per_file.join(" | ")
        },
    ));
    out.push((
        "mutated_params",
        sort_join(
            idx.mutated_params
                .iter()
                .map(|(k, v)| {
                    let mut ix: Vec<usize> = v.iter().copied().collect();
                    ix.sort_unstable();
                    format!("{k}->{ix:?}")
                })
                .collect(),
        ),
    ));
    out
}

/// Permute the AST REFERENCES, never re-`lower()`: `file_id` comes from a
/// process-global counter, so re-lowering would inject a spurious diff into
/// the `literal_constants` / `qualified_literal_constants` fingerprints.
fn build_perm(
    asts: &[LoweredAst],
    perm: &[usize],
    core: &CoreIndex,
) -> Vec<(&'static str, String)> {
    fingerprint(&SourceIndex::build_project(&perm_refs(asts, perm), core), false)
}

fn perm_refs<'a>(asts: &'a [LoweredAst], perm: &[usize]) -> Vec<&'a LoweredAst> {
    perm.iter().map(|&i| &asts[i]).collect()
}

fn diff(
    a: &[(&'static str, String)],
    b: &[(&'static str, String)],
) -> Vec<(&'static str, String, String)> {
    a.iter()
        .zip(b.iter())
        .filter(|((_, x), (_, y))| x != y)
        .map(|((k, x), (_, y))| (*k, x.clone(), y.clone()))
        .collect()
}

/// PROBE 1 — permutation sensitivity, field by field. Prints every field
/// whose built value depends on the ORDER of the `asts` slice, and PINS the
/// finding: file order may move only the registry ids and the override
/// index. Anything else moving means the merge acquired an order dependence
/// the passes never had.
#[test]
fn probe_permutation_field_diff() {
    let core = CoreIndex::new();
    let srcs = probe1_sources();
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
    let base = build_perm(&asts, &[0, 1, 2, 3], &core);
    let perms: [[usize; 4]; 5] =
        [[1, 0, 2, 3], [3, 2, 1, 0], [2, 3, 0, 1], [0, 2, 1, 3], [3, 0, 1, 2]];
    for p in perms {
        let other = build_perm(&asts, &p, &core);
        let d = diff(&base, &other);
        if !d.is_empty() {
            println!("--- permutation {p:?}: {} field(s) differ", d.len());
            for (field, x, y) in &d {
                println!("  [{field}]\n    base : {x}\n    perm : {y}");
            }
        } else {
            println!("--- permutation {p:?}: identical");
        }
        // The CANONICAL content: only the two order-bearing fields may move.
        // (`names` sorted is the same SET; it is `name_to_id` that carries
        // the assignment.)
        let canonical = diff(
            &fingerprint(&SourceIndex::build_project(&perm_refs(&asts, &[0, 1, 2, 3]), &core), true),
            &fingerprint(&SourceIndex::build_project(&perm_refs(&asts, &p), &core), true),
        );
        for (field, _, _) in canonical {
            assert!(
                matches!(field, "name_to_id" | "override_classes"),
                "permutation {p:?} moved `{field}`, which is not order-bearing"
            );
        }
    }
}

/// PROBE 2 — incremental equality: build over ALL files vs ALL-BUT-ONE, and
/// report which fields change in a way a per-file harvest of the dropped
/// file could NOT have reconstructed on its own (i.e. file X's recorded
/// contribution depends on file Y's content).
#[test]
fn probe_drop_one_field_diff() {
    let core = CoreIndex::new();
    let srcs: Vec<&[u8]> = vec![
        // f0: the "other" file — reopens, conflicting constant.
        b"class Base\n  def m\n    1\n  end\nend\nSHARED = 1\nmodule Wrap\n  DUP = 1\nend\n",
        // f1: the file under test — its OWN contribution depends on f0.
        b"class Base\n  private\n  def m\n    2\n  end\nend\nmodule Wrap\n  DUP = 2\nend\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\nSOLO = 7\n",
        // f2: an unrelated third file.
        b"class Solo\n  def q\n    1\n  end\nend\n",
    ];
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
    for drop in 0..asts.len() {
        let all: Vec<&LoweredAst> = asts.iter().collect();
        let kept: Vec<&LoweredAst> =
            asts.iter().enumerate().filter(|(i, _)| *i != drop).map(|(_, a)| a).collect();
        let fa = fingerprint(&SourceIndex::build_project(&all, &core), false);
        let fk = fingerprint(&SourceIndex::build_project(&kept, &core), false);
        println!("--- dropping f{drop}");
        for (field, x, y) in diff(&fa, &fk) {
            println!("  [{field}]\n    all  : {x}\n    kept : {y}");
        }
    }
}

/// PROBE 3 — is `build_project` over N files equal to the UNION of N
/// single-file `build_project`s for the "obviously additive" fields? Any
/// field where it is NOT is a cross-file computation.
#[test]
fn probe_union_of_singletons_vs_project() {
    let core = CoreIndex::new();
    let srcs: Vec<&[u8]> = vec![
        b"class A\n  def x\n    \"s\"\n  end\nend\nC1 = 1\n",
        b"class B < A\n  def y\n    A.new\n  end\nend\nC1 = 2\nC2 = 3\n",
        b"class C\n  def z\n    C2\n  end\nend\n",
    ];
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
    let all: Vec<&LoweredAst> = asts.iter().collect();
    let project = fingerprint(&SourceIndex::build_project(&all, &core), true);
    let singles: Vec<Vec<(&'static str, String)>> = asts
        .iter()
        .map(|a| fingerprint(&SourceIndex::build_project(&[a], &core), true))
        .collect();
    for (i, (field, joined)) in project.iter().enumerate() {
        let parts: Vec<String> =
            singles.iter().map(|s| s[i].1.clone()).filter(|s| !s.is_empty()).collect();
        println!("[{field}]\n  project : {joined}\n  singles : {}", parts.join("  ||  "));
    }
}

// PROBES 4, 5 and 8 were PRINTING probes of the Pass-3 / Pass-4b / both-maps
// couplings. They are promoted, above, into the asserting tests
// `coupling_pass3_reads_the_merged_constant_table`,
// `coupling_pass4b_degrade_is_cross_file` and
// `method_can_appear_in_both_return_maps` — same inputs, same finding, now a
// gate instead of a printout.

/// PROBE 6 — cross-PROCESS instability of the Vec-valued maps. Prints the
/// built order of `definers` / `literal_constants` /
/// `nested_constant_namespaces` for a FIXED file order; running the test
/// twice and diffing the output shows whether the order is a function of
/// the input at all.
#[test]
fn probe_vec_order_stability_across_processes() {
    let core = CoreIndex::new();
    let srcs: Vec<&[u8]> = vec![
        b"module Alpha\n  KEY = 1\n  class Time\n  end\n  def shared; 1; end\nend\n",
        b"module Beta\n  KEY = 2\n  class Time\n  end\n  def shared; 2; end\nend\n",
        b"module Gamma\n  KEY = 3\n  class Time\n  end\n  def shared; 3; end\nend\n",
        b"module Delta\n  KEY = 4\n  class Time\n  end\n  def shared; 4; end\nend\n",
    ];
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
    let refs: Vec<&LoweredAst> = asts.iter().collect();
    let idx = SourceIndex::build_project(&refs, &core);
    println!(
        "definers[shared/Instance] = {:?}",
        idx.definers.get(&("shared".to_string(), DefKind::Instance))
    );
    println!(
        "literal_constants[KEY] namespaces = {:?}",
        idx.literal_constants
            .get("KEY")
            .map(|v| v.iter().map(|(ns, _, _)| ns.join("::")).collect::<Vec<_>>())
    );
    println!(
        "nested_constant_namespaces[Time] = {:?}",
        idx.nested_constant_namespaces.get("Time")
    );
    println!("names = {:?}", idx.names);
}

/// PROBE 7 — the `names` (ClassId) ORDER LEAK CHANNEL, now CLOSED at the
/// renderer. `Interner::cmp` still canonicalises union members by
/// `ClassId` for `Nominal`/`Singleton` (`crates/rigor-types/src/
/// interner.rs:135,137`), but `named_union` now sorts members by their
/// rendered short description — the reference's `Union#describe`
/// (`members.sort_by { |m| m.describe(:short) }`) — so file order →
/// registration order → ClassId order no longer reaches the rendered
/// union.
#[test]
fn probe_classid_order_reaches_union_rendering() {
    use rigor_types::{Algebra, Type};
    let core = CoreIndex::new();
    let a = lower(&parse(b"class Alpha\nend\n"));
    let b = lower(&parse(b"class Beta\nend\n"));

    let render = |idx: &SourceIndex| {
        let mut i = Interner::new();
        let ca = idx.class_id("Alpha").unwrap();
        let cb = idx.class_id("Beta").unwrap();
        let na = i.intern(Type::Nominal { class: ca, args: Vec::new() });
        let nb = i.intern(Type::Nominal { class: cb, args: Vec::new() });
        let u = Algebra::join(&mut i, na, nb);
        let resolve = |c: rigor_types::ClassId| idx.class_name_for_id(c).map(str::to_string);
        (ca.0, cb.0, rigor_types::describe_named(&i, u, &resolve))
    };

    let (ida, idb, sab) = render(&SourceIndex::build_project(&[&a, &b], &core));
    println!("order [a,b]: Alpha={ida} Beta={idb} union renders as {sab:?}");
    assert!(ida < idb, "ids are handed out in file order");
    assert_eq!(sab, "Alpha | Beta");
    let (ida, idb, sba) = render(&SourceIndex::build_project(&[&b, &a], &core));
    println!("order [b,a]: Alpha={ida} Beta={idb} union renders as {sba:?}");
    assert!(ida > idb);
    assert_eq!(
        sba, "Alpha | Beta",
        "the ClassId channel is CLOSED: members sort by rendered short \
         description (the reference's `Union#describe`), so the same \
         union renders identically regardless of registration order"
    );
}

/// PROBE 8 — `infer_method_returns`'s old doc claim that "a method never
/// appears in BOTH maps" is FALSE per `(class, method)` KEY: a CROSS-FILE
/// reopen has two independent def sites, each dispatched on its own. Pinned
/// as an assertion because the corrected doc now states the truth, and the
/// merge must not re-acquire the old assumption.
#[test]
fn method_can_appear_in_both_return_maps() {
    let core = CoreIndex::new();
    let a = lower(&parse(b"class A\n  def m\n    \"s\"\n  end\nend\n"));
    let b = lower(&parse(b"class A\n  def m(x)\n    x\n  end\nend\n"));
    let idx = SourceIndex::build_project(&[&a, &b], &core);
    assert_eq!(idx.method_return("A", "m"), Some("String"));
    assert_eq!(
        idx.param_bound_return("A", "m"),
        Some(&ParamBoundReturn { param_index: 0, chain: Vec::new() }),
        "a cross-file reopen lands in BOTH maps — the call site's \
         method_return-first precedence is what makes that harmless"
    );
}

// =======================================================================
// The PRE-#92 build path, kept alive as the equivalence oracle.
// =======================================================================

/// `build_project` exactly as it stood before the harvest/merge split, with
/// its own copies of the three per-file walkers so the comparison is against
/// independent code and not a rename. The shared fold primitives
/// (`add_source`, `ingest_override_class`, `infer_method_returns`,
/// `compute_literal_returns`) are deliberately the production ones — this
/// oracle exists to pin the ORCHESTRATION (pass order, barrier placement,
/// replay order), which is what the decomposition actually moved.
fn build_project_legacy(asts: &[&LoweredAst], core: &CoreIndex) -> SourceIndex {
    let mut idx = SourceIndex::default();

    // Pass 1.
    for ast in asts {
        for (_, node) in ast.iter() {
            match node {
                Node::ClassDef { name, superclass, methods, .. } => {
                    if name.is_empty() {
                        continue;
                    }
                    idx.add_source(name, superclass.clone(), methods);
                }
                Node::ModuleDef { name, methods, .. } => {
                    if name.is_empty() {
                        continue;
                    }
                    idx.add_source(name, None, methods);
                }
                _ => {}
            }
        }
    }

    // Pass 1b.
    for ast in asts {
        legacy_collect_override_classes(&mut idx, ast, ast.root(), &[]);
    }

    // C1.
    let qualified_defs: Vec<String> = idx.override_classes.keys().cloned().collect();
    for qualified in &qualified_defs {
        let segs: Vec<&str> = qualified.split("::").collect();
        let Some((name, ns)) = segs.split_last() else { continue };
        if ns.is_empty() {
            idx.toplevel_constants.insert((*name).to_string());
        } else {
            let ns_vec: Vec<String> = ns.iter().map(|s| (*s).to_string()).collect();
            let entry = idx.nested_constant_namespaces.entry((*name).to_string()).or_default();
            if !entry.contains(&ns_vec) {
                entry.push(ns_vec);
            }
        }
    }

    // Passes 1c + 1d: the same def-attribution walk as `harvest` —
    // a `def` inside `Recv.class_eval` belongs to `Recv`, not toplevel —
    // plus the same `subtract_def_methods` barrier `merge` runs.
    let mut union_def_names: HashMap<String, HashSet<String>> = HashMap::new();
    for (i, ast) in asts.iter().enumerate() {
        let declared = collect_declared_names(ast);
        let mut tables = DefTables::default();
        let mut visited = HashSet::new();
        walk_defs(
            ast,
            &declared,
            &mut tables,
            &mut visited,
            ast.root(),
            &def_root_cx(),
        );
        file_orphan_defs(ast, &declared, &visited, &mut tables);
        for (key, new_name, old_name) in std::mem::take(&mut tables.pending_aliases) {
            if tables.def_names.get(&key).is_some_and(|defs| defs.contains(&old_name)) {
                tables.def_names.entry(key.clone()).or_default().insert(new_name.clone());
                tables
                    .file_methods
                    .entry(key.clone())
                    .or_default()
                    .insert(new_name.clone());
                if key == "Object" {
                    tables.file_toplevel.insert(new_name);
                }
            }
        }
        idx.file_index.insert(ast.file_key().clone(), i);
        idx.toplevel_defs.extend(std::mem::take(&mut tables.toplevel));
        for (owner, methods) in std::mem::take(&mut tables.macro_methods) {
            idx.discovered_methods
                .entry(owner)
                .or_default()
                .extend(methods);
        }
        for (owner, defs) in std::mem::take(&mut tables.def_names) {
            union_def_names
                .entry(owner)
                .or_default()
                .extend(defs);
        }
        idx.file_defs.push(FileDefs {
            toplevel: std::mem::take(&mut tables.file_toplevel),
            methods: std::mem::take(&mut tables.file_methods),
        });
    }
    for (owner, defs) in &union_def_names {
        if let Some(methods) = idx.discovered_methods.get_mut(owner) {
            methods.retain(|m| !defs.contains(m));
        }
    }
    let object_macros: Vec<String> = idx
        .discovered_methods
        .get("Object")
        .map(|ms| ms.iter().cloned().collect())
        .unwrap_or_default();
    idx.toplevel_defs.extend(object_macros);

    // Pass 1e.
    for ast in asts {
        for (_, node) in ast.iter() {
            let Node::Definition { params: Some(names), span, .. } = node else {
                continue;
            };
            if names.is_empty() {
                continue;
            }
            for (_, inner) in ast.iter() {
                let Node::Call { receiver: Some(r), method, span: cspan, .. } = inner else {
                    continue;
                };
                if !(span.0 <= cspan.0 && cspan.1 <= span.1) {
                    continue;
                }
                if !crate::MUTATOR_METHODS.contains(&method.as_str()) {
                    continue;
                }
                let Node::LocalVariableRead { name: recv_name, .. } = ast.get(*r) else {
                    continue;
                };
                if let Some(i) = names.iter().position(|p| p == recv_name) {
                    for key in def_names(node) {
                        idx.mutated_params.entry(key).or_default().insert(i);
                    }
                }
            }
        }
    }

    // C5.
    let mut lit_first: HashMap<String, (Vec<String>, FileKey, Option<ConstLit>)> =
        HashMap::new();
    let mut lit_multi: HashSet<String> = HashSet::new();
    for ast in asts {
        legacy_collect_literal_constants(
            ast,
            ast.root(),
            &[],
            ast.file_key(),
            &mut lit_first,
            &mut lit_multi,
            &mutated_constant_names(ast),
        );
    }
    for qualified in lit_first.keys() {
        let bare = qualified.rsplit("::").next().unwrap_or(qualified).to_string();
        idx.project_constant_write_names.insert(bare);
    }
    for (qualified, (namespace, file, lit)) in lit_first {
        if lit_multi.contains(&qualified) {
            continue;
        }
        let bare = qualified.rsplit("::").next().unwrap_or(&qualified).to_string();
        if idx.override_classes.contains_key(&qualified) || idx.classes.contains_key(&bare) {
            continue;
        }
        if let Some(l) = lit {
            idx.qualified_literal_constants
                .insert(qualified, (namespace.clone(), file.clone(), l.clone()));
            idx.literal_constants.entry(bare).or_default().push((namespace, file, l));
        }
    }

    // Pass 2.
    for ast in asts {
        for (_, node) in ast.iter() {
            if let Node::ConstantRead { name, .. } = node {
                if !name.is_empty()
                    && !idx.classes.contains_key(name)
                    && (core.knows_class(name) || core.knows_qualified_class(name))
                {
                    idx.register(name);
                }
            }
        }
    }

    // Pass 2b.
    for name in core.tuple_return_class_names() {
        if !idx.classes.contains_key(name)
            && (core.knows_class(name) || core.knows_qualified_class(name))
        {
            if !idx.name_to_id.contains_key(name) {
                idx.declaration_only_classes.insert(name.to_string());
            }
            idx.register(name);
        }
    }

    // Pass 3.
    let (returns, param_bound) = infer_method_returns(&idx, core, asts);
    idx.method_returns = returns;
    idx.param_bound_returns = param_bound;

    // Pass 4 — the PRE-CAPTURE fold, verbatim (issue #113).
    let (defs, definers) = legacy_collect_fold_defs(asts);
    idx.definers = definers;
    idx.literal_returns = legacy_compute_literal_returns(&idx, asts, &defs);

    idx
}

fn legacy_collect_override_classes(
    idx: &mut SourceIndex,
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                legacy_collect_override_classes(idx, ast, child, prefix);
            }
        }
        Node::ClassDef {
            name,
            superclass_path,
            methods,
            method_visibilities,
            includes,
            body,
            ..
        } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            idx.ingest_override_class(
                &qualified,
                superclass_path.clone(),
                methods,
                method_visibilities,
                includes,
                &[],
                false,
            );
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                legacy_collect_override_classes(idx, ast, child, &child_prefix);
            }
        }
        Node::ModuleDef { name, methods, method_visibilities, includes, body, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            idx.ingest_override_class(
                &qualified,
                None,
                methods,
                method_visibilities,
                includes,
                &[],
                true,
            );
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                legacy_collect_override_classes(idx, ast, child, &child_prefix);
            }
        }
        _ => {}
    }
}

fn legacy_collect_literal_constants(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    file: &FileKey,
    first: &mut HashMap<String, (Vec<String>, FileKey, Option<ConstLit>)>,
    multi: &mut HashSet<String>,
    mutated: &HashSet<String>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                legacy_collect_literal_constants(
                    ast, child, prefix, file, first, multi, mutated,
                );
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let child_prefix = split_qualified(&qualify(prefix, name));
            for &child in body {
                legacy_collect_literal_constants(
                    ast,
                    child,
                    &child_prefix,
                    file,
                    first,
                    multi,
                    mutated,
                );
            }
        }
        Node::ConstantWrite { name, value, .. } => {
            let qualified = qualify(prefix, name);
            let lit =
                const_lit_of(ast, *value).map(|l| widen_if_mutated(&qualified, l, mutated));
            match first.entry(qualified) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    multi.insert(e.key().clone());
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((prefix.to_vec(), file.clone(), lit));
                }
            }
        }
        _ => {}
    }
}

// =======================================================================
// ISSUE #113 — the PRE-CAPTURE Pass-4b fold, kept VERBATIM as the oracle.
//
// `FoldSite` used to carry `(ast_idx, tail: NodeId)` and `fold_expr` walked
// the real arena with a `depth` counter. Everything from here to
// `legacy_fold_expr` is that code, unchanged except for being free functions
// over `&SourceIndex` (they were `&self` methods) — so the equivalence tests
// grade the capture against an INDEPENDENT copy of the AST-walking fold, not
// against a rename of itself. The shared primitives it calls
// (`resolve_instance_owner`, `overridden_in_project`, `folding::fold`) are
// deliberately the production ones: the capture did not touch them, and the
// claim under test is that the mini-tree reaches the same calls with the
// same arguments.
// =======================================================================

/// The pre-#113 `FoldSite`: a POSITION in the `asts` slice plus a `NodeId`
/// into that file's arena.
#[derive(Clone, Copy)]
struct LegacyFoldSite {
    ast_idx: usize,
    tail: NodeId,
    has_explicit_return: bool,
}

type LegacyFoldDefs = HashMap<(String, String, DefKind), Vec<LegacyFoldSite>>;

fn legacy_compute_literal_returns(
    idx: &SourceIndex,
    asts: &[&LoweredAst],
    defs: &LegacyFoldDefs,
) -> HashMap<(String, String, DefKind), Scalar> {
    let mut memo: HashMap<(String, String, DefKind), Option<Scalar>> = HashMap::new();
    let mut closures: AncestorClosures = AncestorClosures::new();
    for key in defs.keys() {
        let mut visiting: HashSet<(String, String, DefKind)> = HashSet::new();
        legacy_resolve_fold_key(idx, key, defs, asts, &mut memo, &mut visiting, &mut closures);
    }
    memo.into_iter().filter_map(|(k, v)| v.map(|s| (k, s))).collect()
}

fn legacy_resolve_fold_key(
    idx: &SourceIndex,
    key: &(String, String, DefKind),
    defs: &LegacyFoldDefs,
    asts: &[&LoweredAst],
    memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
    visiting: &mut HashSet<(String, String, DefKind)>,
    closures: &mut AncestorClosures,
) -> Option<Scalar> {
    if let Some(v) = memo.get(key) {
        return v.clone();
    }
    if visiting.contains(key) {
        return None; // cycle (recursive method) ⇒ decline, don't memoize.
    }
    visiting.insert(key.clone());
    let raw = legacy_fold_key_sites(idx, key, defs, asts, memo, visiting, closures);
    let result = match raw {
        Some(_) if idx.overridden_in_project(&key.0, &key.1, key.2, closures) => None,
        other => other,
    };
    visiting.remove(key);
    memo.insert(key.clone(), result.clone());
    result
}

fn legacy_fold_key_sites(
    idx: &SourceIndex,
    key: &(String, String, DefKind),
    defs: &LegacyFoldDefs,
    asts: &[&LoweredAst],
    memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
    visiting: &mut HashSet<(String, String, DefKind)>,
    closures: &mut AncestorClosures,
) -> Option<Scalar> {
    let sites = defs.get(key)?;
    let mut acc: Option<Scalar> = None;
    for site in sites {
        if site.has_explicit_return {
            return None;
        }
        let ast = asts[site.ast_idx];
        let s = legacy_fold_expr(
            idx, ast, site.tail, &key.0, key.2, defs, asts, memo, visiting, closures, 0,
        )?;
        match &acc {
            None => acc = Some(s),
            Some(prev) if *prev != s => return None, // disagreeing reopen.
            _ => {}
        }
    }
    acc
}

#[allow(clippy::too_many_arguments)]
fn legacy_fold_expr(
    idx: &SourceIndex,
    ast: &LoweredAst,
    node_id: NodeId,
    self_qual: &str,
    self_kind: DefKind,
    defs: &LegacyFoldDefs,
    asts: &[&LoweredAst],
    memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
    visiting: &mut HashSet<(String, String, DefKind)>,
    closures: &mut AncestorClosures,
    depth: usize,
) -> Option<Scalar> {
    if depth > FOLD_DEPTH_CAP {
        return None;
    }
    match ast.get(node_id) {
        Node::StringLit { value, .. } => Some(Scalar::Str(value.clone())),
        Node::IntegerLit { value, .. } => value.map(Scalar::Int),
        Node::FloatLit { value, .. } => Some(Scalar::Float(*value)),
        Node::SymbolLit { value, .. } => Some(Scalar::Sym(value.clone())),
        Node::NilLit { .. } => Some(Scalar::Nil),
        Node::TrueLit { .. } => Some(Scalar::Bool(true)),
        Node::FalseLit { .. } => Some(Scalar::Bool(false)),
        Node::Call { receiver: None, method, block_body, .. } if block_body.is_empty() => {
            let method = method.clone();
            let (owner, kind) = match self_kind {
                DefKind::Singleton => (self_qual.to_string(), DefKind::Singleton),
                DefKind::Instance => {
                    (idx.resolve_instance_owner(self_qual, &method)?, DefKind::Instance)
                }
            };
            legacy_resolve_fold_key(
                idx,
                &(owner, method, kind),
                defs,
                asts,
                memo,
                visiting,
                closures,
            )
        }
        Node::Call { receiver: Some(r), method, args, block_body, .. }
            if block_body.is_empty() =>
        {
            let (r, method, args) = (*r, method.clone(), args.clone());
            if method == "!" && args.is_empty() {
                let s = legacy_fold_expr(
                    idx,
                    ast,
                    r,
                    self_qual,
                    self_kind,
                    defs,
                    asts,
                    memo,
                    visiting,
                    closures,
                    depth + 1,
                )?;
                return Some(Scalar::Bool(!scalar_truthy(&s)));
            }
            if args.is_empty() {
                if let Node::ConstantRead { name, .. } = ast.get(r) {
                    if !name.is_empty() {
                        let owner = name.strip_prefix("::").unwrap_or(name).to_string();
                        return legacy_resolve_fold_key(
                            idx,
                            &(owner, method, DefKind::Singleton),
                            defs,
                            asts,
                            memo,
                            visiting,
                            closures,
                        );
                    }
                }
            }
            let recv = legacy_fold_expr(
                idx,
                ast,
                r,
                self_qual,
                self_kind,
                defs,
                asts,
                memo,
                visiting,
                closures,
                depth + 1,
            )?;
            let mut arg_scalars = Vec::with_capacity(args.len());
            for a in args {
                arg_scalars.push(legacy_fold_expr(
                    idx,
                    ast,
                    a,
                    self_qual,
                    self_kind,
                    defs,
                    asts,
                    memo,
                    visiting,
                    closures,
                    depth + 1,
                )?);
            }
            crate::folding::fold(&recv, &method, &arg_scalars)
        }
        _ => None,
    }
}

#[allow(clippy::type_complexity)]
fn legacy_collect_fold_defs(
    asts: &[&LoweredAst],
) -> (LegacyFoldDefs, HashMap<(String, DefKind), Vec<String>>) {
    let mut defs: LegacyFoldDefs = HashMap::new();
    for (ai, ast) in asts.iter().enumerate() {
        legacy_walk_fold_defs(ai, ast, ast.root(), &[], &mut defs);
    }
    let mut definers: HashMap<(String, DefKind), Vec<String>> = HashMap::new();
    for (owner, method, kind) in defs.keys() {
        let owners = definers.entry((method.clone(), *kind)).or_default();
        if !owners.contains(owner) {
            owners.push(owner.clone());
        }
    }
    (defs, definers)
}

fn legacy_walk_fold_defs(
    ast_idx: usize,
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    defs: &mut LegacyFoldDefs,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                legacy_walk_fold_defs(ast_idx, ast, child, prefix, defs);
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            for &child in body {
                if let Node::Definition {
                    name,
                    singleton_name,
                    body: def_body,
                    has_explicit_return,
                    ..
                } = ast.get(child)
                {
                    let entry = match (name, singleton_name) {
                        (Some(m), _) => Some((m.clone(), DefKind::Instance)),
                        (None, Some(m)) => Some((m.clone(), DefKind::Singleton)),
                        _ => None,
                    };
                    if let Some((method, kind)) = entry {
                        if let Some(&tail) = def_body.last() {
                            defs.entry((qualified.clone(), method, kind)).or_default().push(
                                LegacyFoldSite {
                                    ast_idx,
                                    tail,
                                    has_explicit_return: *has_explicit_return,
                                },
                            );
                        }
                    }
                }
            }
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                legacy_walk_fold_defs(ast_idx, ast, child, &child_prefix, defs);
            }
        }
        _ => {}
    }
}

// =======================================================================
// INVARIANT 7 — merge(harvests) ≡ the legacy inline path, field by field.
// =======================================================================

/// The fields whose ORDER is a function of the input (and therefore MUST
/// match exactly): the registry bijection and the override index. The
/// remaining `Vec`-valued fields (`definers`, `literal_constants`,
/// `nested_constant_namespaces`) come out of `HashMap` iteration and are
/// already unstable between processes on identical input (§3.4) — comparing
/// them ordered would pin noise, so they are compared CANONICALISED, which
/// is the whole content either way.
const ORDER_BEARING: [&str; 4] =
    ["names", "name_to_id", "override_classes", "file_defs"];

/// Assert `SourceIndex::build_project` (harvest + merge) and the pre-#92
/// inline path agree on every field: canonicalised for content, and
/// order-exact for the order-bearing fields.
fn assert_paths_agree(asts: &[&LoweredAst], core: &CoreIndex, label: &str) {
    let new_idx = SourceIndex::build_project(asts, core);
    let old_idx = build_project_legacy(asts, core);

    let (fresh, legacy) = (fingerprint(&new_idx, true), fingerprint(&old_idx, true));
    assert_eq!(fresh.len(), 18, "the fingerprint must cover every field");
    if let Some((field, x, y)) = diff(&fresh, &legacy).into_iter().next() {
        panic!("[{label}] canonical field `{field}` diverged\n  merge  : {x}\n  legacy : {y}");
    }
    let (fresh, legacy) = (fingerprint(&new_idx, false), fingerprint(&old_idx, false));
    let ordered =
        diff(&fresh, &legacy).into_iter().find(|(field, _, _)| ORDER_BEARING.contains(field));
    if let Some((field, x, y)) = ordered {
        panic!("[{label}] ORDER of `{field}` diverged\n  merge  : {x}\n  legacy : {y}");
    }
}

/// Every corpus this module builds, under every permutation the probes use:
/// the merge must reproduce the legacy path bit for bit.
#[test]
fn merge_equals_legacy_build_project() {
    let core = CoreIndex::new();
    let corpora: Vec<(&str, Vec<&[u8]>)> = vec![
        ("order-conflicts", probe1_sources()),
        (
            "drop-one",
            vec![
                b"class Base\n  def m\n    1\n  end\nend\nSHARED = 1\nmodule Wrap\n  DUP = 1\nend\n",
                b"class Base\n  private\n  def m\n    2\n  end\nend\nmodule Wrap\n  DUP = 2\nend\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\nSOLO = 7\n",
                b"class Solo\n  def q\n    1\n  end\nend\n",
            ],
        ),
        (
            "singleton-union",
            vec![
                b"class A\n  def x\n    \"s\"\n  end\nend\nC1 = 1\n",
                b"class B < A\n  def y\n    A.new\n  end\nend\nC1 = 2\nC2 = 3\n",
                b"class C\n  def z\n    C2\n  end\nend\n",
            ],
        ),
        (
            "cross-file-couplings",
            vec![
                b"MAX = 5\nclass A\n  def m\n    MAX\n  end\nend\n",
                b"MAX = 6\n",
                b"class Base\n  def m\n    1\n  end\nend\n",
                b"class Sub < Base\n  def m\n    2\n  end\nend\n",
                b"pid, status = Process.wait2\nputs pid\nstatus.nosuchthing\n",
            ],
        ),
        (
            "intra-file-duplicate-writes",
            vec![
                b"DUP = 1\nDUP = 2\nSOLO = 3\nmodule N\n  INNER = 4\nend\n",
                b"OTHER = [1, 2].freeze\nPathname.new(\"/\")\n",
            ],
        ),
        (
            "toplevel-and-mutation",
            vec![
                b"def tl_a; 1; end\nclass Object\n  def injected; 2; end\nend\ndef fill(a)\n  a << 1\nend\n",
                b"def IO.console_size; 1; end\nmodule Kernel\n  def kern; 3; end\nend\n",
            ],
        ),
    ];
    for (label, srcs) in corpora {
        let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
        let refs: Vec<&LoweredAst> = asts.iter().collect();
        assert_paths_agree(&refs, &core, label);
        // Reverse order too: the ordered replay must track the caller's
        // order in BOTH directions, not just the one that happens to be
        // insertion-order-friendly.
        let rev: Vec<&LoweredAst> = asts.iter().rev().collect();
        assert_paths_agree(&rev, &core, &format!("{label} (reversed)"));
    }
}

/// The same equivalence under the probe's five permutations of the
/// order-conflicting corpus — the shape where file order actually moves a
/// diagnostic, so the shape a mis-ordered merge would break first.
#[test]
fn merge_equals_legacy_under_every_permutation() {
    let core = CoreIndex::new();
    let srcs = probe1_sources();
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
    for p in [[0, 1, 2, 3], [1, 0, 2, 3], [3, 2, 1, 0], [2, 3, 0, 1], [0, 2, 1, 3], [3, 0, 1, 2]]
    {
        let refs: Vec<&LoweredAst> = p.iter().map(|&i| &asts[i]).collect();
        assert_paths_agree(&refs, &core, &format!("perm {p:?}"));
    }
}

/// The single-file entry point (`SourceIndex::build`) goes through the same
/// wrapper — every single-file tool (`sig-gen`, `annotate`, `type_of`)
/// depends on it.
#[test]
fn merge_equals_legacy_for_a_single_file() {
    let core = CoreIndex::new();
    let ast = lower(&parse(
        b"module N\n  K = 1\n  class Time\n  end\n  class Foo < Bar\n    include M1\n    private\n    def m; 2; end\n  end\nend\ndef tl; 1; end\n",
    ));
    assert_paths_agree(&[&ast], &core, "single file");
    let built = SourceIndex::build(&ast, &core);
    let wrapped = SourceIndex::build_project(&[&ast], &core);
    assert_eq!(fingerprint(&built, true), fingerprint(&wrapped, true));
}

/// `merge` over an EMPTY file list must still produce the Pass-2b registry
/// (the declaration-only classes are core-driven, not source-driven).
#[test]
fn merge_of_no_files_still_runs_the_barrier_passes() {
    let core = CoreIndex::new();
    assert_paths_agree(&[], &core, "empty");
    // Turbofished because an empty literal fixes no `H` (the parameter is
    // generic over anything that lends a `Harvest` out); `check`'s own element
    // type is the owned one, so that is what the empty case must instantiate.
    let idx = SourceIndex::merge::<Harvest>(&[], &core);
    assert!(idx.is_declaration_only_class("Process::Status"));
}

// =======================================================================
// ISSUE #113 — the Pass-4b CAPTURE: equivalence, non-vacuity, and the
// FOLD_DEPTH_CAP boundary the corpora never reach.
// =======================================================================

/// A deliberately FOLD-RICH corpus: every shape `capture_fold_tail` keeps
/// (all seven scalars, implicit-self instance + singleton, `!`, `Const.method`
/// including a `::`-prefixed one, a core fold on a receiver + args) beside a
/// representative of every shape it prunes (a block-bearing call, an ivar, a
/// bare `ConstantRead`, an arg-bearing const call, an `if` carrier, an
/// explicit `return`, a recursive cycle), plus the two cross-file joins the
/// fold has — a disagreeing reopen (`Dup#two`) and the overridable degrade
/// (`Base#kind` vs `Sub#kind`).
///
/// The existing `merge_equals_legacy_*` corpora fold almost nothing, so
/// grading the capture on them alone would be near-vacuous —
/// `fold_capture_is_non_vacuous` pins that this one is not.
fn fold_corpus() -> Vec<&'static [u8]> {
    vec![
        // f0 — the ADR-0038 canonical singleton pair + half of the reopen.
        b"module Gitlab\n  module Database\n    def self.read_only?\n      false\n    end\n\n    def self.read_write?\n      !read_only?\n    end\n  end\nend\n\nclass Dup\n  def two\n    1\n  end\nend\n",
        // f1 — every scalar shape, a core fold, and a CROSS-FILE Const.method.
        b"class Flags\n  def self.enabled?\n    Gitlab::Database.read_write?\n  end\n\n  def self.label\n    \"on\"\n  end\n\n  def self.num\n    1 + 2\n  end\n\n  def self.pi\n    3.5\n  end\n\n  def self.tag\n    :sym\n  end\n\n  def self.nothing\n    nil\n  end\n\n  def self.yes\n    true\n  end\n\n  def self.cmp\n    \"a\" == \"b\"\n  end\nend\n",
        // f2 — instance implicit-self, a `::`-prefixed const call, and one of
        // every pruned shape. `blocky_self` / `blocky_const` are the
        // OVER-capture discriminators: both would fold if the capture lost
        // either `block_body.is_empty()` guard, and both must stay silent.
        b"class Conf\n  def enabled\n    flag\n  end\n\n  def flag\n    true\n  end\n\n  def deep_const\n    ::Flags.label\n  end\n\n  def loopy\n    loopy\n  end\n\n  def blocky\n    [1].map { |x| x }\n  end\n\n  def blocky_self\n    flag { 1 }\n  end\n\n  def blocky_const\n    Flags.label { 1 }\n  end\n\n  def ivar\n    @x\n  end\n\n  def branchy\n    if flag\n      1\n    else\n      2\n    end\n  end\n\n  def early\n    return 1\n  end\n\n  def bare_const\n    MAX\n  end\n\n  def const_args\n    Gitlab::Database.pick(1)\n  end\nend\n",
        // f3 — the overridable degrade + the other half of the reopen.
        b"class Base\n  def kind\n    1\n  end\nend\n\nclass Sub < Base\n  def kind\n    2\n  end\nend\n\nclass Dup\n  def two\n    2\n  end\nend\n",
    ]
}

fn folded(idx: &SourceIndex, owner: &str, method: &str, kind: DefKind) -> Option<Scalar> {
    idx.literal_returns.get(&(owner.to_string(), method.to_string(), kind)).cloned()
}

/// The capture graded against the VERBATIM pre-capture fold over the
/// fold-rich corpus, forward, reversed, and under **every** one of its 24
/// permutations. Permutations are what would catch a capture that leaked file
/// order — the mini-tree is built per file and joined per key, so a mistake
/// there shows up as a key whose sites arrive in a different order.
#[test]
fn merge_equals_legacy_over_the_fold_corpus() {
    let core = CoreIndex::new();
    let srcs = fold_corpus();
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
    let refs: Vec<&LoweredAst> = asts.iter().collect();
    assert_paths_agree(&refs, &core, "fold corpus");
    let rev: Vec<&LoweredAst> = asts.iter().rev().collect();
    assert_paths_agree(&rev, &core, "fold corpus (reversed)");
    for p in ALL_PERMUTATIONS_OF_4 {
        let refs: Vec<&LoweredAst> = p.iter().map(|&i| &asts[i]).collect();
        assert_paths_agree(&refs, &core, &format!("fold corpus perm {p:?}"));
    }
}

/// The 24 permutations of a 4-element list, spelled out so the test reads as
/// exhaustive rather than as a sample.
const ALL_PERMUTATIONS_OF_4: [[usize; 4]; 24] = [
    [0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 1, 3], [0, 2, 3, 1], [0, 3, 1, 2], [0, 3, 2, 1],
    [1, 0, 2, 3], [1, 0, 3, 2], [1, 2, 0, 3], [1, 2, 3, 0], [1, 3, 0, 2], [1, 3, 2, 0],
    [2, 0, 1, 3], [2, 0, 3, 1], [2, 1, 0, 3], [2, 1, 3, 0], [2, 3, 0, 1], [2, 3, 1, 0],
    [3, 0, 1, 2], [3, 0, 2, 1], [3, 1, 0, 2], [3, 1, 2, 0], [3, 2, 0, 1], [3, 2, 1, 0],
];

/// **The non-vacuity floor.** A capture that silently declined EVERYTHING
/// would be byte-identical to the oracle on a corpus with no folds, so the
/// equivalence tests above are only worth what this test pins: each captured
/// shape actually produces its folded value, and each pruned shape actually
/// produces no entry.
#[test]
fn fold_capture_is_non_vacuous() {
    use DefKind::{Instance, Singleton};
    let core = CoreIndex::new();
    let srcs = fold_corpus();
    let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
    let refs: Vec<&LoweredAst> = asts.iter().collect();
    let idx = SourceIndex::build_project(&refs, &core);

    // Every KEPT shape folds, and to the same value the oracle folds it to.
    let kept: [(&str, &str, DefKind, Scalar); 14] = [
        // the seven scalar shapes
        ("Gitlab::Database", "read_only?", Singleton, Scalar::Bool(false)),
        ("Flags", "label", Singleton, Scalar::Str("on".to_string())),
        ("Flags", "pi", Singleton, Scalar::Float(3.5)),
        ("Flags", "tag", Singleton, Scalar::Sym("sym".to_string())),
        ("Flags", "nothing", Singleton, Scalar::Nil),
        ("Flags", "yes", Singleton, Scalar::Bool(true)),
        ("Conf", "flag", Instance, Scalar::Bool(true)),
        // `!expr` over a singleton implicit-self call
        ("Gitlab::Database", "read_write?", Singleton, Scalar::Bool(true)),
        // a cross-file `Const.method` singleton call
        ("Flags", "enabled?", Singleton, Scalar::Bool(true)),
        // a `::`-prefixed const call from an INSTANCE body
        ("Conf", "deep_const", Instance, Scalar::Str("on".to_string())),
        // an instance implicit-self call resolved through the override index
        ("Conf", "enabled", Instance, Scalar::Bool(true)),
        // core folds on a value-pinned receiver + args
        ("Flags", "num", Singleton, Scalar::Int(3)),
        ("Flags", "cmp", Singleton, Scalar::Bool(false)),
        // a subclass override is itself foldable
        ("Sub", "kind", Instance, Scalar::Int(2)),
    ];
    for (owner, method, kind, want) in &kept {
        assert_eq!(
            folded(&idx, owner, method, *kind).as_ref(),
            Some(want),
            "{owner}.{method}/{kind:?} must fold to {want:?} — the capture kept its shape"
        );
    }

    // Every PRUNED shape declines. A capture that kept one of these would
    // start emitting a `Type::Constant` where the pre-capture fold emitted
    // nothing, which is the FP direction.
    let pruned: [(&str, &str, DefKind); 11] = [
        ("Conf", "loopy", Instance),        // recursive cycle
        ("Conf", "blocky", Instance),       // a receiver-bearing call WITH a block
        ("Conf", "blocky_self", Instance),  // an implicit-self call WITH a block
        ("Conf", "blocky_const", Instance), // `Const.method { }` — a block
        ("Conf", "ivar", Instance),         // an ivar read
        ("Conf", "branchy", Instance),      // an `if` carrier
        ("Conf", "early", Instance),        // an explicit `return`
        ("Conf", "bare_const", Instance),   // a bare `ConstantRead` tail
        ("Conf", "const_args", Instance),   // `Const.method(1)` — args non-empty
        ("Base", "kind", Instance),         // the overridable degrade
        ("Dup", "two", Instance),           // a disagreeing cross-file reopen
    ];
    for (owner, method, kind) in pruned {
        assert_eq!(
            folded(&idx, owner, method, kind),
            None,
            "{owner}.{method}/{kind:?} must decline"
        );
    }

    // …and the table is not accidentally the whole table: the corpus folds
    // exactly the 14 keys above.
    assert_eq!(idx.literal_returns.len(), kept.len());
}

/// A `!`-chain `n` deep: `def m; !!!…!true; end`. The tail is the OUTERMOST
/// `!` at depth 0 and each nested `!` is one deeper, so the `true` leaf sits
/// at depth `n` — which is exactly the axis [`FOLD_DEPTH_CAP`] cuts.
fn not_chain(n: usize) -> Vec<u8> {
    let mut s = b"class Deep\n  def m\n    ".to_vec();
    s.extend(std::iter::repeat_n(b'!', n));
    s.extend_from_slice(b"true\n  end\nend\n");
    s
}

/// An argument-nested arithmetic chain `1 + (1 + (1 + … + 1))`, `n` deep. A
/// second, structurally different depth axis: the recursion here runs through
/// `CoreCall`'s ARGS, not through `Not`'s single operand, and it also fans out
/// (a receiver AND an arg at every level).
fn add_chain(n: usize) -> Vec<u8> {
    let mut s = b"class Deep\n  def m\n    ".to_vec();
    for _ in 0..n {
        s.extend_from_slice(b"1 + (");
    }
    s.extend_from_slice(b"1");
    s.extend(std::iter::repeat_n(b')', n));
    s.extend_from_slice(b"\n  end\nend\n");
    s
}

/// **The `FOLD_DEPTH_CAP` boundary harness (mini-spec evidence item 2).** The
/// cap has never been observed to fire on any corpus, so the sweep and the
/// fixtures give it ZERO coverage — and the capture makes it load-bearing,
/// because it is now the capture that applies it (a mini-tree truncated one
/// level too shallow or too deep changes a fold, silently).
///
/// Both depth axes, at 15 / 16 / 17, graded against the verbatim pre-capture
/// fold. `#94`'s cap-boundary lesson applies verbatim: a cap's only coverage
/// is a purpose-built test.
#[test]
fn fold_depth_cap_boundary_agrees_with_the_oracle() {
    let core = CoreIndex::new();
    for n in [0, 1, 14, 15, 16, 17, 18, 20] {
        for (axis, src) in [("not", not_chain(n)), ("add", add_chain(n))] {
            let ast = lower(&parse(&src));
            assert_paths_agree(&[&ast], &core, &format!("{axis} chain depth {n}"));
        }
    }
}

/// …and the boundary is where it is claimed to be, not merely agreed upon.
/// `assert_paths_agree` alone would pass if BOTH sides declined everything;
/// this pins the exact 16-in / 17-out step, in both engines.
#[test]
fn fold_depth_cap_fires_at_seventeen() {
    let core = CoreIndex::new();
    let key = ("Deep".to_string(), "m".to_string(), DefKind::Instance);

    // `!`-chain: `n` bangs put the literal at depth `n`. `fold_expr` declined
    // on ENTRY at `depth > 16`, so `n = 16` is the last one that folds.
    for n in [0, 1, 15, 16] {
        let ast = lower(&parse(&not_chain(n)));
        let idx = SourceIndex::build_project(&[&ast], &core);
        assert_eq!(
            idx.literal_returns.get(&key),
            Some(&Scalar::Bool(n % 2 == 0)),
            "a {n}-deep `!` chain is INSIDE the cap and must fold"
        );
    }
    for n in [17, 18, 25] {
        let ast = lower(&parse(&not_chain(n)));
        let idx = SourceIndex::build_project(&[&ast], &core);
        assert_eq!(
            idx.literal_returns.get(&key),
            None,
            "a {n}-deep `!` chain is PAST the cap and must decline"
        );
    }

    // …and the same step on the arg-nested axis.
    for n in [15, 16] {
        let ast = lower(&parse(&add_chain(n)));
        let idx = SourceIndex::build_project(&[&ast], &core);
        assert_eq!(
            idx.literal_returns.get(&key),
            Some(&Scalar::Int(n as i64 + 1)),
            "a {n}-deep `+` chain is INSIDE the cap and must fold"
        );
    }
    for n in [17, 18] {
        let ast = lower(&parse(&add_chain(n)));
        let idx = SourceIndex::build_project(&[&ast], &core);
        assert_eq!(
            idx.literal_returns.get(&key),
            None,
            "a {n}-deep `+` chain is PAST the cap and must decline"
        );
    }
}

/// The cap resets per SITE, exactly as `fold_key_sites` reset it per key by
/// always entering `fold_expr` at depth 0: a 16-deep tail that calls ANOTHER
/// 16-deep method still folds, because the callee's tail is a fresh capture
/// rooted at 0. A capture that threaded a running depth across
/// `resolve_fold_key` would break this and nothing else would notice.
#[test]
fn fold_depth_cap_resets_per_site() {
    let core = CoreIndex::new();
    let mut src = b"class Deep\n  def outer\n    ".to_vec();
    src.extend(std::iter::repeat_n(b'!', 16));
    src.extend_from_slice(b"inner\n  end\n\n  def inner\n    ");
    src.extend(std::iter::repeat_n(b'!', 16));
    src.extend_from_slice(b"true\n  end\nend\n");
    let ast = lower(&parse(&src));
    assert_paths_agree(&[&ast], &core, "per-site depth reset");
    let idx = SourceIndex::build_project(&[&ast], &core);
    // 16 `!` over `inner`: the innermost operand is the SelfCall at depth 16,
    // which is inside the cap; `inner` then folds from its own depth 0.
    assert_eq!(
        idx.literal_returns.get(&("Deep".to_string(), "inner".to_string(), DefKind::Instance)),
        Some(&Scalar::Bool(true))
    );
    assert_eq!(
        idx.literal_returns.get(&("Deep".to_string(), "outer".to_string(), DefKind::Instance)),
        Some(&Scalar::Bool(true)),
        "32 syntactic levels fold because the cap resets at the call boundary"
    );
}

// =======================================================================
// INVARIANTS 1-6 — the five minimal coupling examples, plus the three
// semantics the merge is built on top of.
// =======================================================================

/// INVARIANT 1, example 1 (probe §3.2 i) — `method_visibilities` is
/// FIRST-WRITE-WINS, so file order decides whether
/// `def.override-visibility-reduced` fires. `a,b` records `m` public on
/// `Base` (⇒ `Sub#m` private reduces it ⇒ fires); `b,a` records it private
/// (⇒ silent). The diagnostic-level twin lives in rigor-rules
/// (`override_vis_project_order_is_normative`).
#[test]
fn order_leak_visibility_first_write_wins() {
    let core = CoreIndex::new();
    let a = lower(&parse(b"class Base\n  def m\n    1\n  end\nend\n"));
    let b = lower(&parse(
        b"class Base\n  private\n  def m\n    2\n  end\nend\n\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\n",
    ));
    let vis = |idx: &SourceIndex| {
        idx.override_classes.get("Base").and_then(|c| c.method_visibilities.get("m")).copied()
    };
    assert_eq!(vis(&SourceIndex::build_project(&[&a, &b], &core)), Some(Visibility::Public));
    assert_eq!(vis(&SourceIndex::build_project(&[&b, &a], &core)), Some(Visibility::Private));
    // …and the walk that reads it flips with them.
    let ab = SourceIndex::build_project(&[&a, &b], &core);
    let ba = SourceIndex::build_project(&[&b, &a], &core);
    assert_eq!(
        ab.nearest_ancestor_defining("Sub", "m"),
        Some(("Base".to_string(), Some(Visibility::Public)))
    );
    assert_eq!(
        ba.nearest_ancestor_defining("Sub", "m"),
        Some(("Base".to_string(), Some(Visibility::Private)))
    );
}

/// INVARIANT 1, example 2 (probe §3.2 ii) — `includes` accumulate in FILE
/// order and `override_ancestor_names` walks them in that order, so the
/// MRO's nearest defining ancestor flips with the file order. Idiomatic
/// Ruby: one class reopened in two files, each adding an `include`.
#[test]
fn order_leak_includes_accumulation_order() {
    let core = CoreIndex::new();
    let mods = lower(&parse(
        b"module M1\n  def m; 1; end\nend\nmodule M2\n  private\n  def m; 2; end\nend\n",
    ));
    let a = lower(&parse(b"class Foo\n  include M1\nend\n"));
    let b = lower(&parse(b"class Foo\n  include M2\n  private\n  def m; 3; end\nend\n"));
    let incs = |idx: &SourceIndex| {
        idx.override_classes.get("Foo").map(|c| c.includes.clone()).unwrap_or_default()
    };
    let ab = SourceIndex::build_project(&[&mods, &a, &b], &core);
    let ba = SourceIndex::build_project(&[&mods, &b, &a], &core);
    assert_eq!(incs(&ab), vec!["M1".to_string(), "M2".to_string()]);
    assert_eq!(incs(&ba), vec!["M2".to_string(), "M1".to_string()]);
    // `Foo` defines `m` itself, so the ancestor walk starts at the includes:
    // M1 first ⇒ the public definer is found ⇒ the rule fires; M2 first ⇒ a
    // private definer ⇒ silent.
    assert_eq!(
        ab.nearest_ancestor_defining("Foo", "m"),
        Some(("M1".to_string(), Some(Visibility::Public)))
    );
    assert_eq!(
        ba.nearest_ancestor_defining("Foo", "m"),
        Some(("M2".to_string(), Some(Visibility::Private)))
    );
}

/// INVARIANT 2, example 3 (probe §2.2) — Pass 3 is typed against the
/// COMPLETE index, so a second file's constant write can delete a tier-4b
/// return for a byte-identical first file. The merge must keep Pass 3 after
/// the C5 barrier or this silently re-appears as an over-emission.
#[test]
fn coupling_pass3_reads_the_merged_constant_table() {
    let core = CoreIndex::new();
    let a = lower(&parse(b"MAX = 5\nclass A\n  def m\n    MAX\n  end\nend\n"));
    let b = lower(&parse(b"MAX = 6\n"));
    assert_eq!(SourceIndex::build_project(&[&a], &core).method_return("A", "m"), Some("Integer"));
    assert_eq!(SourceIndex::build_project(&[&a, &b], &core).method_return("A", "m"), None);
}

/// INVARIANT 2, example 4 (probe §2.2) — the Pass-4b overridable degrade is
/// cross-file: `Base.m`'s folded literal exists alone and VANISHES once a
/// second file declares a related subclass that redefines `m`.
#[test]
fn coupling_pass4b_degrade_is_cross_file() {
    let core = CoreIndex::new();
    let a = lower(&parse(b"class Base\n  def m\n    1\n  end\nend\n"));
    let b = lower(&parse(b"class Sub < Base\n  def m\n    2\n  end\nend\n"));
    let key = ("Base".to_string(), "m".to_string(), DefKind::Instance);
    assert_eq!(
        SourceIndex::build_project(&[&a], &core).literal_returns.get(&key),
        Some(&Scalar::Int(1))
    );
    assert_eq!(SourceIndex::build_project(&[&a, &b], &core).literal_returns.get(&key), None);
}

/// INVARIANT 2, example 5 (probe §2.2) — Pass 2b's declaration-only set asks
/// "did NO analyzed file name this class?", which is unanswerable per file.
/// `Process::Status` is declaration-only until some file names it.
#[test]
fn coupling_pass2b_declaration_only_is_cross_file() {
    let core = CoreIndex::new();
    let a = lower(&parse(b"pid, status = Process.wait2\nputs pid\nstatus.nosuchthing\n"));
    let b = lower(&parse(b"KLASS = Process::Status\nputs KLASS\n"));
    assert!(SourceIndex::build_project(&[&a], &core)
        .is_declaration_only_class("Process::Status"));
    assert!(!SourceIndex::build_project(&[&a, &b], &core)
        .is_declaration_only_class("Process::Status"));
}

/// INVARIANT 3 — the constant single-assignment gate counts INTRA-file
/// duplicates too, which is why `Harvest` carries a per-file write COUNT and
/// not a "was written" bool. All three shapes must decline identically:
/// twice in one file, once each in two files, twice in one of two files.
#[test]
fn constant_single_assignment_counts_intra_file_duplicates() {
    let core = CoreIndex::new();
    let twice_here = lower(&parse(b"DUP = 1\nDUP = 2\nSOLO = 9\n"));
    let once_here = lower(&parse(b"DUP = 1\nSOLO = 9\n"));
    let once_there = lower(&parse(b"DUP = 2\n"));
    let solo = lower(&parse(b"UNRELATED = 3\n"));

    let harvested = |idx: &SourceIndex, name: &str| idx.literal_constants.contains_key(name);
    // Twice in ONE file ⇒ declined; the single write beside it survives.
    let one = SourceIndex::build_project(&[&twice_here], &core);
    assert!(!harvested(&one, "DUP"));
    assert!(harvested(&one, "SOLO"));
    // Once in each of two files ⇒ declined.
    let split = SourceIndex::build_project(&[&once_here, &once_there], &core);
    assert!(!harvested(&split, "DUP"));
    // Twice in one of two files ⇒ still declined.
    let mixed = SourceIndex::build_project(&[&twice_here, &solo], &core);
    assert!(!harvested(&mixed, "DUP"));
    // …and a lone write in a two-file project still harvests.
    assert!(harvested(&SourceIndex::build_project(&[&once_here, &solo], &core), "DUP"));
}

/// INVARIANT 4 — `register` is idempotent. The Pass-2 pre-filter drops
/// today's `!classes.contains_key(name)` term and the harvest deduplicates
/// repeated reads; both are no-ops ONLY because a repeat `register` neither
/// appends a name nor moves an id.
#[test]
fn register_is_idempotent() {
    let mut idx = SourceIndex::default();
    idx.register("Alpha");
    idx.register("Beta");
    let (names, ids) = (idx.names.clone(), idx.name_to_id.clone());
    for _ in 0..3 {
        idx.register("Alpha");
        idx.register("Beta");
    }
    assert_eq!(idx.names, names);
    assert_eq!(idx.name_to_id, ids);
    // The same fact end to end: a file that reads `Time` fifty times
    // registers it exactly once, at the same id as reading it once.
    let core = CoreIndex::new();
    let once = lower(&parse(b"Time\n"));
    let many = lower(&parse(b"Time\nTime\nTime\nTime\n"));
    assert_eq!(
        SourceIndex::build_project(&[&once], &core).names,
        SourceIndex::build_project(&[&many], &core).names
    );
}

/// INVARIANT 5 — `HarvestedConst`'s file key keeps its PER-FILE consumption
/// semantics: the stamp is the ASSIGNING file's `LoweredAst::file_key`, and a
/// use site in another file never folds. The merge stamps it from the paired
/// AST (never from the harvest), which is the discipline a persisted harvest
/// would have to keep — see the type's doc.
#[test]
fn harvested_const_file_id_is_the_assigning_file() {
    let core = CoreIndex::new();
    let a = lower(&parse(b"TOPL = 7\n"));
    let b = lower(&parse(b"puts TOPL\n"));
    let idx = SourceIndex::build_project(&[&a, &b], &core);
    let entries = idx.literal_constants.get("TOPL").expect("harvested");
    assert_eq!(entries.len(), 1);
    assert_eq!(&entries[0].1, a.file_key(), "the ASSIGNING file's key, not the reader's");
    assert!(idx.literal_constant("TOPL", &[], a.file_key()).is_some());
    assert!(
        idx.literal_constant("TOPL", &[], b.file_key()).is_none(),
        "a cross-file read must not fold (the oracle is silent there)"
    );
}

/// Issue #102 — the same INVARIANT 5 gate, now stated over PATH-derived keys:
/// two distinct FILES that assign the SAME bare constant name each fold only
/// at their own use sites. This is the C5 per-file rule itself, and the one
/// thing the identity swap must not widen — the gate was never "same
/// `lower()` call", it was always "same file"; the counter was the accident.
#[test]
fn path_keyed_gate_still_folds_per_file() {
    let dir = std::env::temp_dir().join(format!(
        "rigor_s102_perfile_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, src: &str| {
        let p = dir.join(name);
        std::fs::write(&p, src).unwrap();
        p
    };
    // Same bare name `LIMIT`, different NAMESPACES (so the project-wide
    // single-assignment gate does not decline both), each read in its own
    // file.
    let pa = write("a.rb", "module A\n  LIMIT = 5\n  LIMIT\nend\n");
    let pb = write("b.rb", "module B\n  LIMIT = 9\n  LIMIT\nend\n");
    let key = |p: &std::path::Path| FileKey::for_path(p);
    let a = rigor_parse::lower_with_key(&parse(b"module A\n  LIMIT = 5\n  LIMIT\nend\n"), key(&pa));
    let b = rigor_parse::lower_with_key(&parse(b"module B\n  LIMIT = 9\n  LIMIT\nend\n"), key(&pb));

    let core = CoreIndex::new();
    let idx = SourceIndex::build_project(&[&a, &b], &core);
    let seg = |parts: &[&str]| parts.iter().map(|s| (*s).to_string()).collect::<Vec<String>>();
    let a_ns = seg(&["A"]);
    let b_ns = seg(&["B"]);
    // Each file folds its OWN constant …
    assert_eq!(
        idx.literal_constant("LIMIT", &a_ns, a.file_key()),
        Some(&ConstLit::Scalar(Scalar::Int(5)))
    );
    assert_eq!(
        idx.literal_constant("LIMIT", &b_ns, b.file_key()),
        Some(&ConstLit::Scalar(Scalar::Int(9)))
    );
    // … and NEITHER folds the other's, even though the bare name matches and
    // the entry is present in the project-wide map.
    assert_eq!(idx.literal_constant("LIMIT", &b_ns, a.file_key()), None);
    assert_eq!(idx.literal_constant("LIMIT", &a_ns, b.file_key()), None);

    // A RE-lowering of the same file is the SAME file — this is the whole
    // point of #102, and the property the old counter did not have.
    let a_again =
        rigor_parse::lower_with_key(&parse(b"module A\n  LIMIT = 5\n  LIMIT\nend\n"), key(&pa));
    assert_eq!(a_again.file_key(), a.file_key(), "a re-lowering keeps the file's identity");
    assert_eq!(
        idx.literal_constant("LIMIT", &a_ns, a_again.file_key()),
        Some(&ConstLit::Scalar(Scalar::Int(5))),
        "the fold survives a re-lowering of the assigning file"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Issue #102 — PATHLESS inputs must not collide. `lower()` (a test, stdin, a
/// synthesized buffer) has no filesystem identity to key on, so it gets a
/// fresh anonymous key: two different pathless lowerings are two different
/// files, and neither folds the other's constants. Without this the retired
/// counter's one real job would have been lost.
#[test]
fn pathless_lowerings_never_collide() {
    let core = CoreIndex::new();
    let a = lower(&parse(b"module A\n  LIMIT = 5\n  LIMIT\nend\n"));
    let b = lower(&parse(b"module B\n  LIMIT = 9\n  LIMIT\nend\n"));
    assert_ne!(a.file_key(), b.file_key(), "two pathless lowerings are two files");
    // Even BYTE-IDENTICAL pathless lowerings are distinct files.
    let c = lower(&parse(b"module A\n  LIMIT = 5\n  LIMIT\nend\n"));
    assert_ne!(a.file_key(), c.file_key(), "identical bytes, still two files");

    let idx = SourceIndex::build_project(&[&a, &b], &core);
    let seg = |parts: &[&str]| parts.iter().map(|s| (*s).to_string()).collect::<Vec<String>>();
    let (a_ns, b_ns) = (seg(&["A"]), seg(&["B"]));
    assert!(idx.literal_constant("LIMIT", &a_ns, a.file_key()).is_some());
    assert!(idx.literal_constant("LIMIT", &b_ns, b.file_key()).is_some());
    assert_eq!(idx.literal_constant("LIMIT", &a_ns, b.file_key()), None);
    assert_eq!(idx.literal_constant("LIMIT", &b_ns, a.file_key()), None);
    // …and a THIRD pathless lowering of `a`'s bytes folds nothing, because it
    // is not the file the index was built from.
    assert_eq!(idx.literal_constant("LIMIT", &a_ns, c.file_key()), None);
}

/// The `Harvest` contract itself: it is a function of ONE file and the
/// frozen core, so harvesting a file alone or beside others is the same
/// object — this is what makes the CLI's stage-1 hoist legal.
#[test]
fn harvest_is_file_local() {
    let core = CoreIndex::new();
    let a = lower(&parse(b"MAX = 5\nclass A\n  def m\n    MAX\n  end\nend\n"));
    let b = lower(&parse(b"MAX = 6\nclass A\n  include M\nend\n"));
    let solo = SourceIndex::harvest(&a, &core);
    let beside = SourceIndex::harvest(&a, &core);
    let render = |h: &Harvest| {
        (
            h.source_classes.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            h.override_classes.iter().map(|c| c.qualified.clone()).collect::<Vec<_>>(),
            h.constant_writes.iter().map(|w| (w.qualified.clone(), w.writes)).collect::<Vec<_>>(),
            h.rbs_constant_names.clone(),
            h.fold_defs.iter().map(|d| (d.owner.clone(), d.method.clone())).collect::<Vec<_>>(),
        )
    };
    assert_eq!(render(&solo), render(&beside));
    // And merging that harvest with the second file's reproduces the
    // all-at-once build exactly.
    let pairs = vec![(SourceIndex::harvest(&a, &core), &a), (SourceIndex::harvest(&b, &core), &b)];
    assert_eq!(
        fingerprint(&SourceIndex::merge(&pairs, &core), false),
        fingerprint(&SourceIndex::build_project(&[&a, &b], &core), false)
    );
}

/// The four files probe 1 permutes: two visibility conflicts, an include
/// order conflict, constants (toplevel + nested, single + multiply
/// assigned), a toplevel def and an RBS-known constant read.
///
/// `pub(super)` so the #94 equivalence harness (`probes_s94`) can grade the
/// ancestor closure on the same override-graph shapes.
pub(super) fn probe1_sources() -> Vec<&'static [u8]> {
    vec![
        b"class Base\n  def m\n    1\n  end\nend\nmodule M1\n  def shared\n    1\n  end\nend\nTOPC = 1\ndef tl_a; 1; end\nmodule N1\n  K = 1\n  class Time\n  end\nend\n",
        b"class Base\n  include M1\n  private\n  def m\n    2\n  end\nend\n",
        b"module M2\n  private\n  def shared\n    2\n  end\nend\nclass Base\n  include M2\nend\nclass Sub < Base\n  private\n  def shared\n    3\n  end\nend\n",
        b"class Other\n  def name\n    \"x\"\n  end\nend\nOTHERC = [1, 2].freeze\nPathname.new(\"/\")\nmodule N2\n  K = 2\n  class Time\n  end\nend\n",
    ]
}
