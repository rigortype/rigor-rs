# The `resolved` BIT and the oracle's TRAVERSAL BLIND SPOTS.
#
# Promoted from the twelve scratch probes of the slice-4 probe
# (`docs/notes/20260826-effects-s4-probe.md` § 5b/§ 6), which measured that the
# first seven corpus projects cannot tell a correct implementation from a no-op
# one: an arm emitting NO `unresolved-self-call` taint whatsoever scored
# `MATCH=327 UNDER=237 OVER=0`, byte-identical to a carefully tuned arm. Nothing
# in 01-07 has a summary that turns on the `resolved` bit, and none of them
# contains a position the reference's own typer never visits.
#
# Two properties, and every method below names which one it discriminates.
#
# RESOLVED — `Collector::CallRecord#resolved` (`collector.rb:38`) is NOT "the
# project defines the method". It is "some dispatch tier typed the call", and
# for a project method that means the callee has REQUIREDS-ONLY parameters and
# the call's positional arity matches exactly (`expression_typer.rb:2388-2392`,
# `:2427`). A kwarg, optional, rest or block parameter on the CALLEE makes every
# implicit-self call to it `unresolved-self-call` — while the edge still
# resolves and the labels still propagate. It gates exactly one thing: that
# taint (`unit_scan.rb:503-511`). It does not gate the edge.
#
# VISITED — `Effects::Collector.record_call` fires from ONE place,
# `ExpressionTyper#call_type_for` (`expression_typer.rb:1141`). A call node the
# typer never types has no `CallRecord`, so `push_edge` returns
# (`unit_scan.rb:515`) — NO EDGE, hence no propagated label — while
# `record_edge` taints unconditionally because `record.nil?` (`:503`). One
# missing record therefore costs both lanes at once.
#
# The blind set MEASURED at the pin, and it is not a grammar (2026-08-26, this
# project):
#
#   blind    `return` values (bare and guarded), regexp and symbol
#            interpolation, `next` / `break` values, the receiver of a compound
#            write, and any arm of a STATICALLY DECIDED branch
#   visited   string interpolation, `if` arms, `unless` bodies, `elsif` arms,
#            simple block bodies, non-tail statements — including through a
#            superclass, so an inherited plain `def` resolves where an inherited
#            `attr_*` unit does not
#
# **This corrects three rows of that probe note's § 4b table.** It lists
# "modifier `unless` body", "block-form `unless` body" and "`elsif` arm" as
# blind; measured here with an ivar or parameter condition all three are
# VISITED, and they go blind only when the condition FOLDS — a local assigned a
# literal, or a project method whose body is one — because the arm is then dead
# code the typer never evaluates. That is the same property as `if false`, and
# `Blind#in_dead_*` below pins it. The probe's three rows were an artifact of
# its own condition expression, and this file is the reproducer.
#
# What this project grades TODAY, against the shipped binary: the RESOLVED half
# bites now (`Stranger` is the first method group in the corpus whose oracle bit
# is false for no reason but that taint, with no project edge to reach it by).
# The VISITED half is a trap laid for the transitive lane — the shipped port
# propagates nothing, so those methods are UNDER on the label lane; they exist
# so that the FIRST slice which joins labels along a syntactic edge set fails
# here rather than on a real project three months later.
#
# Nothing in this file carries an annotation: the declared lane is graded as an
# exact match, and `04_declared` is where it is probed.

# --- 1. The binder admission test — RESOLVED ------------------------------
#
# Seven call sites, one shape: an implicit-self call to a project method that
# exists. Only the CALLEE's parameter list and the site's positional arity
# differ, and that difference alone decides the taint. Every callee proves a
# DIFFERENT label, so a mis-propagation names itself.

class Binder
  # DISCRIMINATES: the control for the whole section. Requireds-only callee,
  # exactly matching positional arity — the one shape the first-iteration binder
  # admits, so the oracle resolves the call, contributes no taint, and this is
  # the only method in § 1 the oracle calls exhaustive. A port whose taint rule
  # is coarser than the binder's marks it non-exhaustive (UNDER, sound, and what
  # the shipped binary does); a port that never taints marks the other six
  # exhaustive (OVER).
  def calls_required
    required_callee(1)
  end

  def required_callee(_a)
    puts "required"
  end

  # DISCRIMINATES: a KEYWORD parameter on the callee.
  # `user_method_param_shape_simple?` (`:2427-2435`) requires `keywords.empty?`,
  # so the tier declines, the call is `unresolved-self-call`, and the oracle's
  # bit is FALSE — even though the edge resolves and `io.fs.read` propagates.
  def calls_kw
    kw_callee(path: "x")
  end

  def kw_callee(path:)
    File.read(path)
  end

  # DISCRIMINATES: an OPTIONAL parameter on the callee (`optionals.empty?`).
  # Same verdict as the keyword case, and worth its own fixture because a port
  # reproducing the binder syntactically has four disjuncts to get right, not one.
  def calls_optional
    optional_callee
  end

  def optional_callee(path = "x")
    File.write(path, "body")
  end

  # DISCRIMINATES: a REST parameter on the callee (`rest.nil?`).
  def calls_splat
    splat_callee(1, 2)
  end

  def splat_callee(*_parts)
    Time.now
  end

  # DISCRIMINATES: a BLOCK parameter on the callee (`block.nil?`). The callee is
  # requireds-only in every other respect, which is the trap: a port checking
  # only "keywords and optionals" passes the cases above and still over-claims
  # here.
  def calls_block_param
    block_callee
  end

  def block_callee(&_blk)
    rand(10)
  end

  # DISCRIMINATES: ARITY, with the parameter shape admitted. `required.size ==
  # arg_types.size` (`:2392`) is an exact test, so the same callee that resolves
  # in `calls_required` does not resolve here.
  def calls_wrong_arity
    required_callee
  end

  # DISCRIMINATES: arity in the other direction — too MANY positionals. The
  # oracle's test is equality, not a minimum.
  def calls_too_many_args
    required_callee(1, 2)
  end
