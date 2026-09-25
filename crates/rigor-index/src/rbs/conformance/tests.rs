use super::closure::Checker;
use super::*;

/// Build the index over a throwaway project `sig/` holding `files`.
fn project(files: &[(&str, &str)]) -> (CoreData, std::path::PathBuf) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rigor-conformance-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let sig = dir.join("sig");
    std::fs::create_dir_all(&sig).unwrap();
    for (name, body) in files {
        let path = sig.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    (CoreData::load_for_project(&[], &[sig]), dir)
}

fn render(data: &CoreData) -> Vec<(String, &'static str, String)> {
    data.conformance_findings()
        .into_iter()
        .map(|f| {
            let file = std::path::Path::new(f.file).file_name().unwrap().to_string_lossy();
            (format!("{file}@{}", f.start_offset), f.rule_id(), f.message())
        })
        .collect()
}

fn rows(files: &[(&str, &str)]) -> Vec<(String, &'static str, String)> {
    let (data, dir) = project(files);
    let out = render(&data);
    std::fs::remove_dir_all(dir).ok();
    out
}

/// The messages only, for compact assertions.
fn messages(files: &[(&str, &str)]) -> Vec<String> {
    rows(files).into_iter().map(|r| r.2).collect()
}

const Z: &str = "interface _Z\n  def zzz: () -> void\nend\n";

fn z_row(class: &str) -> String {
    format!(
        "`{class}` declares `conforms-to _Z` but does not provide required method: `#zzz`. \
         Implement the missing method(s) or remove the directive."
    )
}

/// The allow-list must admit the bundled ancestors every class inherits —
/// refusing one would silence the rule for everything.
#[test]
fn bundled_essentials_are_buildable() {
    let data = CoreData::load();
    let ck = Checker::new(&data.conformance, false);
    for name in [
        "BasicObject",
        "Object",
        "Kernel",
        "Comparable",
        "Enumerable",
        "IO",
        "String",
        "Array",
        "Hash",
        "Integer",
        "Exception",
        "StandardError",
    ] {
        assert!(ck.build_ok(name), "{name} not provably buildable");
    }
    for role in [
        "_Closable",
        "_RewindableStream",
        "_ClosableStream",
        "_FileDescriptorBacked",
        "_Callable",
    ] {
        assert!(ck.required(role).is_some(), "{role} not provably buildable");
    }
}

/// Issue #129 acceptance: the catalogue loads per DECLARATION — a project
/// `_Closable` wins (and its extra member is required) while the shipped
/// `_ClosableStream` still loads; a conforming class and an unannotated one
/// stay silent; an unknown name is the unresolved row.
#[test]
fn per_declaration_catalogue_and_controls() {
    let got = rows(&[(
        "a.rbs",
        "interface _Closable\n  def close: () -> void\n  def shut_hard: () -> void\nend\n\
         %a{rigor:v1:conforms-to _Closable}\nclass P\n  def close: () -> void\nend\n\
         %a{rigor:v1:conforms-to _ClosableStream}\nclass Ok\n  def close: () -> void\n  def closed?: () -> bool\nend\n\
         class Bare\nend\n\
         %a{rigor:v1:conforms-to _NoSuchRoleZzz}\nclass U\nend\n",
    )]);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].1, UNSATISFIED_CONFORMANCE);
    assert!(got[0].2.starts_with(
        "`P` declares `conforms-to _Closable` but does not provide required method: `#shut_hard`."
    ));
    assert_eq!(got[1].1, RBS_EXTENDED_UNRESOLVED);
}

/// A file the reference quarantines (any duplicate declaration) loses every
/// annotation; a class whose definition cannot be built is skipped — both
/// oracle-measured silences.
#[test]
fn quarantine_and_build_failures_are_silent() {
    let got = rows(&[
        ("a.rbs", "RUBY_VERSION: String\n%a{rigor:v1:conforms-to _Closable}\nclass Q\nend\n"),
        (
            "b.rbs",
            "%a{rigor:v1:conforms-to _Closable}\nclass D\n  def x: () -> void\n  def x: () -> void\nend\n",
        ),
        (
            "c.rbs",
            "%a{rigor:v1:conforms-to _Closable}\nclass String\n  def upcase: () -> String\nend\n",
        ),
        (
            "d.rbs",
            "class Base\n  attr_reader z: Integer\n  def z: () -> Integer\nend\n\
             %a{rigor:v1:conforms-to _Closable}\nclass Sub < Base\nend\n",
        ),
        ("e.rbs", "%a{rigor:v1:conforms-to _Closable}\nclass Al\n  alias r nosuch\nend\n"),
        ("f.rbs", "%a{rigor:v1:conforms-to _Closable}\nclass Fires\nend\n"),
    ]);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].2.starts_with("`Fires` declares"));
}

