//! Best-effort human-readable rendering of a carrier.
//!
//! `describe(TypeId)` matches the reference spelling where known (the
//! analyzer-internal `Constant[3]` convention, `Dynamic[top]`, `T | nil`,
//! `(1..10)` for ranges). Display MAY render a normalized type more readably
//! (e.g. `bool` for `true | false`) but MUST NOT change type identity
//! (normalization.md §"Interaction with display").

use crate::algebra::is_bool_pair;
use crate::interner::Interner;
use crate::ty::{ClassId, Scalar, ShapeKey, Type, TypeId};

/// The user-facing type display — a faithful port of the reference's
/// `Type#describe(:short)` (`lib/rigor/type/*.rb`).
///
/// Distinct from [`describe`], which is the analyzer-INTERNAL display
/// (`Constant[3]`, `Class<id>`, `Tuple[...]`) and cannot resolve class names.
/// This resolves class names (via `resolve`) and matches the reference's
/// user-facing spelling exactly: a `Nominal` renders its class name (`String`,
/// `Array[Integer]`), a `Constant` renders Ruby's `inspect` (`3`, `"hi"`,
/// `:sym`), a `Tuple` renders value-pinned (`[1, 2, 3]`), an optional union
/// renders `T?`, an integer range renders `int<…>` / its alias.
///
/// The shared renderer for every user-facing surface (`check` receiver messages,
/// `type-of`, `triage` selectors) so they speak one reference-faithful type
/// vocabulary. `resolve` maps a [`ClassId`] to its name (core RBS + project
/// `sig/`); a class it cannot name falls back to `Class<id>` (rare — an unknown
/// class is normally `Dynamic`, not a bare `Nominal`).
pub fn describe_named(
    i: &Interner,
    id: TypeId,
    resolve: &dyn Fn(ClassId) -> Option<String>,
) -> String {
    match i.get(id) {
        Type::Top => "top".to_string(),
        Type::Bottom => "bot".to_string(),
        Type::Dynamic(facet) => format!("Dynamic[{}]", describe_named(i, *facet, resolve)),

        Type::Nominal { class, args } => {
            let name = named_class(*class, resolve);
            if args.is_empty() {
                name
            } else {
                let inner: Vec<String> =
                    args.iter().map(|&a| describe_named(i, a, resolve)).collect();
                format!("{name}[{}]", inner.join(", "))
            }
        }
        Type::Singleton(class) => format!("singleton({})", named_class(*class, resolve)),

        Type::Constant(s) => scalar_inspect(s),

        Type::Tuple(elems) => {
            if elems.is_empty() {
                "[]".to_string()
            } else {
                let inner: Vec<String> =
                    elems.iter().map(|&e| describe_named(i, e, resolve)).collect();
                format!("[{}]", inner.join(", "))
            }
        }

        Type::HashShape(members) => {
            if members.is_empty() {
                "{}".to_string()
            } else {
                let inner: Vec<String> = members
                    .iter()
                    .map(|m| {
                        let key = named_key(&m.key);
                        let key = if m.optional { format!("?{key}") } else { key };
                        format!("{key}: {}", describe_named(i, m.value, resolve))
                    })
                    .collect();
                format!("{{ {} }}", inner.join(", "))
            }
        }

        Type::IntegerRange { min, max } => named_integer_range(*min, *max),

        Type::Union(members) => named_union(i, members, resolve),

        Type::Difference { base, removed } => format!(
            "{} - {}",
            describe_named(i, *base, resolve),
            describe_named(i, *removed, resolve)
        ),
        Type::Intersection(members) => members
            .iter()
            .map(|&m| describe_named(i, m, resolve))
            .collect::<Vec<_>>()
            .join(" & "),
        Type::Complement(inner) => format!("~{}", describe_named(i, *inner, resolve)),

        // Refinement / App / DataInstance carry names/tables the reference
        // resolves through machinery rigor-rs has not ported; render the closest
        // faithful approximation rather than leaking an internal id.
        Type::Refined { base, .. } => describe_named(i, *base, resolve),
        Type::App { .. } => "App".to_string(),
        Type::DataInstance { class, members } => {
            let inner: Vec<String> = members
                .iter()
                .map(|(name, ty)| format!("{name}: {}", describe_named(i, *ty, resolve)))
                .collect();
            format!("{}({})", named_class(*class, resolve), inner.join(", "))
        }

        Type::Void => "void".to_string(),
        Type::SelfType => "self".to_string(),
        Type::Instance => "instance".to_string(),
        Type::ClassType => "class".to_string(),
    }
}