end

# --- 2. Selectors the closed world does not answer — RESOLVED -------------

class Stranger
  # DISCRIMINATES: THE BITE. A receiver-less call to a selector NO unit in this
  # project defines and no catalogue row claims. It is the one shape whose
  # oracle bit is false for a single reason — the `unresolved-self-call` taint —
  # with no project edge to reach it by. Every other non-exhaustive method in
  # the corpus is ALSO reachable through the port's selector-set stand-in
  # (`collect.rs:257`), which is why suppressing the taint entirely used to
  # change no number anywhere.
  def calls_nothing_at_all
    no_such_helper_anywhere
  end

  # DISCRIMINATES: the same, with arguments, so a port cannot pass by treating
  # bare-name calls as local-variable reads.
  def calls_nothing_with_args
    no_such_helper_anywhere(1, "two")
  end

  # DISCRIMINATES: the same taint in a method that ALSO proves a label. A port
  # that stops walking once an origin has fired, or that lets a catalogue answer
  # stand in for exhaustiveness, keeps the label and loses the bit.
  def proves_and_calls_nothing
    puts "before"
    no_such_helper_anywhere
  end
end

class Beta
  def only_on_beta
    puts "beta"
  end
end

class Gamma
  # DISCRIMINATES: a selector that names a unit on an UNRELATED class. The
  # oracle taints (nothing typed the call) and propagates nothing, because
  # `targets_for` walks Gamma's ancestry and Beta is not in it. The port's
  # slice-3 stand-in reaches the same FALSE by a different road — the selector
  # set ignores the receiver class — so a later slice that replaces the stand-in
  # with a real class-scoped closure and does NOT reconstruct the `resolved` bit
  # goes silent here and claims exhaustiveness. Measured as an OVER on exactly
  # this shape (probe arm v0).
  def calls_it
    only_on_beta
  end
end

# --- 3. Traversal blind spots — VISITED -----------------------------------
#
# One leaf, `Leaky#leaky`, proving `io.output.stdout`, called once per position
# from a method that does nothing else. `Visited#*` and `Blind#*` are the same
# call in different syntax; the oracle propagates the label to the first group
# and not to the second.

class Leaky
  def leaky
    puts "leaked"
  end
end

# Both groups inherit `leaky` rather than defining it, which pins a second fact:
# an inherited plain `def` RESOLVES (every `Visited#*` below is exhaustive in the
# oracle), where an inherited `attr_*` unit does not — synthesised units resolve
# only on the receiver's own class (probe `p_attr`).
class Visited < Leaky
  # DISCRIMINATES: STRING interpolation IS visited — the twin of
  # `Blind#in_regexp_interpolation` and `Blind#in_symbol_interpolation`. Three
  # interpolations, one visited and two not, so "interpolation" is not the
  # discriminating property and a port cannot mirror the blind set by node kind.
  def in_string_interpolation
    "value: #{leaky}"
  end

  # DISCRIMINATES: a modifier `if` body is visited.
  def in_modifier_if
    leaky if @flag
  end

  # DISCRIMINATES: a block-form `if` arm is visited.
  def in_if_arm
    if @flag
      leaky
    end
  end

  # DISCRIMINATES: a modifier `unless` body IS visited when the condition is not
  # statically decidable — the row the probe note's § 4b table gets wrong. Its
  # blind twin is `Blind#in_dead_unless`, which differs only in the condition.
  def in_modifier_unless
    leaky unless @flag
  end

  # DISCRIMINATES: a block-form `unless` body is visited, same correction.
  def in_block_unless
    unless @flag
      leaky
    end
  end

  # DISCRIMINATES: an `elsif` ARM is visited, same correction. Its blind twin is
  # `Blind#in_dead_elsif`, whose `if` condition folds true.
  def in_elsif_arm
    if @a
      nil
    elsif @b
      leaky
    end
  end

  # DISCRIMINATES: a simple block body is visited — while the measured residual
  # on mastodon's `BackupService` is a self-call inside a block nested in a block
  # whose receiver chain the typer gave up on, and is NOT. The blind set is
  # semantic, keyed on how far the typer got, so "suppress all block bodies" is
  # refuted by this method and that residual together.
  def in_simple_block
    [1, 2].each { leaky }
  end

  # DISCRIMINATES: a non-tail statement is visited — the twin of
  # `Blind#in_return_value`, whose only difference is the `return` keyword.
  def in_non_tail_statement
    leaky
    :done
  end
