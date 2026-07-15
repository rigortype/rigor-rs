//! Best-effort human-readable rendering of a carrier.
//!
//! `describe(TypeId)` matches the reference spelling where known (the
//! analyzer-internal `Constant[3]` convention, `Dynamic[top]`, `T | nil`,
//! `(1..10)` for ranges). Display MAY render a normalized type more readably
//! (e.g. `bool` for `true | false`) but MUST NOT change type identity
//! (normalization.md §"Interaction with display").

use crate::algebra::is_bool_pair;
use crate::interner::Interner;
use crate::ty::{ClassId, Scalar, ShapeKey, ShapeMember, Type, TypeId};

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
                        // Reference `key_separator`: Symbol/String keys keep the
                        // colon form (`a: 1`, `"k": 2`); every other scalar key
                        // uses the hashrocket (`1 => 2`, `nil => 0`).
                        let sep = match m.key {
                            ShapeKey::Sym(_) | ShapeKey::Str(_) => ":",
                            _ => " =>",
                        };
                        format!("{key}{sep} {}", describe_named(i, m.value, resolve))
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

/// The reference's `Type#erase_to_rbs` — a **valid-RBS erasure** of a carrier.
///
/// Distinct from [`describe_named`] (the human-facing display): erasure
/// generalizes the value-pins RBS cannot spell so the output is always
/// well-formed RBS — a non-primitive `Constant` erases to its class name
/// (`3.5` → `Float`), an `IntegerRange` to `Integer`, an open / non-symbol-keyed
/// `HashShape` to `Hash[K, V]`, and a `Dynamic` to `untyped`. Primitive
/// value-pins are PRESERVED (`3`, `"hi"`, `:sym`, `[1, 2, 3]`, `{ a: 1 }`) — the
/// erasure keeps the tuple / record spelling RBS *does* support. Unions dedup and
/// short-circuit to `untyped`, with NO `bool` / `T?` collapse (that is
/// display-only in `describe_named`).
///
/// This is the substrate `rigor sig-gen` will consume; it is surfaced today
/// through `type-of`'s `erased:` field. `resolve` maps a [`ClassId`] to its name
/// exactly as [`describe_named`] uses it (core RBS + project `sig/`).
pub fn erase_to_rbs_named(
    i: &Interner,
    id: TypeId,
    resolve: &dyn Fn(ClassId) -> Option<String>,
) -> String {
    match i.get(id) {
        Type::Top => "top".to_string(),
        Type::Bottom => "bot".to_string(),
        // Any dynamic-origin value erases to `untyped` (its class name is unknown).
        Type::Dynamic(_) => "untyped".to_string(),

        Type::Nominal { class, args } => {
            let name = named_class(*class, resolve);
            if args.is_empty() {
                name
            } else {
                let inner: Vec<String> =
                    args.iter().map(|&a| erase_to_rbs_named(i, a, resolve)).collect();
                format!("{name}[{}]", inner.join(", "))
            }
        }
        Type::Singleton(class) => format!("singleton({})", named_class(*class, resolve)),

        Type::Constant(s) => scalar_erase(s),

        Type::Tuple(elems) => {
            if elems.is_empty() {
                "[]".to_string()
            } else {
                let inner: Vec<String> =
                    elems.iter().map(|&e| erase_to_rbs_named(i, e, resolve)).collect();
                format!("[{}]", inner.join(", "))
            }
        }

        Type::HashShape(members) => erase_hash_shape(i, members, resolve),

        // A refined integer range generalizes to the nominal `Integer` (the RBS
        // has no `int<lo, hi>` spelling).
        Type::IntegerRange { .. } => "Integer".to_string(),

        Type::Union(members) => erase_union(i, members, resolve),

        // A difference erases through its base; an intersection through its first
        // member (reference `Difference` / `Intersection#erase_to_rbs`).
        Type::Difference { base, .. } => erase_to_rbs_named(i, *base, resolve),
        Type::Intersection(members) => members
            .first()
            .map(|&m| erase_to_rbs_named(i, m, resolve))
            .unwrap_or_else(|| "untyped".to_string()),
        Type::Refined { base, .. } => erase_to_rbs_named(i, *base, resolve),

        // Carriers with no faithful RBS spelling in this port erase conservatively
        // to `untyped` — always sound (it widens, never narrows).
        Type::Complement(_) | Type::App { .. } => "untyped".to_string(),

        // reference `DataInstance#erase_to_rbs` → the class name, or `Data` when
        // the class is anonymous / unresolved.
        Type::DataInstance { class, .. } => {
            resolve(*class).unwrap_or_else(|| "Data".to_string())
        }

        // Valid RBS keyword carriers render literally.
        Type::Void => "void".to_string(),
        Type::SelfType => "self".to_string(),
        Type::Instance => "instance".to_string(),
        Type::ClassType => "class".to_string(),
    }
}

