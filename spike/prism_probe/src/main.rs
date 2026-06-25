// Spike — ADR-0003 in Rust: prove the cached `ruby-prism` crate parses Ruby,
// exposes comments + node spans, and recovers from errors. Mirrors probe_prism.rb.
use ruby_prism::Visit;

fn main() {
    let src: &[u8] = b"# rigor: leading pragma\ns = \"x\"\ns.lenght\n";
    let result = ruby_prism::parse(src);

    println!("errors      = {}", result.errors().count());
    println!("comments    = {}", result.comments().count());
    for c in result.comments() {
        let loc = c.location();
        let text = std::str::from_utf8(&src[loc.start_offset()..loc.end_offset()]).unwrap_or("?");
        println!("  comment {}..{}: {:?}", loc.start_offset(), loc.end_offset(), text);
    }

    // Walk for the `lenght` call and print its precise span + receiver.
    let mut finder = Finder { src };
    finder.visit(&result.node());
}

struct Finder<'a> {
    src: &'a [u8],
}

impl<'a> ruby_prism::Visit<'a> for Finder<'a> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'a>) {
        let name = node.name();
        let name = std::str::from_utf8(name.as_slice()).unwrap_or("?");
        if name == "lenght" {
            let ml = node.message_loc().unwrap();
            let recv = node.receiver().map(|r| {
                let l = r.location();
                std::str::from_utf8(&self.src[l.start_offset()..l.end_offset()])
                    .unwrap_or("?")
                    .to_string()
            });
            println!(
                "  call .{} receiver={:?} message_loc={}..{}",
                name,
                recv,
                ml.start_offset(),
                ml.end_offset()
            );
        }
        ruby_prism::visit_call_node(self, node);
    }
}
