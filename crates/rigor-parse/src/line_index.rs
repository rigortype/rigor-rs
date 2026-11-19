//! [`LineIndex`] — the byte offsets of every line start in one source, built
//! ONCE and shared (rigor-rs#642).
//!
//! The lowering builder always computed this table for node lines and dropped
//! it with the `LoweredAst`; every other consumer rebuilt it (`check`'s stage
//! 3, `comment_lines`, the pre_eval scanner, `shadowed_rescue`) or skipped it
//! and walked the source prefix per offset (MCP, the LSP's
//! `offset_to_position`, `annotate`) — O(offsets × file bytes) on a
//! pathological file. The table now rides on the [`crate::LoweredAst`] it was
//! built beside, so each offset resolves by binary search.

/// Byte offsets of every line start in a source: `[0]`, then `i + 1` per
/// `\n` — plus the source length, so offset queries clamp exactly the way the
/// per-call byte walk they replace did.
#[derive(Clone, Debug)]
pub struct LineIndex {
    /// `starts[0] == 0`; `starts[k]` is the byte offset line `k + 1` begins at.
    starts: Vec<usize>,
    /// `source.len()` at build time — the clamp bound for every query.
    len: usize,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self { starts: vec![0], len: 0 }
    }
}

impl LineIndex {
    /// Index one source's line starts (one scan of `source`).
    #[must_use]
    pub fn new(source: &[u8]) -> Self {
        let mut starts = Vec::with_capacity(source.len() / 64 + 1);
        starts.push(0);
        for (i, &b) in source.iter().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { starts, len: source.len() }
    }

    /// Wrap a table a caller already built (the lowering builder's), avoiding
    /// a second scan. `starts` must be the [`Self::new`] shape: sorted,
    /// `starts[0] == 0`, `i + 1` per `\n`.
    pub(crate) fn from_starts(starts: Vec<usize>, len: usize) -> Self {
        Self { starts, len }
    }

    /// The 1-based line byte `offset` sits on. `starts[0] == 0` keeps the
    /// partition point ≥ 1, and it IS the 1-based line: a `\n` AT the offset
    /// starts the NEXT line (recorded as `i + 1`, excluded by `<=`), matching
    /// the walk's `i >= clamped` break. An out-of-range offset clamps to the
    /// source end — the walk's clamped answer too.
    #[must_use]
    pub fn line(&self, offset: usize) -> usize {
        self.starts.partition_point(|&s| s <= offset.min(self.len))
    }

    /// The byte offset `offset`'s line begins at — for a consumer that
    /// measures its column in a different unit (the LSP's UTF-16 `character`).
    #[must_use]
    pub fn line_start(&self, offset: usize) -> usize {
        self.starts[self.line(offset) - 1]
    }

    /// 1-based `(line, column)` of a byte offset, columns counted in BYTES
    /// within the line — the reference's unit: it reports
    /// `Prism::Location#start_column + 1`, and Prism's `start_column` is a
    /// byte index into the line (`reference/rigor/lib/rigor/source/node_locator.rb`
    /// documents the same convention for the inverse mapping). Counting
    /// Unicode scalars instead shifted every column right of a multi-byte
    /// character, which on real corpora reported the same diagnostic at a
    /// different column than the reference — a parity break on 8 of the 24
    /// survey FP candidates.
    #[must_use]
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let clamped = offset.min(self.len);
        let line = self.line(clamped);
        (line, clamped - self.starts[line - 1] + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    /// The byte walk `LineIndex` replaces: 1-based (line, byte-column), offset
    /// clamped to the source end. Kept as the equivalence oracle — the indexed
    /// resolver must agree with it on EVERY offset.
    fn walk_line_col(source: &str, byte_offset: usize) -> (usize, usize) {
        if source.is_empty() {
            return (1, 1);
        }
        let clamped = byte_offset.min(source.len());
        let mut line = 1usize;
        let mut line_start = 0usize;
        for (i, b) in source.as_bytes().iter().enumerate() {
            if i >= clamped {
                break;
            }
            if *b == b'\n' {
                line += 1;
                line_start = i + 1;
            }
        }
        (line, clamped - line_start + 1)
    }

    #[test]
    fn line_col_matches_the_byte_walk_at_every_offset() {
        // Multi-byte scalars, a `\n` as the offset itself (stays on the
        // newline's own line), a trailing `\n`, and a past-EOF clamp.
        let src = "a = \"\u{e4}x\"\n\u{1f48c} = 2\nfoo.bar\n";
        let index = LineIndex::new(src.as_bytes());
        for off in 0..=src.len() + 3 {
            assert_eq!(index.line_col(off), walk_line_col(src, off), "offset {off}");
        }
        assert_eq!(LineIndex::new(b"").line_col(5), walk_line_col("", 5));
        let cr = "a\rb\nc\n";
        let index = LineIndex::new(cr.as_bytes());
        for off in 0..=cr.len() {
            assert_eq!(index.line_col(off), walk_line_col(cr, off), "cr offset {off}");
        }
    }

    #[test]
    fn line_col_counts_bytes_not_scalars() {
        // ä = 2 bytes: a byte column keeps parity with Prism's `start_column`.
        let src = "\"ex\u{e4}mple\".lenght\n";
        let off = src.find("lenght").unwrap();
        assert_eq!(LineIndex::new(src.as_bytes()).line_col(off), (1, 12));

        // 4-byte scalar, and a second line so the line walk is exercised too.
        let src = "x = 1\n\"\u{1f48c}\".lenght\n";
        let off = src.find("lenght").unwrap();
        assert_eq!(LineIndex::new(src.as_bytes()).line_col(off), (2, 8));
    }

    #[test]
    fn line_col_start_of_line_and_eof() {
        let src = "\u{1f48c}\nb\n";
        let index = LineIndex::new(src.as_bytes());
        assert_eq!(index.line_col(0), (1, 1));
        // Start of line 2 is column 1 regardless of line 1's byte width.
        assert_eq!(index.line_col(src.find('b').unwrap()), (2, 1));
        // An offset past the buffer clamps to its end rather than panicking.
        assert_eq!(index.line_col(9_999), (3, 1));
        assert_eq!(LineIndex::new(b"").line_col(7), (1, 1));
    }
}
