//! Opaque cursor pagination for the four list operations that support it.
//!
//! [Pagination](https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/pagination):
//! the cursor is an opaque string, page size is the server's choice, a client
//! **MUST NOT** parse the cursor, and an invalid cursor **SHOULD** be `-32602`.
//!
//! Two things make the cursor more than a base64 integer. It carries the name
//! of the list it came from, so a `tools/list` cursor replayed against
//! `resources/list` is refused instead of silently indexing into the wrong
//! collection; and it carries the length of the list it was minted against, so
//! a cursor that outlived a catalog change is refused rather than skipping or
//! repeating entries. Both are cases where the naive implementation returns a
//! plausible page and no error.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::Value;

/// Entries per page. Small enough that a page is never the thing that makes a
/// response large, large enough that this server's catalogs are one page.
pub const PAGE_SIZE: usize = 100;

/// Why a cursor was refused. Carried out to the caller so the `-32602` says
/// which of the three it was rather than "invalid cursor".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorError {
    Malformed,
    WrongList { expected: &'static str, got: String },
    Stale { minted_for: usize, now: usize },
}

impl CursorError {
    pub fn message(&self) -> String {
        match self {
            Self::Malformed => "invalid cursor: not a cursor this server issued".to_string(),
            Self::WrongList { expected, got } => format!(
                "invalid cursor: issued for {got}, replayed against {expected}; \
                 restart the listing without a cursor"
            ),
            Self::Stale { minted_for, now } => format!(
                "invalid cursor: the list changed since it was issued \
                 ({minted_for} entries then, {now} now); restart the listing without a cursor"
            ),
        }
    }
}

fn encode(list: &str, offset: usize, total: usize) -> String {
    URL_SAFE_NO_PAD.encode(format!("{list}:{offset}:{total}"))
}

/// Resolve `cursor` to a start offset into a `total`-entry list named `list`.
///
/// `None` is the first page — a client is free to start without one.
pub fn offset(
    cursor: Option<&str>,
    list: &'static str,
    total: usize,
) -> Result<usize, CursorError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    // An empty string is a valid cursor value per the spec ("MUST NOT be
    // treated as the end of results"), so it reaches the decoder like any
    // other and is refused there as one we did not issue.
    let decoded = URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or(CursorError::Malformed)?;
    let mut parts = decoded.rsplitn(3, ':');
    let minted_total: usize = parts
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or(CursorError::Malformed)?;
    let offset: usize = parts
        .next()
        .and_then(|o| o.parse().ok())
        .ok_or(CursorError::Malformed)?;
    // `rsplitn(3, ..)` puts everything before the last two separators here, so
    // a list name containing a colon survives the round trip intact.
    let minted_list = parts.next().ok_or(CursorError::Malformed)?;

    if minted_list != list {
        return Err(CursorError::WrongList {
            expected: list,
            got: minted_list.to_string(),
        });
    }
    if minted_total != total {
        return Err(CursorError::Stale {
            minted_for: minted_total,
            now: total,
        });
    }
    if offset > total {
        return Err(CursorError::Malformed);
    }
    Ok(offset)
}

/// One page of `items`, plus the cursor for the next one when more remain.
#[derive(Debug)]
pub struct Page {
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
}

/// Slice `items` at `cursor` and mint the follow-on cursor.
///
/// Takes the whole list by value because every catalog in this server is built
/// in full on each call; there is nothing to stream.
pub fn slice(
    items: Vec<Value>,
    cursor: Option<&str>,
    list: &'static str,
) -> Result<Page, CursorError> {
    let total = items.len();
    let start = offset(cursor, list, total)?;
    let end = start.saturating_add(PAGE_SIZE).min(total);
    let next_cursor = (end < total).then(|| encode(list, end, total));
    Ok(Page {
        items: items.into_iter().skip(start).take(end - start).collect(),
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn items(n: usize) -> Vec<Value> {
        (0..n).map(|i| json!({ "i": i })).collect()
    }

    #[test]
    fn a_short_list_is_one_page_with_no_next_cursor() {
        let page = slice(items(3), None, "tools").unwrap();
        assert_eq!(page.items.len(), 3);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn a_list_exactly_one_page_long_does_not_advertise_another() {
        // The off-by-one that hands a client a cursor to an empty page.
        let page = slice(items(PAGE_SIZE), None, "tools").unwrap();
        assert_eq!(page.items.len(), PAGE_SIZE);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn paging_visits_every_entry_exactly_once() {
        let total = PAGE_SIZE * 2 + 7;
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = slice(items(total), cursor.as_deref(), "tools").unwrap();
            pages += 1;
            seen.extend(page.items.iter().map(|v| v["i"].as_u64().unwrap()));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
            assert!(pages < 10, "pagination did not terminate");
        }
        assert_eq!(pages, 3);
        assert_eq!(seen, (0..total as u64).collect::<Vec<_>>());
    }

    #[test]
    fn a_cursor_from_another_list_is_refused() {
        let first = slice(items(PAGE_SIZE + 1), None, "resources").unwrap();
        let cursor = first.next_cursor.unwrap();
        let error = slice(items(PAGE_SIZE + 1), Some(&cursor), "tools").unwrap_err();
        assert_eq!(
            error,
            CursorError::WrongList {
                expected: "tools",
                got: "resources".into()
            }
        );
    }

    #[test]
    fn a_cursor_that_outlived_its_list_is_refused_not_silently_reindexed() {
        let cursor = slice(items(PAGE_SIZE + 5), None, "tools")
            .unwrap()
            .next_cursor
            .unwrap();
        let error = slice(items(PAGE_SIZE + 9), Some(&cursor), "tools").unwrap_err();
        assert_eq!(
            error,
            CursorError::Stale {
                minted_for: PAGE_SIZE + 5,
                now: PAGE_SIZE + 9
            }
        );
    }

    #[test]
    fn garbage_and_empty_cursors_are_malformed_not_page_one() {
        for bad in ["", "!!!!", "Zm9v", "dG9vbHM6", "dG9vbHM6eDo1"] {
            assert_eq!(
                slice(items(5), Some(bad), "tools").unwrap_err(),
                CursorError::Malformed,
                "{bad}"
            );
        }
    }

    #[test]
    fn a_list_name_containing_a_colon_round_trips() {
        let cursor = slice(items(PAGE_SIZE + 1), None, "resources:templates")
            .unwrap()
            .next_cursor
            .unwrap();
        let page = slice(items(PAGE_SIZE + 1), Some(&cursor), "resources:templates").unwrap();
        assert_eq!(page.items.len(), 1);
    }

    #[test]
    fn an_empty_list_pages_cleanly() {
        let page = slice(Vec::new(), None, "prompts").unwrap();
        assert!(page.items.is_empty());
        assert!(page.next_cursor.is_none());
    }
}
