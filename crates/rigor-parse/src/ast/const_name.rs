//! [`ConstName`] — a `ConstantRead` name that shares one allocation across a
//! constant-path chain (rigor-rs#657).

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

/// The dotted name of a `Node::ConstantRead`, rendered leniently
/// (`constant_path_string`): `::Foo` reads as `"Foo"` and a dynamic base
/// contributes nothing (`k::LIMIT` reads as `"LIMIT"`).
///
/// Every level of one `ConstantPathNode` chain renders to a byte-PREFIX of the
/// outermost level's name (`A`, `A::B`, `A::B::C`), so the lowering stores the
/// full name once behind an [`Arc`] and each node keeps only its own prefix
/// length: a depth-n `A::A::…` chain costs O(n) bytes of name storage instead
/// of the O(n²) a per-level `String` paid (197 MB at 10k, ~750 MB at 20k —
/// rigor-rs#657).
///
/// The type derefs to `&str`, so the ~45 consumer files read `name` exactly as
/// they read the `String` it replaces; [`Clone`] is an `Arc` bump, not a copy.
#[derive(Clone)]
pub struct ConstName {
    /// The OUTERMOST level's full name; this level renders its first `len`
    /// bytes. Every byte length is a segment boundary, so the slice is always
    /// valid UTF-8.
    full: Arc<str>,
    /// Byte length of THIS level's prefix of `full` (`<= full.len()`).
    len: usize,
}

impl ConstName {
    /// A shared-prefix name: `full` is the chain's outermost name, `len` the
    /// byte length of this level's prefix. `len` must be a `::`-segment
    /// boundary of `full` (or `full.len()`); anything else panics, because a
    /// mid-segment cut would break the deref's UTF-8 contract.
    pub fn prefix_of(full: &Arc<str>, len: usize) -> Self {
        debug_assert!(
            len <= full.len() && full.is_char_boundary(len),
            "ConstName prefix {len} is not a boundary of {full:?}"
        );
        Self {
            full: full.clone(),
            len,
        }
    }

    /// The rendered name of this level.
    pub fn as_str(&self) -> &str {
        &self.full[..self.len]
    }
}

impl From<String> for ConstName {
    fn from(s: String) -> Self {
        Self {
            len: s.len(),
            full: Arc::from(s),
        }
    }
}

impl From<&str> for ConstName {
    fn from(s: &str) -> Self {
        Self::from(s.to_string())
    }
}

impl Deref for ConstName {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for ConstName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for ConstName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for ConstName {
    /// Same rendering the `String` it replaces produced — quoted name only —
    /// so `Node` debug output is unchanged.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for ConstName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq for ConstName {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}
impl Eq for ConstName {}

impl PartialEq<str> for ConstName {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<&str> for ConstName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl PartialEq<String> for ConstName {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<ConstName> for str {
    fn eq(&self, other: &ConstName) -> bool {
        self == other.as_str()
    }
}
impl PartialEq<ConstName> for &str {
    fn eq(&self, other: &ConstName) -> bool {
        *self == other.as_str()
    }
}
impl PartialEq<ConstName> for String {
    fn eq(&self, other: &ConstName) -> bool {
        self == other.as_str()
    }
}

impl PartialOrd for ConstName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ConstName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for ConstName {
    /// Hashes as the rendered `str` — matching `Borrow<str>`, so a
    /// `HashMap<ConstName, _>` looks up correctly by `&str`.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}
