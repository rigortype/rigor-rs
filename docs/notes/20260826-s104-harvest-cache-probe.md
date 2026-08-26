# S104 — per-file harvest cache (Design A): implementation probe (2026-08-26)

Probe for [issue #104](https://github.com/rigortype/rigor-rs/issues/104), the
persistence slice of the frozen-index arc. The GO/NO-GO is **already decided**
([the re-measure](20260826-harvest-cache-remeasure.md): Design A GO, Design B
NO-GO) and this note does not re-open it — no timing was taken here. The
question answered is **how to build it soundly**: what serializes, what the key
must cover, where the bytes live, and what mechanically checks "hit ≡
recompute".

Read alongside: [the recon note](20260825-rust-glancer-frozen-index-recon.md)
(the disposable-cache discipline), [ADR-0017](../adr/0017-analysis-cache.md) /
[ADR-0028](../adr/0028-cache-descriptor-and-incremental.md) (the
accepted-but-unimplemented designs), [#92](20260825-s92-harvest-merge-impl.md)
(what `Harvest` IS), [#102/#103](20260826-s102-file-identity-impl.md)
(`FileKey`), [ADR-0020](../adr/0020-normalization-and-determinism.md)
(determinism is normative).

---

## 0. Scope correction the mini-spec MUST carry: the AST cannot be skipped

The re-measure's Design A ceiling is stated as "**saved = stage1**
(parse+lower+harvest)". That is a ceiling on *skipping stage 1*, and stage 1
cannot be skipped in `check` as it stands, for two independent reasons that are
both plain in the code:

| consumer | needs | evidence |
|---|---|---|
| stage 2, merge phase **M3** | every file's `&LoweredAst` — Pass 3 (tier-4b return typing) and Pass 4 (interprocedural literal fold, which resolves calls into *other* files' bodies) | `crates/rigor-infer/src/source_index.rs:966` (`let asts: Vec<&LoweredAst> = …`), `:973` (`infer_method_returns(&idx, core, &asts)`), `:997` (`compute_literal_returns(&asts, &defs)`); this is #92 §5, and the #92 impl note's "Not done" section says in as many words that **AST eviction is still blocked** |
| stage 3, analyze | every file's `&LoweredAst` **and** its raw `source` text | `crates/rigor-cli/src/main.rs:843` (`analyze_with_source_and_folder(&p.ast, …)`), `:850` (`shadowed_rescue_diagnostics(&p.ast, …, &p.source)`), `:858` (`void_value_use_diagnostics(&p.ast, …)`), `:873` (`line_col(&p.source, …)`) |

Stage 3 is the fatal one: it is the per-file rule walk, so *every* analysed file
needs its AST every run, cached harvest or not. A per-file harvest cache
therefore skips exactly one call — `SourceIndex::harvest(&ast, &index)` at
`crates/rigor-cli/src/main.rs:776` — and pays `read + parse + lower`
unconditionally.

**Consequences for the spec, in order of importance:**

1. The mini-spec must **not** restate "25 % of wall at 4 675 files" as this
   slice's expected saving. That number is the ceiling of a design that also
   removes the AST from M3 (blocked, #92 §5) and from stage 3 (not removable
   without a per-file *diagnostics* cache, which is the recon note's slice 4 and
   is Design-B-shaped — an all-or-nothing project-index dependency — so it is
   not a free follow-on).
2. The implementation slice should **split the `RIGOR_TIMING` stage-1 label**
   into `parse+lower` and `harvest` as its first commit. That makes the actual
   reachable saving observable, costs two `Instant::now()` calls, and is the
   honest replacement for the ceiling number. Everything downstream of it is a
   measurement the implementer takes on a quiet machine; this probe deliberately
   takes none.
3. The GO itself is not disturbed by this note — the invalidation-surface and
   realistic-reach arguments in the re-measure are structural and still hold —
   but the *size* of the prize is now an open number, and a mini-spec that
   quotes 25 % would be quoting a different design's ceiling.

There *is* a variant that reaches the measured ceiling: cache the **lowering**
too (`LoweredAst` is structurally serializable — a flat `Vec<Node>` +
`NodeId` root + `FileKey`, and the `Node` enum carries no `Arc`/`Box`/`&'static`
anywhere: `crates/rigor-parse/src/ast.rs:296-758`, `:871-881`). It is explicitly
**out of scope** here: at the ~40 KB/file the S4b notes measured it is ~190 MB of
disk at 4 675 files, and whether reading that back beats re-parsing is an
unmeasured question with a plausible "no". Record it as a later, separately
measured slice, not as a stretch goal of this one.

---

## 1. Serializability audit — `Harvest`, field by field

`Harvest` is `crates/rigor-infer/src/source_index.rs:307-338`. All ten fields
are **plain owned data**: `String`, `Vec`, `HashSet`, `HashMap`, `bool`,
`usize`, and four small enums. There is **no `Arc`, no borrow, no interned id,
and no `FileKey`** anywhere in the transitive closure.

| field | type | serializes as | notes |
|---|---|---|---|
| `toplevel_defs` | `HashSet<String>` | sorted string list | order-free value (merge `extend`s into a `HashSet`, `:836`) |
| `discovered_methods` | `HashMap<String, HashSet<String>>` | sorted (key, sorted values) | per-key union at `:837-841` |
| `mutated_params` | `HashMap<String, HashSet<usize>>` | sorted (key, sorted values) | **encode `usize` as fixed-width `u64`** (cross-arch) |
| `constant_write_bare_names` | `HashSet<String>` | sorted string list | `:849` |
| `source_classes` | `Vec<HarvestedClass>` | sequence, **verbatim order** | `HarvestedClass` = `{ name: String, superclass: Option<String>, methods: Vec<String> }` (`:223-227`) |
| `override_classes` | `Vec<HarvestedOverrideClass>` | sequence, **verbatim order** | `:234-247`; see below |
| `rbs_constant_names` | `Vec<String>` | sequence, **verbatim order** | `:333` |
| `constant_writes` | `Vec<HarvestedConstWrite>` | sequence, **verbatim order** | `:256-263`; `{ qualified, namespace: Vec<String>, lit: Option<ConstLit>, writes: usize }` |
| `fold_defs` | `Vec<HarvestedFoldDef>` | sequence, **verbatim order** | `:269-275`; `{ owner, method, kind: DefKind, tail: NodeId, has_explicit_return: bool }` |

Nested non-`std` types, each checked:

* **`Visibility`** (`crates/rigor-parse/src/ast.rs:125-129`) — 3-variant fieldless
  enum. One tag byte.
* **`DefKind`** (`source_index.rs:121-124`) — 2-variant fieldless enum. One tag
  byte.
* **`NodeId`** (`ast.rs:35`) — newtype `u32`. **Not trivially safe** — see §1.2.
* **`ConstLit`** (`source_index.rs:55-94`) — recursive
  `Scalar | Tuple(Vec<ConstLit>) | Hash(Vec<(ShapeKey, ConstLit)>) | Range |
  BareArray | BareHash`. Serializable, with a float caveat (§1.1). Note the
  happy accident: its doc at `:47-53` says it is deliberately "owned,
  interner-INDEPENDENT … so a value can be recorded project-wide once and
  re-interned against each analyzed file's own `Interner`". That design choice,
  made for per-file interners, is exactly what leaves `Harvest` free of interned
  ids today.
* **`Scalar`** (`crates/rigor-types/src/ty.rs:41-48`) — `Int(i64) | Str | Sym |
  Bool | Nil | Float(f64)`.
* **`ShapeKey`** (`ty.rs:124-138`) — already stores floats as raw `u64` bits
  (`Float(u64)`), so it has no float hazard.

### 1.1 The one genuine encoding hazard: `Scalar::Float(f64)`

`Scalar`'s own `PartialEq`/`Hash`/`Ord` compare by `f64::to_bits`
(`ty.rs:64-114`) — bit identity is the type's contract. A **text** format breaks
that contract at the edges: `serde_json` writes non-finite floats as `null` and
cannot read them back as `f64`, and a Ruby literal *can* be non-finite
(`X = 1e400` parses to `inf`). Rule: **encode `Scalar::Float` as
`f64::to_bits()` (a `u64`), never as a decimal rendering.** That single rule
also removes every shortest-round-trip formatting question, and is one more
reason to prefer a binary encoding (§4).

### 1.2 `NodeId` is safe only because the AST is re-derived — and that is load-bearing

`HarvestedFoldDef::tail` is a `NodeId`, i.e. **an index into this file's own
lowered arena** (`source_index.rs:267-268` says so explicitly; `LoweredAst::get`
is `self.nodes[id.0 as usize]`, `ast.rs:908-910`). A cache hit is paired at
merge time with a *freshly lowered* AST for the same bytes, so the index is
valid **iff the lowering is byte-for-byte the same function it was when the
entry was written**.

This makes the generation fingerprint's *binary-identity* term (§3) not a
nicety but the thing that stops a stale `tail` from indexing the wrong node — or
panicking out of bounds. Add a cheap belt: on decode, **reject the entry if any
`tail.0 >= ast.len()`**. O(#fold_defs), turns the worst failure mode into an
ordinary miss.

### 1.3 `FileKey`: the rule is "never serialize one", and today that is free

`FileKey` (`ast.rs:837-846`) is `Path(Arc<Path>) | Anonymous(u64)`. The #103
impl note's §6 flagged that a persistence slice "must refuse to serialize a
`FileKey::Anonymous` rather than re-stamp it". The probe finding is **stronger
and simpler**:

> **`Harvest` does not contain a `FileKey` at all.** The key is stamped at MERGE
> time from the paired `&LoweredAst` (`source_index.rs:910`,
> `ast.file_key().clone()`), never carried in the harvest — which is exactly
> what `HarvestedConst`'s doc at `:198-201` says. So the serialized value is
> **path-free**, and the `Anonymous` question does not arise for this slice.

Two consequences worth writing into the spec as invariants:

1. **Keep it that way.** A path-free harvest is what makes an entry *shareable
   across checkouts* (§4) — two clones of the same repo with the same content
   and the same generation legitimately share entries. The moment a future slice
   makes the harvest self-identifying (which an AST-free merge would require),
   that sharing becomes wrong and the canonical project root must enter the key.
2. If a `FileKey` ever does enter the value: `Path(_)` encodes as its bytes;
   `Anonymous(_)` is a process-local counter (`ast.rs:819-820`) and **must be a
   refusal to cache**, not a re-stamp. Anonymous is unreachable from `check`
   anyway — `check`'s single lowering is `lower_with_key(&result,
   FileKey::for_path(Path::new(path)))` (`main.rs:759`), and every LSP lowering
   goes through `document_file_key` — but the pathless `lower()` *is* still
   reachable from the single-file tools (`coverage.rs:545`, `sig_gen.rs:319`,
   `annotate.rs:93`, `mcp.rs:422`, `lsp.rs:3020`, `:3143`), which is one more
   reason the cache belongs at the `check` call site and **not inside
   `SourceIndex::harvest`** (§6).

### 1.4 Privacy: the codec has to live in `rigor-infer`

Every field of `Harvest` and every `Harvested*` struct is private to
`source_index.rs`. So the encoder/decoder is `rigor-infer`'s (`Harvest::encode`
/ `Harvest::decode`), and `rigor-cli` owns only the *policy* — keys, paths,
atomic writes, fail-open. That seam is worth keeping: `rigor-infer` has no
dependencies at all today (`crates/rigor-infer/Cargo.toml`), and a hand-rolled
codec (§4) keeps it that way.

---

## 2. Determinism audit (ADR-0020)

### 2.1 Where `HashMap`/`HashSet` order could leak into bytes

Exactly four fields, all in the "pure unions" half of `Harvest`
(`source_index.rs:308-318`):

| field | leak | fix |
|---|---|---|
| `toplevel_defs` | set iteration | sort |
| `discovered_methods` | map iteration **and** each inner `HashSet<String>` | sort keys, sort each value set |
| `mutated_params` | map iteration **and** each inner `HashSet<usize>` | sort keys, sort each value set |
| `constant_write_bare_names` | set iteration | sort |

Nothing else can leak: every remaining field is a `Vec` produced by a
deterministic tree walk — Pass 1 and Pass 2 iterate `ast.iter()`
(`:574`, `:742`), Pass 1b/C5a/4a are recursive walkers over `body` slices
(`collect_override_classes` `:2215-2226`, `walk_fold_defs` `:2534-2546`,
`collect_literal_constants`), and Pass 1d reads `lexical_scopes(ast)`, itself a
walk (`:2318-2322`). The transient `seen_writes` / `seen_names` hash containers
inside `harvest` are used only for *dedup and counting*, never iterated.

**The precise rule, and it matters:** sort the four hash-container fields, and
**leave every `Vec` verbatim**. Sorting a `Vec` would make `decode(encode(h))
!= h` under a derived `PartialEq` (`Vec` equality is order-exact) — i.e. the
round-trip gate in §5 would fail — while buying nothing, because those `Vec`
orders are already deterministic.

### 2.2 Does `merge(deserialized)` equal `merge(fresh)`?

**Yes, iff the codec preserves (a) every `Vec`'s order verbatim and (b) the four
hash fields' *value*.** The argument is mechanical, from how `merge` consumes
each half (`source_index.rs:802-1000`):

* the four hash fields are consumed by `extend`/`entry().or_default().extend()`
  into hash containers (`:836-849`) — commutative and idempotent, so only the
  value matters;
* every other field is replayed **sequentially, in slice order**, so its order
  is the whole of its meaning;
* the `FileKey` comes from the paired AST (`:910`), not the harvest, so a
  round-tripped harvest cannot move it.

Given equal harvest values, the merged index is equal up to the three `Vec`
orders that are *already* process-unstable today — `definers`,
`literal_constants`, `nested_constant_namespaces` — which the #92 probe proved
inert, with a per-site argument rather than an appeal to luck
(`docs/notes/20260825-s92-buildproject-pass-inventory.md:304-345`, including the
non-obvious `literal_constant` `max_by_key` tie argument). Nothing about a cache
changes that.

### 2.3 The fields where round-trip order is LOAD-BEARING

Named explicitly, because this is the list a codec review must check:

1. **`override_classes[*].method_visibilities`** (`Vec<(String, Visibility)>`) —
   first-write-wins at `ingest_override_class` (`:1541-1543`). **REACHES
   DIAGNOSTICS**: probe §3.2(i) shows `rigor check a.rb b.rb` warning and
   `b.rb a.rb` silent on the same files; pinned by
   `override_vis_project_order_is_normative` in rigor-rules.
2. **`override_classes[*].includes`** (`Vec<String>`) — ordered append-with-dedup
   (`:1544-1548`), walked by `override_ancestor_names` so the *nearest* defining
   ancestor flips with order. **REACHES DIAGNOSTICS**: probe §3.2(ii), pinned by
   `override_vis_project_include_order_is_normative`.
3. **`override_classes`** itself (the outer `Vec`) — carries 1 and 2 across
   reopens within the file, and decides first-write-wins on `superclass` /
   `is_module`.
4. **`source_classes`** — replayed through `add_source` (`:810-812`), so slice
   order is registration order is **`ClassId` assignment order**, which reaches
   rendered union member order (probe §3.3: `Alpha | Beta` vs `Beta | Alpha`).
5. **`rbs_constant_names`** — `register` order (`:855-859`), same `ClassId`
   channel; it interleaves with Pass 1's registrations, which is why merge
   replays pass-by-pass rather than file-by-file.
6. **`constant_writes`** — `lit_first`'s `or_insert_with` (`:908-911`) records
   the FIRST write's namespace, `FileKey` and value; `writes` sums into the
   single-assignment gate. Order decides *which* value survives.
7. **`fold_defs`** — becomes the `Vec<FoldSite>` per `(owner, method, kind)`
   (`:985-995`), consumed by `compute_literal_returns`, and feeds
   `invert_definers`' push order (`:2593-2604`).
8. **Inside `ConstLit`**: `Tuple(Vec<ConstLit>)` is positional and
   `Hash(Vec<(ShapeKey, ConstLit)>)` is last-wins-on-duplicate *and* the
   rendering order of a `HashShape` in a diagnostic message.

Order-free (value only): `toplevel_defs`, `discovered_methods`,
`mutated_params`, `constant_write_bare_names`, and — a subtlety —
`HarvestedClass::methods` / `HarvestedOverrideClass::methods`, both of which land
in `HashSet`s (`:811`, `:1537-1539`). They are still encoded verbatim, per §2.1's
rule, because they are `Vec`s in the value.

---

## 3. The key and the generation fingerprint

`harvest` is a pure function of `(ast, core)` (`:569`). `ast` is a pure function
of `(file bytes, the binary)`. And — checked, not assumed — **`core` is read at
exactly ONE site inside `harvest`**: `:745`,
`core.knows_class(name) || core.knows_qualified_class(name)`, both of which are
plain keyset membership (`crates/rigor-index/src/rbs.rs:877-879`, `:899-901`).
Nothing else in the 190-line body touches `core`.

So the complete determinant is `(file bytes, binary identity, core identity)`.

### 3.1 (a) Per-file content key

`sha256(source.as_bytes())`, where `source` is the exact `String` stage 1
already read (`main.rs:722`). No extra I/O and no extra pass — the contrast with
Design B, whose whole warm path needed a dedicated read+hash sweep, is that
Design A's hash is a by-product of work already done.

Two notes: `read_to_string` rejects non-UTF-8 before the hash is reachable
(`main.rs:722-727` → `Stage1::IoError`), so the key's domain is exactly the
files that get harvested; and only *successful* harvests are cached — excluded,
ERB-sniffed and parse-error files (`:719`, `:732`, `:749`) produce no harvest and
need no entry, because their gates run off the AST that stage 1 builds anyway.

### 3.2 (b) Generation fingerprint — every input that can change a harvest's VALUE for unchanged bytes

| # | input | why it can change the value | how to hash it cheaply |
|---|---|---|---|
| 1 | **codec schema** | a field added to `Harvest`, a tag renumbered | `HARVEST_SCHEMA_VERSION: u32` const, hand-bumped; it also names the directory (§4) so a bump is a free wipe |
| 2 | **the `CoreIndex`'s class surface** | Pass 2 pre-filters RBS constant names against it (`:745`) — a name the core stops knowing drops from `rbs_constant_names`, changing the merged `ClassId` order | **`sha256` over the SORTED `classes` keys ∪ SORTED `qualified` keys**, via a new `CoreIndex::surface_digest()`. This is *exactly* what harvest reads, and it subsumes items 3–6 below in one term. ~10³ short strings, once per run, against an `index-load` the re-measure clocks flat at ~23 ms |
| 3 | plugin set (`plugins:` + `Gemfile.lock` auto-detect) | reopens core classes ⇒ new class names (`config.rs:581-591`, `rigor-index/src/lib.rs:110-118`) | subsumed by 2; **also** hash the sorted `effective_plugins` list — see the belt-and-braces note |
| 4 | project `sig/**` | ADR-0033 ingestion introduces class names (`rbs.rs:764-766`) | subsumed by 2 |
| 5 | `rbs_collection` dirs | `all_signature_dirs` folds them in (`config.rs:564-572`) | subsumed by 2 |
| 6 | `RIGOR_RBS_CORE_DIR` + its contents | replaces the whole embedded core (`rbs.rs:736-746`) | subsumed by 2 |
| 7 | `.rigor.yml` | reaches 3/4/5 — and is the cheapest place a *future* harvest input could hide | `sha256` of the file's bytes (absent ⇒ a fixed sentinel) |
| 8 | `Gemfile.lock` | reaches 3 | `sha256` of the file's bytes (absent ⇒ sentinel) |
| 9 | **the binary itself** | Prism version, the lowering, the walkers, `MUTATOR_METHODS` (`rigor-infer/src/lib.rs:5432`), the harvest passes — and the `NodeId` arena shape (§1.2) | `(canonical path, len, mtime_nanos)` of `std::env::current_exe()`, **plus** a hand-bumped `HARVEST_LOGIC_VERSION` const |

**Belt-and-braces, and I recommend taking it.** Term 2 is *exact* for today's
harvest, but its exactness is a property of one line (`:745`). Terms 3, 7 and 8
cost two small file hashes and a string join, and they convert "exact today"
into "still safe after someone widens what `harvest` reads from `core`". The
issue text lists `.rigor.yml` / `Gemfile.lock` / `sig/**` for the fingerprint;
this is how to honour that cheaply *and* precisely, rather than walking `sig/**`
directly (which term 2 already covers, at O(class names) instead of O(bytes of
RBS)).

**The binary term is the weak one and the spec should say so.**
`CARGO_PKG_VERSION` is `0.0.1` and never moves, so it is useless alone; the exe
`(len, mtime)` tuple catches every rebuild and every reinstall in practice but
is not content identity; the hand-bumped const is a backstop that a developer
can forget. Hashing the 10 MB executable every run would be exact and is the
alternative if the pair is judged too weak — that is a cost the implementer can
measure. Whatever is chosen, §1.2's `tail.0 < ast.len()` decode check is the
cheap guard against the residual case.

**Deliberately NOT in the fingerprint**, with reasons (each one would cause
needless whole-cache invalidation):

* `paths:` / `exclude:` / the argument order — these choose *which* files are
  analysed and in *what order they merge*. Neither changes any individual
  harvest's value. (`exclude:` is applied before the read, `main.rs:719`.)
* `disable:`, `severity_profile:`, `severity_overrides:`, `bleeding_edge:`,
  `baseline` — stage-3 filtering and stamping only (`main.rs:881-919`).
* `--ruby` / the `RubyFolder` — passed to stage 3
  (`analyze_with_source_and_folder`, `:842`), never to `harvest`.
* the process cwd — it selects *which* `.rigor.yml` / `Gemfile.lock`, whose
  resolved contents are already hashed (7, 8).

---

## 4. Placement and format

### 4.1 Where on disk

**Not `.rigor/`.** The recon note's rule is confirmed by
`.gitignore`, whose entry is commented "*Analysis cache (ADR-0017; also written
by the reference when run here)*" — the directory is the reference's, and
ADR-0028's cache root owns a `<root>/schema_version.txt` marker whose mismatch
rule is *wipe everything under `<root>`*. Two tools sharing that root is a
mutual-wipe hazard, not a namespace collision.

**Not `target/` either.** `target/` is the *rigor-rs build tree*; an installed
`rigor` analysing a Ruby project has none. The cache must be keyed to the
analysed project, and the least invasive home for it is the user's OS cache
directory:

```
<root>/harvest/v<SCHEMA>-<gen16>/<kk>/<content-key-hex>.hv
```

* `<root>`, resolved in order: `RIGOR_RS_CACHE_DIR` (absolute; the harness/test
  lever) → `$XDG_CACHE_HOME/rigor-rs` → macOS `$HOME/Library/Caches/rigor-rs` →
  other unix `$HOME/.cache/rigor-rs` → Windows `%LOCALAPPDATA%\rigor-rs`. None
  resolvable ⇒ **cache silently disabled** (fail-open, §4.4).
* `v<SCHEMA>-<gen16>` — the schema version and the first 16 hex of the
  generation digest. Glancer's "generation-fingerprint directory": a new
  generation is a new empty directory, so whole-cache invalidation costs
  nothing and needs no marker file to police.
* `<kk>` — first two hex of the content key, so no directory holds 4 675 entries.

This deliberately **conflicts with ADR-0017's letter**, which says the cache
lives "under `.rigor/cache` (honouring the reference's `cache.path`)"
(`docs/adr/0017-analysis-cache.md:5`). The conflict is real and the mini-spec
must resolve it explicitly — an ADR-0017 amendment recording that rigor-rs's own
artifacts do not share the reference's cache root, and that `cache.path`
compatibility (ADR-0009) means *reading the key without colliding on the
directory*.

### 4.2 Concurrency: no locks

Glancer needed per-instance directories and OS file locks because its cached
scopes retain dependency-local arena IDs, so entries are coupled across
packages. rigor-rs has no analogue (recon note, §"The keystone"). Here:

* entries are **content-addressed**, so two writers racing on one key write
  identical bytes;
* **atomic write**: create `<key>.tmp.<pid>.<nanos>` *in the same directory*
  (hence the same filesystem), `fsync` optional, then `fs::rename` onto the
  final name. A reader therefore only ever sees a complete file;
* **skip the write entirely if the target already exists** — content-addressing
  makes that correct, it removes most write traffic on a warm run, and it
  sidesteps Windows' refusal to rename onto an existing file;
* a failed rename is ignored (someone else won).

So: no lock files, no per-instance directories, and concurrent `rigor check`
runs (CI matrix, agent loop beside an editor) are safe by construction.

**Multi-checkout**: sharing entries between two clones is *sound and desirable*
today, precisely because the value is path-free (§1.3) and the generation
fingerprint covers everything the two checkouts could differ on that reaches a
harvest. Guard it with the §1.3 invariant, not with a per-checkout directory.

### 4.3 Format: a hand-rolled binary codec, no new dependency

| option | verdict |
|---|---|
| **`serde_json`** (already in `Cargo.lock` via rigor-cli) | rejected — the `Scalar::Float` non-finite hole (§1.1), map-order control that must be hand-written anyway, and it would put `serde` into `rigor-infer`, which has *zero* dependencies today (`crates/rigor-infer/Cargo.toml`) |
| **`bincode` / `postcard`** | rejected for this slice — both are new dependencies, absent from `Cargo.lock`, so a network fetch and a supply-chain line item (ADR-0010) for something whose whole job is a length-prefixed byte stream |
| **hand-rolled, dependency-free** | **recommended** — little-endian fixed-width primitives, `u32` length prefix + UTF-8 bytes for strings, a `u8` tag per enum, `f64::to_bits` for floats, `u64` for every `usize`. ~200 lines plus round-trip tests, `rigor-infer` stays dependency-free, and every determinism decision is explicit rather than delegated |

The precedent is already in the tree twice over: `sha256_hex` is hand-written in
`crates/rigor-cli/src/diagnostic_formats.rs:302` ("*a small, self-contained
implementation … without pulling a crypto crate in for one hash*") and again in
`crates/rigor-effects/src/digest.rs`. The hash this slice needs is the same one
— so **no `blake3` dependency is required either**, and the recon note's
"blake3" should be read as "a content hash", not as a crate. (blake3 is faster
on bulk bytes; if the exe-hash option in §3.2 is taken, or if sha256 over the
corpus shows up in the split timing, that is the moment to argue for it — with a
measurement, not in advance.)

**Do not add a third copy of sha256.** `rigor-index` needs it for
`surface_digest()` and `rigor-cli` needs it for the file keys; `rigor-effects`
must not become a dependency of the `check` path (ADR-0043 §1 keeps that edge
non-existent on purpose). Move the implementation to `rigor-types` (no
dependencies; already depended on by `rigor-index`, `rigor-infer` and
`rigor-cli`), keep its `sha256_known_vectors` test, and have
`diagnostic_formats.rs` call it.

Header on every entry, before the payload: magic `b"RGHV"`, `u32` schema
version, `u32` payload length. Cheap, and it makes a truncated or foreign file a
clean rejection.

### 4.4 Fail-open and no partial salvage

* **Any** failure — root unresolvable, directory creation refused, read error,
  bad magic, short read, unknown tag, trailing bytes, `tail` out of range — is a
  **miss**. Recompute, continue, exit code unchanged.
* **Rejected, never salvaged**: a decode that fails halfway discards the whole
  entry. There is no partial-harvest path, which is what keeps the §5 gate a
  simple equality.
* **Silent.** The cache layer must not write to stderr on any path unless the
  env var explicitly asks. `check`'s standard battery compares stdout **and
  stderr** byte-for-byte against master (#92 gate 5, #103 gate 4); one stray
  `eprintln!` on a cold run fails it.
* Write failures are ignored the same way (a read-only or full `$HOME` degrades
  to today's behaviour).

### 4.5 Bounds

This slice: none, plus a documented size (one `Harvest` is ~4 KB in the LSP's
held table — `lsp.rs:644`, "*~4 KB/file, ~9 % of the AST it sits beside*" — so a
generation directory for a 4 675-file project is on the order of 20 MB).
Recommended: best-effort removal of generation directories older than N days, on
write, never fatal. ADR-0017's LRU cap stays **deferred**, and the mini-spec
should say so rather than leave the ADR looking implemented.

---

## 5. The gate: what `--verify-incremental` means here

ADR-0028 specifies `rigor check --verify-incremental` as a full re-analysis
asserting byte-for-byte diagnostic identity against the incremental result,
mandatory in CI, mismatch fatal (`docs/adr/0028-…:58-60`). Applied to this
slice, the cheapest correct form is a **per-file differential inside one run**,
not two runs:

**Form 1 — `RIGOR_HARVEST_CACHE=verify` (recommended).** In stage 1, on a cache
HIT, *also* compute the fresh harvest and assert `cached == fresh`. A mismatch
is fatal, and the message names the path and the key. One run; pinpoints the
file; and it catches the failure that matters most — a **key that is missing an
input** — which a diagnostics-level diff can easily miss, because most harvest
differences are silent until some later corpus makes them speak.

This needs `#[derive(Clone, PartialEq)]` on `Harvest` and the four `Harvested*`
structs, and that derive is *exactly the right notion of equality*: `Vec`
equality is order-exact where order is load-bearing (§2.3), `HashMap`/`HashSet`
equality is order-free where it is not, and `Scalar`'s hand-written `PartialEq`
compares floats by bits (`ty.rs:64-78`).

**Form 2 — the run-level differential.** `check` with the cache off vs on, twice
each, stdout + stderr + exit compared. No new code; it is #92 gate 5 / #103 gate
4 with one more axis. Keep it as the CI wrapper, and run it warm *and* cold, and
under `RAYON_NUM_THREADS=1` as those gates already do.

**Form 3 — artifact byte-stability (the ADR-0020 gate for the cache itself).**
Run `check` twice in separate processes, each into a wiped cache root, then
`diff -r` the two trees. They must be byte-identical. This is a direct,
mechanical test of §2.1's sorting rule — cheap, and it fails loudly if a future
field is added to `Harvest` without a sort.

**Form 4 — unit round-trip.** `decode(encode(h)) == h` over the harvests of the
existing `probes_s92` corpora (six corpora already exist, plus the
order-conflicting one), and `Harvest::default()`. Non-vacuity should be shown
the way #92 and #103 both showed it: break the encoder once (drop the
`method_visibilities` order, say) and record which tests fail.

The full battery on top, unchanged from the standing bar: fixtures
(`harness/run_snapshot.rb`, 98 fixtures, 2 registered divergences, 0
unregistered), `harness/fp_audit.py --gaps --sweep` at 0 FP / 9 204 files with
the gap set byte-unchanged — **run both cold and warm**, since a warm sweep is
the only thing that exercises the hit path at corpus scale — `cargo clippy
--workspace --all-targets -- -D warnings` in a fresh `CARGO_TARGET_DIR`, and
`python3 harness/docs_check.py`.

On the flag itself: `check`'s argument loop ends in
`other => files.push(other)` (`main.rs:204`), so an unrecognised
`--verify-incremental` is silently treated as a **file path** today. Prefer the
`RIGOR_TIMING` precedent — an env var, invisible by default, that the
differential harness never sets (`main.rs:658-662` argues exactly this) —
`RIGOR_HARVEST_CACHE=off|on|verify`. If a real flag is wanted later it must be
added to that loop *before* the fallthrough, and the name
`--verify-incremental` should be reserved for ADR-0028's whole-run semantics
rather than spent on this slice's per-file one.

---

## 6. Blast radius

### 6.1 What changes

| crate / file | change |
|---|---|
| `crates/rigor-types` | new dependency-free `sha256_hex` (moved from `diagnostic_formats.rs`, test vectors kept) |
| `crates/rigor-cli/src/diagnostic_formats.rs` | its private `sha256_hex` becomes a call into `rigor-types` (no behaviour change; `sha256_known_vectors` still guards it) |
| `crates/rigor-index` | new `CoreIndex::surface_digest()` — sha256 over sorted `classes` ∪ `qualified` keys (`rbs.rs:615`, `:655`) |
| `crates/rigor-infer/src/source_index.rs` | `#[derive(Clone, PartialEq)]` on `Harvest`, `HarvestedClass`, `HarvestedOverrideClass`, `HarvestedConstWrite`, `HarvestedFoldDef`; `HARVEST_SCHEMA_VERSION`; a new `harvest_codec` module with `Harvest::encode`/`decode` |
| `crates/rigor-cli/src/cache.rs` (new) | root resolution, generation digest, atomic write, fail-open read, env gates |
| `crates/rigor-cli/src/main.rs` | the generation digest computed once after `CoreIndex::for_project` (`:670`); stage 1's `:776` becomes lookup-else-compute-and-store; the `RIGOR_TIMING` stage-1 label split (§0) |

Stage 1's shape barely moves. The cache handle is a `PathBuf` + a digest string
— `Sync`, immutable, no shared mutable state — so it drops into the existing
`par_iter` closure with no synchronisation, and the `Box<Harvest>` boxing and
the `catch_unwind` placement (#92 deviation 3: the harvest sits deliberately
*outside* it) both stay exactly as they are.

**The lookup goes at the CLI call site, not inside `SourceIndex::harvest`.**
`harvest` is also reached from `SourceIndex::build` by five single-file tools
(`coverage.rs:546`, `sig_gen.rs:323`, `annotate.rs:95`/`:341`, `type_of.rs:111`,
`mcp.rs:304`/`:354`/`:424`) and from the LSP's per-dispatch **buffer** harvest
(`lsp.rs:2462`). None of them wants a disk round-trip, and the buffer one would
pour a per-keystroke entry into the cache for text that was never on disk.

### 6.2 The LSP: keep it out of this slice

The LSP already holds `Arc<Harvest>` per project file (`lsp.rs:661`,
`HeldFile = (PathBuf, Arc<LoweredAst>, Arc<Harvest>)`), so the *steady-state*
value of a disk cache there is zero — the in-memory table is strictly better.
The only reachable win is **cold start**: `build_overlay`'s
`parse+lower+harvest` sweep (`lsp.rs:810-825`, `held_pair` at `:851-854`). And
it inherits §0's constraint verbatim — the LSP holds the ASTs *because* merge M3
needs them, so a cached harvest saves only the harvest half there too.

Recommendation: **later slice.** Write `cache.rs` so `held_pair` can adopt it
with a one-line change (take `(&[u8] source, &generation)` and return
`Option<Harvest>`), and record the two rules it will need when it does: never
cache a buffer harvest (only on-disk tier-1 files), and the LSP's generation
must be the same *content* digest `check` computes, not the in-memory
`ctx.generation` counter (which is per-session and would make every session a
cold cache).

---

## 7. What this note does not claim

* **No timing was taken.** §0 says the reachable saving is smaller than the
  measured ceiling and does not say by how much; the split `RIGOR_TIMING` label
  is the instrument that answers it, on a quiet machine, in the implementation
  slice.
* **No production code was changed.** This is a note-only branch.
* The `LoweredAst` cache variant (§0) is described, not evaluated — its
  read-vs-reparse question is unmeasured and plausibly negative.
* Whether the cache should ship default-ON with a kill switch, or default-OFF
  for one release, is a judgement left to the mini-spec. The fail-open
  discipline (§4.4) means the worst case of ON is today's behaviour; the worst
  case of OFF is an untested code path. This note leans ON with
  `RIGOR_HARVEST_CACHE=off`, provided the battery in §5 is run warm as well as
  cold.