/// A `Constant` scalar erased to valid RBS (reference `Constant#erase_to_rbs`):
/// `true`/`false`/`nil` and integers keep their literal spelling; a symbol / string
/// keeps its `inspect` form; every OTHER constant (here, only a `Float`)
/// generalizes to its class name — the reference's `else value.class.name` branch.
fn scalar_erase(s: &Scalar) -> String {
    match s {
        Scalar::Bool(true) => "true".to_string(),
        Scalar::Bool(false) => "false".to_string(),
        Scalar::Nil => "nil".to_string(),
        Scalar::Int(n) => n.to_string(),
        Scalar::Str(v) => format!("{v:?}"),
        Scalar::Sym(v) => format!(":{v}"),
        Scalar::Float(_) => "Float".to_string(),
    }
}

/// A union erased (reference `Union#erase_to_rbs`): each member erased, the whole
/// short-circuiting to `untyped` if any member is `untyped`, else the members
/// deduped (first-occurrence order, Ruby `Array#uniq`) and joined with ` | `.
/// No `bool` / `T?` collapse — those are display-only.
fn erase_union(
    i: &Interner,
    members: &[TypeId],
    resolve: &dyn Fn(ClassId) -> Option<String>,
) -> String {
    let erased: Vec<String> =
        members.iter().map(|&m| erase_to_rbs_named(i, m, resolve)).collect();
    if erased.iter().any(|e| e == "untyped") {
        return "untyped".to_string();
    }
    uniq_join(erased)
}

/// A `HashShape` erased (reference `HashShape#erase_to_rbs`). rigor-rs only ever
/// builds a value-pinned `HashShape` from static keys (an open / splat hash
/// degrades to the `Hash` nominal upstream), so it is always effectively
/// *closed*: an empty shape erases to `{}`; a shape with any non-symbol key
/// generalizes to `Hash[K, V]` (RBS record keys must be symbols); an all-symbol
/// shape keeps the record spelling `{ key: T, ?opt: T }`.
fn erase_hash_shape(
    i: &Interner,
    members: &[ShapeMember],
    resolve: &dyn Fn(ClassId) -> Option<String>,
) -> String {
    if members.is_empty() {
        return "{}".to_string();
    }
    if !members.iter().all(|m| matches!(m.key, ShapeKey::Sym(_))) {
        return hash_erasure(i, members, resolve);
    }
    let rendered: Vec<String> = members
        .iter()
        .map(|m| {
            let ShapeKey::Sym(name) = &m.key else { unreachable!() };
            let key = if m.optional { format!("?{name}") } else { name.clone() };
            format!("{key}: {}", erase_to_rbs_named(i, m.value, resolve))
        })
        .collect();
    format!("{{ {} }}", rendered.join(", "))
}

