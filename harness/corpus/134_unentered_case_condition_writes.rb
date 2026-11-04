# A `case` in value position reads a `when` condition's write as untyped
# (upstream #1484/#1961, v0.4.4).
#
# The statement evaluator does not enter a `case` written as an array element,
# a call argument or a receiver; the scope indexer fills it, applying no write
# a `when` condition makes, so each local one writes reads `Dynamic[top]` from
# the write on. At v0.4.3 the rows in the three value positions below fired
# `undefined method 'upcase' for 1` (the pre-write binding).
#
# Must-still-fire controls: the statement form and the value of a local write,
# which the evaluator enters and where the condition write is dropped, a
# local the condition does not write, and a read after the `case`.

y = 1
[case gets
 when (y = "a") then y.upcase
 end]

y = 1
p(case gets
  when (y = "a") then y.upcase
  end)

y = 1
(case gets
 when (y = "a") then y.upcase
 end).to_s

y = 1
[case gets
 when (y = "a") then 0
 when String then y.upcase
 else y.upcase
 end]

x = 1
[case gets
 when String then x.upcase
 end]

y = 1
case gets
when (y = "a") then y.upcase
end

y = 1
r = case gets
    when (y = "a") then y.upcase
    end

y = 1
[case gets
 when (y = "a") then 0
 end, y.upcase]
p r
