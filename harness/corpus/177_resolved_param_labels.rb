# rigor-rs#811 — the `expected:` label renders the env-resolved absolute
# name, not the retained written leaf. On master these emitted the leaf
# (`Mutex`, `Widget`); the oracle's labels are qualified.

# Bare sibling inside `class Top` → `expected Top::Widget`.
Top.new.take(1)

# Bundled `class Mutex = Thread::Mutex` alias → `expected Thread::Mutex`.
W2.new.take_mutex(1)

# Project `class Aliased = Thread::Mutex` → `expected Thread::Mutex`.
W3.new.take_alias(1)

# Nested `class B = Outer::Inner` inside `class Mixin` → `expected Outer::Inner`.
Mixin.new.take(1)

# Tail-position alias `M::Mutex2` → `expected Thread::Mutex`.
M.new.m2(1)

# `use Lib::Al as Al` (single clause landing on an alias) →
# `expected Thread::Mutex`.
UseCl.new.f(1)