/// Generalize a non-symbol-keyed (or otherwise non-record) shape to a
/// `Hash[K, V]` bound: the key type is the union of each key's class, the value
/// type the union of the member values (reference `HashShape#hash_erasure`).
///
/// The reference builds `union(*key_types)` / `union(*pairs.values)`, and its
/// normalizer orders union members by `describe(:short)` (combinator.rb `sort_by
/// { |m| m.describe(:short) }`), so both the key and value member lists are
/// sorted by that display string before being erased — NOT by rigor-rs's own
/// tag/ClassId canonical order. We reproduce that here so the erased bound is
/// byte-identical to the oracle's (load-bearing for sig-gen output).
fn hash_erasure(
    i: &Interner,
    members: &[ShapeMember],
    resolve: &dyn Fn(ClassId) -> Option<String>,
) -> String {
    // Key bound: every key class's `describe(:short)` equals its erasure (a
    // nominal renders its name; the `true`/`false`/`nil` literals render
    // themselves), so sorting the erase strings reproduces the reference's
    // describe-sorted union member order; `uniq_join` then dedups.
    let mut key_strs: Vec<String> =
        members.iter().map(|m| shape_key_class_name(&m.key)).collect();
    key_strs.sort();
    let key_type = uniq_join(key_strs);

    // Value bound: sort the value TYPES by their `describe(:short)` (the
    // reference's canonical union order), then erase each in that order; a single
    // `untyped` member collapses the whole union (reference `Union#erase_to_rbs`).
    let mut vals: Vec<TypeId> = members.iter().map(|m| m.value).collect();
    vals.sort_by_key(|&v| describe_named(i, v, resolve));
    let val_erased: Vec<String> =
        vals.iter().map(|&v| erase_to_rbs_named(i, v, resolve)).collect();
    let value_type = if val_erased.iter().any(|e| e == "untyped") {
        "untyped".to_string()
    } else {
        uniq_join(val_erased)
    };
    format!("Hash[{key_type}, {value_type}]")
}

/// The RBS class name a shape key's value belongs to (reference
/// `hash_erasure_key_type`'s `nominal_of(key.class)`).
fn shape_key_class_name(k: &ShapeKey) -> String {
    match k {
        ShapeKey::Sym(_) => "Symbol".to_string(),
        ShapeKey::Str(_) => "String".to_string(),
        ShapeKey::Int(_) => "Integer".to_string(),
        ShapeKey::Float(_) => "Float".to_string(),
        // Reference `hash_erasure_key_type`: the `true` / `false` / `nil`
        // singletons keep their literal carrier (the constant IS the class's
        // whole value set, and RBS spells the literal), unlike Integer/Float
        // which widen to the class nominal.
        ShapeKey::Bool(true) => "true".to_string(),
        ShapeKey::Bool(false) => "false".to_string(),
        ShapeKey::Nil => "nil".to_string(),
        ShapeKey::Other => "untyped".to_string(),
    }
}

/// Join strings with ` | `, deduping in first-occurrence order (Ruby
/// `Array#uniq` semantics) — the shared spelling of an erased union.
fn uniq_join(items: Vec<String>) -> String {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|e| seen.insert(e.clone()))
        .collect::<Vec<_>>()
        .join(" | ")
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
    ruby_float_to_s(f)
}

/// Ruby `Float#to_s` / `Float#inspect` spelling of a finite float: a decimal
/// point is always present (`3.0.to_s == "3.0"`, `3.14.to_s == "3.14"`), and
/// non-integral values use Rust's shortest round-trip (which matches Ruby's
/// `flo_to_s` dtoa for the overwhelming majority of values). Exposed for the
/// Kernel `String()` / `sprintf` folds (`kernel_fold`), which must reproduce
/// Ruby's `to_s` byte-for-byte. Non-finite inputs (`NaN`/`±Infinity`) fall to
/// Rust's spelling; callers that fold must guard those out separately.
pub fn ruby_float_to_s(f: f64) -> String {
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
        ShapeKey::Float(bits) => named_float(f64::from_bits(*bits)),
        ShapeKey::Bool(b) => b.to_string(),
        ShapeKey::Nil => "nil".to_string(),
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
        ShapeKey::Float(bits) => named_float(f64::from_bits(*bits)),
        ShapeKey::Bool(b) => b.to_string(),
        ShapeKey::Nil => "nil".to_string(),
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

    #[test]
    fn hash_shape_scalar_keys_use_hashrocket() {
        // Symbol / String keys keep the colon form; Integer / Float / true /
        // false / nil keys render with the hashrocket (reference `key_separator`).
        let mut i = Interner::new();
        let (one, two, three) = (i.int(1), i.int(2), i.int(3));
        let sym = i.intern(Type::Constant(Scalar::Sym("v".to_string())));
        let shape = i.intern(Type::HashShape(vec![
            ShapeMember { key: ShapeKey::Sym("a".to_string()), value: one, optional: false },
            ShapeMember { key: ShapeKey::Str("k".to_string()), value: two, optional: false },
            ShapeMember { key: ShapeKey::Int(3), value: three, optional: false },
            ShapeMember {
                key: ShapeKey::Float(1.5f64.to_bits()),
                value: sym,
                optional: false,
            },
            ShapeMember { key: ShapeKey::Bool(true), value: one, optional: false },
            ShapeMember { key: ShapeKey::Nil, value: two, optional: false },
        ]));
        assert_eq!(
            describe_named(&i, shape, &resolver),
            "{ a: 1, \"k\": 2, 3 => 3, 1.5 => :v, true => 1, nil => 2 }"
        );
    }
}