/// Oracle-measured on Ruby 4.0 at `e59b7b89` (PR #150 audit): a module's
/// surface is its self types' OWN (default `Object`, which brings `Kernel`
/// but not `BasicObject`), so `#==` / `#!` are missing there; a generic
/// class reopened without its parameters fails to build, silencing it and
/// every class below it.
#[test]
fn module_surface_and_generic_mismatch() {
    let got = rows(&[(
        "a.rbs",
        "interface _Eq\n  def ==: (untyped) -> bool\n  def !: () -> bool\n  def inspect: () -> String\nend\n\
         %a{rigor:v1:conforms-to _Eq}\nmodule ModEq\nend\n\
         %a{rigor:v1:conforms-to _Eq}\nclass ClsEq\nend\n\
         class Set\nend\n\
         %a{rigor:v1:conforms-to _Closable}\nclass SetSub < Set[Integer]\nend\n\
         %a{rigor:v1:conforms-to _Closable}\nmodule Enumerable\nend\n\
         %a{rigor:v1:conforms-to _Closable}\nclass HashSub < Hash[Integer, Integer]\nend\n",
    )]);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].2.starts_with(
            "`ModEq` declares `conforms-to _Eq` but does not provide 2 required methods: `#==`, `#!`."
        ),
        "{got:?}"
    );
}

/// The reference collects project signature files into a set of expanded
/// paths: a file reached through two `signature_paths:` entries loads once
/// (it is neither reported twice nor a duplicate declaration).
#[test]
fn a_file_reached_twice_loads_once() {
    let (_, dir) = project(&[(
        "a.rbs",
        "interface _Mine\n  def mm: () -> void\nend\n\
         %a{rigor:v1:conforms-to _Mine}\nclass Twice\n  def x: () -> void\nend\n",
    )]);
    let sig = dir.join("sig");
    let data = CoreData::load_for_project(&[], &[sig.clone(), dir.join(".").join("sig")]);
    let got = render(&data);
    std::fs::remove_dir_all(dir).ok();
    assert_eq!(got.len(), 1, "{got:?}");
}

/// `RBS::DefinitionBuilder#build_interface` order: the interface ancestors
/// in `interface_ancestors` order — the LAST include first, each followed by
/// its own ancestors (pre-order) — then own members with an alias's target
/// before it. Oracle: `_I` including `_B` including `_C` lists `b, c, i`.
#[test]
fn required_member_order_matches_rbs() {
    let got = messages(&[(
        "a.rbs",
        "interface _Z\n  def zm: () -> void\nend\ninterface _A\n  def am: () -> void\nend\n\
         interface _F\n  alias bb aa\n  include _Z\n  include _A\n  def aa: () -> void\n  def cc: () -> void\nend\n\
         %a{rigor:v1:conforms-to _F}\nclass C\nend\n\
         interface _C3\n  def c: () -> void\nend\ninterface _B3\n  include _C3\n  def b: () -> void\nend\n\
         interface _I3\n  include _B3\n  def i: () -> void\nend\n\
         %a{rigor:v1:conforms-to _I3}\nclass K\nend\n\
         interface _Ch\n  alias c b\n  alias b a\n  def a: () -> void\nend\n\
         %a{rigor:v1:conforms-to _Ch}\nclass K2\nend\n",
    )]);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got[0].contains("5 required methods: `#am`, `#zm`, `#aa`, `#bb`, `#cc`."), "{got:?}");
    assert!(got[1].contains("3 required methods: `#b`, `#c`, `#i`."), "{got:?}");
    assert!(got[2].contains("3 required methods: `#a`, `#b`, `#c`."), "{got:?}");
}

