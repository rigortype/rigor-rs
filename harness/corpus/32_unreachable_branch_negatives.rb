# Adversarial FP guard for flow.unreachable-branch: every DECLINE case. None of
# the conditionals below may emit an unreachable-branch diagnostic on rigor-rs OR
# the reference. An extra here would be a false positive.

# 1. Non-literal predicate (a variable) -> no syntactic literal -> silent.
if x
  a
else
  b
end

# 2. Constant predicate -> the rule uses SYNTACTIC literal detection, NOT the
#    folder, so a constant never flags -> silent.
if DEBUG
  a
else
  b
end

# 3. Empty dead THEN branch (`if false` with no then body) but a live else ->
#    the dead branch node is absent -> silent.
if false
else
  live2
end

# 4. `if false; end` with no branches at all -> silent.
if false
end
