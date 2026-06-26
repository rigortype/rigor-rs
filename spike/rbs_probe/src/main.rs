// Spike — ADR-0004 gate: prove `ruby-rbs` parses real stdlib RBS into a typed
// AST that rigor-rs can build its own index from. Parses the rbs gem's
// string.rbs and collects class + method-definition names via the Visit trait.
// API confirmed compiler-driven: `Visit` takes no lifetime arg; node accessors
// like `ClassNode::name() -> TypeNameNode`, `MethodDefinitionNode::name()`.
use ruby_rbs::node::{
    parse, visit_class_node, visit_method_definition_node, ClassNode, MethodDefinitionNode, Visit,
};

#[derive(Default)]
struct Collector {
    classes: Vec<String>,
    methods: Vec<String>,
}

impl Visit for Collector {
    fn visit_class_node(&mut self, node: &ClassNode) {
        self.classes.push(format!("{:?}", node.name()));
        visit_class_node(self, node);
    }
    fn visit_method_definition_node(&mut self, node: &MethodDefinitionNode) {
        self.methods.push(format!("{}", node.name()));
        visit_method_definition_node(self, node);
    }
}

fn main() {
    let path = "/Users/megurine/.local/share/mise/installs/ruby/4.0.5/\
lib/ruby/gems/4.0.0/gems/rbs-4.0.3/core/string.rbs";
    let code = std::fs::read_to_string(path).expect("read string.rbs");
    let sig = parse(&code).expect("parse string.rbs");
    let mut c = Collector::default();
    c.visit(&sig.as_node());
    println!("parsed real stdlib RBS: string.rbs");
    println!("  class decls: {}", c.classes.len());
    println!("  method defs: {}", c.methods.len());
    let want = ["length", "upcase", "gsub", "downcase"];
    let found: Vec<_> = c.methods.iter().filter(|m| want.contains(&m.as_str())).collect();
    println!("  sample methods present: {found:?}");
}
