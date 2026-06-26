# Realistic VALID Ruby — both tools should emit zero error/warning diagnostics.
# Stress test for the real-RBS index: every call below is a real method on its
# inferred receiver class (incl. inherited). A false positive here = regression.
greeting = "Hello, World"
slug = greeting.downcase.strip.reverse
len = slug.length
upper = greeting.upcase
n = 42
doubled = n.abs.succ
label = n.to_s.rjust(5)
flag = greeting.empty?
