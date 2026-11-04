# A String fold past 4096 bytes declines whole (upstream v0.4.6, #2125 /
# #2247, `STRING_FOLD_BYTE_LIMIT`): a format width, a pad width, a repeat or
# a concatenation that would build a larger String does not fold to a
# constant. The reference then answers a `literal-string` carrier, which
# the port does not model, so it stays silent there.
#
# Must-still-fire controls: the same operations at or under the limit.

big_repeat = "a" * 5000
big_repeat.zzz
big_pair = "ab" * 3000
big_pair.zzz
big_concat = ("a" * 3000) + ("b" * 3000)
big_concat.zzz
big_format = "%05000d" % 1
big_format.zzz
big_pad = "a".center(5000)
big_pad.zzz

small_repeat = "ab" * 3
small_repeat.zzz
small_format = "%05d" % 1
small_format.zzz
small_pad = "a".center(5)
small_pad.zzz
