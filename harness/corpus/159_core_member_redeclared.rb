# A project sig/ that redeclares a core member (`String#upcase`, not an
# overloading `| ...`): `DuplicatedMethodDefinitionError` takes all of `String`
# down in the reference, so nothing is witnessed on a String receiver.
#
# Must-still-fire controls: other core receivers.
"a".upcase.even?
"a".zork
"a".upcase.zork
1.zork
[1].zork
