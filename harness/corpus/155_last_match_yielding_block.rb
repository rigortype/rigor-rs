# `Regexp.last_match` inside a `scan`/`gsub`/`sub` block (upstream v0.4.7,
# #2468 / issue #2443, `match_rebinding/yielding_match.rb`).
#
# The literal block of `recv.scan|gsub|gsub!|sub|sub!` on a String-or-untyped
# receiver binds `$~` to `MatchData` on entry, so a zero-arg `last_match` read
# there is non-nil — nested blocks and lambdas inherit it. The port has no `$~`
# scope and declines the zero-arg arm syntactically.
#
# Must-still-fire controls: `last_match(1)` (numbered groups stay `String?`), a
# direct `=~` statement earlier in the block (rebinds to `MatchData?`), a read
# outside any block, an `each_line`/`tr` block, and a `def` nested in the block.
def a1(value)
  value.gsub(/y/) do
    m = Regexp.last_match
    m[0]
  end
end

def a2
  "abc".scan(/b/) do
    m = Regexp.last_match
    m.begin(0)
  end
end

def a3(s)
  s.sub!(/z/) do
    m = ::Regexp.last_match
    m.pre_match
  end
end

def a4(v)
  v.gsub(/y/) do
    c = Regexp.last_match(1)
    c.upcase
  end
end

def a6(v)
  v.gsub(/y/) do
    [1].each do
      m = Regexp.last_match
      m[0]
    end
  end
end

def a7(v)
  v.each_line do
    m = Regexp.last_match
    m[0]
  end
end

def a8(v)
  v.gsub(/y/) do
    v =~ /q/
    m = Regexp.last_match
    m[0]
  end
end

def a9
  m = Regexp.last_match
  m[0]
end

def b5(v)
  v.sub(/y/) { |x| m = Regexp.last_match; m.end(0) }
end

def b9(v)
  v.gsub(/y/) do
    x = lambda do
      m = Regexp.last_match
      m[0]
    end
    x.call
  end
end

def b10(v)
  v.gsub(/y/) do
    def inner
      m = Regexp.last_match
      m[0]
    end
  end
end

def b11(v)
  v.gsub(/y/) do
    "zz".match?(/q/)
    m = Regexp.last_match
    m[0]
  end
end

def c4(v)
  v.gsub(/y/) do
    v =~ /q/
    if v
      m = Regexp.last_match
      m[0]
    end
  end
end

def c9(v)
  v.gsub(/y/) do
    "zz".gsub(/q/) do
      v =~ /q/
    end
    m = Regexp.last_match
    m[0]
  end
end

def a11(v)
  v.tr("a", "b") do
    m = Regexp.last_match
    m[0]
  end
end
