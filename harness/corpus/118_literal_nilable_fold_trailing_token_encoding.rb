# encoding: binary extra
# Sibling of 118_literal_nilable_fold_binary_encoding: one file per honored
# magic-comment FORM, because a file's script encoding is set by exactly one
# comment (line 1, or line 2 after a `ruby` shebang) — the forms cannot share
# a fixture. The port resolves the encoding the way Prism itself does: the
# `key: value` pass on the honored comment, then — only when that pass cannot
# consume the comment — the loose `coding` fallback scan, with the resolved
# name checked against Prism's own encoding table (issue #164 round 3).
#
# Trailing tokens keep the pair pass from consuming the whole comment, so
# the fallback still reads `encoding: binary` — non-UTF-8 (`# encoding:
# binary # ascii-8bit` resolves the same way, probed).
#
# Every firing line and every silent row is oracle-measured at the
# `e59b7b89` pin, one fresh temp cwd per case, `--no-cache`, both reference
# libs pinned onto `-I` (UPSTREAM.md hazard 1). All literals use `\x` escapes
# so every byte is ASCII.

def m1 = "\xC3\xA9"[1].upcase    # silent on both: the resolved encoding is not UTF-8, so the non-ASCII fold declines
def c1 = "e"[1].upcase           # `upcase` for nil — ASCII is encoding-proof
