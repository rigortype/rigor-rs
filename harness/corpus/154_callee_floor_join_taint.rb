# PR #481 review: the port's callee floor over-approximates the
# reference's (ADR-127), so a local it floors may stay precise on the
# reference. A join whose predicate reads such a local keeps both arms
# here (`"s" | 1`) where the reference folds it (`"s"`): every row that
# judges a value from such a join declines rather than fire with another
# message (`rigor_infer::floor_tainted_reads`).
#
# Must-still-fire controls: a local no floor touches (`z`), and the same
# join over an unfloored literal (`q`).

def go(h) = h.to_s
h = {format: "text"}
go(h)
w = h[:format] == "json" ? 1 : "s"
w.foo
if h[:format] == "json"
  w2 = 1
else
  w2 = "s"
end
w2.foo
w3 = case h[:format] when "json" then 1 else "s" end
w3.foo
v = h[:format] == "json" ? nil : "s"
v.upcase
w5 = (h[:format] == "json" && 1) || "s"
w5.foo
t = h[:format]
t.zork
arr = []
arr << (h[:format] == "json" ? 1 : "s")
arr.first.foo
z = "s"
z.foo
g = {format: "text"}
q = g[:format] == "json" ? 1 : "s"
q.foo
