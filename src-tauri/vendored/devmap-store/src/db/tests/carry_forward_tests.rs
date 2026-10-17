use super::*;
use devmap_analyze::analyze;
use devmap_extract::extract_file;
use devmap_extract::model::Extraction;
use devmap_resolve::Resolver;
use rusqlite::params;

/// Every stored column of one `generation_files` row, in schema order.
type FileRow = (
    i64,
    String,
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn read_row(store: &Store, generation: u32, path: &str) -> FileRow {
    let conn = lock_conn(&store.conn).expect("connection");
    conn.query_row(
        "SELECT f.file_id, f.language, f.content_hash, f.parse_outcome_json,
                    f.engine_json, f.extraction_json, f.grammar_version, f.analyzer_version
             FROM generation_files f
             JOIN paths p ON p.id = f.file_id
             WHERE f.generation_id = ?1 AND p.path = ?2",
        params![generation, path],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        },
    )
    .unwrap_or_else(|error| panic!("no row for {path} in generation {generation}: {error}"))
}

fn commit(store: &Store, exts: &[Extraction], opts: GenerationWriteOpts) -> Result<u32> {
    let mut resolver = Resolver::new();
    resolver.index_extractions(exts);
    let resolution = resolver.resolve_all(exts).unwrap();
    let analysis = analyze(exts, &resolution);
    store.save_generation_with_opts(exts, &resolution, &analysis, opts)
}

fn tree(a_body: &str) -> Vec<Extraction> {
    vec![
        extract_file("a.py", a_body),
        extract_file("b.py", "def beta():\n    return 2\n"),
        extract_file("c.py", "def gamma():\n    return 3\n"),
    ]
}

/// A carried row is the stored row — every column, `file_id` included.
///
/// The carry-forward moved from a Rust loop that read each row (payload and
/// all) and wrote it back, to a single `INSERT … SELECT` inside SQLite. The
/// rows must be indistinguishable, so this compares them column by column
/// rather than trusting that the copy "looks right": a transposed column in
/// the nine-column insert list is exactly the kind of defect that leaves a
/// store readable and wrong.
///
/// `file_id` is asserted too. The old loop re-derived it from the path
/// string through `paths`; this carries the stored id, and the two must
/// agree or a carried row would point at a different file than the one it
/// was written for.
///
/// The identity columns are `Option`, but a carried row can never hold
/// `None` in them — `identity_matches` above requires `Some` on both — so
/// this pins that they survive, not that NULL is carryable.
#[test]
fn the_carried_row_is_the_stored_row() {
    let store = Store::open_in_memory().expect("store");
    let first = tree("def alpha():\n    return 1\n");
    assert_eq!(
        commit(&store, &first, GenerationWriteOpts::default()).expect("cold"),
        1
    );
    let before_b = read_row(&store, 1, "b.py");
    let before_c = read_row(&store, 1, "c.py");

    // Only a.py changes, so b.py and c.py are carried.
    let second = tree("def alpha():\n    return 11\n");
    let gen = commit(
        &store,
        &second,
        GenerationWriteOpts {
            affected_paths: vec!["a.py".into()],
            ..GenerationWriteOpts::default()
        },
    )
    .expect("incremental");
    assert_eq!(gen, 2);

    assert_eq!(
        before_b,
        read_row(&store, 2, "b.py"),
        "b.py carried verbatim"
    );
    assert_eq!(
        before_c,
        read_row(&store, 2, "c.py"),
        "c.py carried verbatim"
    );
    // ... and the file that did change is not carried: its payload moved.
    assert_ne!(
        read_row(&store, 1, "a.py").5,
        read_row(&store, 2, "a.py").5,
        "a.py was affected, so its payload must be the freshly extracted one"
    );
}

/// A row produced by a different extractor is replaced, never carried.
///
/// This is the invariant the identity gate exists for, and nothing in this
/// crate covered it: with the gate's result ignored, all 127 devmap-store
/// tests still passed. The comment above `current_hashes` records what that
/// costs in production — after two schema bumps a store still held 1,152
/// `extract-v23` rows under a `v25` binary, and the first changed build had
/// no way forward but deleting the database.
///
/// The stored identity is rewritten directly rather than by bumping a
/// version constant, because the point is to reproduce a store written by
/// *some* other extractor, not to pin which one.
#[test]
fn a_row_from_a_different_extractor_is_replaced_not_carried() {
    let store = Store::open_in_memory().expect("store");
    let first = tree("def alpha():\n    return 1\n");
    commit(&store, &first, GenerationWriteOpts::default()).expect("cold");
    let current_grammar = read_row(&store, 1, "b.py").6;

    // b.py now looks like it was written by an extractor this build is not.
    {
        let conn = lock_conn(&store.conn).expect("connection");
        // Written through `file_payloads`, not `generation_files`: since
        // v17 the latter is a view and the identity columns live on the
        // payload the generation references.
        conn.execute(
            "UPDATE file_payloads SET grammar_version = 'grammar-from-another-era'
                  WHERE payload_id = (
                        SELECT r.payload_id
                          FROM generation_file_rows r
                          JOIN paths p ON p.id = r.file_id
                         WHERE r.generation_id = 1 AND p.path = 'b.py')",
            [],
        )
        .expect("age the row");
        // Guard the guard: an UPDATE that matched nothing would leave the
        // identity current and the assertion below would pass by carrying
        // rather than by re-extracting.
        {
            let aged: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM file_payloads
                          WHERE grammar_version = 'grammar-from-another-era'",
                    [],
                    |row| row.get(0),
                )
                .expect("count aged payloads");
            assert_eq!(aged, 1, "the aging UPDATE must match exactly one payload");
        }
    }

    // Only a.py is declared affected, so b.py would be carried on content
    // alone — which is precisely the bug.
    let second = tree("def alpha():\n    return 11\n");
    commit(
        &store,
        &second,
        GenerationWriteOpts {
            affected_paths: vec!["a.py".into()],
            ..GenerationWriteOpts::default()
        },
    )
    .expect("incremental");

    assert_eq!(
        read_row(&store, 2, "b.py").6,
        current_grammar,
        "a row whose stored identity is not this build's must be re-extracted, \
             not carried forward under an identity nothing checked"
    );
}

