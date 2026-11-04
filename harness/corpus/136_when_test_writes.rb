# A `when` test that writes a local is code the reference runs (upstream
# #2036, v0.4.5): Ruby evaluates each test in order before the clause body
# and every later clause, so the write binds in the body, in a later clause,
# in the `else` and after the `case`. The port reads such a local untyped
# from the write on. At v0.4.4 every row below fired `undefined method
# 'upcase' for 1` (the pre-write binding).
#
# Must-still-fire controls: a test that runs no code keeps its narrowing
# (`ctl_noncode`), a later plain rebind restores the binding, and a local
# the test does not write.

y = 1
case gets
when (y = "a") then y.upcase
end

y = 1
case gets
when (y = "a") then 0
when "b" then y.upcase
end

y = 1
case gets
when "q" then 0
when (y = "a") then y.upcase
end

y = 1
case gets
when (y = "a") then 0
else y.upcase
end

y = 1
case gets
when (y = "a") then 0
end
y.upcase

y = 1
case gets
when (y = "s"; "x") then y.upcase
end

# ctl_noncode
z = 1
case gets
when "a" then z.upcase
end

# a later plain rebind
w = 1
case gets
when (w = "a") then 0
end
w = 2
w.upcase

# a local the test does not write
v = 1
case gets
when (u = "a") then v.upcase
end
p u

# always-truthy reads the widened local too (#2036): silent on v0.4.5.
z = nil
case gets
when (z = gets) then 1
end
if z
  1
end

# Control: a `case` whose tests write nothing leaves the fold intact.
q = nil
case gets
when "a" then 1
end
if q
  1
end
