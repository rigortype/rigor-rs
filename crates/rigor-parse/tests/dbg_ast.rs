#[test]
fn dbg_pattern_lowering() {
    let src = b"case 1; in w; end; (w = \"s\") rescue nil; w.upcase";
    let result = rigor_parse::parse(src);
    let ast = rigor_parse::lower(&result);
    for (id, n) in ast.iter() {
        eprintln!("{id:?} {n:?}");
    }
}