/// Family 1 (PR #150 review): builds the reference cannot finish — every one
/// oracle-measured silent — are silent here. Must-still-fire controls sit
/// beside them.
#[test]
fn unprovable_builds_are_silent() {
    let cases: &[(&str, &str)] = &[
        ("variance", "class Box[out T]\n  def set: (T) -> void\nend\n"),
        ("variance_in", "class Box[in T]\n  def get: () -> T\nend\n"),
        ("ivar_dup", "class C\n  @x: Integer\n  @x: String\nend\n"),
        ("iface_dup", "class C\n  include _A\n  include _B\nend\n"),
        ("overload_nobase", "class C\n  def foo: () -> void | ...\nend\n"),
        ("include_class", "class C\n  include Integer\nend\n"),
        ("prepend_class", "class C\n  prepend String\nend\n"),
        ("super_module", "class C < Kernel\nend\n"),
        ("include_noargs", "class C\n  include Enumerable\nend\n"),
        ("super_noargs", "class C < Array\nend\n"),
        ("include_extra_args", "class C\n  include Comparable[Integer]\nend\n"),
        ("self_type_noargs", "module C : Array\nend\n"),
        ("included_selftype_broken", "class C\n  include M\nend\n"),
        ("ivar_diamond", "class C\n  include M1\n  include M2\nend\n"),
        ("generic_ref", "class C\n  def s: () -> Set[Integer]\nend\n"),
        ("mod_self_class_overload", "module C : S\nend\n"),
        ("alias_to_includer", "class C\n  include Ma\n  def bar: () -> void\nend\n"),
        ("ns_missing", "class Nope::C\nend\n"),
    ];
    let prelude = "interface _A\n  def foo: () -> void\nend\ninterface _B\n  def foo: () -> void\nend\n\
                   class Bad\n  def x: () -> void\n  def x: () -> void\nend\nmodule M : Bad\nend\n\
                   module M3\n  @x: Integer\nend\nmodule M1\n  include M3\nend\nmodule M2\n  include M3\nend\n\
                   class Set\nend\n\
                   class Bs\n  def foo: () -> void\nend\nclass S < Bs\n  def foo: (Integer) -> void | ...\nend\n\
                   module Ma\n  alias foo bar\nend\n";
    for (name, body) in cases {
        let src = format!("{Z}{prelude}%a{{rigor:v1:conforms-to _Z}}\n{body}");
        let got = messages(&[("a.rbs", &src)]);
        assert!(got.is_empty(), "{name}: {got:?}");
    }
    // Controls: the same prelude, provable builds, rows.
    let controls: &[(&str, &str)] = &[
        ("", "class C\nend\n"),
        ("", "class C[T]\n  def get: () -> T\nend\n"),
        ("", "class C[unchecked out T]\n  def get: () -> T\nend\n"),
        ("module Mx\n  def a: () -> void\nend\n", "class C\n  include Mx\n  alias b a\nend\n"),
        ("", "class C < Bs\n  alias bar foo\n  def foo: (Integer) -> void | ...\nend\n"),
        ("", "class C\n  @x: Integer\n  attr_reader x: Integer\nend\n"),
        ("", "class C < Array[Integer]\n  include Comparable\nend\n"),
        ("", "module C\n  include Enumerable[Integer]\nend\n"),
        ("", "class C\n  include _A\nend\n"),
        ("module Mp\n  def p: () -> void\nend\n", "class C\n  prepend Mp\n  def q: () -> Integer\nend\n"),
    ];
    for (extra, body) in controls {
        let src = format!("{Z}{prelude}{extra}%a{{rigor:v1:conforms-to _Z}}\n{body}");
        let got = messages(&[("a.rbs", &src)]);
        assert_eq!(got, vec![z_row("C")], "control {body:?}");
    }
}

/// Family 2: an interface whose build fails makes the reference report "not
/// loaded" (or fall through to an outer candidate); the port stays silent.
#[test]
fn unprovable_interfaces_are_silent() {
    for iface in [
        "interface _A\n  include _Each\n  def a: () -> void\nend\n",
        "interface _A\n  include _ToS[Integer]\n  def a: () -> void\nend\n",
        "interface _A\n  def a: () -> void | ...\nend\n",
        "interface _A\n  include _Each[NoSuch]\nend\n",
        "interface _A[out T]\n  def set: (T) -> void\nend\n",
        "interface _A\n  include _A\n  def a: () -> void\nend\n",
    ] {
        let src = format!("{iface}%a{{rigor:v1:conforms-to _A}}\nclass K\nend\n");
        let got = messages(&[("a.rbs", &src)]);
        assert!(got.is_empty(), "{iface:?}: {got:?}");
    }
    // An interface under a namespace nobody declares: the reference cannot
    // build it and reports "not loaded"; the port stays silent.
    let got = messages(&[(
        "a.rbs",
        "interface Nope::_I\n  def a: () -> void\nend\n%a{rigor:v1:conforms-to Nope::_I}\nclass K\nend\n",
    )]);
    assert!(got.is_empty(), "{got:?}");
    // A name only written in a project type position is stubbed upstream (an
    // empty interface): "not loaded" would be a false positive.
    let got = messages(&[(
        "a.rbs",
        "class K\n  def x: () -> _Missing\nend\n%a{rigor:v1:conforms-to _Missing}\nclass K2\nend\n\
         %a{rigor:v1:conforms-to _Other}\nclass K3\nend\n",
    )]);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].starts_with("`K3` declares `conforms-to _Other` but interface"), "{got:?}");
    // Controls: an overloading member over an included one, an alias to an
    // included member.
    let got = messages(&[(
        "a.rbs",
        "interface _B\n  def a: () -> void\nend\n\
         interface _A\n  include _B\n  def a: (Integer) -> void | ...\n  def b: () -> void\n  alias c a\nend\n\
         %a{rigor:v1:conforms-to _A}\nclass K\nend\n",
    )]);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("3 required methods: `#a`, `#b`, `#c`."), "{got:?}");
}

