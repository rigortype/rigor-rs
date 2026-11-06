//! #412 measuring probe: Prism's nesting cap per shape, and the deep-nesting
//! limits of parse / visit / lower on a thread of a given stack size.
//! Extends the #279 depthprobe (proto/279-tools) with the uncapped shapes.
//!
//!   depthprobe gen KIND N                       print the source
//!   depthprobe errors KIND N                    parse on a 4 GiB thread; error count + messages
//!   depthprobe cap KIND MAXN                    largest N <= MAXN that parses with 0 errors
//!   depthprobe child MODE KIND N STACK_KIB      one trial, in-process
//!   depthprobe bisect MODE KIND STACK_KIB CAP   max surviving N (children)
//!
//! MODE: parse (parse + drop), visit (parse + default Prism `Visit` walk),
//! lower (parse + `rigor_parse::lower`).

use std::process::Command;

use rigor_parse::ruby_prism;

pub fn gen(kind: &str, n: usize) -> String {
    let rep = |s: &str, k: usize| s.repeat(k);
    match kind {
        // ---- capped by Prism's depth limit (the #279 table) ----
        "array" => rep("[", n) + &rep("]", n),
        "call" => rep("f(", n) + "1" + &rep(")", n),
        "block" => rep("a { ", n) + &rep("}", n),
        "if" => rep("if x\n", n) + "1\n" + &rep("end\n", n),
        "paren" => rep("(", n) + "1" + &rep(")", n),
        "begin" => rep("begin\n", n) + "1\n" + &rep("end\n", n),
        "defined" => rep("defined?(", n) + "x" + &rep(")", n),
        "super" => "def m\n".to_string() + &rep("super(", n) + "1" + &rep(")", n) + "\nend\n",
        "yield" => "def m\n".to_string() + &rep("yield(", n) + "1" + &rep(")", n) + "\nend\n",
        "def" => rep("def m\n", n) + &rep("end\n", n),
        "class" => rep("class A\n", n) + &rep("end\n", n),
        "hash" => rep("{a: ", n) + "1" + &rep("}", n),
        "and" => rep("x && (", n) + "1" + &rep(")", n),
        "ivopw" => rep("@a += (", n) + "1" + &rep(")", n),
        "interp" => rep("\"#{", n) + "1" + &rep("}\"", n),
        // ---- more capped kinds (beyond the #279 table) ----
        "while" => rep("while x\n", n) + "1\n" + &rep("end\n", n),
        "unless" => rep("unless x\n", n) + "1\n" + &rep("end\n", n),
        "case" => rep("case x\nwhen 1\n", n) + "1\n" + &rep("end\n", n),
        "lambda" => rep("-> { ", n) + "1" + &rep(" }", n),
        "module" => rep("module A\n", n) + &rep("end\n", n),
        "sclass" => rep("class << self\n", n) + &rep("end\n", n),
        "doblock" => rep("a do\n", n) + "1\n" + &rep("end\n", n),
        "rescue" => rep("begin\n", n) + "1\n" + &rep("rescue\nend\n", n),
        "rcall" => rep("x.f(", n) + "1" + &rep(")", n),
        "idx" => rep("x[", n) + "1" + &rep("]", n),
        "splat" => rep("[*", n) + "1" + &rep("]", n),
        "kwsplat" => rep("{**", n) + "{}" + &rep("}", n),
        "regex" => rep("/#{", n) + "1" + &rep("}/", n),
        "ternary" => rep("a ? 1 : ", n) + "1",
        "assign" => rep("a = ", n) + "1",
        "opassign" => rep("a += ", n) + "1",
        "pow" => "2".to_string() + &rep(" ** 2", n),
        "bang" => rep("!", n) + "a",
        "not" => rep("not ", n) + "a",
        "uminus" => rep("-", n) + "a",
        "ret" => "def m\n".to_string() + &rep("return (", n) + "1" + &rep(")", n) + "\nend\n",
        "masgn" => rep("a, (", n) + "b" + &rep(")", n) + " = 1",
        "blockparam" => rep("a { |x| ", n) + &rep("}", n),
        // ---- capped mixes: which shape packs the most tree depth per depth unit ----
        "arrparen" => rep("[(", n) + "1" + &rep(")]", n),
        "bangparen" => rep("!(", n) + "1" + &rep(")", n),
        "splatparen" => rep("[*(", n) + "1" + &rep(")]", n),
        "parenbegin" => rep("(begin\n", n) + "1\n" + &rep("end)", n),
        "defparen" => rep("defined?((", n) + "1" + &rep("))", n),
        "lambdaparen" => rep("->{(", n) + "1" + &rep(")}", n),
        "interpparen" => rep("\"#{(", n) + "1" + &rep(")}\"", n),
        "andparen" => rep("a&&(", n) + "1" + &rep(")", n),
        "ifparen" => rep("if (", n) + "1" + &rep(")\nend", n),
        "notparen" => rep("not(", n) + "1" + &rep(")", n),
        // ---- candidates for "parsed iteratively, no cap" ----
        "chain" => "x".to_string() + &rep(".a", n),
        "plus" => "1".to_string() + &rep(" + 1", n),
        "strcat" => rep("\"a\" ", n) + "\n",
        "andand" => "a".to_string() + &rep(" && a", n),
        "oror" => "a".to_string() + &rep(" || a", n),
        "kwand" => "a".to_string() + &rep(" and a", n),
        "index" => "x".to_string() + &rep("[1]", n),
        "safenav" => "x".to_string() + &rep("&.a", n),
        "argchain" => "x".to_string() + &rep(".a(1)", n),
        "blkchain" => "x".to_string() + &rep(".a { }", n),
        "cpath" => "A".to_string() + &rep("::A", n),
        "shovel" => "x".to_string() + &rep(" << 1", n),
        "lt" => "1".to_string() + &rep(" < 1", n),
        "ifmod" => "1".to_string() + &rep(" if a", n),
        "rescmod" => "1".to_string() + &rep(" rescue 1", n),
        "whilemod" => "1".to_string() + &rep(" while a", n),
        "minus" => "1".to_string() + &rep(" - 1", n),
        "orkw" => "a".to_string() + &rep(" or a", n),
        "unlessmod" => "1".to_string() + &rep(" unless a", n),
        "callblk" => "x".to_string() + &rep(".a(1) { |y| y }", n),
        "range" => "1".to_string() + &rep("..1", n),
        // ---- uncapped chains inside scopes (def / block / class / rescue / string) ----
        "defchain" => "def m\n  x".to_string() + &rep(".a", n) + "\nend\n",
        "defretchain" => "def m\n  return x".to_string() + &rep(".a", n) + "\nend\n",
        "blkinchain" => "a { |y| y".to_string() + &rep(".a", n) + " }\n",
        "classchain" => "class A\n  X = x".to_string() + &rep(".a", n) + "\nend\n",
        "asgnchain" => "v = x".to_string() + &rep(".a", n) + "\nv\n",
        "ivchain" => "@v = x".to_string() + &rep(".a", n) + "\n",
        "ifchain" => "if x".to_string() + &rep(".a", n) + "\n  1\nend\n",
        "defplus" => "def m(y)\n  y".to_string() + &rep(" + y", n) + "\nend\n",
        "deforor" => "def m(y)\n  y".to_string() + &rep(" || y", n) + "\nend\n",
        "defifmod" => "def m(y)\n  y".to_string() + &rep(" if y", n) + "\nend\n",
        // ---- typed-field chains (recursion that does not pass through `Visit::visit`) ----
        "rescuechain" => "begin\n  1\n".to_string() + &rep("rescue A\n", n) + "end\n",
        "elsifchain" => "if a\n  1\n".to_string() + &rep("elsif a\n  1\n", n) + "end\n",
        "whenchain" => "case x\n".to_string() + &rep("when 1\n  1\n", n) + "end\n",
        _ => panic!("unknown kind {kind}"),
    }
}