#[cfg(test)]
mod erase_tests {
    //! Tests for the reference-faithful [`erase_to_rbs_named`] valid-RBS erasure,
    //! on CONSTRUCTED carriers (independent of inference) so the substrate's
    //! per-variant behaviour is pinned directly.
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
    fn nominal_generic_and_singleton() {
        let mut i = Interner::new();
        let int = nominal(&mut i, 1, vec![]);
        assert_eq!(erase_to_rbs_named(&i, int, &resolver), "Integer");
        let arr = nominal(&mut i, 3, vec![int]);
        assert_eq!(erase_to_rbs_named(&i, arr, &resolver), "Array[Integer]");
        let s = i.intern(Type::Singleton(ClassId(7)));
        assert_eq!(erase_to_rbs_named(&i, s, &resolver), "singleton(Time)");
    }

    #[test]
    fn constants_keep_primitives_but_generalize_float() {
        let mut i = Interner::new();
        let three = i.int(3);
        assert_eq!(erase_to_rbs_named(&i, three, &resolver), "3");
        let s = i.intern(Type::Constant(Scalar::Str("hi".to_string())));
        assert_eq!(erase_to_rbs_named(&i, s, &resolver), "\"hi\"");
        let sym = i.intern(Type::Constant(Scalar::Sym("foo".to_string())));
        assert_eq!(erase_to_rbs_named(&i, sym, &resolver), ":foo");
        // A Float value-pin has no RBS literal → generalizes to its class name.
        let f = i.intern(Type::Constant(Scalar::Float(3.5)));
        assert_eq!(erase_to_rbs_named(&i, f, &resolver), "Float");
        assert_eq!(erase_to_rbs_named(&i, i.true_(), &resolver), "true");
        assert_eq!(erase_to_rbs_named(&i, i.nil(), &resolver), "nil");
    }

    #[test]
    fn dynamic_and_integer_range_generalize() {
        let mut i = Interner::new();
        let dyn_top = i.untyped();
        assert_eq!(erase_to_rbs_named(&i, dyn_top, &resolver), "untyped");
        // An integer range has no `int<lo, hi>` RBS spelling → `Integer`.
        let r = i.intern(Type::IntegerRange { min: Some(1), max: Some(10) });
        assert_eq!(erase_to_rbs_named(&i, r, &resolver), "Integer");
        let open = i.intern(Type::IntegerRange { min: Some(1), max: None });
        assert_eq!(erase_to_rbs_named(&i, open, &resolver), "Integer");
    }

    #[test]
    fn union_dedups_without_bool_or_optional_collapse() {
        let mut i = Interner::new();
        // `true | false` erases to the two literals joined by ` | ` (no `bool`
        // collapse — that is display-only). Order follows rigor-rs's canonical
        // union order (`false` sorts before `true`), not a reordering by erase.
        let (t, f) = (i.true_(), i.false_());
        let b = Algebra::join(&mut i, t, f);
        let bool_erased = erase_to_rbs_named(&i, b, &resolver);
        assert!(bool_erased.contains("true") && bool_erased.contains("false"));
        assert!(bool_erased.contains(" | ") && !bool_erased.contains("bool"));
        // `String | nil` keeps `nil` inline (no `T?` collapse).
        let string = nominal(&mut i, 2, vec![]);
        let nil = i.nil();
        let opt = Algebra::join(&mut i, string, nil);
        let erased = erase_to_rbs_named(&i, opt, &resolver);
        assert!(erased.contains("String") && erased.contains("nil") && erased.contains(" | "));
        assert!(!erased.contains('?'));
    }