/// `interface_methods` keys its hash by `Ancestor::Instance` (name and
/// arguments, NOT the include that reached it): an interface met twice is
/// imported once. Oracle: `_Ord` including `_Za` and `_Ab` (which includes
/// `_Za`) builds, members `am, zm, aa`; a class including `_Za` twice builds.
/// A generic interface met twice with other arguments raises upstream
/// (`DuplicatedInterfaceMethodDefinitionError`): silent.
#[test]
fn interface_diamonds_import_once() {
    let got = messages(&[(
        "a.rbs",
        "interface _Za\n  def zm: () -> void\nend\ninterface _Ab\n  include _Za\n  def am: () -> void\nend\n\
         interface _Ord\n  include _Za\n  include _Ab\n  def aa: () -> void\nend\n\
         %a{rigor:v1:conforms-to _Ord}\nclass OrdA\nend\n",
    )]);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("3 required methods: `#am`, `#zm`, `#aa`."), "{got:?}");
    let got = messages(&[(
        "a.rbs",
        &format!(
            "{Z}interface _Za\n  def zm: () -> void\nend\n\
             %a{{rigor:v1:conforms-to _Z}}\nclass C\n  include _Za\n  include _Za\nend\n\
             interface _G[T]\n  def g: () -> T\nend\n\
             %a{{rigor:v1:conforms-to _Z}}\nclass D\n  include _G[Integer]\n  include _G[String]\nend\n"
        ),
    )]);
    assert_eq!(got, vec![z_row("C")]);
}

/// Family 4: a class first declared by bundled RBS (a reopen) sits at a
/// position in the reference's declaration order the port cannot reproduce,
/// so its directives are silent; a project-first class keeps its row.
#[test]
fn bundled_first_classes_are_silent() {
    let got = messages(&[(
        "a.rbs",
        "%a{rigor:v1:conforms-to _Nope}\nclass OptionParser\nend\n\
         %a{rigor:v1:conforms-to _Nope}\nmodule JSON\nend\n\
         %a{rigor:v1:conforms-to _Nope}\nclass Mine\nend\n",
    )]);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].starts_with("`Mine` declares"), "{got:?}");
}

/// Family 5: the project walk is Ruby's `Dir.glob("**/*.rbs")`: no dot-files,
/// no dot-directories, no symlinked directories.
#[cfg(unix)]
#[test]
fn project_glob_matches_ruby() {
    let (_, dir) = project(&[
        ("a.rbs", "interface _R\n  def r: () -> void\nend\n"),
        (".hidden.rbs", "%a{rigor:v1:conforms-to _R}\nclass Kh\nend\n"),
        (".hid/x.rbs", "%a{rigor:v1:conforms-to _R}\nclass Kd\nend\n"),
        ("sub/y.rbs", "%a{rigor:v1:conforms-to _R}\nclass Ky\nend\n"),
    ]);
    let other = dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("z.rbs"), "%a{rigor:v1:conforms-to _R}\nclass Kl\nend\n").unwrap();
    std::os::unix::fs::symlink(&other, dir.join("sig").join("linked")).unwrap();
    let data = CoreData::load_for_project(&[], &[dir.join("sig")]);
    let got: Vec<String> = render(&data).into_iter().map(|r| r.2).collect();
    std::fs::remove_dir_all(dir).ok();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].starts_with("`Ky` declares"), "{got:?}");
}

