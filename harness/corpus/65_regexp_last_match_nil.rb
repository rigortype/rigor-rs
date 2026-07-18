# P2 (Regexp.last_match optional-local nil source): `Regexp.last_match` is a
# core SINGLETON returning an optional — `last_match() -> MatchData?`,
# `last_match(n) / (name) -> String?`. Bound to a local and dereferenced
# straight-line (inside the same `gsub`/`gsub!` block), the nilable local fires
# `call.possible-nil-receiver` on a method absent on NilClass and present on the
# concrete arm. Both `Regexp` and `::Regexp` are recognized.
#
# Byte-for-byte against the oracle on (rule, line, column). The decline backstop
# keeps every ambiguous / guarded / splat / multi-arg shape silent (zero-FP).

# --- FIRES: last_match(int) -> String, straight-line String deref ----------

def dictionary(schema, database_name)
  schema.gsub(/x/) do
    content = ::Regexp.last_match(2)
    # `gsub` is on String, absent on NilClass -> possible-nil on `content`.
    content.gsub(database_name, "$DB")
  end
end

# --- FIRES: last_match() -> MatchData, straight-line MatchData derefs -------

def collection(value)
  value.gsub(/y/) do
    match = Regexp.last_match
    full_match = match[0]
    variable_name = match[:key]
    [full_match, variable_name]
  end
end

def collapsible(content)
  content.gsub!(/z/) do
    match_data = ::Regexp.last_match
    title = match_data[1]
    level = match_data.begin(0)
    [title, level]
  end
end

# --- STAYS SILENT ----------------------------------------------------------

def guarded
  # An intervening `if`/`unless` guard narrows nil away (clears the fact) ->
  # matches the reference staying silent after a real narrowing guard.
  m = Regexp.last_match(1)
  return if m.nil?

  m.upcase
end

def safe_nav
  # Safe-nav short-circuits on nil at runtime -> never a bug.
  c = Regexp.last_match(2)
  c&.gsub("a", "b")
end

# --- FIRES: a non-literal arg still selects `String?` BY ARITY (every 1-arg
#     overload returns `String?` — `(Integer)` and `(name)` alike), matching
#     the reference's overload resolution (compat plan S2).
def non_literal_arg(i)
  c = Regexp.last_match(i)
  c.gsub("a", "b")
end

def not_core_regexp
  # A constant that is not the core `Regexp` is not this source.
  c = Foo.last_match(2)
  c.gsub("a", "b")
end