    #[test]
    fn union_short_circuits_to_untyped() {
        let mut i = Interner::new();
        let string = nominal(&mut i, 2, vec![]);
        let dyn_ = i.untyped();
        let u = Algebra::join(&mut i, string, dyn_);
        // Any `untyped` member collapses the whole union (reference `Union#erase`).
        assert_eq!(erase_to_rbs_named(&i, u, &resolver), "untyped");
    }

    #[test]
    fn tuple_and_symbol_record_verbatim() {
        let mut i = Interner::new();
        let (a, b, c) = (i.int(1), i.int(2), i.int(3));
        let tup = i.intern(Type::Tuple(vec![a, b, c]));
        assert_eq!(erase_to_rbs_named(&i, tup, &resolver), "[1, 2, 3]");
        let empty_tup = i.intern(Type::Tuple(vec![]));
        assert_eq!(erase_to_rbs_named(&i, empty_tup, &resolver), "[]");

        let int = nominal(&mut i, 1, vec![]);
        let rec = i.intern(Type::HashShape(vec![
            ShapeMember { key: ShapeKey::Sym("name".to_string()), value: int, optional: false },
            ShapeMember { key: ShapeKey::Sym("age".to_string()), value: int, optional: true },
        ]));
        assert_eq!(erase_to_rbs_named(&i, rec, &resolver), "{ name: Integer, ?age: Integer }");
        let empty_hash = i.intern(Type::HashShape(vec![]));
        assert_eq!(erase_to_rbs_named(&i, empty_hash, &resolver), "{}");
    }

    #[test]
    fn non_symbol_keyed_shape_generalizes_to_hash_bound() {
        let mut i = Interner::new();
        let two = i.int(2);
        // A string key cannot be an RBS record key → `Hash[String, 2]`.
        let str_keyed = i.intern(Type::HashShape(vec![ShapeMember {
            key: ShapeKey::Str("k".to_string()),
            value: two,
            optional: false,
        }]));
        assert_eq!(erase_to_rbs_named(&i, str_keyed, &resolver), "Hash[String, 2]");
    }

    #[test]
    fn scalar_keyed_shape_erases_key_union_sorted() {
        // Integer / Float keys widen to their class nominal; true / false / nil
        // keep their literal carrier. The key and value unions are ordered by
        // `describe(:short)` (the reference's `union` member order), NOT source
        // order — so `{ true => 1, false => 2, nil => 3 }` erases with the key
        // union `false | nil | true`.
        let mut i = Interner::new();
        let (one, two, three) = (i.int(1), i.int(2), i.int(3));
        let bool_nil = i.intern(Type::HashShape(vec![
            ShapeMember { key: ShapeKey::Bool(true), value: one, optional: false },
            ShapeMember { key: ShapeKey::Bool(false), value: two, optional: false },
            ShapeMember { key: ShapeKey::Nil, value: three, optional: false },
        ]));
        assert_eq!(
            erase_to_rbs_named(&i, bool_nil, &resolver),
            "Hash[false | nil | true, 1 | 2 | 3]"
        );

        // Integer + Float keys → `Float | Integer` (sorted); symbol values
        // `:i` / `:f` → `:f | :i` (sorted by describe).
        let si = i.intern(Type::Constant(Scalar::Sym("i".to_string())));
        let sf = i.intern(Type::Constant(Scalar::Sym("f".to_string())));
        let int_float = i.intern(Type::HashShape(vec![
            ShapeMember { key: ShapeKey::Int(1), value: si, optional: false },
            ShapeMember { key: ShapeKey::Float(1.0f64.to_bits()), value: sf, optional: false },
        ]));
        assert_eq!(erase_to_rbs_named(&i, int_float, &resolver), "Hash[Float | Integer, :f | :i]");
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
