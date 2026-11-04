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
}

/// FQN → the first loaded declaration's type parameters (core and stdlib
/// first, then the project's signature dirs), generic classes only.
#[derive(Default)]
pub struct Generics {
    params: HashMap<String, Vec<Param>>,
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
                        match tp.variance() {
                            TypeParamVariance::Covariant => text.push_str("out "),
                            TypeParamVariance::Contravariant => text.push_str("in "),
                            TypeParamVariance::Invariant => {}
                        }
                        text.push_str(tp.name().as_str());
                        let plain = tp.upper_bound().is_none()
                            && tp.lower_bound().is_none()
                            && tp.default_type().is_none();
                        Some(Param { text, plain })
                    }
                    _ => None,
                })
                .collect();
            // `entry_declarations(entry).first`: the first declaration loaded
            // decides, generic or not.
            self.params.entry(fqn).or_insert(params);
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
}