/// Every path the latest generation holds a row for.
fn latest_paths(store: &Store) -> Vec<String> {
    let generation = store
        .latest_generation_id()
        .expect("latest generation")
        .expect("a committed generation");
    store
        .list_generation_paths(generation)
        .expect("generation paths")
}

/// An affected path the write holds no extraction for, and was not told is
/// deleted, is refused by name rather than committed as a silent delete.
///
/// The carry set skips every affected path, and fresh rows are written only
/// for the extractions in hand, so a path that is affected, absent from the
/// slice and absent from `deleted_paths` gets neither row: the transaction
/// used to commit `Ok` and the next generation simply lacked the file, its
/// symbols gone without a deletion anyone declared. The daemon reached it
/// through a symlinked directory; `devmap build --affected` reaches it with
/// any path the scan did not extract.
#[test]
fn an_affected_path_with_no_extraction_and_no_deletion_is_refused() {
    let store = Store::open_in_memory().expect("store");
    let cold = vec![
        extract_file("gone.py", "def gone():\n    return 1\n"),
        extract_file("b.py", "def beta():\n    return 2\n"),
    ];
    let first = commit(&store, &cold, GenerationWriteOpts::default()).expect("cold");
    assert!(
        latest_paths(&store).contains(&"gone.py".to_string()),
        "fixture precondition: the previous generation holds gone.py"
    );

    // gone.py is declared affected, is not in the slice, and is not deleted.
    let warm = vec![extract_file("b.py", "def beta():\n    return 2\n")];
    let outcome = commit(
        &store,
        &warm,
        GenerationWriteOpts {
            affected_paths: vec!["gone.py".into()],
            deleted_paths: Vec::new(),
            ..GenerationWriteOpts::default()
        },
    );
    let error = match outcome {
        Err(error) => error.to_string(),
        Ok(generation) => panic!(
            "generation {generation} was committed without gone.py, which was declared \
             affected but neither extracted nor deleted: {:?}",
            latest_paths(&store)
        ),
    };
    assert!(
        error.contains("gone.py") && error.contains("deleted"),
        "the refusal must name the path and the two ways to account for it: {error}"
    );
    assert_eq!(
        store.latest_generation_id().expect("latest"),
        Some(first),
        "the refused write must leave the previous generation as the latest"
    );
    assert!(latest_paths(&store).contains(&"gone.py".to_string()));

    // Positive control: the same write, with the deletion declared, commits and
    // drops exactly that file.
    commit(
        &store,
        &warm,
        GenerationWriteOpts {
            affected_paths: vec!["gone.py".into()],
            deleted_paths: vec!["gone.py".into()],
            ..GenerationWriteOpts::default()
        },
    )
    .expect("a declared deletion commits");
    assert_eq!(latest_paths(&store), vec!["b.py".to_string()]);
}

/// A differential write that would have to carry a stale payload it was not
/// handed a re-extraction for is still refused.
///
/// The refusal above is about affected paths; this one is about unaffected
/// paths whose stored identity this build cannot reuse. Both exist so a file
/// never vanishes from the generation without a declared deletion, and the
/// newer guard must not have displaced the older one.
#[test]
fn a_stale_payload_the_caller_did_not_re_extract_is_still_refused() {
    let store = Store::open_in_memory().expect("store");
    let first = commit(
        &store,
        &tree("def alpha():\n    return 1\n"),
        GenerationWriteOpts::default(),
    )
    .expect("cold");
    {
        let conn = lock_conn(&store.conn).expect("connection");
        let aged = conn
            .execute(
                "UPDATE file_payloads SET analyzer_version = 'analyzer-from-another-era'
                  WHERE payload_id = (
                        SELECT r.payload_id
                          FROM generation_file_rows r
                          JOIN paths p ON p.id = r.file_id
                         WHERE r.generation_id = 1 AND p.path = 'b.py')",
                [],
            )
            .expect("age the row");
        assert_eq!(aged, 1, "the aging UPDATE must match exactly one payload");
    }

    // The daemon's partial shape: only the re-read file is handed over, so the
    // aged b.py has nothing to replace it.
    let partial = vec![extract_file("a.py", "def alpha():\n    return 11\n")];
    let error = commit(
        &store,
        &partial,
        GenerationWriteOpts {
            affected_paths: vec!["a.py".into()],
            ..GenerationWriteOpts::default()
        },
    )
    .expect_err("a stale payload with no replacement must be refused, not dropped")
    .to_string();
    assert!(
        error.contains("cannot carry forward") && error.contains("b.py"),
        "refused for the stale identity, naming the path: {error}"
    );
    assert_eq!(store.latest_generation_id().expect("latest"), Some(first));
}
