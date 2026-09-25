# `vendor/capability_roles/` provenance

`capability_roles.rbs` is the reference's shipped capability-role catalogue,
`reference/rigor/data/capability_roles/capability_roles.rbs` (upstream #976,
`v0.3.9`), copied **byte-for-byte** from the pinned submodule. It declares the
five roles `_Closable`, `_RewindableStream`, `_ClosableStream`,
`_FileDescriptorBacked` and `_Callable`.

| | |
|---|---|
| Source | `reference/rigor/data/capability_roles/capability_roles.rbs` |
| Pin | `e59b7b89` |
| sha256 | `00e4494601c58764946e9daab0057b380b5aba615eba20b3f1574fa0ce961ebf` |

It is embedded with `include_str!` (`src/rbs/conformance.rs`) and read ONLY by
the `rigor:v1:conforms-to` scan (issue #129, ADR-0044): loaded after the
project's own signatures, one interface at a time, skipping any name already
declared — the reference's `add_capability_role_signatures`. It is NOT part of
the embedded `vendor/rbs/` set and does not feed any other rule.

**Re-sync at every pin bump** (`UPSTREAM.md` step 3):

```sh
diff -r -x PROVENANCE.md reference/rigor/data/capability_roles crates/rigor-index/vendor/capability_roles
cp reference/rigor/data/capability_roles/*.rbs crates/rigor-index/vendor/capability_roles/
touch crates/rigor-index/src/rbs/conformance.rs   # `include_str!` re-embed
```

`rbs::conformance::tests::vendored_catalogue_matches_the_pin` fails when the
submodule is checked out and the bytes differ. A NEW file in the upstream
directory is a new decision (the scan embeds exactly this one file).