struct Count(usize, usize, usize);
impl<'pr> ruby_prism::Visit<'pr> for Count {
    fn visit_branch_node_enter(&mut self, _node: ruby_prism::Node<'pr>) {
        self.0 += 1;
        self.1 += 1;
        self.2 = self.2.max(self.1);
    }
    fn visit_branch_node_leave(&mut self) {
        self.1 -= 1;
    }
    fn visit_leaf_node_enter(&mut self, _node: ruby_prism::Node<'pr>) {
        self.0 += 1;
        self.2 = self.2.max(self.1 + 1);
    }
}

// ---- the C-level depth walk (Prism's exported pm_visit_child_nodes) ----
unsafe extern "C" {
    fn pm_visit_child_nodes(
        node: *const std::ffi::c_void,
        visitor: extern "C" fn(*const std::ffi::c_void, *mut std::ffi::c_void) -> bool,
        data: *mut std::ffi::c_void,
    );
}

struct CDepth {
    depth: usize,
    max: usize,
    limit: usize,
    over: bool,
}

extern "C" fn cdepth_visit(node: *const std::ffi::c_void, data: *mut std::ffi::c_void) -> bool {
    // SAFETY: `data` is the `CDepth` passed by `cdepth`, alive for the walk.
    let st = unsafe { &mut *data.cast::<CDepth>() };
    if st.over {
        return false;
    }
    st.depth += 1;
    st.max = st.max.max(st.depth);
    if st.depth > st.limit {
        st.over = true;
    } else {
        // SAFETY: `node` is a live node of the parse result being walked.
        unsafe { pm_visit_child_nodes(node, cdepth_visit, data) };
    }
    st.depth -= 1;
    false
}

