<% if namespaced? -%>
require_dependency "<%= namespaced_path %>/application_controller"
<% end -%>
# FP-audit regression guard: this `.rb` file is actually an ERB generator
# template (`<%= … %>`), not Ruby. Prism's error recovery over it yields a
# garbage AST that the structural rules would over-fire on (jbuilder/redmine
# generator templates surfaced ~58 FPs). Both rigor-rs and the reference detect
# the `%>` closing marker and SKIP analysis entirely. Expected set: EMPTY.
class <%= @controller_class_name %>Controller < ApplicationController
  def index
    @<%= plural_table_name %> = <%= class_name %>.all
  end
end
