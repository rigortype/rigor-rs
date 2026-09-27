use super::*;
use rigor_parse::{lower, parse, Node};

/// The type of the LAST receiver-bearing call in `src`, rendered. Wired like
/// the analyze pass (source index + lexical scopes) so the shadow/lexical
/// gates the stage-2 arms consult are live.
fn ty_of_last_recv_call(src: &[u8]) -> String {
    let ast = lower(&parse(src));
    let index = CoreIndex::new();
    let source = SourceIndex::build(&ast, &index);
    let scopes = lexical_scopes(&ast);
    let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
    let mut i = Interner::new();
    let env = TypeEnv::new();
    let call_id = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
        .last()
        .unwrap();
    let ty = typer.type_of(&ast, call_id, &env, &mut i);
    let name = index.class_name_of(&i, ty).map(str::to_string);
    name.unwrap_or_else(|| rigor_types::describe(&i, ty))
}

// --- 2a: `Dir.glob` / `Dir.[]` -----------------------------------------

/// FIRE (oracle c02): `Dir.glob(...)` types `Array` on the BLOCK-FREE call
/// path even though its block overload returns `nil`. `Dir[...]` (single
/// overload, oracle c01) is the untouched control that already worked.
#[test]
fn s2a_dir_glob_block_free_types_array() {
    assert_eq!(ty_of_last_recv_call(b"x = Dir.glob('*.rb')\n"), "Array");
    assert_eq!(ty_of_last_recv_call(b"x = Dir['*.rb']\n"), "Array");
}

/// DECLINE: a singleton whose overloads diverge for a reason OTHER than a
/// block (`Regexp.last_match`: `MatchData?` vs `String?`) is unchanged — the
/// block-free slot is empty for it, so the arm cannot invent a return.
/// A BLOCK-bearing `Dir.glob { }` also stays Dynamic (it routes to
/// `type_block_call`, which reads `block_returns`, not this slot).
#[test]
fn s2a_divergent_overloads_and_block_form_decline() {
    assert_eq!(ty_of_last_recv_call(b"m = Regexp.last_match(2)\n"), "Dynamic[top]");
    assert_eq!(
        ty_of_last_recv_call(b"x = Dir.glob('*.rb') { |f| f }\n"),
        "Dynamic[top]"
    );
}

// --- 2b: `ENV` ----------------------------------------------------------

/// FIRE (oracle c03): `ENV` is declared `ENV: RBS::Unnamed::ENVClass` in
/// core RBS, so `ENV.keys` types `Array`.
#[test]
fn s2b_env_object_constant_types_its_declared_return() {
    assert_eq!(ty_of_last_recv_call(b"def f\n  x = ENV.keys\nend\n"), "Array");
    assert_eq!(ty_of_last_recv_call(b"def f\n  x = ENV.to_hash\nend\n"), "Hash");
}

/// DECLINE: a PROJECT `ENV` constant makes the core declaration the wrong
/// surface. Probed at the pin: with `ENV = Object.new` the reference reports
/// `undefined method 'keys' for Object` — a different diagnostic — so typing
/// the chain as `Array` here was an oracle FP. A project `module ENV`
/// declines through the same lexical shadow gate, and a method the declared
/// class does not define declines for lack of a return.
#[test]
fn s2b_project_env_shadow_declines() {
    assert_eq!(
        ty_of_last_recv_call(b"ENV = Object.new\ndef f\n  x = ENV.keys\nend\n"),
        "Dynamic[top]"
    );
    assert_eq!(
        ty_of_last_recv_call(b"module ENV\nend\ndef f\n  x = ENV.keys\nend\n"),
        "Dynamic[top]"
    );
    assert_eq!(
        ty_of_last_recv_call(b"def f\n  x = ENV.frobnicate_zzz\nend\n"),
        "Dynamic[top]"
    );
}

// --- 2c: block-free INSTANCE returns (`String#split`) -------------------

