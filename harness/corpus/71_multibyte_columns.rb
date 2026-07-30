# Column parity across multi-byte characters.
#
# The reference reports `Prism::Location#start_column + 1`, and Prism counts
# `start_column` in BYTES within the line. Counting Unicode scalars instead
# agrees on every ASCII line and silently shifts every column to the LEFT of
# where the oracle points as soon as a multi-byte character precedes the token
# on the same line — invisible to an all-ASCII corpus, and on real Ruby (RSpec
# specs with emoji/kana in their subjects) it reported the same diagnostic at a
# different column than the oracle.
#
# Every case below puts the offending token to the RIGHT of a character whose
# UTF-8 width differs from 1: 2 bytes (ä), 3 bytes (ミ), 4 bytes (💌).

"exämple".lenght

"ミケル".lenght

"💌".lenght

# Two different widths on one line, and the token to the right of both.
"ä💌".lenght

# Multi-byte characters inside earlier arguments, unresolved call after them.
[1].each { |n| n }
undefined_toplevel_thing("ああ") && another_missing_call("💌")
