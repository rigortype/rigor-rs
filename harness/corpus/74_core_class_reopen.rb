# A project reopening of a CORE class contributes methods RBS cannot know about,
# and a `def` with an explicit non-self receiver registers a toplevel name.
#
# Both are suppression gates the reference already has and rigor-rs did not:
#   * `source_declared_method?` -> `Scope#discovered_method?` runs BEFORE the RBS
#     surface is consulted, keyed by qualified class name, over EVERY `def` in
#     the body — including one nested in a block or a conditional.
#   * `ScopeIndexer#record_def_node` keys a def with an empty lexical prefix
#     under `<toplevel>` unless its receiver is `self`, so `def IO.foo` makes a
#     bare `foo` resolve at toplevel.
#
# Measured on rigor-survey: `rake-13.4.2/lib/rake/ext/string.rb` (`class String`
# gains `#ext` / `#pathmap_explode` inside `rake_extension(...) do … end`) and
# `io-console-0.8.2/lib/io/console/size.rb` (`def IO.console_size` calls
# `default_console_size`, defined by `def IO.default_console_size`).

class String
  # Direct child of the reopened body.
  def rigor_direct_ext
    self
  end

  # Nested in a BLOCK — invisible to a direct-children-only harvest.
  [1].each do
    def rigor_block_ext
      self
    end
  end

  # Nested in a CONDITIONAL, both arms.
  if RUBY_VERSION > "3.0"
    def rigor_if_ext
      self
    end
  else
    def rigor_if_ext
      self
    end
  end
end

# A core-typed receiver (String literal, and an RBS-declared String return) must
# see all three, so NOTHING fires here.
"a".rigor_direct_ext
"a".rigor_block_ext
"a".rigor_if_ext
File.basename("a").rigor_direct_ext
File.basename("a").rigor_block_ext

# The reopening did not make up a surface: a genuine typo on String still fires.
"a".lenght

# A toplevel `def <Const>.<name>` registers `<name>` as a toplevel name.
def IO.rigor_default_size
  [25, 80]
end

def IO.rigor_console_size
  rigor_default_size
end

# ... including from an unrelated toplevel def body and from file scope.
def Kernel.rigor_other
  rigor_default_size
end
rigor_default_size

# `def self.x` at toplevel is NOT registered (the reference excludes a `self`
# receiver), so this one still fires.
def self.rigor_self_only
  1
end
rigor_self_only
