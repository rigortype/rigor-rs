//! Superclass spelling and resolvability for sig-gen (reference
//! `Generator#record_superclass`, `#resolve_superclass_spellings` (#722,
//! #2579, #2661), `#demote_unresolvable_superclasses` (#735, #2437, #2478)).
//!
//! A written subclass declaration must carry its superclass, and that
//! superclass must resolve in the next run's RBS environment: a header whose
//! superclass nothing declares fails its build (`RBS::NoSuperclassFoundError`)
//! and collapses the class. So every recorded superclass token is resolved
//! against the header's lexical nesting, written rooted (`< ::Base`), and a
//! class whose chain does not end in a known class (or a class this run
//! writes) is dropped from the output and reported on stderr.

use std::collections::{HashMap, HashSet};

use rigor_parse::ruby_prism::{self, ClassNode, ModuleNode, Node as PrismNode, ParseResult, Visit};

/// One `class` header with a plain-constant superclass.
pub struct Header {
    /// The declared class, fully qualified (`declaration_prefix`).
    pub full: String,
    /// The nesting the header is WRITTEN in — Ruby evaluates the superclass
    /// expression there, even for a rooted header.
    pub nesting: Vec<String>,
    /// The superclass expression as `qualified_constant_path` spells it:
    /// `::Base` for a rooted one.
    pub token: String,
}

/// Every class header in the file whose superclass is a plain constant path.
pub fn collect(result: &ParseResult<'_>) -> Vec<Header> {
    let mut walker = HeaderWalker { prefix: Vec::new(), out: Vec::new() };
    walker.visit(&result.node());
    walker.out
}

struct HeaderWalker {
    prefix: Vec<String>,
    out: Vec<Header>,
}

/// `Source::ConstantPath.declaration_prefix` over a Prism constant path:
/// `None` for a path whose base is not a constant (`self::X`, `expr::X`).
fn declared_path(prefix: &[String], node: &PrismNode<'_>) -> Option<Vec<String>> {
    if let Some(read) = node.as_constant_read_node() {
        let mut out = prefix.to_vec();
        out.push(String::from_utf8_lossy(read.name().as_slice()).into_owned());
        return Some(out);
    }
    let path = node.as_constant_path_node()?;
    let last = String::from_utf8_lossy(path.name()?.as_slice()).into_owned();
    let mut out = match path.parent() {
        None => Vec::new(),
        Some(parent) => declared_path(prefix, &parent)?,
    };
    out.push(last);
    Some(out)
}

/// `qualified_constant_path`: `Foo`, `Foo::Bar`, `::Foo`; `None` for anything
/// that is not a constant path (a computed superclass).
fn qualified_constant_path(node: &PrismNode<'_>) -> Option<String> {
    if let Some(read) = node.as_constant_read_node() {
        return Some(String::from_utf8_lossy(read.name().as_slice()).into_owned());
    }
    let path = node.as_constant_path_node()?;
    let name = String::from_utf8_lossy(path.name()?.as_slice()).into_owned();
    match path.parent() {
        None => Some(format!("::{name}")),
        Some(parent) => qualified_constant_path(&parent).map(|p| format!("{p}::{name}")),
    }
}

impl<'pr> Visit<'pr> for HeaderWalker {
    fn visit_class_node(&mut self, node: &ClassNode<'pr>) {
        let Some(child) = declared_path(&self.prefix, &node.constant_path()) else {
            ruby_prism::visit_class_node(self, node);
            return;
        };
        let full = child.join("::");
        if let Some(token) = node.superclass().as_ref().and_then(qualified_constant_path) {
            // #2579: a token naming the class being opened would write a
            // self-superclass; no superclass is safer.
            if bare_superclass_name(&token) != full {
                self.out.push(Header { full, nesting: self.prefix.clone(), token });
            }
        }
        let saved = std::mem::replace(&mut self.prefix, child);
        if let Some(body) = node.body() {
            self.visit(&body);
        }
        self.prefix = saved;
    }

