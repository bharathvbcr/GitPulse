use super::EXPLORE_DEFINITION_OVERHEAD_TOKENS;
use crate::model::{
    AffectedTest, BlastLayer, ExploreBudget, ExploreDefinition, ResolutionAvailability, Response,
    SourceFreshness,
};
use devmap_extract::model::Span;
use devmap_store::Store;
use std::collections::{BTreeMap, BTreeSet};

/// Divide one caller-supplied budget across `explore`'s four parts.
///
/// Halves and quarters, computed before any work, so the split is deterministic
/// and reportable. `edges_per_direction` is filled in later — it cannot be
/// known until the packer has decided how many definitions there are to divide
/// the edge pool between.
pub(super) fn explore_budget(total: u32) -> ExploreBudget {
    let definitions = total / 2;
    let blast_radius = total / 4;
    ExploreBudget {
        total,
        definitions,
        edges_per_direction: 0,
        blast_radius,
    }
}

/// Split the edge pool evenly across every direction of every definition shown.
///
/// Deliberately not floored at one edge's worth: a floor would let a large
/// answer exceed the budget the caller set, and `DevMapClient._budgeted` treats
/// the budget as a hard contract. A direction that gets nothing still reports
/// `total` — "0 shown of 42 callers" is a complete answer to "how many", which
/// is the question the count exists for.
pub(super) fn edges_per_direction(budget: &ExploreBudget, shown: u32) -> u32 {
    let pool = budget
        .total
        .saturating_sub(budget.definitions)
        .saturating_sub(budget.blast_radius);
    let directions = shown.saturating_mul(2);
    if directions == 0 {
        return 0;
    }
    pool / directions
}