fn root_ptr(result: &ruby_prism::ParseResult<'_>) -> *const std::ffi::c_void {
    match result.node() {
        ruby_prism::Node::ProgramNode { pointer, .. } => pointer.cast(),
        _ => unreachable!("the root is a ProgramNode"),
    }
}

fn cdepth(result: &ruby_prism::ParseResult<'_>, limit: usize) -> (usize, bool) {
    let mut st = CDepth { depth: 0, max: 0, limit, over: false };
    let data: *mut CDepth = &mut st;
    cdepth_visit(root_ptr(result), data.cast());
    (st.max, st.over)
}

/// One trial; returns a summary line. Runs on the CURRENT thread.
fn trial(mode: &str, src: &[u8]) -> String {
    let result = rigor_parse::parse(src);
    let errors = result.errors().count();
    let out = match mode {
        "parse" => format!("errors={errors}"),
        "visit" => {
            use ruby_prism::Visit as _;
            let mut c = Count(0, 0, 0);
            c.visit(&result.node());
            format!("errors={errors} prism_nodes={} prism_depth={}", c.0, c.2)
        }
        "lower" => {
            let ast = rigor_parse::lower(&result);
            format!("errors={errors} lowered_nodes={}", ast.len())
        }
        "cvisit" => {
            let (d, over) = cdepth(&result, usize::MAX);
            format!("errors={errors} c_depth={d} over={over}")
        }
        "cguard" => {
            let (d, over) = cdepth(&result, 25_000);
            let out = format!("errors={errors} c_depth_seen={d} over={over}");
            std::mem::forget(result);
            return out;
        }
        "destroy" => {
            // parse elsewhere is not possible (borrow); time only the drop
            format!("errors={errors}")
        }
        _ => panic!("unknown mode {mode}"),
    };
    drop(result);
    out
}

fn on_big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(4 << 30)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("join")
}

fn errors(kind: &str, n: usize) -> (usize, Vec<String>, usize) {
    let src = gen(kind, n);
    on_big(move || {
        use ruby_prism::Visit as _;
        let result = rigor_parse::parse(src.as_bytes());
        let msgs: Vec<String> = result
            .errors()
            .take(3)
            .map(|e| format!("{:?}", e.message()))
            .collect();
        let mut c = Count(0, 0, 0);
        c.visit(&result.node());
        (result.errors().count(), msgs, c.2)
    })
}

fn child(mode: &str, kind: &str, n: usize, stack_kib: usize) -> ! {
    let src = gen(kind, n);
    let mode = mode.to_string();
    let h = std::thread::Builder::new()
        .name("probe".into())
        .stack_size(stack_kib * 1024)
        .spawn(move || trial(&mode, src.as_bytes()))
        .expect("spawn");
    match h.join() {
        Ok(line) => {
            println!("OK {line}");
            std::process::exit(0)
        }
        Err(_) => {
            println!("PANIC");
            std::process::exit(3)
        }
    }
}

fn run_child(mode: &str, kind: &str, n: usize, stack_kib: usize) -> (bool, String) {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["child", mode, kind, &n.to_string(), &stack_kib.to_string()])
        .output()
        .expect("run child");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && stdout.starts_with("OK") {
        (true, stdout[3..].to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = if stderr.contains("overflowed its stack") {
            "overflow".to_string()
        } else {
            use std::os::unix::process::ExitStatusExt;
            format!(
                "status={:?} signal={:?} {}",
                out.status.code(),
                out.status.signal(),
                stderr.lines().next().unwrap_or("")
            )
        };
        (false, why)
    }
}

