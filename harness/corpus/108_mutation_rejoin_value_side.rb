# A LATER store into an already-widened collection grows the carrier's value
# side, so two branch edges that stored different value classes no longer carry
# the same instantiation.
#
# Upstream `1ad7351e` (#580, `v0.3.9`), `Inference::MutationRejoin`: mutation
# widening used to be a one-way door — the first content mutation replaced the
# literal `Tuple` / `HashShape` with a `Nominal`, `widen_for_mutator` had no
# `Nominal` arm, and every later store was invisible to content evidence. Both
# edges of an `if` therefore carried the SAME `Nominal[Hash]`, `Scope#join` kept
# it, and the receiver kept witnessing. Since the re-join the edges differ, the
# join is a `Type::Union`, and `receiver_descriptor` has no union arm — so the
# receiver witnesses nothing.
#
# rigor-rs models exactly that difference and nothing else: the accumulated set
# of erased store-value classes rides in the carrier's `args`, so this pass's
# standing identical-`TypeId` join decline does the union for it. The members
# are never read, only compared.
#
# This family is the one the `v0.3.8 -> v0.3.9` bump's SWEEP found and the
# fixture harness could not: gitlab-foss's `duo_agent_platform/config.rb:98`
# (`normalized_cache.presence`), the single FP candidate on 9,337 files.
#
# EVERY line — firing and silent alike — was oracle-measured at the `v0.3.9` pin
# (`d0c370f7`), one fresh temp cwd per case, `--no-cache`, both reference libs
# pinned onto `-I` (UPSTREAM.md hazard 1).

# --- STAYS SILENT: the edges disagree, so the join is a union ----------------

# (r1) the shape the sweep found: an unconditional store of one class, then a
# branch store of another.
def r1(flag)
  h = {}
  h['a'] = [1]
  if flag
    h['b'] = 2
  end
  h.frobnicate_r1
end

# (r2) the Array twin, through `push`.
def r2(flag)
  a = []
  a.push([1])
  if flag
    a.push(2)
  end
  a.frobnicate_r2
end

# (r3) `<<` stores too.
def r3(flag)
  a = []
  a << "s"
  if flag
    a << 1
  end
  a.frobnicate_r3
end

# (r4) a LOCAL's own carrier names the stored class — this is the gitlab row's
# `normalized_cache['key'] = key_config` half.
def r4(flag)
  inner = {}
  inner['x'] = 1
  h = {}
  h['a'] = "s"
  if flag
    h['b'] = inner
  end
  h.frobnicate_r4
end

# --- STILL FIRES: the edges agree, or there is no join at all ----------------

# (r5) two UNCONDITIONAL stores of different classes: one edge, no join. The
# carrier grows and still witnesses — answer `untyped` here and this row is a
# lost matched row, not an FP.
def r5
  h = {}
  h['a'] = [1]
  h['b'] = 2
  h.frobnicate_r5
end

# (r6) a branch store of the SAME class as the one before it: both edges end at
# the same member set, the join keeps it. This is the row that separates "a
# store inside a branch" from "a store the branch's edge does not share".
def r6(flag)
  h = {}
  h['a'] = 1
  if flag
    h['b'] = 2
  end
  h.frobnicate_r6
end

# (r7) the Array twin of r6.
def r7(flag)
  a = []
  a.push(1)
  if flag
    a.push(2)
  end
  a.frobnicate_r7
end

# (r8) a single store, no branch.
def r8
  h = {}
  h['a'] = 1
  h.frobnicate_r8
end

# (r9) an UNNAMED store value (a call result) contributes nothing, exactly as
# the reference's `Dynamic[top]` seed does — it is in every instantiation and so
# never separates two of them.
def r9(flag, other)
  h = {}
  h['a'] = other.to_s
  if flag
    h['b'] = other.to_s
  end
  h.frobnicate_r9
end

# (r10) a non-storing mutator leaves the member set alone.
def r10(flag)
  a = []
  a.push(1)
  if flag
    a.sort!
  end
  a.frobnicate_r10
end

# (r11) the unmutated literal keeps its shape.
def r11
  h = {}
  h.frobnicate_r11
end

# --- THE UNION RE-JOINS: a later store collapses it back into one carrier ----

# (r12) mastodon's `application_helper.rb:180`, the row this family cost before
# the union was modelled: the branch push makes the two edges diverge, the
# UNCONDITIONAL push after it re-joins them into one carrier, and the reference
# fires on the call that follows. Collapse the join to untyped instead and this
# row is silent — a mutator on a Dynamic carrier never mints.
def r12(flag, other)
  out = []
  out << other.to_s
  out << 'system-font' if flag
  out << (flag ? 'reduce-motion' : 'no-reduce-motion')
  out.frobnicate_r12
end

# (r13) the same shape WITHOUT the re-joining store: the edges are still apart
# at the use site, so both engines are silent. This is what keeps r12 from being
# read as "a branch push never matters".
def r13(flag, other)
  out = []
  out << other.to_s
  out << 'system-font' if flag
  out.frobnicate_r13
end

# (r14) a ternary's value class is the join of its arms — the reference's typer
# names it `String` before the store, so the branch push of the same class that
# follows leaves the edges in agreement and the call fires.
def r14(flag, other)
  out = []
  out << (flag ? 'a' : 'b')
  out << 'c' if flag
  out.frobnicate_r14
end

# (r15) arms of DIFFERENT classes name nothing, so the store contributes nothing
# — the conservative direction, which keeps today's answer.
def r15(flag)
  out = []
  out << (flag ? 'a' : 1)
  out << 'c' if flag
  out.frobnicate_r15
end
