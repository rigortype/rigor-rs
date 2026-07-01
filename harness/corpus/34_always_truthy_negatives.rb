# Adversarial FP guard for flow.always-truthy-condition: every DECLINE case.
# None of the predicates below may emit an always-truthy-condition diagnostic on
# rigor-rs OR the reference. An extra here would be a false positive — and this
# rule's value is precisely that it does NOT fire where the flat (non-flow) env
# would wrongly retain a constant.

# 1. Branch-reassignment widening (the keystone). `na` is `5` on the straight
#    line, but a CONDITIONAL reassignment means it is `5 | <recompute>` at the
#    second `if` — the branch join widens it to non-constant, so NO fire. The
#    flat env would keep `na = 5` and falsely fire; the flow join is what makes
#    this sound.
na = 5
if guard
  na = recompute
end
if na
  puts "na"
end

# 2. Defensive predicate call (`nil?`/`empty?`/`zero?`/`any?`/`none?`/`all?`/
#    `respond_to?`). The user reads as explicitly checking a runtime condition;
#    the reference skips these, so we do too -> silent.
nb = 5
if nb.nil?
  puts "nb"
end

# 3. Loop-nested predicate. Loop-mutation modelling is incomplete, so a constant
#    read inside a `while`/`until`/`for`/block body is suspect -> suppressed.
nc = 7
while guard
  if nc
    puts "nc"
  end
end

# 4. A method parameter is `Dynamic[top]`, never a constant -> no fold -> silent.
#    (Params are the most common predicate; they must never fold.)
def check(flag)
  if flag
    puts "flag"
  end
end