end

class Blind < Leaky
  # DISCRIMINATES: a `return` VALUE is not visited. No edge, forced taint. Pair
  # with `Visited#in_non_tail_statement`.
  def in_return_value
    return leaky
  end

  # DISCRIMINATES: a guarded `return` value — the shape that actually occurs in
  # application code, and the reason this section is not a curiosity. Pair with
  # `Visited#in_modifier_if`: the modifier is visited, the `return` is not.
  def in_guarded_return
    return leaky if @flag

    nil
  end

  # DISCRIMINATES: REGEXP interpolation is not visited, while string
  # interpolation is.
  def in_regexp_interpolation
    /#{leaky}/
  end

  # DISCRIMINATES: SYMBOL interpolation is not visited, while string
  # interpolation is.
  def in_symbol_interpolation
    :"#{leaky}"
  end

  # DISCRIMINATES: a `next` VALUE is not visited — and it sits inside a block
  # body, which is, so a port cannot mirror this by suppressing the enclosing
  # block.
  def in_next_value
    [1, 2].map { next leaky }
  end

  # DISCRIMINATES: a `break` VALUE is not visited, same containment.
  def in_break_value
    [1, 2].each { break leaky }
  end

  # DISCRIMINATES: a statically FALSE branch is not visited. The typer folds the
  # condition and never evaluates the arm, so dead code contributes neither an
  # edge nor a label — while the port's syntactic walk sees the call.
  def in_dead_branch
    if false
      leaky
    end
  end

  # DISCRIMINATES: a dead `unless` BODY — the condition is a local holding a
  # literal, so it folds truthy and the body is never evaluated. Together with
  # `Visited#in_modifier_unless` this isolates the real property: deadness, not
  # `unless`.
  def in_dead_unless
    truthy = 1
    leaky unless truthy
  end

  # DISCRIMINATES: a dead `elsif` ARM, and the fold crossing a CALL — the `if`
  # condition is a project method whose body is the literal `true`, so the typer
  # decides the branch and the `elsif` arm is dead. A port would need constant
  # folding through the call graph to mirror this, which is why the blind set is
  # not portable at parity.
  def in_dead_elsif
    if always_true
      nil
    elsif @b
      leaky
    end
  end

  def always_true
    true
  end

  # DISCRIMINATES: a PARAMETER DEFAULT is invisible to the effect scan on BOTH
  # sides — `UnitScan` only ever walks the body (`unit_scan.rb:138`) — so this
  # one is free, and it is here so that a port which starts scanning parameter
  # defaults finds out in the corpus rather than on a real project.
  def in_parameter_default(_a = leaky)
    nil
  end
end

class CompoundWrite
  # DISCRIMINATES: the RECEIVER of a compound index write is not visited. The
  # write itself is classified on both sides; what is lost is the edge into
  # `counters`, so the label that receiver call proves does not propagate.
  def in_index_or_write
    counters["k"] ||= 1
  end

  # DISCRIMINATES: the same for an arithmetic compound write, so the property is
  # the compound write and not `||=`.
  def in_index_plus_write
    counters["k"] += 1
  end

  def counters
    puts "built"
    @counters ||= {}
  end
end

# --- 4. Two smaller traps -------------------------------------------------

class Selfy
  # DISCRIMINATES: a `define_method` BODY's `self` is the CLASS OBJECT. The unit
  # key is `Selfy#from_define_method` (instance), but the typer sees the block's
  # self as the class, so the edge upstream records is `Selfy.dm_helper` —
  # SINGLETON — and resolves to nothing: no label, and an `unresolved-self-call`
  # cause. A port that takes the instance/singleton flag from the enclosing unit
  # scores 2 OVER here, one label and one bit (probe arm `S4_DM=instance`).
  define_method(:from_define_method) do
    dm_helper
  end

  def dm_helper
    Time.now
  end
end

# DISCRIMINATES: a `<toplevel>` self-call records `receiver_class: "Object"`,
# while the summary key is `<toplevel>#top_helper`, so the edge resolves to
# nothing and `global.read` does not propagate — yet the call IS resolved, so
# `top_caller` is EXHAUSTIVE with an empty summary. Both halves matter: a port
# that maps implicit self to the enclosing unit's own key prefix propagates the
# label and scores 1 OVER (probe arm `S4_TOP=unit`), and this is also the only
# method in the file whose bit the shipped port is stricter about.
def top_caller
  top_helper
end

def top_helper
  ENV["HOME"]
end
