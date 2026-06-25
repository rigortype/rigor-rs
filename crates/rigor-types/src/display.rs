//! Best-effort human-readable rendering of a carrier.
//!
//! `describe(TypeId)` matches the reference spelling where known (the
//! analyzer-internal `Constant[3]` convention, `Dynamic[top]`, `T | nil`,
//! `(1..10)` for ranges). Display MAY render a normalized type more readably
//! (e.g. `bool` for `true | false`) but MUST NOT change type identity
//! (normalization.md §"Interaction with display").

use crate::algebra::is_bool_pair;
use crate::interner::Interner;
use crate::ty::{Scalar, ShapeKey, Type, TypeId};

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
