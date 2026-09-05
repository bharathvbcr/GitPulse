//! `gitpulse://` resource URIs: minting, parsing, and the boundary check.
//!
//! Two shapes, both RFC 3986:
//!
//! * `gitpulse://server/manifest` — a fixed document about this server. No
//!   repository, so nothing to validate beyond the name.
//! * `gitpulse://insights/Users/me/proj` — a facet of one repository. The
//!   authority component names the facet and the path is the repository's
//!   absolute path, so the RFC 6570 template is `gitpulse://insights{+repo_path}`.
//!
//! Putting the facet in the authority rather than the path is what makes the
//! template expressible: `{+repo_path}` expands with reserved characters intact,
//! so a client can build the URI by concatenation and the parser can split it
//! on the first `/` after the scheme without guessing where the path begins.
//!
//! [Resources](https://modelcontextprotocol.io/specification/2026-07-28/server/resources)
//! requires that servers *"validate all resource URIs"* and *"sanitize file
//! paths to prevent directory traversal attacks"*. [`parse`] is where that
//! happens, and it is the only way to turn a client string into a path this
//! server will read.

use std::fmt;

pub const SCHEME: &str = "gitpulse://";

/// What a well-formed `gitpulse://` URI addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A fixed server document: `gitpulse://server/<name>`.
    Server(String),
    /// One facet of one repository.
    Repo { facet: String, path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriError {
    WrongScheme,
    MissingFacet,
    MissingPath,
    NotAbsolute(String),
    Traversal(String),
    InteriorNul,
    BadPercentEscape(String),
}

impl fmt::Display for UriError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongScheme => write!(f, "not a {SCHEME} URI"),
            Self::MissingFacet => write!(f, "URI names no facet"),
            Self::MissingPath => write!(f, "URI names a facet but no repository path"),
            Self::NotAbsolute(p) => {
                write!(f, "repository path must be absolute, got {p:?}")
            }
            Self::Traversal(p) => write!(
                f,
                "repository path must not contain a `..` component, got {p:?}"
            ),
            Self::InteriorNul => write!(f, "repository path contains a NUL byte"),
            Self::BadPercentEscape(s) => write!(f, "malformed percent-escape in {s:?}"),
        }
    }
}

/// Percent-encode everything outside the RFC 3986 unreserved set plus `/`.
///
/// `/` is kept literal because it is the path separator we are expressing, not
/// data inside a segment; everything else — spaces, `#`, `?`, `%`, non-ASCII —
/// is escaped so the URI survives concatenation and re-parsing.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn decode_path(encoded: &str) -> Result<String, UriError> {
    let bytes = encoded.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = encoded
                .get(i + 1..i + 3)
                .ok_or_else(|| UriError::BadPercentEscape(encoded.to_string()))?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|_| UriError::BadPercentEscape(encoded.to_string()))?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // A percent-escape can encode any byte, so the decoded result is not
    // guaranteed to be UTF-8 and must be checked rather than assumed.
    String::from_utf8(out).map_err(|_| UriError::BadPercentEscape(encoded.to_string()))
}

/// Build the URI for one facet of one repository.
pub fn repo_uri(facet: &str, repo_path: &str) -> String {
    format!("{SCHEME}{facet}{}", encode_path(&to_uri_path(repo_path)))
}

/// A filesystem path as it appears in the URI's path component.
///
/// POSIX paths already start with `/`. A Windows path (`C:\proj`) does not, so
/// one is prepended and the separators normalised — otherwise `C:/proj` would
/// parse as authority `C:` and the drive letter would be read as the facet.
fn to_uri_path(repo_path: &str) -> String {
    let slashed = repo_path.replace('\\', "/");
    if slashed.starts_with('/') {
        slashed
    } else {
        format!("/{slashed}")
    }
}

/// The inverse of [`to_uri_path`]: `/C:/proj` is a Windows path, `/home/me` is not.
fn from_uri_path(uri_path: &str) -> String {
    let bytes = uri_path.as_bytes();
    let is_drive = bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'/');
    if is_drive {
        uri_path[1..].to_string()
    } else {
        uri_path.to_string()
    }
}