fn bisect(mode: &str, kind: &str, stack_kib: usize, cap: usize) -> (usize, String, String) {
    let mut lo = 0usize;
    let mut lo_detail = String::new();
    let mut hi;
    let mut hi_detail = String::new();
    let mut n = 64usize;
    loop {
        let (ok, d) = run_child(mode, kind, n.min(cap), stack_kib);
        if ok {
            lo = n.min(cap);
            lo_detail = d;
            if lo >= cap {
                return (lo, format!(">=cap {lo_detail}"), String::new());
            }
            n *= 2;
        } else {
            hi = n.min(cap);
            hi_detail = d;
            break;
        }
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let (ok, d) = run_child(mode, kind, mid, stack_kib);
        if ok {
            lo = mid;
            lo_detail = d;
        } else {
            hi = mid;
            hi_detail = d;
        }
    }
    (lo, lo_detail, hi_detail)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("gen") => print!("{}", gen(&a[2], a[3].parse().unwrap())),
        Some("errors") => {
            let (n, msgs, depth) = errors(&a[2], a[3].parse().unwrap());
            println!("{}\t{}\terrors={n}\tprism_depth={depth}\t{}", a[2], a[3], msgs.join(" | "));
        }
        Some("cap") => {
            // largest N in [1, maxn] with 0 parse errors (monotone)
            let kind = a[2].clone();
            let maxn: usize = a[3].parse().unwrap();
            let ok = |n: usize| errors(&kind, n).0 == 0;
            if ok(maxn) {
                println!("{kind}\tnocap(>={maxn})");
                return;
            }
            let (mut lo, mut hi) = (0usize, maxn);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if ok(mid) {
                    lo = mid
                } else {
                    hi = mid
                }
            }
            let (_, msgs, _) = errors(&kind, hi);
            println!("{kind}\t{lo}\t{}", msgs.join(" | "));
        }
        Some("guardbench") => {
            // cost of a depth-guard pass (a bounded `Visit` with enter/leave
            // counters) over a corpus: parse-only vs parse + guard, best of REPS
            use std::io::BufRead;
            let files: Vec<Vec<u8>> = std::io::BufReader::new(std::fs::File::open(&a[2]).unwrap())
                .lines()
                .map(|l| std::fs::read(l.unwrap()).unwrap_or_default())
                .collect();
            let reps: usize = a[3].parse().unwrap();
            let r = on_big(move || {
                use ruby_prism::Visit as _;
                let (mut bp, mut bg, mut maxd) = (f64::MAX, f64::MAX, 0usize);
                for _ in 0..reps {
                    let t = std::time::Instant::now();
                    for src in &files {
                        let r = rigor_parse::parse(src);
                        std::hint::black_box(r.errors().count());
                    }
                    bp = bp.min(t.elapsed().as_secs_f64());
                    let t = std::time::Instant::now();
                    for src in &files {
                        let r = rigor_parse::parse(src);
                        let mut c = Count(0, 0, 0);
                        c.visit(&r.node());
                        maxd = maxd.max(c.2);
                    }
                    bg = bg.min(t.elapsed().as_secs_f64());
                }
                let (mut bc, mut maxc) = (f64::MAX, 0usize);
                for _ in 0..reps {
                    let t = std::time::Instant::now();
                    for src in &files {
                        let r = rigor_parse::parse(src);
                        maxc = maxc.max(cdepth(&r, 25_000).0);
                    }
                    bc = bc.min(t.elapsed().as_secs_f64());
                }
                format!("files={} best_parse={bp:.3}s best_parse+guard={bg:.3}s guard~={:.3}s max_visit_depth={maxd} best_parse+cguard={bc:.3}s cguard~={:.3}s max_c_depth={maxc}", files.len(), bg - bp, bc - bp)
            });
            println!("{r}");
        }
        Some("child") => child(&a[2], &a[3], a[4].parse().unwrap(), a[5].parse().unwrap()),
        Some("bisect") => {
            let (n, d, h) = bisect(&a[2], &a[3], a[4].parse().unwrap(), a[5].parse().unwrap());
            println!("{}\t{}\t{}\t{n}\t{d}\t{h}", a[2], a[3], a[4]);
        }
        _ => {
            eprintln!("usage: see the module doc");
            std::process::exit(2)
        }
    }
}
