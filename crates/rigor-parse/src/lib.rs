//! Parsing (ADR-0003) + lowering to an owned, `NodeId`-indexed AST (ADR-0012).
//!
//! Verified in the spike: the official `ruby-prism` binding builds offline
//! (libprism via clang) and exposes comments + precise node spans + error
//! recovery. Lowering the borrowed Prism tree into owned nodes (to free
//! inference from the parse-buffer lifetime) is TODO.
#![allow(dead_code)]

pub use ruby_prism;

pub mod ast;

pub use ast::{lower, LoweredAst, MethodBody, Node, NodeId, ParamShape, Span, Visibility};

/// Parse Ruby source with Prism. The borrowed result is lowered into an owned,
/// `NodeId`-indexed AST ([`ast::lower`]) before inference (ADR-0012).
pub fn parse(source: &[u8]) -> ruby_prism::ParseResult<'_> {
    ruby_prism::parse(source)
}

/// Whether `source` is actually an ERB template (a Rails generator
/// `templates/foo.rb` using `<%= … %>`), not Ruby — in which case Prism's
/// error recovery yields a garbage AST that the structural rules over-fire on.
/// The analysis pipeline skips such files entirely, matching the reference's
/// `ErbTemplateDetector`.
///
/// Detection mirrors the reference EXACTLY (byte-level): any `%>` closing marker
/// proves it. `%>` cannot occur in valid Ruby — `%` is a binary operator that
/// needs a right operand, never `>`. Mirroring the exact heuristic keeps parity
/// even on its imperfections (a `%>` inside a string makes BOTH tools skip).
#[must_use]
pub fn looks_like_erb_template(source: &[u8]) -> bool {
    source.windows(2).any(|w| w == b"%>")
}

/// Collect each comment as `(start_line /* 1-based */, comment_text)`.
///
/// Used by in-source diagnostic suppression (`# rigor:disable` /
/// `# rigor:disable-file`): the rules crate scans the returned text for the
/// suppression directives and the line number scopes a line-level disable to
/// its source line.
///
/// Prism's `Location` exposes only a start byte offset (no 1-based line), so the
/// line is derived by counting newlines in `source` up to that offset. `source`
/// MUST be the same byte slice that was parsed. The comment text is the verbatim
/// `# ...` source slice, lossily decoded from UTF-8 (invalid bytes become the
/// replacement character — the directives are ASCII, so this never drops them).
///
/// Total and panic-free: a malformed comment offset is clamped to the source
/// length, and the empty/inverted range that `as_slice` would panic on is
/// guarded.
#[must_use]
pub fn comment_lines(result: &ruby_prism::ParseResult<'_>, source: &[u8]) -> Vec<(usize, String)> {
    // Precompute the byte offset of every line start so each comment's line is a
    // binary search instead of a re-scan from the top of the file.
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in source.iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }

    let mut out = Vec::new();
    for comment in result.comments() {
        let loc = comment.location();
        let start = loc.start_offset().min(source.len());
        let end = loc.end_offset().min(source.len());
        // Guard the empty/inverted range that `as_slice` would panic on.
        let text = if end > start {
            String::from_utf8_lossy(&source[start..end]).into_owned()
        } else {
            String::new()
        };
        // 1-based line: number of line-starts at or before `start`.
        let line = line_starts.partition_point(|&ls| ls <= start);
        out.push((line, text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::looks_like_erb_template;

    #[test]
    fn erb_template_detected_by_closing_marker() {
        // A `%>` closing marker proves an ERB template (cannot occur in Ruby).
        assert!(looks_like_erb_template(b"class <%= @name %> < Base\nend\n"));
        assert!(looks_like_erb_template(b"<% if x -%>\nrequire 'y'\n<% end -%>\n"));
        // Plain Ruby (including a lone `%` modulo op) is not a template.
        assert!(!looks_like_erb_template(b"x = 5 % 2\nputs x\n"));
        assert!(!looks_like_erb_template(b"class Foo\n  def bar; end\nend\n"));
        assert!(!looks_like_erb_template(b""));
    }
}
