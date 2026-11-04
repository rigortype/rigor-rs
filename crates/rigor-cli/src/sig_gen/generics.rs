//! Generic reopen headers (reference `Generator#stamp_generic_reopenings` /
//! `#demote_generic_reopenings`, #2436/#2473/#2602): a class the loaded RBS
//! declares generic is reopened with its declared type parameters copied onto
//! the header (`class Hash[unchecked out K, unchecked out V]`) — a bare
//! `class Hash` is an `RBS::GenericParameterMismatchError` that collapses the
//! class. A parameter with a bound or a default cannot be copied faithfully
//! under another nesting, so such a class is not reopened at all.

use std::collections::HashMap;

use ruby_rbs::node::{Node as RbsNode, TypeParamVariance};

use super::{collect_rbs_files, decl_full_name};

/// One declared type parameter: its RBS spelling, and whether it is plain (no
/// upper/lower bound, no default).
#[derive(Clone)]
struct Param {
    text: String,
    plain: bool,
    /// The parameter with its name erased: variance, `unchecked`, and which
    /// of upper bound / lower bound / default it carries. Two declarations
    /// agree on a parameter list only when these agree position by position
    /// (`ClassEntry#validate_type_params` compares after renaming).
    shape: (bool, u8, bool, bool, bool),
}

/// FQN → the first loaded declaration's type parameters (core and stdlib
/// first, then the project's signature dirs), generic classes only.
#[derive(Default)]
pub struct Generics {
    params: HashMap<String, Vec<Param>>,
    /// Classes whose declarations disagree on their type parameters: RBS
    /// raises `GenericParameterMismatchError` building them, so the
    /// reference's definition lookup answers nothing for any of their methods.
    mismatched: std::collections::HashSet<String>,
}

impl Generics {
    pub fn build(signature_dirs: &[std::path::PathBuf]) -> Self {
        let mut g = Generics::default();
        for (_, src) in rigor_index::EMBEDDED_RBS {
            g.ingest(src);
        }
        for dir in signature_dirs {
            if !dir.is_dir() {
                continue;
            }
            let mut files: Vec<std::path::PathBuf> = Vec::new();
            collect_rbs_files(dir, &mut files);
            files.sort();
            for f in files {
                if let Ok(src) = std::fs::read_to_string(&f) {
                    g.ingest(&src);
                }
            }
        }
        g
    }

    fn ingest(&mut self, src: &str) {
        let Ok(sig) = ruby_rbs::node::parse(src) else { return };
        let decls: Vec<RbsNode<'_>> = sig.declarations().iter().collect();
        self.walk(decls, &[]);
    }

