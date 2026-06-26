# A genuine dead-assignment: `result` is written inside `compute` but never
# read in that body (the trailing statement is `final`, not the write), so
# `flow.dead-assignment` fires. Byte-for-byte against the oracle on
# (rule=flow.dead-assignment, line, column = the `result` name token).
def compute
  result = 1
  final = 2
  final
end