/// A project `prepend` of an interface crashes the reference's whole run:
/// the port emits no conformance row at all.
#[test]
fn prepend_of_an_interface_stands_everything_down() {
    let got = messages(&[(
        "a.rbs",
        &format!(
            "{Z}%a{{rigor:v1:conforms-to _Z}}\nclass C\n  prepend _Z\nend\n\
             %a{{rigor:v1:conforms-to _Z}}\nclass D\nend\n"
        ),
    )]);
    assert!(got.is_empty(), "{got:?}");
}

/// Family 3: a name the reference's default environment holds but the port's
/// does not (or holds with another surface) is never resolved or trusted.
#[test]
fn load_set_divergent_names_are_silent() {
    for name in ["Prism::_Visitor", "RDoc::Constant", "BigDecimal", "Prism::VERSION"] {
        assert!(super::load_set::LOAD_SET_DIVERGENT.contains(&name), "{name}: regenerate load_set.rs");
    }
    // A project file redeclaring a constant only the reference's environment
    // holds is quarantined there (oracle: `rbs.coverage.quarantined-signature`,
    // no conformance row); its directives are silent here.
    let got = messages(&[(
        "a.rbs",
        &format!(
            "{Z}Prism::VERSION: String\n%a{{rigor:v1:conforms-to _Z}}\nclass C\nend\n\
             %a{{rigor:v1:conforms-to _Nope}}\nclass D\nend\n"
        ),
    )]);
    assert!(got.is_empty(), "{got:?}");
    let got = messages(&[(
        "a.rbs",
        "%a{rigor:v1:conforms-to Prism::_Visitor}\nclass MyVisitor\nend\n\
         interface _Named\n  def value: () -> String\nend\n\
         %a{rigor:v1:conforms-to _Named}\nclass RDoc::Constant\nend\n",
    )]);
    assert!(got.is_empty(), "{got:?}");
}

/// Round 3 (ADR-0044 § "Environment-parity gate"): a project signature file
/// the port cannot prove it reads as the reference does stands the WHOLE scan
/// down — never a silent per-file drop. Each case was a port-only row on the
/// oracle: the port's parser rejects non-ASCII identifiers rbs 4.2 accepts;
/// a NUL byte crashes the reference; a `use` directive stubs its target in
/// every file; `resolve-type-names` is a magic comment the parser never sees.
#[test]
fn unprovable_project_files_stand_the_scan_down() {
    let gate = "interface _Cl\n  def close: () -> void\n  def closed?: () -> bool\nend\n\
                %a{rigor:v1:conforms-to _Cl}\nclass Gate\n  def close: () -> void\nend\n";
    // Control: the same project fires.
    assert_eq!(messages(&[("a.rbs", gate)]).len(), 1);
    for other in [
        "class Gate\n  def closed?: () -> bool\n  def été: () -> void\nend\n",
        "class Other\nend\n\0\n",
        "use Foo::_Bar as _Baz\nclass User\n  def x: () -> _Baz\nend\n",
        "# resolve-type-names: false\nclass User\nend\n",
    ] {
        let got = messages(&[("a.rbs", gate), ("b.rbs", other)]);
        assert!(got.is_empty(), "{other:?}: {got:?}");
    }
}

/// Round 4: a non-UTF-8 entry name under a signature dir is skipped by the
/// port's walk but seen by Ruby's glob — the scan stands down; and a row is
/// positioned against the text the index parsed.
#[cfg(unix)]
#[test]
fn non_utf8_names_stand_down_and_sources_are_kept() {
    use std::os::unix::ffi::OsStrExt;
    let gate = "interface _Cl\n  def close: () -> void\nend\n%a{rigor:v1:conforms-to _Cl}\nclass Gate\nend\n";
    let (data, dir) = project(&[("a.rbs", gate)]);
    let found = data.conformance_findings();
    assert_eq!(found.len(), 1);
    assert_eq!(data.conformance_source(found[0].file), Some(gate));
    let odd = std::ffi::OsStr::from_bytes(b"\xff.rbs");
    if std::fs::write(dir.join("sig").join(odd), "class X\nend\n").is_ok() {
        let again = CoreData::load_for_project(&[], &[dir.join("sig")]);
        assert!(again.conformance_findings().is_empty());
    }
    std::fs::remove_dir_all(dir).ok();
}

