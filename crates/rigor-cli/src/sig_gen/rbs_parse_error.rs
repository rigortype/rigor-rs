//! The reference's `RBS::ParsingError#message` for an RBS source that does not
//! parse (v0.4.7 sig-gen `parse_target_signature`, #2480/#2587):
//!
//! `"{buffer}:{l1}:{c1}...{l2}:{c2}: Syntax error: {message}, token=`{src}` ({type})"`
//!
//! `ruby_rbs::node::parse` reports only the message, so this reads the C
//! parser's `rbs_error_t` (token range + type) directly through the same
//! `ruby-rbs-sys` bindings `ruby-rbs` wraps. Positions follow
//! `RBS::Location`: 1-based line, 0-based character column, from the token's
//! `char_pos` range.

use std::ffi::CStr;
use std::os::raw::{c_char, c_uint};

use ruby_rbs_sys::bindings::{
    rbs_encoding_type_t::RBS_ENCODING_UTF_8, rbs_encodings, rbs_parse_signature,
    rbs_parser_free, rbs_parser_new, rbs_signature_t, rbs_string_new,
};

unsafe extern "C" {
    // Declared in `rbs/lexer.h`, compiled into the `ruby-rbs-sys` static
    // library, but not in its generated bindings.
    fn rbs_token_type_str(type_: c_uint) -> *const c_char;
}

/// `None` when `source` parses; else the `ParsingError` message, its buffer
/// named `buffer_name` (the reference passes the target path).
pub(super) fn parsing_error_message(buffer_name: &str, source: &str) -> Option<String> {
    let bytes = i32::try_from(source.len()).ok()?;
    // SAFETY: mirrors `ruby_rbs::node::parse`: the parser borrows `source`
    // for its lifetime, is freed exactly once below, and the error (owned by
    // the parser) is copied out before the free.
    let (message, type_name, start, end) = unsafe {
        let start_ptr = source.as_ptr().cast::<c_char>();
        let end_ptr = start_ptr.add(source.len());
        let string = rbs_string_new(start_ptr, end_ptr);
        let encoding = &rbs_encodings[RBS_ENCODING_UTF_8 as usize] as *const _;
        let parser = rbs_parser_new(string, encoding, 0, bytes);
        let mut signature: *mut rbs_signature_t = std::ptr::null_mut();
        let ok = rbs_parse_signature(parser, &mut signature);
        let out = if ok {
            None
        } else {
            (*parser).error.as_ref().map(|err| {
                let message = if err.message.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(err.message).to_string_lossy().into_owned()
                };
                let tname = rbs_token_type_str(err.token.type_);
                let type_name = if tname.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(tname).to_string_lossy().into_owned()
                };
                (message, type_name, err.token.range.start.char_pos, err.token.range.end.char_pos)
            })
        };
        rbs_parser_free(parser);
        if !ok && out.is_none() {
            return Some(format!("{buffer_name}: Syntax error"));
        }
        out?
    };
    let chars: Vec<char> = source.chars().collect();
    let clamp = |p: i32| usize::try_from(p).unwrap_or(0).min(chars.len());
    let (s, e) = (clamp(start), clamp(end).max(clamp(start)));
    let loc = |pos: usize| -> (usize, usize) {
        let mut line = 1;
        let mut col = 0;
        for &c in &chars[..pos] {
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    };
    let ((l1, c1), (l2, c2)) = (loc(s), loc(e));
    let token: String = chars[s..e].iter().collect();
    Some(format!(
        "{buffer_name}:{l1}:{c1}...{l2}:{c2}: Syntax error: {message}, token=`{token}` ({type_name})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parse_error_renders_like_rbs_parsing_error() {
        // Oracle (rbs 4.2 under the pinned reference): `sig/a.rbs:2:11...2:13:
        // Syntax error: unexpected token for simple type, token=`->` (pARROW)`.
        let msg = parsing_error_message("sig/a.rbs", "class A\n  def f: ( -> \nend\n").unwrap();
        assert_eq!(
            msg,
            "sig/a.rbs:2:11...2:13: Syntax error: unexpected token for simple type, token=`->` (pARROW)"
        );
        assert!(parsing_error_message("sig/a.rbs", "class A\nend\n").is_none());
    }
}