/// Parse and validate a client-supplied URI.
///
/// This is the boundary: a path that comes back from here is absolute, free of
/// `..` and NUL, and decoded. Whether it is a *repository* is a separate
/// question the caller answers by asking git, because "syntactically safe" and
/// "exists as a repo" are different failures and a caller has to be able to say
/// which one it hit.
pub fn parse(uri: &str) -> Result<Target, UriError> {
    let rest = uri.strip_prefix(SCHEME).ok_or(UriError::WrongScheme)?;
    let (facet, encoded_path) = match rest.split_once('/') {
        Some((facet, path)) => (facet, path),
        None => (rest, ""),
    };
    if facet.is_empty() {
        return Err(UriError::MissingFacet);
    }

    if facet == "server" {
        if encoded_path.is_empty() {
            return Err(UriError::MissingPath);
        }
        return Ok(Target::Server(decode_path(encoded_path)?));
    }

    if encoded_path.is_empty() {
        return Err(UriError::MissingPath);
    }
    // The split above consumed the separator that makes the path absolute.
    let decoded = decode_path(&format!("/{encoded_path}"))?;

    if decoded.contains('\0') {
        return Err(UriError::InteriorNul);
    }
    // Component-wise, not substring: a directory legitimately named `..config`
    // contains ".." and is not traversal. Both separators are checked because a
    // Windows path arrives here still carrying backslashes if a client built the
    // URI by hand rather than through `repo_uri`.
    if decoded
        .split(['/', '\\'])
        .any(|component| component == ".." || component == ".")
    {
        return Err(UriError::Traversal(decoded));
    }

    let path = from_uri_path(&decoded);
    let absolute = path.starts_with('/') || is_windows_absolute(&path);
    if !absolute {
        return Err(UriError::NotAbsolute(path));
    }

    Ok(Target::Repo {
        facet: facet.to_string(),
        path,
    })
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posix_repo_uri_round_trips() {
        let uri = repo_uri("insights", "/Users/me/proj");
        assert_eq!(uri, "gitpulse://insights/Users/me/proj");
        assert_eq!(
            parse(&uri).unwrap(),
            Target::Repo {
                facet: "insights".into(),
                path: "/Users/me/proj".into()
            }
        );
    }

    #[test]
    fn a_windows_repo_uri_round_trips_without_eating_the_drive_letter() {
        // `C:/proj` without the leading slash would parse as authority `C:`,
        // making the drive letter the facet and losing the path entirely.
        let uri = repo_uri("status", "C:\\Users\\me\\proj");
        assert_eq!(uri, "gitpulse://status/C%3A/Users/me/proj");
        assert_eq!(
            parse(&uri).unwrap(),
            Target::Repo {
                facet: "status".into(),
                path: "C:/Users/me/proj".into()
            }
        );
    }

    #[test]
    fn spaces_and_non_ascii_survive_the_round_trip() {
        for path in [
            "/Users/me/my project",
            "/Users/me/projet-café",
            "/Users/me/a#b?c",
            "/Users/me/100%",
        ] {
            let uri = repo_uri("insights", path);
            assert!(!uri.contains(' '), "{uri} still has a raw space");
            match parse(&uri).unwrap() {
                Target::Repo { path: back, .. } => assert_eq!(back, path),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn traversal_is_refused_in_both_raw_and_encoded_form() {
        assert_eq!(
            parse("gitpulse://insights/Users/me/../../etc/passwd"),
            Err(UriError::Traversal("/Users/me/../../etc/passwd".into()))
        );
        // Percent-encoding the dots is the version a naive substring check misses.
        assert_eq!(
            parse("gitpulse://insights/Users/me/%2E%2E/%2E%2E/etc"),
            Err(UriError::Traversal("/Users/me/../../etc".into()))
        );
        // Backslash separators, as a hand-built Windows URI would carry.
        assert!(matches!(
            parse("gitpulse://insights/C%3A/proj%5C..%5Cwindows"),
            Err(UriError::Traversal(_))
        ));
    }

    #[test]
    fn a_single_dot_component_is_refused_too() {
        // `/a/./b` resolves to `/a/b`, so accepting it would let one repository
        // be addressed by unboundedly many URIs — and a cache key is a URI.
        assert!(matches!(
            parse("gitpulse://insights/Users/./me"),
            Err(UriError::Traversal(_))
        ));
    }

    #[test]
    fn a_dotfile_prefix_is_not_traversal() {
        // `..config` contains ".." and is a perfectly ordinary directory name.
        match parse("gitpulse://insights/Users/me/..config/proj").unwrap() {
            Target::Repo { path, .. } => assert_eq!(path, "/Users/me/..config/proj"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_embedded_nul_is_refused() {
        assert_eq!(
            parse("gitpulse://insights/Users/me%00/etc"),
            Err(UriError::InteriorNul)
        );
    }

    #[test]
    fn server_documents_parse_without_a_repository() {
        assert_eq!(
            parse("gitpulse://server/manifest").unwrap(),
            Target::Server("manifest".into())
        );
    }

    #[test]
    fn malformed_uris_are_named_rather_than_guessed_at() {
        assert_eq!(parse("file:///etc/passwd"), Err(UriError::WrongScheme));
        assert_eq!(parse("gitpulse://"), Err(UriError::MissingFacet));
        assert_eq!(parse("gitpulse:///x"), Err(UriError::MissingFacet));
        assert_eq!(parse("gitpulse://insights"), Err(UriError::MissingPath));
        assert_eq!(parse("gitpulse://server"), Err(UriError::MissingPath));
        assert!(matches!(
            parse("gitpulse://insights/a%ZZb"),
            Err(UriError::BadPercentEscape(_))
        ));
        // A truncated escape at the very end must not index past the string.
        assert!(matches!(
            parse("gitpulse://insights/ab%"),
            Err(UriError::BadPercentEscape(_))
        ));
        assert!(matches!(
            parse("gitpulse://insights/ab%4"),
            Err(UriError::BadPercentEscape(_))
        ));
    }

    #[test]
    fn a_percent_escape_that_decodes_to_invalid_utf8_is_refused() {
        assert!(matches!(
            parse("gitpulse://insights/Users/%FF%FE"),
            Err(UriError::BadPercentEscape(_))
        ));
    }

    #[test]
    fn a_bare_drive_letter_is_not_an_absolute_path() {
        // `/C:` is the one input that survives decoding, is recognised as a
        // drive prefix, and still names nothing to open.
        assert_eq!(
            parse("gitpulse://insights/C%3A"),
            Err(UriError::NotAbsolute("C:".into()))
        );
    }

    #[test]
    fn a_directory_whose_name_contains_a_colon_is_still_a_posix_path() {
        // `/C:proj` is a directory named `C:proj` under root, not a drive
        // reference — the drive test requires the separator right after the
        // colon, and getting that wrong would strip a leading slash off a
        // legitimate absolute path.
        match parse("gitpulse://insights/C%3Aproj").unwrap() {
            Target::Repo { path, .. } => assert_eq!(path, "/C:proj"),
            other => panic!("{other:?}"),
        }
    }
}