/// Round 3, family 6: a bundled plugin's `sig/` is DEFERRED upstream and
/// dropped whole when one of its classes clashes in arity with that class's
/// FIRST declaration — bundled, else the first project one. The port used to
/// compare against its own first declaration (the plugin's), so a project
/// `Duration[T]` against the plugin's `Duration` went unnoticed.
#[test]
fn plugin_arity_standdown_compares_against_the_first_non_plugin_declaration() {
    let plugin = crate::plugins::bundled_plugin("activesupport-core-ext").unwrap();
    let pres = "interface _Pres\n  def present?: () -> bool\n  def closed?: () -> bool\nend\n\
                %a{rigor:v1:conforms-to _Pres}\nclass Gate\nend\n";
    let run = |other: &str| {
        let (_, dir) = project(&[("a.rbs", pres), ("b.rbs", other)]);
        let data = CoreData::load_for_project(&[plugin], &[dir.join("sig")]);
        let got: Vec<String> = render(&data).into_iter().map(|r| r.2).collect();
        std::fs::remove_dir_all(dir).ok();
        got
    };
    // Oracle: the plugin stands down, `present?` goes missing too — silent here.
    assert!(run("module ActiveSupport\n  class Duration[T]\n  end\nend\n").is_empty());
    // Control (oracle-identical): same arity, the plugin loads, `present?` is provided.
    let got = run("module ActiveSupport\n  class Duration\n  end\nend\n");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("required method: `#closed?`"), "{got:?}");
}

/// The vendored catalogue is byte-identical to the pinned reference's
/// (skipped when the submodule is not checked out).
#[test]
fn vendored_catalogue_matches_the_pin() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../reference/rigor/data/capability_roles");
    let Ok(pinned) = std::fs::read_to_string(dir.join("capability_roles.rbs")) else {
        return;
    };
    assert_eq!(pinned, CAPABILITY_ROLES_RBS, "re-sync vendor/capability_roles/");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(files.len(), 1, "a new catalogue file upstream: {files:?}");
}

/// `harness/conformance_load_set.rb` runs this to read the port's side.
#[test]
#[ignore = "driven by harness/conformance_load_set.rb"]
fn dump_conformance_surface() {
    let Ok(path) = std::env::var("RIGOR_CONFORMANCE_DUMP") else {
        return;
    };
    // `RIGOR_CONFORMANCE_DUMP_PLUGIN=<id>`: the same view with one bundled
    // plugin's RBS loaded (the generator's `--plugin` check).
    let plugins: Vec<&crate::plugins::BundledPlugin> = std::env::var("RIGOR_CONFORMANCE_DUMP_PLUGIN")
        .ok()
        .and_then(|id| crate::plugins::bundled_plugin(&id))
        .into_iter()
        .collect();
    std::fs::write(path, CoreData::load_with_plugins(&plugins).conformance_surface_dump()).unwrap();
}

#[test]
fn parses_the_directive_like_the_reference_regex() {
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to _Closable"), Some("_Closable"));
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to ::_Closable"), Some("_Closable"));
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to   _Callable  "), Some("_Callable"));
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to Foo::Bar::_X1"), Some("Foo::Bar::_X1"));
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to _1x"), None);
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to Closable"), None);
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to foo::_X"), None);
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to_X"), None);
    assert_eq!(parse_conforms_to(" rigor:v1:conforms-to _X"), None);
    assert_eq!(parse_conforms_to("rigor:v1:conforms-to _X _Y"), None);
    assert_eq!(parse_conforms_to("rigor:v1:return: Integer"), None);
}

#[test]
fn messages_match_the_reference() {
    let f = ConformanceFinding {
        kind: ConformanceKind::Unsatisfied { missing: vec!["closed?"] },
        class_name: "Gate",
        interface_name: "_ClosableStream".into(),
        file: "/x/sig/a.rbs",
        start_offset: 0,
        end_offset: 40,
    };
    assert_eq!(
        f.message(),
        "`Gate` declares `conforms-to _ClosableStream` but does not provide required method: \
         `#closed?`. Implement the missing method(s) or remove the directive."
    );
    let two = ConformanceFinding {
        kind: ConformanceKind::Unsatisfied { missing: vec!["zeta", "alpha"] },
        ..f.clone()
    };
    assert!(two.message().contains("does not provide 2 required methods: `#zeta`, `#alpha`."));
    let u = ConformanceFinding { kind: ConformanceKind::Unresolved, ..f };
    assert_eq!(u.rule_id(), RBS_EXTENDED_UNRESOLVED);
    assert_eq!(
        u.message(),
        "`Gate` declares `conforms-to _ClosableStream` but interface `_ClosableStream` is not \
         loaded. Check for a typo or add the `sig`/library that declares it to the RBS load path."
    );
}