/// Token cost of one packed definition: its source span plus a fixed overhead,
/// charged at the same [`BYTES_PER_TOKEN`] every other surface uses.
pub(super) fn explore_definition_tokens(definition: &ExploreDefinition) -> u32 {
    u32::try_from(definition.source.len() / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(EXPLORE_DEFINITION_OVERHEAD_TOKENS)
}

/// Token cost of one blast-radius band: its listed node ids plus a small header.
pub(super) fn blast_layer_tokens(layer: &BlastLayer) -> u32 {
    let bytes: usize = layer.nodes.iter().map(|node| node.len() + 1).sum();
    u32::try_from(bytes / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(10)
}

/// Token cost of one affected-test row: path, listed symbols, and a header.
pub(super) fn affected_test_tokens(test: &AffectedTest) -> u32 {
    let bytes: usize = test.path.len()
        + test
            .symbols
            .iter()
            .map(|symbol| symbol.len() + 1)
            .sum::<usize>();
    u32::try_from(bytes / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(10)
}

/// Whether `path` names a test file.
///
/// Deliberately stricter than the Python predicate it replaces, which asked
/// `"/test" in "/" + path` and so counted `src/testing_utils.py`,
/// `lib/latest/mod.rs` and any path containing the substring anywhere as tests.
/// Here a directory must be *exactly* a test directory, and a file must carry a
/// recognised test affix. Recorded in `DIVERGENCES.md`: the affected-test list
/// gets shorter and the entries that leave were never tests.
pub fn is_test_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let Some((directories, file_name)) = normalized.rsplit_once('/') else {
        return is_test_file_name(&normalized);
    };
    let in_test_directory = directories.split('/').any(|segment| {
        matches!(
            segment.to_lowercase().as_str(),
            "test" | "tests" | "spec" | "specs" | "__tests__" | "testing" | "e2e"
        )
    });
    in_test_directory || is_test_file_name(file_name)
}

/// Whether a bare file name carries a test affix.
///
/// Affixes only, never bare substrings, and the boundary is the point: `test`
/// as a suffix of a *word* (`contest`, `attestation`, `latest`) is not a test
/// affix, and `testing_utils.py` is not `test_utils.py`. The camel-case arm
/// reads the original casing, so `FooTest.java` is recognised while `contest`
/// is not — which is why the name is not lower-cased wholesale first.
pub(super) fn is_test_file_name(file_name: &str) -> bool {
    let lowered = file_name.to_lowercase();
    let stem = file_name.split('.').next().unwrap_or(file_name);
    let lowered_stem = lowered.split('.').next().unwrap_or(&lowered);
    lowered.starts_with("test_")
        || lowered.starts_with("spec_")
        || lowered.contains(".test.")
        || lowered.contains(".spec.")
        || lowered.contains("_test.")
        || lowered.contains("_spec.")
        || lowered.contains("-test.")
        || lowered.contains("-spec.")
        || matches!(lowered_stem, "test" | "tests" | "spec" | "specs")
        || stem.ends_with("Test")
        || stem.ends_with("Tests")
        || stem.ends_with("Spec")
        || stem.ends_with("Specs")
}

/// Fold one reached symbol into the nearest-test table.
///
/// A free function rather than a closure so the table can be read back in the
/// same scope it is built in.
pub(super) fn record_test_hit(
    nearest: &mut BTreeMap<String, (usize, BTreeSet<String>)>,
    test_symbols: &std::collections::HashSet<String>,
    symbol: &str,
    file: &str,
    depth: usize,
) {
    if !is_test_path(file) && !test_symbols.contains(symbol) {
        return;
    }
    let entry = nearest
        .entry(file.to_string())
        .or_insert((depth, BTreeSet::new()));
    entry.0 = entry.0.min(depth);
    entry.1.insert(symbol.to_string());
}

pub(super) fn unavailable_response<T>(resolution: ResolutionAvailability) -> Response<T> {
    Response {
        source_freshness: SourceFreshness::unverified(
            "no persisted generation is available to verify against the working tree",
        ),
        items: Vec::new(),
        shown: 0,
        hidden: 0,
        total: 0,
        truncated: false,
        tokens_used: 0,
        resolution,
        walk_incomplete: None,
        rungs: None,
        dead_clusters: None,
        dead_clusters_truncated: 0,
        dead_clusters_incomplete: None,
        unresolved_namesakes: None,
        scope: None,
    }
}

pub(super) fn attach_source_freshness<T>(store: &Store, mut response: Response<T>) -> Response<T> {
    response.source_freshness = SourceFreshness::from_store(store.query_source_freshness());
    response
}

pub(crate) fn byte_span_to_line_range(source: &str, span: &Span) -> (u32, u32) {
    byte_span_to_line_range_in(&devmap_extract::model::LineIndex::new(source), span)
}

/// [`byte_span_to_line_range`] over a table built once per file, for a loop
/// that converts every span in the file — the string form scans from the top
/// of the file on every call, which the artifact's node loop paid twice per
/// symbol.
pub(crate) fn byte_span_to_line_range_in(
    lines: &devmap_extract::model::LineIndex,
    span: &Span,
) -> (u32, u32) {
    // Delegates the counting to `Span::line_range`, which is the canonical
    // owner and is already UTF-8-safe.
    //
    // This function used to count newlines itself with `source[..start]` —
    // slicing a `&str`, which **panics** on an index that is not a character
    // boundary. Spans are byte offsets recorded at extraction time while
    // `source` is re-read from disk when the graph is exported, so any
    // multi-byte character inserted before an indexed symbol's end offset put
    // the offset mid-character: one emoji added to a file aborted
    // `dev map manifest` outright, and the release profile is `panic = "abort"`,
    // so there was no recovery.
    //
    // Two copies of one computation existed and only one was safe. The wrapper
    // survives for the single thing it adds beyond the canonical version — the
    // `max(start)` below — and no longer restates the arithmetic.
    let len = lines.len();
    let clamped = Span {
        start_byte: span.start_byte.min(len),
        // A stored span whose end precedes its start would otherwise report an
        // end line above its start line. Clamping keeps the range orderable for
        // the consumers that render it as `line..end_line`.
        end_byte: span.end_byte.min(len).max(span.start_byte.min(len)),
    };
    lines.line_range(&clamped)
}

/// Per-hit token overhead in [`StoreQueryEngine::search`]'s cost function.
/// Kept next to [`cap_source_span`] because the cap must invert the same
/// arithmetic the packer uses, and a drift between the two reintroduces the
/// oversized-hit bug in a form no test names.
pub(crate) const SEARCH_HIT_OVERHEAD_TOKENS: u32 = 20;

/// Bytes of source per token, matching the `len / 4` estimate in the search
/// cost function.
pub const BYTES_PER_TOKEN: u32 = 4;

/// How many ranked candidates a budget could conceivably show.
///
/// A hit costs at least [`SEARCH_HIT_OVERHEAD_TOKENS`], so `budget / overhead`
/// bounds how many can fit; the `+ 1` keeps the page from cutting a hit the
/// packer would still have admitted, and the floor of 1 keeps a zero budget
/// from asking for an empty page and reporting "nothing matched".
///
/// Both search paths use this as the ceiling on *materialised* hits — the ones
/// whose source is read off disk. Keyword search draws its candidates from a
/// wider page ([`search_rank_pool_size`]) and cuts to this after ranking;
/// semantic search bounds how far down its ranking it materialises hits before
/// budgeting them. Two copies of the arithmetic would let the same budget mean
/// different page sizes depending on which command asked.
pub(super) fn budget_page_size(token_budget: u32) -> usize {
    (token_budget / SEARCH_HIT_OVERHEAD_TOKENS)
        .saturating_add(1)
        .max(1) as usize
}

/// Hard ceiling on hits one `search` may materialise, whatever the budget asks.
///
/// K-B1: every hit on a search page is a file opened and read, and the page was
/// `token_budget / 20 + 1` — 5,001 rows at a 100,000-token budget. The token
/// budget is a *presentation* limit the caller chooses; letting it set the
/// number of files opened turns "give me a generous budget" into "open five
/// thousand files". 200 is far past any page a reader consumes and far below
/// anything that reads a corpus.
///
/// It applies to `search` and not to `explore`, and the difference is where the
/// I/O is: `explore` scores stored rows and opens a file only for the
/// definitions that survive its own `limit` —
/// `explore_reads_one_file_per_definition_it_returns_not_per_candidate` pins
/// that — so capping its candidate page would cost ranking quality on a
/// high-match query and buy no bounded-ness at all.
///
/// `search_semantic` and `ask` (and so the evidence pack) materialise their
/// pages the same way — one verified file read per hit — and share the cap;
/// `ask_and_semantic_search_share_the_page_ceiling` pins all three.
///
/// The cap trims the page, never the count: `total` is still measured over the
/// whole index and a trimmed page still reports `truncated` and `hidden`.
pub const SEARCH_PAGE_MAX: usize = 200;

/// Hits `search` will materialise for this budget: what it can show, capped at
/// what it is allowed to open. See [`SEARCH_PAGE_MAX`].
pub(super) fn search_page_size(token_budget: u32) -> usize {
    budget_page_size(token_budget).min(SEARCH_PAGE_MAX)
}

/// How many candidates keyword search pulls from the store before ranking them.
///
/// The store orders its page by bm25 and this crate ranks by exact/prefix/other
/// match, so the page has to be wider than the answer or the second ranking
/// only ever sees what the first one liked. Ten times is comfortably past the
/// gap the audit measured (an exact match 100 rows below the cut on a 200-match
/// query) without being a licence to walk the corpus: the pool is a hard
/// ceiling, and when the match set outruns it `search` says so on
/// `walk_incomplete` rather than presenting a sample's best as the corpus's.
pub(super) const SEARCH_RANK_OVERSAMPLE: usize = 10;

/// Hard ceiling on that pool, whatever the budget asks for.
///
/// Every pooled row costs a `String` comparison and no file read, so 2,000 is
/// cheap; it is here so one query can never scan an unbounded number of FTS
/// rows on a corpus where the query matches everything.
pub(super) const SEARCH_RANK_POOL_MAX: usize = 2_000;

pub(super) fn search_rank_pool_size(token_budget: u32) -> usize {
    // The *capped* page: the floor below exists so the pool can never be
    // narrower than what will be shown, and taking it from the uncapped page
    // would let a large budget reopen the ceiling the cap just closed.
    let page = search_page_size(token_budget);
    // Never below the page: a pool smaller than what the budget could show
    // would drop results the caller has already paid for.
    page.saturating_mul(SEARCH_RANK_OVERSAMPLE)
        .min(SEARCH_RANK_POOL_MAX)
        .max(page)
}

pub fn budget_take<T, F>(items: Vec<T>, token_budget: u32, cost_of: F) -> Response<T>
where
    F: Fn(&T) -> u32,
{
    let total = items.len() as u32;
    let mut out = Vec::new();
    let mut current_tokens = 0u32;
    let mut truncated = false;

    // The budget is hard: an item that does not fit is withheld, never emitted
    // over budget. `test_search_never_exceeds_hard_token_budget` pins this, and
    // `DevMapClient._budgeted` raises on any response that breaks it, so a
    // packer that "made progress" by exceeding the budget would turn a thin
    // result into a client-side error.
    //
    // Which is why an oversized *item* is bounded where it is built rather than
    // waved through here — see `cap_source_span`.
    for item in items {
        let cost = cost_of(&item);
        if cost > token_budget.saturating_sub(current_tokens) {
            truncated = true;
            break;
        }
        current_tokens += cost;
        out.push(item);
    }

    Response {
        source_freshness: crate::model::SourceFreshness::unverified(
            "whole-tree source freshness was not checked for this answer",
        ),
        shown: out.len() as u32,
        hidden: total.saturating_sub(out.len() as u32),
        total,
        truncated,
        tokens_used: current_tokens,
        items: out,
        resolution: ResolutionAvailability::Available,
        walk_incomplete: None,
        rungs: None,
        dead_clusters: None,
        dead_clusters_truncated: 0,
        dead_clusters_incomplete: None,
        unresolved_namesakes: None,
        scope: None,
    }
}

pub(super) fn atomic_budget_take<T, F>(items: Vec<T>, token_budget: u32, cost_of: F) -> Response<T>
where
    F: Fn(&T) -> u32,
{
    let total = u32::try_from(items.len()).unwrap_or(u32::MAX);
    let required = items
        .iter()
        .fold(0u32, |sum, item| sum.saturating_add(cost_of(item)));
    if required > token_budget {
        return Response {
            source_freshness: crate::model::SourceFreshness::unverified(
                "whole-tree source freshness was not checked for this answer",
            ),
            items: Vec::new(),
            shown: 0,
            hidden: total,
            total,
            truncated: total > 0,
            tokens_used: 0,
            resolution: ResolutionAvailability::Available,
            walk_incomplete: None,
            rungs: None,
            dead_clusters: None,
            dead_clusters_truncated: 0,
            dead_clusters_incomplete: None,
            unresolved_namesakes: None,
            scope: None,
        };
    }
    Response {
        source_freshness: crate::model::SourceFreshness::unverified(
            "whole-tree source freshness was not checked for this answer",
        ),
        shown: total,
        hidden: 0,
        total,
        truncated: false,
        tokens_used: required,
        items,
        resolution: ResolutionAvailability::Available,
        walk_incomplete: None,
        rungs: None,
        dead_clusters: None,
        dead_clusters_truncated: 0,
        dead_clusters_incomplete: None,
        unresolved_namesakes: None,
        scope: None,
    }
}