/// FIRE (oracle c08b): `String#split` declares `(…) { … } -> self` beside
/// `(…) -> Array[String]`; the block-free call site types `Array`.
#[test]
fn s2c_string_split_block_free_types_array() {
    assert_eq!(ty_of_last_recv_call(b"x = 'a:b'.split(':', 2)\n"), "Array");
}

/// DECLINE: the BLOCK form of the same method does not read the block-free
/// slot (`'a'.split(':') { }` types through `block_returns`, which records
/// the `self` return ⇒ String), and a method with no block overload is
/// unchanged.
#[test]
fn s2c_block_form_and_plain_methods_unchanged() {
    assert_eq!(ty_of_last_recv_call(b"x = 'a:b'.split(':') { |p| p }\n"), "String");
    assert_eq!(ty_of_last_recv_call(b"x = 'a'.upcase\n"), "String");
    assert_eq!(ty_of_last_recv_call(b"x = 'a'.frobnicate_zzz\n"), "Dynamic[top]");
}

// --- 2e: `::`-qualified constant paths ----------------------------------

/// FIRE (oracle u1): the SAME C5 literal constant reached by a fully
/// qualified path types identically to the lexical read — the reference
/// resolves all three spellings to `[:high, :low]`.
const U1: &[u8] = b"module A\n  module B\n    class C\n      PR = { high: 1, low: 2 }.freeze\n\n      def lexical\n        x = PR.keys\n      end\n\n      def qualified\n        x = ::A::B::C::PR.keys\n      end\n    end\n  end\nend\n";

#[test]
fn s2e_qualified_constant_path_resolves() {
    // The last receiver-bearing call is the qualified spelling's `.keys`.
    assert_eq!(ty_of_last_recv_call(U1), "Array");
}

/// DECLINE: an AMBIGUOUS path — two DIFFERENT qualified constants that the
/// use site's lexical candidates both reach — resolves to nothing rather
/// than guessing which one Ruby would pick.
#[test]
fn s2e_ambiguous_qualified_path_declines() {
    // `B::C::PR` at a use site inside `module A` matches BOTH the top-level
    // `B::C::PR` and `A::B::C::PR`.
    let src = b"module B\n  module C\n    PR = { top: 1 }.freeze\n  end\nend\n\nmodule A\n  module B\n    module C\n      PR = { nested: 1 }.freeze\n    end\n  end\n\n  class Use\n    def f\n      x = B::C::PR.keys\n    end\n  end\nend\n";
    assert_eq!(ty_of_last_recv_call(src), "Dynamic[top]");
    // An unknown path declines too.
    assert_eq!(
        ty_of_last_recv_call(b"def f\n  x = ::No::Such::CONST_ZZZ.keys\nend\n"),
        "Dynamic[top]"
    );
}

/// DECLINE (measured on the sweep): a resolved path whose constant is NOT
/// lexically visible from the use site stays untyped, exactly like the bare
/// spelling. Gitlab's `Gitlab::GitalyClient::DiffBlob::ATTRS` read from a
/// SIBLING class `…::DiffBlobsStitcher` is the shape; the reference is
/// silent there, and folding it was an oracle FP on the first cut.
#[test]
fn s2e_cross_namespace_path_declines() {
    let src = b"module G\n  module Client\n    class Blob\n      ATTRS = { a: 1 }.freeze\n    end\n\n    class Stitcher\n      def f\n        x = G::Client::Blob::ATTRS.keys\n      end\n    end\n  end\nend\n";
    assert_eq!(ty_of_last_recv_call(src), "Dynamic[top]");
}

/// DECLINE (measured on the sweep): a NILABLE declared return on the object
/// constant's class. `ENVClass#[]` is `(String) -> String?`; the reference
/// carries `String | nil` and declines dispatch, so the chain must stay
/// untyped rather than become a bare `String`. `ENV.keys` (non-nilable) is
/// the positive control in `s2b_env_object_constant_types_its_declared_return`.
#[test]
fn s2b_nilable_object_constant_return_declines() {
    assert_eq!(ty_of_last_recv_call(b"def f\n  x = ENV['HOME']\nend\n"), "Dynamic[top]");
}