    fn walk<'a>(&mut self, nodes: Vec<RbsNode<'a>>, prefix: &[String]) {
        for node in nodes {
            let (local, params, members) = match &node {
                RbsNode::Class(c) => (decl_full_name(&c.name()), c.type_params(), c.members()),
                RbsNode::Module(m) => (decl_full_name(&m.name()), m.type_params(), m.members()),
                _ => continue,
            };
            let local = local.strip_prefix("::").unwrap_or(&local).to_string();
            let mut path = prefix.to_vec();
            path.extend(local.split("::").map(str::to_string));
            let fqn = path.join("::");
            let params: Vec<Param> = params
                .iter()
                .filter_map(|p| match p {
                    RbsNode::TypeParam(tp) => {
                        let mut text = String::new();
                        if tp.unchecked() {
                            text.push_str("unchecked ");
                        }
                        let variance = match tp.variance() {
                            TypeParamVariance::Covariant => {
                                text.push_str("out ");
                                1
                            }
                            TypeParamVariance::Contravariant => {
                                text.push_str("in ");
                                2
                            }
                            TypeParamVariance::Invariant => 0,
                        };
                        text.push_str(tp.name().as_str());
                        let (upper, lower, default) = (
                            tp.upper_bound().is_some(),
                            tp.lower_bound().is_some(),
                            tp.default_type().is_some(),
                        );
                        let plain = !upper && !lower && !default;
                        let shape = (tp.unchecked(), variance, upper, lower, default);
                        Some(Param { text, plain, shape })
                    }
                    _ => None,
                })
                .collect();
            // `entry_declarations(entry).first`: the first declaration loaded
            // decides, generic or not. A later one with another parameter
            // count or shape fails `validate_type_params`. (Two bounded
            // parameters whose bound TEXT differs also fail there; that is not
            // read, so such a class keeps its declarations, the drop side.)
            match self.params.get(&fqn) {
                Some(first) => {
                    let agree = first.len() == params.len()
                        && first.iter().zip(&params).all(|(a, b)| a.shape == b.shape);
                    if !agree {
                        self.mismatched.insert(fqn.clone());
                    }
                }
                None => {
                    self.params.insert(fqn.clone(), params);
                }
            }
            self.walk(members.iter().collect(), &path);
        }
    }

    /// `generic_header_spelling`: `"[unchecked out E]"` when the class is
    /// declared generic with plain parameters only.
    pub fn header_spelling(&self, class_name: &str) -> Option<String> {
        let params = self.params.get(class_name.strip_prefix("::").unwrap_or(class_name))?;
        if params.is_empty() || !params.iter().all(|p| p.plain) {
            return None;
        }
        Some(format!("[{}]", params.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(", ")))
    }

    /// `class_type_param_names(name).size`.
    pub fn arity(&self, class_name: &str) -> usize {
        self.params.get(class_name.strip_prefix("::").unwrap_or(class_name)).map_or(0, Vec::len)
    }

    /// `generic_superclass_spelling` (#735): a generic superclass is written
    /// with `untyped` arguments (`Hash[untyped, untyped]`); arity 0 keeps the
    /// token.
    pub fn superclass_spelling(&self, token: &str) -> String {
        let bare = super::superclasses::bare_superclass_name(token);
        let n = self.arity(&bare);
        if n == 0 || token.ends_with(']') {
            return token.to_string();
        }
        format!("{token}[{}]", vec!["untyped"; n].join(", "))
    }

    /// Whether the class's declarations disagree on their type parameters
    /// (`RBS::GenericParameterMismatchError`), so its definition does not
    /// build.
    pub fn mismatched(&self, class_name: &str) -> bool {
        self.mismatched.contains(class_name.strip_prefix("::").unwrap_or(class_name))
    }

    /// `bounded_generic?`: generic with a bounded or defaulted parameter.
    pub fn bounded(&self, class_name: &str) -> bool {
        self.params
            .get(class_name)
            .is_some_and(|params| !params.is_empty() && !params.iter().all(|p| p.plain))
    }
}

/// `namespace_prefixes`: `A::B::C` → `A`, `A::B`, `A::B::C`.
pub fn namespace_prefixes(class_name: &str) -> Vec<String> {
    let segs: Vec<&str> = class_name.split("::").collect();
    (1..=segs.len()).map(|n| segs[..n].join("::")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_generic_headers() {
        let g = Generics::build(&[]);
        assert_eq!(g.header_spelling("Hash").as_deref(), Some("[unchecked out K, unchecked out V]"));
        assert_eq!(g.header_spelling("Array").as_deref(), Some("[unchecked out E]"));
        assert_eq!(g.header_spelling("String"), None);
        assert_eq!(namespace_prefixes("A::B"), vec!["A".to_string(), "A::B".to_string()]);
    }

    /// The bundled declarations agree with each other: no core or stdlib
    /// class reads as unbuildable before the project adds anything.
    #[test]
    fn bundled_declarations_agree() {
        let g = Generics::build(&[]);
        let mut bad: Vec<&String> = g.mismatched.iter().collect();
        bad.sort();
        assert!(bad.is_empty(), "{bad:?}");
    }

    /// A project `class Hash` without parameters disagrees with core's
    /// `Hash[unchecked out K, unchecked out V]`; one with the same
    /// parameters renamed agrees.
    #[test]
    fn bare_core_reopening_mismatches() {
        let dir = std::env::temp_dir().join(format!("rigor-generics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rbs"),
            "class Hash\nend\nclass Array[unchecked out T]\nend\nclass Range[E]\nend\n",
        )
        .unwrap();
        let g = Generics::build(std::slice::from_ref(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(g.mismatched("Hash"));
        assert!(!g.mismatched("Array"));
        assert!(g.mismatched("Range"), "invariant against core's `out E`");
        assert!(!g.mismatched("String"));
    }
}
