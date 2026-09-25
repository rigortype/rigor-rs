#!/bin/sh
# encoding: binary
# Sibling of 118_literal_nilable_fold_binary_encoding: one file per honored
# magic-comment FORM, because a file's script encoding is set by exactly one
# comment (line 1, or line 2 after a `ruby` shebang) — the forms cannot share
# a fixture. The port resolves the encoding the way Prism itself does: the
# `key: value` pass on the honored comment, then — only when that pass cannot
# consume the comment — the loose `coding` fallback scan, with the resolved
# name checked against Prism's own encoding table (issue #164 round 3).
#
# A `#!` line WITHOUT `ruby` (`#!/bin/sh`, `#!/usr/bin/env perl`) does NOT
# unlock line 2 — Prism runs no shebang search outside `main_script`/`-x`
# (a library file just keeps UTF-8) — m1 FIRES on both engines (probed).
#
# Every firing line and every silent row is oracle-measured at the
# `e59b7b89` pin, one fresh temp cwd per case, `--no-cache`, both reference
# libs pinned onto `-I` (UPSTREAM.md hazard 1). All literals use `\x` escapes
# so every byte is ASCII.

def m1 = "\xC3\xA9"[1].upcase    # `upcase` for nil — the resolved encoding is UTF-8, so the fold still answers `nil`
def c1 = "e"[1].upcase           # `upcase` for nil — ASCII is encoding-proof
