//! The fields of the `Psych::SyntaxError` the reference rescues in
//! `Configuration.read_yaml` (configuration.rb:471-476), read straight off
//! libyaml's parser.
//!
//! Psych's `make_exception` (psych_parser.c) builds the error from the parser
//! struct: `line`/`column` are ALWAYS `context_mark + 1`, whether or not the
//! error has a context, and `problem`/`context` are the raw C strings.
//! serde_yaml's `Error` Display hides a zero mark and the context mark when it
//! equals the problem mark, so the position cannot be recovered from that
//! text: `...` prints no position at all (Psych `1:1`), and `[a` shows only
//! the problem mark (`2:1`; Psych reports the `[` at `1:1`). So when
//! serde_yaml rejects a file, this re-runs the same libyaml (unsafe-libyaml,
//! the crate serde_yaml drives, a line-for-line port of libyaml 0.2.5) over
//! the same text and reads the error it stops on.

use std::ffi::CStr;
use std::mem::MaybeUninit;

use unsafe_libyaml as sys;

/// `Psych::SyntaxError`'s `line`, `column`, `problem`, `context`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SyntaxError {
    pub line: usize,
    pub column: usize,
    pub problem: String,
    pub context: Option<String>,
}

/// The libyaml error that parsing `text` stops on, or `None` when the first
/// document parses. Only the first document is read: `Psych.parse` returns
/// from `parse_stream` at the first document's end, and serde_yaml's
/// `Deserializer::next` loads only that document too. The encoding and input
/// are set exactly as serde_yaml's parser sets them, so both runs see the
/// same event stream.
pub(super) fn first_document_error(text: &str) -> Option<SyntaxError> {
    // Boxed: `yaml_parser_set_input_string` stores a pointer to the parser in
    // the parser itself (`read_handler_data`), so it must not move.
    let mut parser = Box::new(MaybeUninit::<sys::yaml_parser_t>::uninit());
    // SAFETY: mirrors serde_yaml's `libyaml::parser::Parser::new`/`next`: the
    // parser is initialized before use and deleted exactly once, `text`
    // outlives it, each event is deleted after it is read, and the error
    // strings (static C literals owned by libyaml) are copied out before the
    // delete.
    unsafe {
        let parser = parser.as_mut_ptr();
        if sys::yaml_parser_initialize(parser).fail {
            return None;
        }
        sys::yaml_parser_set_encoding(parser, sys::YAML_UTF8_ENCODING);
        sys::yaml_parser_set_input_string(parser, text.as_ptr(), text.len() as u64);
        let mut out = None;
        loop {
            let mut event = MaybeUninit::<sys::yaml_event_t>::uninit();
            if sys::yaml_parser_parse(parser, event.as_mut_ptr()).fail {
                // The error fields are public only through `Deref`, so read
                // them via a shared borrow (no `&mut` to the parser is live).
                let p = &*parser;
                if p.error != sys::YAML_MEMORY_ERROR {
                    out = Some(SyntaxError {
                        line: p.context_mark.line as usize + 1,
                        column: p.context_mark.column as usize + 1,
                        problem: c_string(p.problem).unwrap_or_default(),
                        context: c_string(p.context),
                    });
                }
                break;
            }
            let kind = (*event.as_ptr()).type_;
            sys::yaml_event_delete(event.as_mut_ptr());
            if kind == sys::YAML_DOCUMENT_END_EVENT || kind == sys::YAML_STREAM_END_EVENT {
                break;
            }
        }
        sys::yaml_parser_delete(parser);
        out
    }
}

/// A libyaml error string, `None` for NULL (Psych's `nil`).
///
/// # Safety
/// `ptr` is NULL or a NUL-terminated string.
unsafe fn c_string(ptr: *const i8) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller's contract.
    Some(unsafe { CStr::from_ptr(ptr.cast()) }.to_string_lossy().into_owned())
}
