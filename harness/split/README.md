# harness/split — move-only module splits, with proof

The tooling behind the #204 split of `rigor-infer/src/lib.rs` (14,084 → 144
lines over PRs #205–#216, each Approved in its first review round). What landed,
and the hazards found along the way, are in
`docs/notes/20260927-lib-rs-split-outcome.md`.

A move-only PR is cheap to review only if it is *provably* move-only. These
scripts do the moving mechanically and produce the evidence. Humans still pick
the module boundaries and write the `//!` headers.

| script | does |
|---|---|
| `split_tests.py FILE [NAME…]` | moves inline `#[cfg(test)] mod NAME { … }` blocks to files; proves it by re-inlining to the original bytes |
| `split_mod.py FILE MOD SELECTION DOC` | moves selected items (and single methods of an inherent `impl`) verbatim into a new child module |
| `fixvis.py --crate C MODFILE [--prune]` | adds the imports and `pub(crate)`s rustc asks for, then prunes unused imports |
| `doclinks.py FILE NAME=PATH…` | adds `/// [`NAME`]: PATH` targets for intra-doc links the move broke |
| `testimports.py --crate C NAME=PATH…` | imports, in the test files, names they had reached through the parent's `use`s |
| `verify_move.py BASE_REV DIR…` | the line-multiset proof: what is left after moved lines cancel out |

`rsitems/` is the Rust item lister the scripts call (syn with
`span-locations`). It is standalone: its own `[workspace]` and `Cargo.lock`,
built on first use with `--offline` into `target/split-tools`.

## Step 1: test modules out (no production code touched)

```sh
python3 harness/split/split_tests.py crates/C/src/FILE.rs
```

For a crate root or `mod.rs` the body goes to `src/NAME.rs`; for any other
file it goes to `src/<stem>/NAME.rs`, which is where rustc looks for `mod NAME;`.
Module paths do not change, so the `cargo test -- --list` names stay the same.

The body is de-indented by four spaces. The interior lines of a multi-line
literal are left byte-for-byte whenever re-indenting them would change the
value: a raw string, a string holding a literal newline, or a `/** */` doc. A
`\`-continued string is de-indented, because the escape drops the leading
whitespace. The script refuses to write anything unless re-inlining the new
files gives back the original bytes.

## Step 2+: one module per PR

1. **Pick the items and write the selection file.** List candidates with
   `target/split-tools/release/rsitems FILE`. Selectors are `NAME` (a top-level
   item), `impl:TYPE` (a whole inherent impl, e.g. `impl:Typer<'i>`) or
   `TYPE::method` (e.g. `Typer::type_of`). Each item takes the comment gap
   above it with it; the script prints any non-blank gap so you can check that
   no banner belonging to a neighbour moved.
2. `split_mod.py FILE MOD sel.txt doc.txt` writes the new module and wires
   `mod MOD;` / `pub(crate) use MOD::*;` / `pub use` (for moved `pub` items)
   into FILE.
3. `fixvis.py --crate C src/MOD.rs --prune` loops `cargo check` to a clean
   build:
   - A name the module cannot resolve is imported from `crate::` (or from
     `super::` below a non-root file).
   - An item, method, field or type that rustc reports as private gets
     `pub(crate)`.
   - Unused imports in the module are dropped, and so is an unused glob
     re-export in the parent.
   - A parent import that is unused in every target is dropped. One unused
     only outside the test build is reported as **test-only**: remove it from
     the parent, then run `testimports.py`.
4. **Doc links.** Diff the rustdoc warnings from before and after the move:
   `cargo doc -p C --no-deps --document-private-items 2>&1 | grep '^warning' | sort`.
   For each new "unresolved link to `X`", run `doclinks.py src/MOD.rs X=path`.
   A doc-only import would trip `unused_imports`, so give the link a target
   instead.
5. **Proof and gates:**
   - `verify_move.py origin/master crates/C/src` must report nothing
     UNEXPECTED;
   - `cargo test -p C -- --list` must be identical to the base;
   - the rustdoc warning set must be identical;
   - `harness/gate.sh` must pass;
   - clippy 1.88 (CI);
   - before ready, the release sweep (`fp_audit.py --gaps --sweep`) must be
     equal to master's.

## Hazards (each one bit #204)

- **Doc blocks glued to the wrong item.** When an item is inserted between a
  doc comment and its item, the doc ends up on the new one. A move-only split
  carries it into the wrong file, as happened six times in rigor-infer. Scan
  for this before moving, and fix it in its own commit.
- **Test-only parent imports.** See step 3. Do not answer them with
  `#[cfg(test)]` imports in the parent.
- **Private type across the boundary.** Reading a field of a moved type from
  another module needs `pub(crate)` on the type as well as the field. rustc
  reports this as "type `X` is private", with no error code, and fixvis
  handles it.
- **Glob shadowing.** A parent item or import that is later added with the
  same name as a globbed item shadows the glob silently. Check this whenever
  a glob re-export is kept.
- **Name resolution.** The proof counts lines, not meaning. Before trusting
  it, check for these in the moved code: a name that could now resolve to a
  different item (prelude shadowing, a glob), `self::`/`super::` paths,
  `macro_rules!` textual scope, and traits in scope for method calls. The
  #204 reviews did exactly this.
