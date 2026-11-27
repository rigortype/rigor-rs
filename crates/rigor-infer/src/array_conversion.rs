//! Ruby's implicit array conversion for multiple assignment — the port of
//! `MethodDispatcher::RbsDispatch.array_conversion_free?`
//! (`reference/rigor/lib/rigor/inference/method_dispatcher/rbs_dispatch.rb`,
//! upstream issue #1094).
//!
//! `a, b = value` first tries `value.to_ary` (and the `respond_to_missing?` /
//! `method_missing` pair). Only when the receiver's class provably has no such
//! conversion does Ruby wrap the scalar as `[value]`, which is what binds `a`
//! to the value and `b` to `nil`. The binder itself is pure; this module is
//! the index-reading predicate it is handed.

use rigor_parse::MultiTargets;
use rigor_types::{Interner, TypeId};

use crate::{multi_target_binder, Typer};

/// The methods through which a value can answer Ruby's implicit array
/// conversion (`rb_check_array_type`).
const ARRAY_CONVERSION_HOOKS: [&str; 3] = ["to_ary", "method_missing", "respond_to_missing?"];

/// The owners whose hook declarations are the defaults rather than an
/// override: `BasicObject#method_missing` raises and
/// `Kernel#respond_to_missing?` answers false.
const ARRAY_CONVERSION_DEFAULT_OWNERS: [&str; 3] = ["BasicObject", "Object", "Kernel"];

/// Core value classes whose instances never convert — the reference's
/// answer even before it reads the RBS ancestry.
const ARRAY_CONVERSION_FREE_CORE_CLASSES: [&str; 11] = [
    "Integer", "Float", "Symbol", "String", "Hash", "Range", "Regexp", "Proc", "NilClass",
    "TrueClass", "FalseClass",
];

/// The classes `Array` itself inherits from. A nominal `Object` is routinely
/// an Array at runtime, so reading Object's own ancestry cannot vouch for it.
const ARRAY_SUPERCLASSES: [&str; 2] = ["Object", "BasicObject"];

impl<'i> Typer<'i> {
    /// Bind a multi-assignment target tree with this typer's RBS/project answer
    /// to the implicit-`to_ary` question. This is the production entry point;
    /// the parameterless binder keeps the opaque-RHS floor for tests and for
    /// callers that have no conversion oracle.
    pub(crate) fn bind_multi_targets(
        &self,
        targets: &MultiTargets,
        rhs_type: TypeId,
        interner: &mut Interner,
    ) -> Vec<(String, TypeId)> {
        multi_target_binder::bind_with_conversion(targets, rhs_type, interner, |i, ty| {
            self.array_conversion_free_type(i, ty)
        })
    }

    /// `conversion_class_name` + `array_conversion_free?`: whether the carrier
    /// names a single class whose instances provably cannot implicitly convert
    /// to an Array. `Dynamic`, unions and classless carriers decline.
    fn array_conversion_free_type(&self, interner: &Interner, ty: TypeId) -> bool {
        let Some(class_name) = self.index.class_name_of(interner, ty) else {
            return false;
        };
        self.array_conversion_free_class(class_name)
    }

    /// The reference's `array_conversion_free?`: project source outranks the
    /// RBS it did not write; the closed core list answers first; Array's own
    /// superclasses and unknown/open hierarchies decline; otherwise the RBS
    /// ancestry is closed when no hook is defined outside the default owners.
    fn array_conversion_free_class(&self, class_name: &str) -> bool {
        let name = class_name.strip_prefix("::").unwrap_or(class_name);
        if self.project_defines_array_conversion(name) {
            return false;
        }
        if ARRAY_CONVERSION_FREE_CORE_CLASSES.contains(&name) {
            return true;
        }
        if ARRAY_SUPERCLASSES.contains(&name) {
            return false;
        }
        self.rbs_ancestry_array_conversion_free(name)
    }

    /// Whether the project may add one of the conversion hooks where this
    /// class's instance lookup can reach it: on the class or a project/RBS
    /// ancestor, or on one of the default owners. A `pre_eval:` patch counts
    /// exactly as a project definition does. This is a pure silencer — a
    /// conservative `true` only loses the scalar wrap, never invents one.
    fn project_defines_array_conversion(&self, class_name: &str) -> bool {
        let mut owners = vec![class_name];
        if let Some(chain) = self.index.ancestor_names(class_name) {
            owners.extend(chain);
        }
        owners.extend(ARRAY_CONVERSION_DEFAULT_OWNERS);
        owners.sort_unstable();
        owners.dedup();

        ARRAY_CONVERSION_HOOKS.iter().any(|hook| {
            self.source.project_declares_method_through_ancestors(self.file_key, class_name, hook)
                || owners.iter().any(|owner| {
                    self.source.project_declares_method(self.file_key, owner, hook)
                        || self
                            .source
                            .pre_eval_defines_on(std::slice::from_ref(owner), hook, false)
                })
        })
    }

    /// The closed-RBS half of `array_conversion_free?`: every conversion hook
    /// the ancestry declares is one of Ruby's default implementations. The
    /// port's `class_name_of` only produces concrete classes, so the
    /// reference's module decline is already implicit in the caller.
    fn rbs_ancestry_array_conversion_free(&self, class_name: &str) -> bool {
        if !self.index.knows_class(class_name) {
            return false;
        }
        ARRAY_CONVERSION_HOOKS.iter().all(|hook| {
            self.index
                .declaring_ancestor(class_name, hook)
                .is_none_or(|owner| ARRAY_CONVERSION_DEFAULT_OWNERS.contains(&owner))
        })
    }
}