    fn visit_module_node(&mut self, node: &ModuleNode<'pr>) {
        let Some(child) = declared_path(&self.prefix, &node.constant_path()) else {
            ruby_prism::visit_module_node(self, node);
            return;
        };
        let saved = std::mem::replace(&mut self.prefix, child);
        if let Some(body) = node.body() {
            self.visit(&body);
        }
        self.prefix = saved;
    }
}

/// `bare_superclass_name`: the class a token names, type arguments and a
/// rooted `::` dropped.
pub fn bare_superclass_name(token: &str) -> String {
    let base = match token.find('[') {
        Some(i) if token.ends_with(']') => &token[..i],
        _ => token,
    };
    base.strip_prefix("::").unwrap_or(base).to_string()
}

/// `SuperclassSpelling.absolute`.
pub fn absolute(token: &str) -> String {
    if token.starts_with("::") {
        token.to_string()
    } else {
        format!("::{token}")
    }
}

/// `resolve_superclass_token`: the first nesting-qualified spelling (innermost
/// first, the class being opened excluded) that names a class this run
/// declares or the RBS environment knows; else the token itself.
pub fn resolve_token(
    token: &str,
    nesting: &[String],
    full: &str,
    declared: &dyn Fn(&str) -> bool,
) -> String {
    if token.starts_with("::") {
        return token.to_string();
    }
    let base = bare_superclass_name(token);
    for i in (1..=nesting.len()).rev() {
        let candidate = format!("{}::{base}", nesting[..i].join("::"));
        if candidate == full {
            continue;
        }
        if declared(&candidate) {
            return candidate;
        }
    }
    token.to_string()
}

/// `wrapper_names`: `A::B::C` is written inside `A` and `A::B`.
pub fn wrapper_names(name: &str) -> Vec<String> {
    let segs: Vec<&str> = name.split("::").collect();
    (1..segs.len()).map(|n| segs[..n].join("::")).collect()
}

/// `unresolved_superclass_root`: the first superclass token on `name`'s
/// chain that neither the RBS environment knows nor this run emits; `None`
/// when the chain terminates in a known class. A cycle resolves nowhere.
pub fn unresolved_root(
    name: &str,
    supers: &HashMap<String, String>,
    emitted: &HashSet<String>,
    rbs_known: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut current = name.to_string();
    loop {
        let superclass = supers.get(&current)?;
        if !seen.insert(current.clone()) {
            return Some(superclass.clone());
        }
        let base = bare_superclass_name(superclass);
        if rbs_known(&base) {
            return None;
        }
        if !emitted.contains(&base) {
            return Some(superclass.clone());
        }
        current = base;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spelling_and_resolution() {
        assert_eq!(bare_superclass_name("::Hash[untyped, untyped]"), "Hash");
        assert_eq!(absolute("Base"), "::Base");
        assert_eq!(absolute("::Base"), "::Base");
        let declared = |n: &str| n == "A::Base" || n == "B::B";
        // Innermost nesting first.
        assert_eq!(resolve_token("Base", &["A".into(), "X".into()], "A::X::K", &declared), "A::Base");
        // #2661: the class being opened is not yet a candidate.
        assert_eq!(resolve_token("B", &["B".into()], "B::B", &declared), "B");
        assert_eq!(resolve_token("::Base", &["A".into()], "A::K", &declared), "::Base");
        assert_eq!(wrapper_names("A::B::C"), vec!["A".to_string(), "A::B".to_string()]);
    }

    #[test]
    fn chain_roots() {
        let supers: HashMap<String, String> = [
            ("Kid".to_string(), "::Base".to_string()),
            ("Kid2".to_string(), "::Typed".to_string()),
            ("Err".to_string(), "::StandardError".to_string()),
        ]
        .into_iter()
        .collect();
        let emitted: HashSet<String> = ["Typed".to_string(), "Kid2".to_string()].into_iter().collect();
        let known = |n: &str| n == "StandardError";
        assert_eq!(unresolved_root("Kid", &supers, &emitted, &known).as_deref(), Some("::Base"));
        assert_eq!(unresolved_root("Kid2", &supers, &emitted, &known), None);
        assert_eq!(unresolved_root("Err", &supers, &emitted, &known), None);
    }
}
