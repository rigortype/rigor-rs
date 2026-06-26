# RBS `alias` resolution (audit R5 / alias-FP regression guard). All valid:
# String/Array/Hash define `alias size length`, so `.size` must NOT flag.
# Both tools emit zero error/warning diagnostics; a regression here = false positive.
s = "hello"
a = s.size
b = [1, 2, 3].size
c = ({ k: 1 }).size
