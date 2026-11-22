# rigor-rs#811 review, round 1 — a `# resolve-type-names: false` project sig
# must keep param labels AS WRITTEN: `Mutex`, not `Thread::Mutex` (the
# `::`-anchored spelling `project_member_name` hands down must not feed the
# lexical resolver).

R.new.a(1)
R.new.b(1)
R.new.c("s")
R.new.d(1)

# Control: a literal `::Mutex` in a NORMAL file resolves the bundled alias —
# `expected Thread::Mutex`.
R2.new.e(1)
