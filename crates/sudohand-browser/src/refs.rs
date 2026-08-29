//! The `ref` grammar — one definition, one parser. **No CDP types here.**
//!
//! A `ref` is how this library names an element across a round trip:
//!
//! ```text
//! "5#214"                 index 5 in the snapshot, backend node id 214
//! "FRAME_ABC123:5#214"    the same, inside the scope whose id starts ABC123
//! "5"                     legacy — index only, no node id
//! ```
//!
//! The scope prefix is a *generic* concept: `<KIND>_<id>` where `KIND` is
//! upper-case ASCII. `FRAME` is the only kind the browser surface mints
//! today; a desktop surface can mint `APP_…` / `WINDOW_…` with the same
//! parser. Output is byte-for-byte identical to the Python `_ref.py`.

/// Scope kind minted for iframe content.
pub const FRAME_SCOPE: &str = "FRAME";

/// A parsed ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRef {
    /// Full scope prefix (`FRAME_ABC123`), if any.
    pub scope: Option<ScopePrefix>,
    /// The local part with any `#node` suffix removed (`"5"`).
    pub local: String,
    /// Backend node id, if the ref carries one.
    pub backend_node_id: Option<i64>,
}

/// `<KIND>_<id>` scope prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePrefix {
    /// Upper-case kind, e.g. `FRAME`.
    pub kind: String,
    /// Opaque id after the underscore, e.g. the first 8 chars of a frame id.
    pub id: String,
}

impl ScopePrefix {
    /// Build a prefix for `kind` + `id`.
    #[must_use]
    pub fn new(kind: &str, id: &str) -> Self {
        Self {
            kind: kind.to_string(),
            id: id.to_string(),
        }
    }

    /// `FRAME_ABC123` form.
    #[must_use]
    pub fn to_prefix_string(&self) -> String {
        format!("{}_{}", self.kind, self.id)
    }
}

impl std::fmt::Display for ScopePrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}_{}", self.kind, self.id)
    }
}

/// Mint a ref. The inverse of [`parse_ref`].
#[must_use]
pub fn make_ref(index: usize, backend_node_id: Option<i64>) -> String {
    match backend_node_id {
        Some(id) => format!("{index}#{id}"),
        None => index.to_string(),
    }
}

/// Prefix a local ref with a scope: `FRAME_ABC123:5#214`.
#[must_use]
pub fn scoped_ref(scope: &ScopePrefix, local_ref: &str) -> String {
    format!("{scope}:{local_ref}")
}

/// The scope prefix minted for a frame id (first 8 chars), matching Python's
/// `f"FRAME_{frame_id[:8]}"`.
#[must_use]
pub fn frame_scope(frame_id: &str) -> ScopePrefix {
    ScopePrefix::new(FRAME_SCOPE, &frame_id.chars().take(8).collect::<String>())
}

/// Try to read a `<KIND>_<id>:` scope prefix off the front of `s`.
///
/// Mirrors the Python regex `^(FRAME_[^:]+):(.+)$` but with any upper-case
/// kind. Returns `(prefix, rest)`.
fn split_scope(s: &str) -> Option<(ScopePrefix, &str)> {
    let (prefix, rest) = s.split_once(':')?;
    if rest.is_empty() {
        return None;
    }
    let (kind, id) = prefix.split_once('_')?;
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        || !kind.as_bytes()[0].is_ascii_uppercase()
        || id.is_empty()
    {
        return None;
    }
    Some((ScopePrefix::new(kind, id), rest))
}

/// Split a ref into (scope, local ref, backend node id).
///
/// ```
/// use sudohand_browser::refs::parse_ref;
/// let r = parse_ref("FRAME_ABC123:9#214");
/// assert_eq!(r.scope.unwrap().to_string(), "FRAME_ABC123");
/// assert_eq!(r.local, "9");
/// assert_eq!(r.backend_node_id, Some(214));
/// ```
#[must_use]
pub fn parse_ref(r: &str) -> ParsedRef {
    let (scope, local) = match split_scope(r) {
        Some((scope, rest)) => (Some(scope), rest),
        None => (None, r),
    };
    // `^(\d+)#(\d+)$`
    let node = local.split_once('#').and_then(|(idx, id)| {
        let ok = !idx.is_empty()
            && !id.is_empty()
            && idx.bytes().all(|b| b.is_ascii_digit())
            && id.bytes().all(|b| b.is_ascii_digit());
        if ok {
            id.parse::<i64>().ok().map(|n| (idx.to_string(), n))
        } else {
            None
        }
    });
    match node {
        Some((idx, id)) => ParsedRef {
            scope,
            local: idx,
            backend_node_id: Some(id),
        },
        None => ParsedRef {
            scope,
            local: local.to_string(),
            backend_node_id: None,
        },
    }
}

/// The backend node id a ref points at, or `None` for a legacy index-only ref.
#[must_use]
pub fn node_id_of(r: &str) -> Option<i64> {
    parse_ref(r).backend_node_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_matches_python() {
        assert_eq!(make_ref(5, Some(214)), "5#214");
        assert_eq!(make_ref(5, None), "5");
    }

    #[test]
    fn parse_plain() {
        let r = parse_ref("9#214");
        assert_eq!(r.scope, None);
        assert_eq!(r.local, "9");
        assert_eq!(r.backend_node_id, Some(214));
    }

    #[test]
    fn parse_legacy() {
        let r = parse_ref("9");
        assert_eq!(r.scope, None);
        assert_eq!(r.local, "9");
        assert_eq!(r.backend_node_id, None);
    }

    #[test]
    fn parse_frame_scoped() {
        let r = parse_ref("FRAME_ABC123:9#214");
        assert_eq!(r.scope.as_ref().unwrap().kind, "FRAME");
        assert_eq!(r.scope.as_ref().unwrap().id, "ABC123");
        assert_eq!(r.local, "9");
        assert_eq!(r.backend_node_id, Some(214));
        assert_eq!(node_id_of("FRAME_ABC123:9#214"), Some(214));
    }

    #[test]
    fn parse_generic_scope_kind() {
        let r = parse_ref("WINDOW_42:3#7");
        assert_eq!(r.scope.as_ref().unwrap().kind, "WINDOW");
        assert_eq!(r.scope.as_ref().unwrap().id, "42");
        assert_eq!(r.backend_node_id, Some(7));
    }

    #[test]
    fn garbage_is_kept_as_local() {
        // Python: no regex match → local_ref is the whole string, node id None.
        let r = parse_ref("abc");
        assert_eq!(r.local, "abc");
        assert_eq!(r.backend_node_id, None);
        let r = parse_ref("frame_x:1#2");
        assert_eq!(r.scope, None, "lower-case kind is not a scope");
        assert_eq!(r.local, "frame_x:1#2");
        let r = parse_ref("5#");
        assert_eq!(r.local, "5#");
        assert_eq!(r.backend_node_id, None);
    }

    #[test]
    fn roundtrip() {
        let scope = frame_scope("ABCDEFGHIJKL");
        assert_eq!(scope.to_string(), "FRAME_ABCDEFGH");
        let r = scoped_ref(&scope, &make_ref(3, Some(99)));
        assert_eq!(r, "FRAME_ABCDEFGH:3#99");
        let p = parse_ref(&r);
        assert_eq!(p.scope, Some(scope));
        assert_eq!(p.local, "3");
        assert_eq!(p.backend_node_id, Some(99));
    }
}
