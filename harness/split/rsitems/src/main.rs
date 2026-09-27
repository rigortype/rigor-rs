//! List a Rust file's items with their line spans, for harness/split/*.py.
//!
//! `rsitems FILE` prints one TSV row per top-level item and per item of a
//! top-level `impl` block:
//!
//! ```text
//! depth  kind  name  start_line  end_line  parent_start_line
//! ```
//!
//! `depth` is 0 for a top-level item and 1 for an impl item (whose
//! `parent_start_line` is its impl's `start_line`). `start_line` includes the
//! item's outer attributes, doc comments among them; plain `//` comments are
//! not items, so the callers attribute them by gap. An impl's `name` is its
//! self type (`Typer<'i>`), or `Trait for Type`.
//!
//! `rsitems FILE --literals` prints every literal that spans lines:
//!
//! ```text
//! start_line  start_col  end_line  end_col
//! ```
//!
//! Columns are 0-based, in chars. A doc comment is a literal to proc-macro2,
//! so a multi-line `/** … */` block shows up here too.
use syn::spanned::Spanned;
use syn::{ImplItem, Item};

fn lines(s: proc_macro2::Span) -> (usize, usize) {
    (s.start().line, s.end().line)
}

fn first_line(attrs: &[syn::Attribute], default: usize) -> usize {
    attrs.iter().map(|a| a.span().start().line).chain(std::iter::once(default)).min().unwrap()
}

fn tokens(t: &impl quote::ToTokens) -> String {
    t.to_token_stream().to_string().replace(' ', "")
}

fn describe(it: &Item) -> (&'static str, String, &[syn::Attribute]) {
    match it {
        Item::Fn(x) => ("fn", x.sig.ident.to_string(), &x.attrs),
        Item::Struct(x) => ("struct", x.ident.to_string(), &x.attrs),
        Item::Enum(x) => ("enum", x.ident.to_string(), &x.attrs),
        Item::Union(x) => ("union", x.ident.to_string(), &x.attrs),
        Item::Const(x) => ("const", x.ident.to_string(), &x.attrs),
        Item::Static(x) => ("static", x.ident.to_string(), &x.attrs),
        Item::Type(x) => ("type", x.ident.to_string(), &x.attrs),
        Item::Mod(x) => ("mod", x.ident.to_string(), &x.attrs),
        Item::Trait(x) => ("trait", x.ident.to_string(), &x.attrs),
        Item::Use(x) => ("use", "-".into(), &x.attrs),
        Item::Macro(x) => {
            ("macro", x.ident.as_ref().map_or("-".into(), |i| i.to_string()), &x.attrs)
        }
        Item::Impl(x) => {
            let name = match &x.trait_ {
                Some((_, path, _)) => format!("{}for{}", tokens(path), tokens(&x.self_ty)),
                None => tokens(&x.self_ty),
            };
            ("impl", name, &x.attrs)
        }
        _ => ("other", "-".into(), &[]),
    }
}

fn print_items(file: &syn::File) {
    for it in &file.items {
        let (kind, name, attrs) = describe(it);
        let (s, e) = lines(it.span());
        let s = first_line(attrs, s);
        println!("0\t{kind}\t{name}\t{s}\t{e}\t0");
        if let Item::Impl(imp) = it {
            for ii in &imp.items {
                let (kind, name, attrs) = match ii {
                    ImplItem::Fn(f) => ("fn", f.sig.ident.to_string(), &f.attrs[..]),
                    ImplItem::Const(c) => ("const", c.ident.to_string(), &c.attrs[..]),
                    ImplItem::Type(t) => ("type", t.ident.to_string(), &t.attrs[..]),
                    _ => ("other", "-".into(), &[][..]),
                };
                let (is, ie) = lines(ii.span());
                println!("1\t{kind}\t{name}\t{}\t{ie}\t{s}", first_line(attrs, is));
            }
        }
    }
}

fn print_literals(ts: proc_macro2::TokenStream) {
    for tt in ts {
        match tt {
            proc_macro2::TokenTree::Group(g) => print_literals(g.stream()),
            proc_macro2::TokenTree::Literal(l) => {
                let (a, b) = (l.span().start(), l.span().end());
                if a.line != b.line {
                    println!("{}\t{}\t{}\t{}", a.line, a.column, b.line, b.column);
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: rsitems FILE [--literals]");
        std::process::exit(2);
    };
    let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    if args.get(1).map(String::as_str) == Some("--literals") {
        print_literals(src.parse().unwrap_or_else(|e| panic!("{path}: {e:?}")));
    } else {
        print_items(&syn::parse_file(&src).unwrap_or_else(|e| panic!("{path}: {e}")));
    }
}