/// A class id's name, or the `Class<id>` fallback when `resolve` cannot name it.
fn named_class(class: ClassId, resolve: &dyn Fn(ClassId) -> Option<String>) -> String {
    resolve(class).unwrap_or_else(|| format!("Class<{}>", class.0))
}

/// A `Constant` scalar as Ruby's `inspect` renders it (reference
/// `Constant#describe`): strings quoted, symbols colon-prefixed, floats always
/// with a decimal (`3.0`, not `3`), everything else by its natural spelling.
fn scalar_inspect(s: &Scalar) -> String {
    match s {
        Scalar::Str(v) => format!("{v:?}"),
        Scalar::Sym(v) => format!(":{v}"),
        Scalar::Int(n) => n.to_string(),
        Scalar::Float(f) => named_float(*f),
        Scalar::Bool(b) => b.to_string(),
        Scalar::Nil => "nil".to_string(),
    }
}

/// Ruby `Float#inspect` always shows a decimal point (`3.0.inspect == "3.0"`),
/// unlike Rust's `3.0_f64.to_string() == "3"`.
fn named_float(f: f64) -> String {
    if f.is_finite() && f == f.trunc() {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

/// A HashShape key as the reference `render_key` renders it: a symbol bare
/// (keyword style `name:`), a string quoted.
fn named_key(k: &ShapeKey) -> String {
    match k {
        ShapeKey::Sym(s) => s.clone(),
        ShapeKey::Str(s) => format!("{s:?}"),
        ShapeKey::Int(v) => v.to_string(),
        ShapeKey::Other => "_".to_string(),
    }
}

/// An `IntegerRange` as the reference renders it: a named alias for the five
/// canonical unbounded ranges, else `int<lo, hi>` with an open bound spelled
/// `min`/`max` (reference `ALIAS_NAMES` + `generic_description`).
fn named_integer_range(min: Option<i64>, max: Option<i64>) -> String {
    match (min, max) {
        (None, None) => "int".to_string(),
        (Some(1), None) => "positive-int".to_string(),
        (Some(0), None) => "non-negative-int".to_string(),
        (None, Some(-1)) => "negative-int".to_string(),
        (None, Some(0)) => "non-positive-int".to_string(),
        (lo, hi) => {
            let lo = lo.map_or_else(|| "min".to_string(), |v| v.to_string());
            let hi = hi.map_or_else(|| "max".to_string(), |v| v.to_string());
            format!("int<{lo}, {hi}>")
        }
    }
}

/// A union as the reference `Union#describe` renders it: an optional union
/// (`T | nil`, one logical non-nil member) collapses to `T?`; a `true | false`
/// pair collapses to `bool`; otherwise the members join with ` | `.
fn named_union(
    i: &Interner,
    members: &[TypeId],
    resolve: &dyn Fn(ClassId) -> Option<String>,
) -> String {
    let is_nil = |id: TypeId| matches!(i.get(id), Type::Constant(Scalar::Nil));
    let is_bool_lit = |id: TypeId| matches!(i.get(id), Type::Constant(Scalar::Bool(_)));
    let has_true =
        members.iter().any(|&m| matches!(i.get(m), Type::Constant(Scalar::Bool(true))));
    let has_false =
        members.iter().any(|&m| matches!(i.get(m), Type::Constant(Scalar::Bool(false))));
    let bool_pair = has_true && has_false;

    let has_nil = members.iter().any(|&m| is_nil(m));
    let significant = members.iter().filter(|&&m| !is_nil(m)).count();
    let logical = significant as i64 - i64::from(bool_pair);

    if has_nil && logical == 1 {
        let inner = if bool_pair {
            "bool".to_string()
        } else {
            let m = *members.iter().find(|&&m| !is_nil(m)).unwrap();
            describe_named(i, m, resolve)
        };
        return format!("{inner}?");
    }

    if bool_pair {
        let rest =
            members.iter().filter(|&&m| !is_bool_lit(m)).map(|&m| describe_named(i, m, resolve));
        return std::iter::once("bool".to_string()).chain(rest).collect::<Vec<_>>().join(" | ");
    }

    // Float `nil` to the end (the common `T | … | nil` reading), keeping every
    // other member in its canonical order — matching the reference's union order.
    let mut rendered: Vec<(bool, String)> =
        members.iter().map(|&m| (is_nil(m), describe_named(i, m, resolve))).collect();
    rendered.sort_by_key(|(nil, _)| *nil);
    rendered.into_iter().map(|(_, s)| s).collect::<Vec<_>>().join(" | ")
}

/// Render `id` as a human-readable string.
pub fn describe(i: &Interner, id: TypeId) -> String {
    match i.get(id) {
        Type::Top => "top".to_string(),
        Type::Bottom => "bot".to_string(),

        // untyped == Dynamic[top] displays as `Dynamic[top]`. The internal
        // form still records provenance; this is display only.
        Type::Dynamic(facet) => format!("Dynamic[{}]", describe(i, *facet)),

        Type::Nominal { class, args } => {
            // Skeleton: render the nominal as `Class<id>` since the class name
            // table lives elsewhere. // TODO(spec): resolve ClassId to its name
            // (Integer, String, ...) via the class index.
            if args.is_empty() {
                format!("Class<{}>", class.0)
            } else {
                let inner: Vec<String> = args.iter().map(|&a| describe(i, a)).collect();
                format!("Class<{}>[{}]", class.0, inner.join(", "))
            }
        }

        // The class object itself. Renders the raw ClassId since the class
        // name table lives elsewhere, mirroring `Nominal`'s `Class<id>` style.
        Type::Singleton(class) => format!("singleton(Class<{}>)", class.0),

        // The singleton `nil` is spelled bare (special-types.md prefers the
        // `nil` singleton over `NilClass`/`Constant[nil]`).
        Type::Constant(Scalar::Nil) => "nil".to_string(),
        Type::Constant(s) => format!("Constant[{}]", scalar(s)),

        Type::Tuple(elems) => {
            let inner: Vec<String> = elems.iter().map(|&e| describe(i, e)).collect();
            format!("Tuple[{}]", inner.join(", "))
        }

        Type::HashShape(members) => {
            let inner: Vec<String> = members
                .iter()
                .map(|m| {
                    let opt = if m.optional { "?" } else { "" };
                    format!("{}{} => {}", shape_key(&m.key), opt, describe(i, m.value))
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }

        // `(1..10)` style. Open bounds elide the endpoint.
        Type::IntegerRange { min, max } => {
            let lo = min.map(|v| v.to_string()).unwrap_or_default();
            let hi = max.map(|v| v.to_string()).unwrap_or_default();
            format!("({lo}..{hi})")
        }

        Type::Refined { base, refinement } => {
            format!("{}[refine #{}]", describe(i, *base), refinement.0)
        }

        Type::Difference { base, removed } => {
            format!("{} - {}", describe(i, *base), describe(i, *removed))
        }

        Type::Intersection(members) => {
            let inner: Vec<String> = members.iter().map(|&m| describe(i, m)).collect();
            inner.join(" & ")
        }

        Type::Complement(inner) => format!("~{}", describe(i, *inner)),

        Type::App { uri, args } => {
            let inner: Vec<String> = args.iter().map(|&a| describe(i, a)).collect();
            if inner.is_empty() {
                format!("App[{}]", uri.0)
            } else {
                format!("App[{}, {}]", uri.0, inner.join(", "))
            }
        }

        Type::DataInstance { class, members } => {
            let inner: Vec<String> = members
                .iter()
                .map(|(name, ty)| format!("{}: {}", name, describe(i, *ty)))
                .collect();
            format!("Data<{}>({})", class.0, inner.join(", "))
        }

        Type::Void => "void".to_string(),
        Type::SelfType => "self".to_string(),
        Type::Instance => "instance".to_string(),
        Type::ClassType => "class".to_string(),

        Type::Union(members) => {
            // Display-only collapse of `true | false` to `bool`. Identity is
            // unchanged (the union is still two members internally).
            if is_bool_pair(i, id) {
                return "bool".to_string();
            }
            // Render `T | nil` with nil last for the common optional spelling.
            let mut rendered: Vec<(bool, String)> = members
                .iter()
                .map(|&m| (is_nil(i, m), describe(i, m)))
                .collect();
            // Stable: keep canonical order but float nil to the end so the
            // common case reads `T | nil`.
            rendered.sort_by_key(|(is_nil, _)| *is_nil);
            let parts: Vec<String> = rendered.into_iter().map(|(_, s)| s).collect();
            parts.join(" | ")
        }
    }
}

fn is_nil(i: &Interner, id: TypeId) -> bool {
    matches!(i.get(id), Type::Constant(Scalar::Nil))
}

fn scalar(s: &Scalar) -> String {
    match s {
        Scalar::Int(v) => v.to_string(),
        Scalar::Str(v) => format!("{v:?}"),
        Scalar::Sym(v) => format!(":{v}"),
        Scalar::Bool(v) => v.to_string(),
        Scalar::Nil => "nil".to_string(),
        Scalar::Float(v) => v.to_string(),
    }
}

fn shape_key(k: &ShapeKey) -> String {
    match k {
        ShapeKey::Sym(s) => format!(":{s}"),
        ShapeKey::Str(s) => format!("{s:?}"),
        ShapeKey::Int(v) => v.to_string(),
        ShapeKey::Other => "_".to_string(),
    }
}

#[cfg(test)]
mod named_tests {
    //! Tests for the reference-faithful [`describe_named`] display layer.
    use super::*;
    use crate::algebra::Algebra;
    use crate::ty::ShapeMember;

    fn resolver(class: ClassId) -> Option<String> {
        match class.0 {
            1 => Some("Integer".to_string()),
            2 => Some("String".to_string()),
            3 => Some("Array".to_string()),
            7 => Some("Time".to_string()),
            _ => None,
        }
    }

    fn nominal(i: &mut Interner, id: u32, args: Vec<TypeId>) -> TypeId {
        i.intern(Type::Nominal { class: ClassId(id), args })
    }

    #[test]
    fn nominal_and_generic() {
        let mut i = Interner::new();
        let int = nominal(&mut i, 1, vec![]);
        assert_eq!(describe_named(&i, int, &resolver), "Integer");
        let arr = nominal(&mut i, 3, vec![int]);
        assert_eq!(describe_named(&i, arr, &resolver), "Array[Integer]");
        let unknown = nominal(&mut i, 99, vec![]);
        assert_eq!(describe_named(&i, unknown, &resolver), "Class<99>");
    }

    #[test]
    fn constants_use_ruby_inspect() {
        let mut i = Interner::new();
        let three = i.int(3);
        assert_eq!(describe_named(&i, three, &resolver), "3");
        let s = i.intern(Type::Constant(Scalar::Str("hi".to_string())));
        assert_eq!(describe_named(&i, s, &resolver), "\"hi\"");
        let sym = i.intern(Type::Constant(Scalar::Sym("foo".to_string())));
        assert_eq!(describe_named(&i, sym, &resolver), ":foo");
        let f = i.intern(Type::Constant(Scalar::Float(3.0)));
        assert_eq!(describe_named(&i, f, &resolver), "3.0");
        let nil = i.nil();
        assert_eq!(describe_named(&i, nil, &resolver), "nil");
    }

    #[test]
    fn tuple_is_value_pinned() {
        let mut i = Interner::new();
        let (a, b, c) = (i.int(1), i.int(2), i.int(3));
        let tup = i.intern(Type::Tuple(vec![a, b, c]));
        assert_eq!(describe_named(&i, tup, &resolver), "[1, 2, 3]");
        let empty = i.intern(Type::Tuple(vec![]));
        assert_eq!(describe_named(&i, empty, &resolver), "[]");
    }

    #[test]
    fn singleton_and_dynamic() {
        let mut i = Interner::new();
        let s = i.intern(Type::Singleton(ClassId(7)));
        assert_eq!(describe_named(&i, s, &resolver), "singleton(Time)");
        let u = i.untyped();
        assert_eq!(describe_named(&i, u, &resolver), "Dynamic[top]");
    }

    #[test]
    fn union_optional_and_bool() {
        let mut i = Interner::new();
        let string = nominal(&mut i, 2, vec![]);
        let nil = i.nil();
        let opt = Algebra::join(&mut i, string, nil);
        assert_eq!(describe_named(&i, opt, &resolver), "String?");
        let (t, f) = (i.true_(), i.false_());
        let b = Algebra::join(&mut i, t, f);
        assert_eq!(describe_named(&i, b, &resolver), "bool");
    }

    #[test]
    fn integer_range_aliases_and_generic() {
        let mut i = Interner::new();
        let pos = i.intern(Type::IntegerRange { min: Some(1), max: None });
        assert_eq!(describe_named(&i, pos, &resolver), "positive-int");
        let open = i.intern(Type::IntegerRange { min: Some(5), max: None });
        assert_eq!(describe_named(&i, open, &resolver), "int<5, max>");
        let bounded = i.intern(Type::IntegerRange { min: Some(1), max: Some(10) });
        assert_eq!(describe_named(&i, bounded, &resolver), "int<1, 10>");
    }

    #[test]
    fn hash_shape_keyword_style() {
        let mut i = Interner::new();
        let int = nominal(&mut i, 1, vec![]);
        let shape = i.intern(Type::HashShape(vec![
            ShapeMember { key: ShapeKey::Sym("name".to_string()), value: int, optional: false },
            ShapeMember { key: ShapeKey::Sym("age".to_string()), value: int, optional: true },
        ]));
        assert_eq!(describe_named(&i, shape, &resolver), "{ name: Integer, ?age: Integer }");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Algebra;

    #[test]
    fn describes_atomics_and_dynamic_top() {
        let mut i = Interner::new();
        let top = i.top();
        let untyped = i.untyped();
        assert_eq!(describe(&i, top), "top");
        assert_eq!(describe(&i, i.bottom()), "bot");
        assert_eq!(describe(&i, untyped), "Dynamic[top]");
    }

    #[test]
    fn describes_constant_and_range() {
        let mut i = Interner::new();
        let three = i.int(3);
        assert_eq!(describe(&i, three), "Constant[3]");
        let r = i.intern(Type::IntegerRange {
            min: Some(1),
            max: Some(10),
        });
        assert_eq!(describe(&i, r), "(1..10)");
    }

    #[test]
    fn true_false_displays_as_bool_but_keeps_identity() {
        let mut i = Interner::new();
        let t = i.true_();
        let f = i.false_();
        let u = Algebra::join(&mut i, t, f);
        assert_eq!(describe(&i, u), "bool");
        // Identity preserved: still a 2-member union carrier.
        assert!(matches!(i.get(u), Type::Union(ms) if ms.len() == 2));
    }

    #[test]
    fn singleton_renders_stable_and_distinct_from_nominal() {
        let mut i = Interner::new();
        let s = i.intern(Type::Singleton(crate::ty::ClassId(7)));
        // Stable spelling.
        assert_eq!(describe(&i, s), "singleton(Class<7>)");
        // Distinct from the Nominal (instance) rendering of the same class.
        let n = i.intern(Type::Nominal {
            class: crate::ty::ClassId(7),
            args: vec![],
        });
        assert_eq!(describe(&i, n), "Class<7>");
        assert_ne!(describe(&i, s), describe(&i, n));
    }

    #[test]
    fn optional_then_nil_renders_t_or_nil() {
        let mut i = Interner::new();
        let s = i.intern(Type::Nominal {
            class: crate::ty::ClassId(10),
            args: vec![],
        });
        let nil = i.nil();
        let u = Algebra::join(&mut i, s, nil);
        // nil floats to the end: `Class<10> | nil`.
        assert_eq!(describe(&i, u), "Class<10> | nil");
    }
}
