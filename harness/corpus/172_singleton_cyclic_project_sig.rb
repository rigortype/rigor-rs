# rigor-rs#494: the singleton twin of fixture 156 — the same cyclic project
# sig/, but the classes are NOT reopened in source, so `X.zork` exercises the
# singleton build. The reference builds the singleton definition on the
# instance one, so a cyclic instance chain fails `X.zork` too: only the
# controls `C` (no cycle) and `Both` (diamond) witness.
UsesM.zork
A.zork
Sub.zork
Self.zork
M1.zork
C.zork
Both.zork
