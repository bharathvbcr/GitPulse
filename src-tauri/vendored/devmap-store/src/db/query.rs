use super::{
    append_kinds, append_languages, append_narrowing, append_paths, checked_min_confidence,
    checked_span, decode_stored_outcome, distinct_kinds, fts_failure, fts_match_query,
    literal_predicate, lock_conn, refusal, sqlite_limit, stored_symbol_from_row, CallersPage,
    DeadPage, FileEdges, IndexedFiles, KeywordNarrowing, LiteralPage, PathRanks, SearchNarrowing,
    SearchPage, Store, StoredEdge, StoredFile, StoredLiteral, StoredSymbol,
};
use crate::edge_index::{EdgeOrder, GenerationEdges, GenerationEdgesBuilder};
use devmap_analyze::clones::CloneCandidate;
use devmap_analyze::model::{AnalysisDisclosure, DeadSymbolReport};
use devmap_analyze::DeadClusterScan;
use devmap_extract::model::{Extraction, SymbolKind};
use rusqlite::types::ToSql;
use rusqlite::OptionalExtension;
use rusqlite::{params, params_from_iter, Connection, Result};
use std::collections::BTreeSet;

impl Store {
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<(String, String, String)>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = lock_conn(&self.conn)?;
        let (snapshot, gen) = match Self::latest_snapshot(&conn)? {
            Some(pinned) => pinned,
            None => return Ok(vec![]),
        };
        let read = || -> Result<Vec<(String, String, String)>> {
            let mut stmt = snapshot.prepare(
                "SELECT name, qualified_name, path
                 FROM nodes_fts
                 WHERE rowid IN (SELECT rowid_ref FROM nodes_fts_map WHERE generation_id = ?1)
                   AND nodes_fts MATCH ?2
                 ORDER BY rowid
                 LIMIT ?3",
            )?;
            let match_q = fts_match_query(query)?;
            let rows = stmt.query_map(params![gen, match_q, sqlite_limit(limit)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            if out.is_empty() {
                Self::require_searchable_index(&snapshot, gen)?;
            }
            Ok(out)
        };
        read().map_err(fts_failure)
    }

    /// Search only the latest persisted generation. This never reads or parses
    /// the source tree, so callers cannot accidentally turn a query into a build.
    /// Every symbol row in the latest generation.
    ///
    /// Semantic ranking scores the whole corpus, not a keyword-matched page:
    /// the point of it is to find symbols whose *names do not contain the query
    /// terms*, which is exactly what `search_symbols` cannot return. A
    /// primary-key range scan over one generation is the cheapest way to get
    /// them, and there is nothing to precompute or keep in step.
    pub fn all_symbols(&self) -> Result<Vec<StoredSymbol>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(Vec::new());
        };
        Self::all_symbols_in(&snapshot, gen)
    }

    /// Semantic ranking needs all symbols, qualified by the same snapshot's
    /// source root and analysis. No lock is held while the caller ranks them.
    pub fn all_symbols_page(&self) -> Result<Option<SearchPage>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let rows = Self::all_symbols_in(&snapshot, generation)?;
        Ok(Some(SearchPage {
            generation,
            total: u32::try_from(rows.len()).map_err(|_| refusal("symbol count exceeds u32"))?,
            rows,
            repo_root: Self::generation_repo_root_in(&snapshot, generation)?,
            analysis: Self::analysis_disclosure_in(&snapshot, generation)?,
            narrowing: None,
        }))
    }

    /// [`Self::all_symbols_page`], with every file the same generation indexed
    /// as `(path, language)`, ordered by path.
    ///
    /// A scoped ranking checks its path prefixes and languages against this
    /// list, so it has to describe the generation the rows came from: a list
    /// read by a second "latest" lookup could refuse a prefix the rows do
    /// cover, or admit one they do not. Read from the generation's files, not
    /// derived from the rows, so a file is listed whatever it declares.
    pub fn all_symbols_page_with_files(&self) -> Result<Option<(SearchPage, IndexedFiles)>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let rows = Self::all_symbols_in(&snapshot, generation)?;
        let mut stmt = snapshot.prepare(
            "SELECT p.path, f.language
             FROM generation_files f
             JOIN paths p ON p.id = f.file_id
             WHERE f.generation_id = ?1
             ORDER BY p.path",
        )?;
        let files = stmt
            .query_map(params![generation], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<(String, String)>>>()?;
        Ok(Some((
            SearchPage {
                generation,
                total: u32::try_from(rows.len())
                    .map_err(|_| refusal("symbol count exceeds u32"))?,
                rows,
                repo_root: Self::generation_repo_root_in(&snapshot, generation)?,
                analysis: Self::analysis_disclosure_in(&snapshot, generation)?,
                narrowing: None,
            },
            files,
        )))
    }

    fn all_symbols_in(snapshot: &Connection, gen: u32) -> Result<Vec<StoredSymbol>> {
        let mut stmt = snapshot.prepare(
            "SELECT n.name, n.qualified_name, n.kind, p.path,
                    n.span_start, n.span_end, n.is_exported, f.content_hash
             FROM generation_nodes n
             JOIN generation_files f ON f.generation_id = n.generation_id AND f.file_id = n.file_id
             JOIN paths p ON p.id = n.file_id
             WHERE n.generation_id = ?1
             ORDER BY n.ordinal",
        )?;
        let rows = stmt.query_map(params![gen], |row| {
            let name: String = row.get(0)?;
            let path: String = row.get(3)?;
            let (span_start, span_end) = checked_span(&path, &name, row.get(4)?, row.get(5)?)?;
            Ok(StoredSymbol {
                name,
                qualified_name: row.get(1)?,
                kind: row.get(2)?,
                path,
                span_start,
                span_end,
                is_exported: row.get::<_, i64>(6)? != 0,
                content_hash: row.get::<_, i64>(7)? as u64,
            })
        })?;
        rows.collect()
    }

    pub fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<StoredSymbol>> {
        let conn = lock_conn(&self.conn)?;
        let (snapshot, gen) = match Self::latest_snapshot(&conn)? {
            Some(pinned) => pinned,
            None => return Ok(Vec::new()),
        };
        Self::search_symbols_locked(&snapshot, gen, query, limit)
    }

    /// The rows, and the count they were drawn from, against **one** generation.
    ///
    /// `count_search_symbols` and `search_symbols` each resolved "the latest
    /// generation" independently, taking and releasing the connection lock on
    /// their own. A writer committing between them — which is precisely what
    /// the daemon does while a client queries — split the answer across two
    /// generations: the count described the old one and the rows the new one.
    ///
    /// `Response` states the contract that breaks: clients enforce
    /// `shown + hidden == total`. When the newer generation matched more rows
    /// than the older one counted, `total` came back *smaller* than `shown`,
    /// `total.saturating_sub(shown)` clamped `hidden` to zero, and the response
    /// claimed `truncated: false` over a list that was neither complete nor
    /// consistent. Measured before this existed: `shown=40 hidden=0 total=1`.
    ///
    /// One lock and one explicitly pinned generation for every read, so the
    /// answer describes a single snapshot. Returns `None` when the store holds
    /// no generation at all, which is a different answer from an empty page.
    pub fn search_page(&self, query: &str, limit: usize) -> Result<Option<SearchPage>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        Ok(Some(SearchPage {
            generation,
            total: Self::count_search_symbols_locked(&snapshot, generation, query)?,
            rows: Self::search_symbols_locked(&snapshot, generation, query, limit)?,
            repo_root: Self::generation_repo_root_in(&snapshot, generation)?,
            // Inside the same snapshot as the rows and the count, through the
            // one reader that strips the summary's two vectors in SQLite. A
            // search that finds nothing is only a completed check if the corpus
            // it searched was complete, and that is the fact this carries.
            analysis: Self::analysis_disclosure_in(&snapshot, generation)?,
            narrowing: None,
        }))
    }

    fn generation_repo_root_in(snapshot: &Connection, generation: u32) -> Result<Option<String>> {
        let root: Option<Option<String>> = snapshot
            .query_row(
                "SELECT repo_root FROM generations WHERE id = ?1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?;
        Ok(root.flatten().filter(|root| !root.is_empty()))
    }

    fn search_symbols_locked(
        conn: &Connection,
        gen: u32,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredSymbol>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let match_query = fts_match_query(query)?;
        let mut stmt = conn
            .prepare(
                "SELECT n.name, n.qualified_name, n.kind, p.path,
                    n.span_start, n.span_end, n.is_exported, f.content_hash
             FROM nodes_fts
             CROSS JOIN nodes_fts_map m ON m.rowid_ref = nodes_fts.rowid
             JOIN generation_nodes n
               ON n.generation_id = m.generation_id
              AND n.ordinal = (nodes_fts.rowid & 4294967295)
             JOIN paths p ON p.id = n.file_id
             JOIN generation_files f ON f.generation_id = n.generation_id AND f.file_id = n.file_id
             WHERE m.generation_id = ?1 AND nodes_fts MATCH ?2
             ORDER BY bm25(nodes_fts), p.path, n.name, n.span_start
             LIMIT ?3",
            )
            .map_err(fts_failure)?;
        let rows = stmt
            .query_map(params![gen, match_query, sqlite_limit(limit)], |row| {
                let name: String = row.get(0)?;
                let path: String = row.get(3)?;
                let (span_start, span_end) = checked_span(&path, &name, row.get(4)?, row.get(5)?)?;
                Ok(StoredSymbol {
                    name,
                    qualified_name: row.get(1)?,
                    kind: row.get(2)?,
                    path,
                    span_start,
                    span_end,
                    is_exported: row.get::<_, i64>(6)? != 0,
                    content_hash: row.get::<_, i64>(7)? as u64,
                })
            })
            .map_err(fts_failure)?;
        let page = rows.collect::<Result<Vec<_>>>().map_err(fts_failure)?;
        if page.is_empty() {
            Self::require_searchable_index(conn, gen).map_err(fts_failure)?;
        }
        Ok(page)
    }

    pub fn count_search_symbols(&self, query: &str) -> Result<u32> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(0);
        };
        Self::count_search_symbols_locked(&snapshot, gen, query)
    }

    fn count_search_symbols_locked(conn: &Connection, gen: u32, query: &str) -> Result<u32> {
        if query.trim().is_empty() {
            return Ok(0);
        }
        let match_query = fts_match_query(query)?;
        // CROSS JOIN pins the FTS table as the outer loop. As a plain JOIN,
        // SQLite 3.45 (the bundled version) leads with `nodes_fts_map` on
        // `generation_id` and re-scans full-text storage once per mapped row:
        // 12.7s at 200k rows, against 1.7ms for the match alone. A subquery
        // does not help because the planner flattens it. `search_symbols`
        // avoids this only by accident, via `ORDER BY bm25(...)`.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*)
             FROM nodes_fts
             CROSS JOIN nodes_fts_map m
               ON m.rowid_ref = nodes_fts.rowid AND m.generation_id = ?1
             WHERE nodes_fts MATCH ?2",
                params![gen, match_query],
                |row| row.get(0),
            )
            .map_err(fts_failure)?;
        if count == 0 {
            Self::require_searchable_index(conn, gen).map_err(fts_failure)?;
        }
        u32::try_from(count).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, count))
    }

    /// Files of one generation as `(path, language)`, ordered by path.
    fn indexed_files_in(snapshot: &Connection, generation: u32) -> Result<IndexedFiles> {
        let mut stmt = snapshot.prepare(
            "SELECT p.path, f.language
             FROM generation_files f
             JOIN paths p ON p.id = f.file_id
             WHERE f.generation_id = ?1
             ORDER BY p.path",
        )?;
        let files = stmt
            .query_map(params![generation], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<IndexedFiles>>()?;
        Ok(files)
    }

    /// The generation a keyword scope must be checked against, with its files.
    ///
    /// The caller then passes this generation id to [`Self::search_page_in`],
    /// so the refusal and the page describe one snapshot.
    pub fn latest_scope_inputs(&self) -> Result<Option<(u32, Option<String>, IndexedFiles)>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let files = Self::indexed_files_in(&snapshot, generation)?;
        let root = Self::generation_repo_root_in(&snapshot, generation)?;
        Ok(Some((generation, root, files)))
    }

    /// [`Self::search_page`] against a generation the caller already pinned.
    ///
    /// `narrowing` of `None`, or one with every list empty, runs the unfiltered
    /// statements unchanged. A set filter joins the same generation's paths and
    /// files so the count and the page see one filtered set — applying the
    /// filter to a page the full-text index already cut would make `total`
    /// describe a different corpus from `rows`.
    pub fn search_page_in(
        &self,
        generation: u32,
        query: &str,
        limit: usize,
        narrowing: Option<&KeywordNarrowing>,
    ) -> Result<Option<SearchPage>> {
        let conn = lock_conn(&self.conn)?;
        let present: Option<i64> = conn
            .query_row(
                "SELECT id FROM generations WHERE id = ?1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?;
        if present.is_none() {
            return Ok(None);
        }
        let active = narrowing.filter(|filter| filter.active());
        let (total, rows, applied) = if let Some(filter) = active {
            Self::search_narrowed(&conn, generation, query, limit, filter)?
        } else {
            (
                Self::count_search_symbols_locked(&conn, generation, query)?,
                Self::search_symbols_locked(&conn, generation, query, limit)?,
                None,
            )
        };
        Ok(Some(SearchPage {
            generation,
            total,
            rows,
            repo_root: Self::generation_repo_root_in(&conn, generation)?,
            analysis: Self::analysis_disclosure_in(&conn, generation)?,
            narrowing: applied,
        }))
    }

    /// Literal sites whose value equals `query`, or starts with it.
    ///
    /// `None` when the store holds no generation. The comparison is binary, so
    /// `Session.` does not match `session.`. At most `page_cap` rows are
    /// loaded; `total` is the full count either way.
    pub fn search_literals(
        &self,
        query: &str,
        exact: bool,
        page_cap: usize,
    ) -> Result<Option<LiteralPage>> {
        if query.is_empty() {
            return Err(refusal("literals requires a non-empty query"));
        }
        if query.contains('\0') {
            return Err(refusal("literals query contains NUL"));
        }
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let (predicate, mut params) = literal_predicate(generation, query, exact)?;
        let count_sql = format!("SELECT COUNT(*) FROM generation_literals l WHERE {predicate}");
        let count: i64 = snapshot
            .query_row(&count_sql, params_from_iter(params.iter()), |row| {
                row.get(0)
            })
            .map_err(fts_failure)?;
        let total =
            u32::try_from(count).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, count))?;
        let page_sql = format!(
            "SELECT p.path, l.line, l.span_start, l.value, l.qualified_name, l.symbol_name
             FROM generation_literals l
             JOIN paths p ON p.id = l.file_id
             WHERE {predicate}
             ORDER BY p.path, l.line, l.span_start
             LIMIT ?"
        );
        let limit = i64::try_from(page_cap.max(1)).unwrap_or(i64::MAX);
        params.push(Box::new(limit));
        let mut stmt = snapshot.prepare(&page_sql).map_err(fts_failure)?;
        let rows = stmt
            .query_map(params_from_iter(params.iter()), |row| {
                Ok(StoredLiteral {
                    file_path: row.get(0)?,
                    line: u32::try_from(row.get::<_, i64>(1)?).unwrap_or(u32::MAX),
                    span_start: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                    value: row.get(3)?,
                    qualified_name: row.get(4)?,
                    symbol_name: row.get(5)?,
                })
            })
            .map_err(fts_failure)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(fts_failure)?);
        }
        Ok(Some(LiteralPage { total, rows: out }))
    }

    fn search_narrowed(
        conn: &Connection,
        generation: u32,
        query: &str,
        limit: usize,
        filter: &KeywordNarrowing,
    ) -> Result<(u32, Vec<StoredSymbol>, Option<SearchNarrowing>)> {
        let present = distinct_kinds(conn, generation, filter)?;
        let missing: Vec<String> = filter
            .kinds
            .iter()
            .filter(|kind| !present.contains(kind.as_str()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            let place = if filter.paths.is_empty() && filter.languages.is_empty() {
                "in the indexed repository"
            } else {
                "under the path/language"
            };
            let listed = if present.is_empty() {
                "(none)".to_string()
            } else {
                present.iter().cloned().collect::<Vec<_>>().join(", ")
            };
            return Err(refusal(format!(
                "kinds entry {missing:?} labels no symbol {place}; kinds present: {listed}"
            )));
        }
        if query.trim().is_empty() {
            let narrowing = Self::narrowing_counts(conn, generation, filter)?;
            return Ok((0, Vec::new(), Some(narrowing)));
        }
        let match_query = fts_match_query(query)?;
        let mut count_sql = String::from(
            "SELECT COUNT(*)
             FROM nodes_fts
             CROSS JOIN nodes_fts_map m
               ON m.rowid_ref = nodes_fts.rowid AND m.generation_id = ?
             JOIN generation_nodes n
               ON n.generation_id = m.generation_id
              AND n.ordinal = (nodes_fts.rowid & 4294967295)
             JOIN paths p ON p.id = n.file_id
             JOIN generation_files f
               ON f.generation_id = n.generation_id AND f.file_id = n.file_id
             WHERE nodes_fts MATCH ?",
        );
        let mut count_params: Vec<Box<dyn ToSql>> = vec![
            Box::new(i64::from(generation)),
            Box::new(match_query.clone()),
        ];
        append_narrowing(&mut count_sql, &mut count_params, filter);
        let count: i64 = conn
            .query_row(&count_sql, params_from_iter(count_params.iter()), |row| {
                row.get(0)
            })
            .map_err(fts_failure)?;
        if count == 0 {
            Self::require_searchable_index(conn, generation).map_err(fts_failure)?;
        }
        let total =
            u32::try_from(count).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, count))?;
        let mut page_sql = String::from(
            "SELECT n.name, n.qualified_name, n.kind, p.path,
                    n.span_start, n.span_end, n.is_exported, f.content_hash
             FROM nodes_fts
             CROSS JOIN nodes_fts_map m ON m.rowid_ref = nodes_fts.rowid
             JOIN generation_nodes n
               ON n.generation_id = m.generation_id
              AND n.ordinal = (nodes_fts.rowid & 4294967295)
             JOIN paths p ON p.id = n.file_id
             JOIN generation_files f ON f.generation_id = n.generation_id AND f.file_id = n.file_id
             WHERE m.generation_id = ? AND nodes_fts MATCH ?",
        );
        let mut page_params: Vec<Box<dyn ToSql>> =
            vec![Box::new(i64::from(generation)), Box::new(match_query)];
        append_narrowing(&mut page_sql, &mut page_params, filter);
        page_sql.push_str(" ORDER BY bm25(nodes_fts), p.path, n.name, n.span_start LIMIT ?");
        page_params.push(Box::new(sqlite_limit(limit)));
        let mut stmt = conn.prepare(&page_sql).map_err(fts_failure)?;
        let mapped = stmt
            .query_map(params_from_iter(page_params.iter()), |row| {
                stored_symbol_from_row(row)
            })
            .map_err(fts_failure)?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(fts_failure)?);
        }
        if rows.is_empty() {
            Self::require_searchable_index(conn, generation).map_err(fts_failure)?;
        }
        let narrowing = Self::narrowing_counts(conn, generation, filter)?;
        Ok((total, rows, Some(narrowing)))
    }

    fn narrowing_counts(
        conn: &Connection,
        generation: u32,
        filter: &KeywordNarrowing,
    ) -> Result<SearchNarrowing> {
        let corpus_files: i64 = conn.query_row(
            "SELECT COUNT(*) FROM generation_files WHERE generation_id = ?1",
            params![generation],
            |row| row.get(0),
        )?;
        let corpus_symbols: i64 = conn.query_row(
            "SELECT COUNT(*) FROM generation_nodes WHERE generation_id = ?1",
            params![generation],
            |row| row.get(0),
        )?;
        let files: i64 = if filter.paths.is_empty() && filter.languages.is_empty() {
            let mut sql = String::from(
                "SELECT COUNT(DISTINCT n.file_id) FROM generation_nodes n
                 WHERE n.generation_id = ?",
            );
            let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(i64::from(generation))];
            append_kinds(&mut sql, &mut params, &filter.kinds);
            conn.query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))?
        } else {
            let mut sql = String::from(
                "SELECT COUNT(*) FROM generation_files f
                 JOIN paths p ON p.id = f.file_id
                 WHERE f.generation_id = ?",
            );
            let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(i64::from(generation))];
            append_paths(&mut sql, &mut params, &filter.paths);
            append_languages(&mut sql, &mut params, &filter.languages);
            conn.query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))?
        };
        let mut symbol_sql = String::from(
            "SELECT COUNT(*) FROM generation_nodes n
             JOIN paths p ON p.id = n.file_id
             JOIN generation_files f
               ON f.generation_id = n.generation_id AND f.file_id = n.file_id
             WHERE n.generation_id = ?",
        );
        let mut symbol_params: Vec<Box<dyn ToSql>> = vec![Box::new(i64::from(generation))];
        append_narrowing(&mut symbol_sql, &mut symbol_params, filter);
        let symbols: i64 =
            conn.query_row(&symbol_sql, params_from_iter(symbol_params.iter()), |row| {
                row.get(0)
            })?;
        Ok(SearchNarrowing {
            paths: filter.paths.clone(),
            languages: filter.languages.clone(),
            kinds: filter.kinds.clone(),
            files: u32::try_from(files).unwrap_or(u32::MAX),
            symbols: u32::try_from(symbols).unwrap_or(u32::MAX),
            corpus_files: u32::try_from(corpus_files).unwrap_or(u32::MAX),
            corpus_symbols: u32::try_from(corpus_symbols).unwrap_or(u32::MAX),
        })
    }

    pub fn latest_path_is_indexed(&self, path: &str) -> Result<bool> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(false);
        };
        snapshot.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM generation_files f
                JOIN paths p ON p.id = f.file_id
                WHERE f.generation_id = ?1 AND p.path = ?2
             )",
            params![gen, path],
            |row| row.get::<_, i64>(0).map(|value| value != 0),
        )
    }

    pub fn latest_file(&self, path: &str) -> Result<Option<StoredFile>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        Self::file_in(&snapshot, gen, path)
    }

    fn file_in(snapshot: &Connection, gen: u32, path: &str) -> Result<Option<StoredFile>> {
        let raw: Option<(String, String, i64, String, String)> = snapshot
            .query_row(
                "SELECT p.path, f.language, f.content_hash,
                        f.parse_outcome_json, f.engine_json
                 FROM generation_files f
                 JOIN paths p ON p.id = f.file_id
                 WHERE f.generation_id = ?1 AND p.path = ?2",
                params![gen, path],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        raw.map(|(path, language, content_hash, parse_json, engine_json)| {
            let (parse_outcome, engine) = decode_stored_outcome(&path, &parse_json, &engine_json)?;
            Ok(StoredFile {
                path,
                language,
                content_hash: content_hash as u64,
                parse_outcome,
                engine,
            })
        })
        .transpose()
    }

    /// Load the canonical extraction payloads for the latest generation. This
    /// supports differential re-resolution without touching unchanged files.
    /// Qualified names a test runner invokes in the latest generation.
    ///
    /// The `RuntimeEntryPoint` wiring annotations whose details
    /// [`devmap_extract::wiring::is_test_harness_reason`] recognises, read
    /// without decoding whole extractions: SQLite pulls out only the
    /// `wiring` array, and only from files whose payload mentions an entry
    /// point at all. `affected_tests` runs on every hook-driven query, and a
    /// full [`Self::latest_extractions`] decode there would cost more than the
    /// walk it serves.
    pub fn latest_test_entry_symbols(&self) -> Result<std::collections::HashSet<String>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(std::collections::HashSet::new());
        };
        let mut stmt = snapshot.prepare(
            "SELECT json_extract(f.extraction_json, '$.wiring'), p.path
             FROM generation_files f
             JOIN paths p ON p.id = f.file_id
             WHERE f.generation_id = ?1
               AND instr(f.extraction_json, 'RuntimeEntryPoint') > 0",
        )?;
        let rows = stmt.query_map(params![gen], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut symbols = std::collections::HashSet::new();
        for row in rows {
            let (json, path) = row?;
            let Some(json) = json else { continue };
            let wiring: Vec<devmap_extract::model::WiringAnnotation> = serde_json::from_str(&json)
                .map_err(|error| {
                    refusal(format!("stored wiring for {path} is invalid: {error}"))
                })?;
            symbols.extend(
                wiring
                    .into_iter()
                    .filter(|annotation| {
                        annotation.kind == devmap_extract::model::WiringKind::RuntimeEntryPoint
                            && devmap_extract::wiring::is_test_harness_reason(&annotation.details)
                    })
                    .map(|annotation| annotation.target_symbol),
            );
        }
        Ok(symbols)
    }

    pub fn latest_extractions(&self) -> Result<Vec<Extraction>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(Vec::new());
        };
        let mut stmt = snapshot.prepare(
            "SELECT f.extraction_json, p.path
             FROM generation_files f
             JOIN paths p ON p.id = f.file_id
             WHERE f.generation_id = ?1
             ORDER BY p.path",
        )?;
        let rows = stmt.query_map(params![gen], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut extractions = Vec::new();
        for row in rows {
            let (json, path) = row?;
            let extraction = serde_json::from_str(&json).map_err(|error| {
                refusal(format!("stored extraction for {path} is invalid: {error}"))
            })?;
            extractions.push(extraction);
        }
        Ok(extractions)
    }

    /// One file's stored extraction, or `None` when the latest generation does
    /// not contain it.
    ///
    /// `latest_extractions` deserialises every file in the generation — 1,300
    /// JSON payloads on this repository — which is the wrong shape for a
    /// question about one path. `None` distinguishes "this file is not
    /// indexed" from "this file is indexed and empty", and a preview has to
    /// tell those apart: against an unindexed file every symbol in the buffer
    /// is an addition, which is true but worth saying out loud rather than
    /// presenting as a diff against known content.
    pub fn latest_extraction_for_path(&self, path: &str) -> Result<Option<Extraction>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let json: Option<String> = snapshot
            .query_row(
                "SELECT f.extraction_json
                 FROM generation_files f
                 JOIN paths p ON p.id = f.file_id
                 WHERE f.generation_id = ?1 AND p.path = ?2",
                params![gen, path],
                |row| row.get(0),
            )
            .optional()?;
        let Some(json) = json else {
            return Ok(None);
        };
        let extraction = serde_json::from_str(&json).map_err(|error| {
            refusal(format!("stored extraction for {path} is invalid: {error}"))
        })?;
        Ok(Some(extraction))
    }

    /// Call edges whose *target* is one of `names`, excluding those originating
    /// in `exclude_file`.
    ///
    /// `names` are **qualified** names (`path::Symbol`), which is what
    /// `generation_edges.target_symbol` holds. Bare names match nothing here,
    /// and match nothing quietly: the query returns zero rows and the caller
    /// reports that nothing depends on the symbol.
    ///
    /// The exclusion is what makes the answer mean "who outside this file
    /// depends on these symbols". A file's own internal calls are not callers
    /// that a rewrite of that file would break — they are being rewritten too —
    /// and counting them inflates every preview of a self-contained module.
    ///
    /// An empty `names` returns no rows without touching the database, rather
    /// than building `IN ()`, which SQLite rejects.
    ///
    /// The threshold goes through [`checked_min_confidence`] *before* that
    /// shortcut. This was the one confidence-filtered edge query that skipped
    /// it, and skipping it is not a missing error message: rusqlite binds
    /// `f32::NAN` as a REAL, SQLite stores that as NULL, and the
    /// `CAST(ROUND(...)) >= CAST(ROUND(?3 * 1000))` predicate below is then
    /// NULL for every row — so the query returned `Ok(vec![])` and `preview`
    /// reported "no calls from other files are affected" for callers at
    /// confidence 1.00, while blaming the omission on the confidence floor.
    /// An empty edge list is also what a filter that ran returns, so the
    /// caller had no way to tell that the filter had not run at all.
    /// A large `names` is **chunked**, never refused and never truncated. Each
    /// name becomes one bind parameter, so an unchunked query with more than
    /// `SQLITE_MAX_VARIABLE_NUMBER` names failed to even prepare — surfacing
    /// `too many SQL variables` from the middle of a `preview`, naming neither
    /// the caller nor the limit. Truncating the list instead would have been
    /// worse: a dropped name contributes zero callers, which reads exactly like
    /// a symbol nothing depends on. Chunking keeps the answer complete and
    /// bounds only the statement, and the documented total order is restored
    /// across chunks by the sort below.
    /// How many callers `callers_of` would return, without building them.
    ///
    /// `preview` needs two numbers from the same query: the confident callers
    /// it lists, and how many more the confidence floor excluded. The second
    /// was obtained by calling `callers_of` again at floor 0.0 and taking
    /// `.len()` — materialising every matching `StoredEdge`, six `String`
    /// allocations apiece, to produce one integer. On this repository the
    /// busiest symbol has 918 callers, so previewing a file that declares one
    /// built ~1,836 rows and discarded all of them.
    ///
    /// Deliberately shares every filter, guard and chunk boundary with
    /// `callers_of` — including `checked_min_confidence`, so a NaN floor is
    /// refused here too rather than counting zero. `count_callers_of_matches_the_listing_it_replaces`
    /// pins the two against each other; a filter added to one and not the other
    /// fails that test rather than silently making the "hidden" number wrong.
    pub fn count_callers_of(
        &self,
        names: &[String],
        exclude_file: &str,
        min_confidence: f32,
    ) -> Result<usize> {
        let min_confidence = checked_min_confidence(min_confidence)?;
        if names.is_empty() {
            return Ok(0);
        }
        let unique: Vec<&String> = {
            let mut seen = BTreeSet::new();
            names.iter().filter(|name| seen.insert(*name)).collect()
        };
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(0);
        };
        Self::count_callers_in(&snapshot, gen, &unique, exclude_file, min_confidence)
    }

    fn count_callers_in(
        snapshot: &Connection,
        gen: u32,
        unique: &[&String],
        exclude_file: &str,
        min_confidence: f32,
    ) -> Result<usize> {
        let mut total: usize = 0;
        for chunk in unique.chunks(Self::MAX_CALLER_BATCH) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT COUNT(*)
                 FROM generation_edges e
                 JOIN paths sp ON sp.id = e.source_file_id
                 WHERE e.generation_id = ?1
                   AND e.edge_kind = 'Calls'
                   AND sp.path <> ?2
                   AND CAST(ROUND(e.confidence * 1000) AS INTEGER) >= CAST(ROUND(?3 * 1000) AS INTEGER)
                   AND e.target_symbol IN ({placeholders})"
            );
            let mut stmt = snapshot.prepare(&sql)?;
            let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 3);
            bound.push(&gen);
            bound.push(&exclude_file);
            bound.push(&min_confidence);
            for name in chunk {
                bound.push(*name);
            }
            // Chunks partition the *names*, and each edge names one target, so
            // the per-chunk counts sum without double-counting — the same
            // property that makes `callers_of`'s chunked concatenation exact.
            let count: i64 = stmt.query_row(bound.as_slice(), |row| row.get(0))?;
            total += usize::try_from(count).unwrap_or(0);
        }
        Ok(total)
    }

    pub fn callers_of(
        &self,
        names: &[String],
        exclude_file: &str,
        min_confidence: f32,
    ) -> Result<Vec<StoredEdge>> {
        // Same guard as `latest_edges` and `latest_edges_for_file`: NaN cannot
        // be compared, and binding it makes SQLite evaluate `>= NULL` as NULL
        // so every row is rejected. The empty result that comes back is the
        // sentence "nothing calls this", produced by a filter that never ran —
        // and `dead_symbols.py` reads exactly that emptiness as proof a symbol
        // is unused. See `checked_min_confidence`.
        let min_confidence = checked_min_confidence(min_confidence)?;
        if names.is_empty() {
            return Ok(Vec::new());
        }
        // `IN` already ignores duplicates, so deduplicating preserves the
        // result exactly while making each edge belong to a single chunk.
        let unique: Vec<&String> = {
            let mut seen = BTreeSet::new();
            names.iter().filter(|name| seen.insert(*name)).collect()
        };
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(Vec::new());
        };
        Self::callers_in(&snapshot, gen, &unique, exclude_file, min_confidence)
    }

    fn callers_in(
        snapshot: &Connection,
        gen: u32,
        unique: &[&String],
        exclude_file: &str,
        min_confidence: f32,
    ) -> Result<Vec<StoredEdge>> {
        let mut out: Vec<StoredEdge> = Vec::new();
        for chunk in unique.chunks(Self::MAX_CALLER_BATCH) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT sp.path, tp.path, e.source_symbol, e.target_symbol,
                        e.edge_kind, e.confidence, e.resolution
                 FROM generation_edges e
                 JOIN paths sp ON sp.id = e.source_file_id
                 JOIN paths tp ON tp.id = e.target_file_id
                 WHERE e.generation_id = ?1
                   AND e.edge_kind = 'Calls'
                   AND sp.path <> ?2
                   AND CAST(ROUND(e.confidence * 1000) AS INTEGER) >= CAST(ROUND(?3 * 1000) AS INTEGER)
                   AND e.target_symbol IN ({placeholders})"
            );
            let mut stmt = snapshot.prepare(&sql)?;
            let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 3);
            bound.push(&gen);
            bound.push(&exclude_file);
            bound.push(&min_confidence);
            for name in chunk {
                bound.push(*name);
            }
            let rows = stmt.query_map(bound.as_slice(), |row| {
                Ok(StoredEdge {
                    source_file: row.get(0)?,
                    target_file: row.get(1)?,
                    source_symbol: row.get(2)?,
                    target_symbol: row.get(3)?,
                    edge_kind: row.get(4)?,
                    confidence: row.get(5)?,
                    resolution: row.get(6)?,
                })
            })?;
            for row in rows {
                out.push(row?);
            }
        }
        // The order the single-statement form got from SQL, restored in Rust so
        // a chunked answer and an unchunked one are byte-identical.
        out.sort_by(|left, right| {
            right
                .confidence
                .total_cmp(&left.confidence)
                .then_with(|| left.target_symbol.cmp(&right.target_symbol))
                .then_with(|| left.source_file.cmp(&right.source_file))
                .then_with(|| left.source_symbol.cmp(&right.source_symbol))
        });
        Ok(out)
    }

    pub fn latest_edges_for_file(
        &self,
        path: &str,
        min_confidence: f32,
    ) -> Result<Vec<StoredEdge>> {
        let min_confidence = checked_min_confidence(min_confidence)?;
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(Vec::new());
        };
        Self::edges_for_file_in(&snapshot, gen, path, min_confidence)
    }

    /// Dependencies must not attach an old parse outcome to a newer edge set.
    pub fn file_edges(&self, path: &str, min_confidence: f32) -> Result<Option<FileEdges>> {
        let min_confidence = checked_min_confidence(min_confidence)?;
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let Some(file) = Self::file_in(&snapshot, generation, path)? else {
            return Ok(None);
        };
        Ok(Some(FileEdges {
            generation,
            file,
            edges: Self::edges_for_file_in(&snapshot, generation, path, min_confidence)?,
            analysis: Self::analysis_disclosure_in(&snapshot, generation)?,
        }))
    }

    fn edges_for_file_in(
        snapshot: &Connection,
        gen: u32,
        path: &str,
        min_confidence: f32,
    ) -> Result<Vec<StoredEdge>> {
        let mut stmt = snapshot.prepare(
            "SELECT sp.path, tp.path, e.source_symbol, e.target_symbol,
                    e.edge_kind, e.confidence, e.resolution
             FROM generation_edges e
             JOIN paths sp ON sp.id = e.source_file_id
             JOIN paths tp ON tp.id = e.target_file_id
             WHERE e.generation_id = ?1
               AND (sp.path = ?2 OR tp.path = ?2)
               AND CAST(ROUND(e.confidence * 1000) AS INTEGER) >= CAST(ROUND(?3 * 1000) AS INTEGER)
             ORDER BY e.confidence DESC, sp.path, tp.path,
                      e.source_symbol, e.target_symbol, e.edge_kind",
        )?;
        let rows = stmt.query_map(params![gen, path, min_confidence], |row| {
            Ok(StoredEdge {
                source_file: row.get(0)?,
                target_file: row.get(1)?,
                source_symbol: row.get(2)?,
                target_symbol: row.get(3)?,
                edge_kind: row.get(4)?,
                confidence: row.get(5)?,
                resolution: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    /// Every edge in the latest generation at or above `min_confidence`.
    ///
    /// Materialised from [`Store::generation_edges`], which is the one read of
    /// a generation's edges: the rows a caller gets here are built from the
    /// index's interned columns rather than from a second query, so a filtered
    /// read and an indexed walk cannot describe different generations or
    /// disagree about the order they are in.
    ///
    /// This is the whole-generation shape, and it costs what a whole generation
    /// costs — six owned `String`s per row. Everything that only needs *some*
    /// rows should ask the index for those, which is what the query engine now
    /// does; this stays for the callers that genuinely want every row.
    pub fn latest_edges(&self, min_confidence: f32) -> Result<Vec<StoredEdge>> {
        // The confidence comparison is the SQL's, moved into Rust unchanged, so
        // a cached answer and a freshly-queried one cannot disagree — *given a
        // finite threshold*. That qualifier was missing and the claim was false:
        // on NaN the two implementations disagreed completely. Rust saturates
        // `(NaN * 1000.0).round() as i64` to 0 and admits everything; SQLite
        // stores NaN as NULL and `>= NULL` is NULL, so the SQL admits nothing.
        // A caller got "this depends on nothing" — a positive claim — from a
        // comparison that never ran. `checked_min_confidence` refuses the input
        // instead, so neither implementation is asked an unanswerable question.
        let min_confidence = checked_min_confidence(min_confidence)?;
        let Some(index) = self.generation_edges()? else {
            return Ok(Vec::new());
        };
        Ok((0..index.len() as u32)
            .filter(|id| index.admits(*id, min_confidence))
            .map(|id| index.stored_edge(id))
            .collect())
    }

    /// Adjacency over the latest generation's edges, built once per generation.
    ///
    /// `None` when no generation has been persisted. The returned index is a
    /// snapshot: it stays internally consistent — every edge in it comes from
    /// one generation — even if a build commits a newer one while a walk is
    /// running, and the next call after that build gets the newer generation
    /// because the memo is keyed by its id.
    ///
    /// Errors on an unknown stored edge kind or resolution label, which is
    /// where the per-request conversion used to fail: a store written by a
    /// binary that knows a kind or a tier this one does not is refused rather
    /// than half-read.
    ///
    /// Keyed by the generation the rows were *read from*, not by the one
    /// sampled before the load. This function asks the question twice — once to
    /// probe the memo, once inside the load's own snapshot — and a writer
    /// committing between the two made the entry `(N, edges of N+1)`: a key
    /// that can never be hit again, so the memo silently stopped being one
    /// until the next load rewrote it. Labelling the entry with the generation
    /// its rows came from makes the key mean what it says.
    pub fn generation_edges(&self) -> Result<Option<std::sync::Arc<GenerationEdges>>> {
        Ok(self.generation_edges_with_id()?.map(|(_, index)| index))
    }

    /// [`Self::generation_edges`], with the generation the index was built from.
    ///
    /// For a caller that pairs the walk with a second read — the unresolved
    /// ledger — and must make that read at the same generation, not at
    /// whichever one a daemon committed in between.
    pub fn generation_edges_with_id(
        &self,
    ) -> Result<Option<(u32, std::sync::Arc<GenerationEdges>)>> {
        let current = {
            let conn = lock_conn(&self.conn)?;
            Self::latest_generation_id_locked(&conn)?
        };
        let Some(current) = current else {
            return Ok(None);
        };
        if let Ok(cache) = self.edge_index.lock() {
            if let Some((generation, index)) = cache.as_ref() {
                if *generation == current {
                    return Ok(Some((current, std::sync::Arc::clone(index))));
                }
            }
        }
        let Some((loaded, index)) = self.latest_edge_index_uncached()? else {
            return Ok(None);
        };
        let index = std::sync::Arc::new(index);
        if let Ok(mut cache) = self.edge_index.lock() {
            *cache = Some((loaded, std::sync::Arc::clone(&index)));
        }
        Ok(Some((loaded, index)))
    }

    /// The latest generation's adjacency, read fresh, and the generation it
    /// came from.
    ///
    /// # Why this does not materialise the generation
    ///
    /// This read is the whole fixed cost of arriving at [`GenerationEdges`],
    /// which is what a one-shot `devmap impact` pays and never amortises — the
    /// index memo above is per *process*, and a CLI process asks one question.
    /// Measured on this repository's 102,239 edges, cold, minima of nine runs:
    /// arriving at the index cost **63.1 ms** and the walk that followed cost
    /// **1.5 ms**. The whole of a cold `impact` was arrival.
    ///
    /// Two shapes were paying for it, and both were proportional to the
    /// generation rather than to the answer:
    ///
    /// | part | cost |
    /// |---|---|
    /// | `ORDER BY confidence DESC, sp.path, tp.path, …` in SQLite | ~72 ms (removed earlier) |
    /// | the row scan and six owned `String`s per `StoredEdge` | ~41 ms |
    /// | four `HashMap<Box<str>, Vec<u32>>` over those rows | ~22 ms |
    ///
    /// A generation's rows are mostly repetition — 102,239 edges naming 17,869
    /// distinct symbols, 1,602 paths, 8 kinds and 7 resolution labels — and the
    /// row shape paid for that repetition twice, once copying the text and
    /// again hashing it. So the rows are never built: the cursor's borrowed
    /// `&str`s go straight into [`GenerationEdgesBuilder`], which interns each
    /// distinct string once and keeps six `u32`s per edge, and the adjacency
    /// becomes a counting sort over those ranks instead of four hash maps over
    /// the text. What a caller needs a row for it gets one row at a time, for
    /// the edges its answer actually contains.
    ///
    /// # Why the order is the same
    ///
    /// [`EdgeOrder::ReadOrder`] hands the ordering to `edge_read_order`, which
    /// is SQL's key for key and is the single owner of it — see the comparator.
    /// `the_rust_edge_order_is_the_sql_order_it_replaced` runs the removed
    /// statement verbatim against the same store and requires row-for-row
    /// agreement.
    fn latest_edge_index_uncached(&self) -> Result<Option<(u32, GenerationEdges)>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let paths = PathRanks::read(&snapshot)?;
        let edge_count: i64 = snapshot.query_row(
            "SELECT COUNT(*) FROM generation_edges WHERE generation_id = ?1",
            params![gen],
            |row| row.get(0),
        )?;
        // Ids are `u32`. A generation with more edges than that cannot be
        // addressed, and answering over a silently truncated prefix would be a
        // wrong answer rather than a bounded one.
        if edge_count > u32::MAX as i64 {
            return Err(refusal(format!(
                "generation {gen} holds {edge_count} edges, more than the {} an \
                 edge index can address; answering over a prefix of it would be \
                 a wrong answer rather than a bounded one",
                u32::MAX
            )));
        }
        let mut builder = GenerationEdgesBuilder::with_capacity(edge_count.max(0) as usize);
        // The `paths` table is read and ranked once — 1,602 rows — and every
        // edge then names its two files by rank. Interning the path *text* per
        // edge would hash 204,478 strings to learn 1,602 facts.
        let file_ranks: Vec<u32> = (0..paths.len())
            .map(|rank| builder.intern_file(paths.path_of(rank as u32)))
            .collect();
        let mut stmt = snapshot.prepare(
            "SELECT e.source_file_id, e.target_file_id, e.source_symbol,
                    e.target_symbol, e.edge_kind, e.confidence, e.resolution
             FROM generation_edges e
             WHERE e.generation_id = ?1",
        )?;
        let mut rows = stmt.query(params![gen])?;
        while let Some(row) = rows.next()? {
            // `rank_of` refuses an edge whose `paths` row is gone rather than
            // dropping it, which is what the `INNER JOIN` this replaced did:
            // an edge set with holes in it under a successful status, whose
            // holes then propagate as positive claims.
            let source_file = file_ranks[paths.rank_of(row.get(0)?)? as usize];
            let target_file = file_ranks[paths.rank_of(row.get(1)?)? as usize];
            builder
                .push_ranked(
                    source_file,
                    target_file,
                    row.get_ref(2)?.as_str()?,
                    row.get_ref(3)?.as_str()?,
                    row.get_ref(4)?.as_str()?,
                    row.get(5)?,
                    row.get_ref(6)?.as_str_or_null()?,
                )
                .map_err(|error| refusal(error.to_string()))?;
        }
        drop(rows);
        drop(stmt);
        // Read for `gen` specifically — the generation the rows came from,
        // which may already be behind the store's latest.
        let analysis = Self::analysis_disclosure_in(&snapshot, gen)?;
        let index = builder
            .finish_with_stored_evidence(analysis, EdgeOrder::ReadOrder)
            .map_err(|error| refusal(error.to_string()))?
            .with_generation(gen);
        Ok(Some((gen, index)))
    }

    /// The callers of `names` and the unfiltered total, against one generation.
    ///
    /// `callers_of` and `count_callers_of` each open their own snapshot, so a
    /// caller that needs both numbers to agree cannot get that by calling them
    /// in sequence — which is what `preview` was doing. See [`CallersPage`].
    pub fn callers_page(
        &self,
        names: &[String],
        exclude_file: &str,
        min_confidence: f32,
    ) -> Result<Option<CallersPage>> {
        let min_confidence = checked_min_confidence(min_confidence)?;
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        if names.is_empty() {
            return Ok(Some(CallersPage {
                generation,
                callers: Vec::new(),
                total_unfiltered: 0,
            }));
        }
        let unique: Vec<&String> = {
            let mut seen = BTreeSet::new();
            names.iter().filter(|name| seen.insert(*name)).collect()
        };
        Ok(Some(CallersPage {
            generation,
            callers: Self::callers_in(
                &snapshot,
                generation,
                &unique,
                exclude_file,
                min_confidence,
            )?,
            // The denominator is deliberately unfiltered: the difference from
            // `callers` is precisely what the floor excluded.
            total_unfiltered: Self::count_callers_in(
                &snapshot,
                generation,
                &unique,
                exclude_file,
                0.0,
            )?,
        }))
    }

    /// How much of the corpus one generation's analysis actually covered.
    ///
    /// The two big arrays are dropped *inside SQLite*, so they never cross into
    /// this process. `AnalysisDisclosure` already skipped them, but skipping is
    /// per token and there are 10 MB of tokens: measured on the benchmark
    /// corpus this column is 10,122,764 bytes and what survives the strip is
    /// 200. `dead_symbols` is the list being paged beside this; `communities`
    /// is the other unbounded array and no disclosure reads it.
    ///
    /// Absence and corruption stay distinguishable, which is the whole reason
    /// this is safe: `json_remove(NULL, ...)` is NULL, so a generation with no
    /// analysis still reads as none, while a malformed blob makes SQLite raise
    /// ("malformed JSON") rather than quietly returning NULL — a corrupt
    /// analysis must not read as an absent one.
    ///
    /// One owner because two readers of the same column would eventually
    /// disagree about which arrays to strip, and the caller that strips less
    /// pulls 10 MB per query without anything saying so.
    fn analysis_disclosure_in(
        snapshot: &Connection,
        generation: u32,
    ) -> Result<Option<AnalysisDisclosure>> {
        let raw: Option<String> = snapshot
            .query_row(
                "SELECT json_remove(analysis_json, '$.dead_symbols', '$.communities')
                 FROM generations WHERE id = ?1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?;
        // Into the disclosure, not the whole summary: the summary embeds a
        // second copy of the dead-symbol list, so parsing it here would undo
        // the bound above. See `AnalysisDisclosure`.
        raw.map(|json| {
            serde_json::from_str::<AnalysisDisclosure>(&json)
                .map_err(|error| refusal(format!("stored generation analysis is invalid: {error}")))
        })
        .transpose()
    }

    /// The abandoned cycles one generation's analysis recorded.
    ///
    /// `None` means the column could not be read as a scan: no analysis row, or
    /// a generation written before `dead_clusters` existed. That is *not* an
    /// empty scan, and the two must not render alike — an empty list is "the
    /// pass ran and found nothing", which is a finding.
    ///
    /// Read with its own `json_extract` rather than through
    /// `AnalysisDisclosure`, which is deserialized on search, edge and status
    /// paths that have no use for a cluster list and would pay for parsing one.
    fn dead_clusters_in(snapshot: &Connection, generation: u32) -> Result<Option<DeadClusterScan>> {
        let raw: Option<String> = snapshot
            .query_row(
                "SELECT json_extract(analysis_json, '$.dead_clusters')
                 FROM generations WHERE id = ?1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let Some(raw) = raw else {
            return Ok(None);
        };
        // A malformed blob is an error, not an absence, for the same reason
        // `analysis_disclosure_in` refuses to round one to the other.
        serde_json::from_str::<DeadClusterScan>(&raw)
            .map(Some)
            .map_err(|error| refusal(format!("stored dead-cluster scan is invalid: {error}")))
    }

    /// The dead-symbol rows and the analysis that qualifies them, against one
    /// generation. See [`DeadPage`].
    pub fn dead_page(&self, limit: usize) -> Result<Option<DeadPage>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let analysis = Self::analysis_disclosure_in(&snapshot, generation)?;
        let dead_clusters = Self::dead_clusters_in(&snapshot, generation)?;
        Ok(Some(DeadPage {
            generation,
            analysis,
            dead_clusters,
            rows: Self::dead_symbols_page_in(&snapshot, generation, limit)?,
            total_non_exempt: Self::count_dead_non_exempt_in(&snapshot, generation)?,
        }))
    }

    /// The ranked head of the non-exempt dead rows.
    ///
    /// The exempt filter and the limit both belong in SQL. `dead_symbols`
    /// discarded exempt rows in Rust after materialising every row of the
    /// generation, and then the budgeter kept a few dozen: measured at 80,000
    /// rows read to show 66, and on this repository 6,976 of 7,176 rows read
    /// were exempt and dropped on arrival. The work was proportional to the
    /// corpus, never to the answer.
    ///
    /// Ordering is unchanged. The old query sorted by `is_exempt` first, so
    /// filtering on it makes that key constant and leaves the surviving rows in
    /// exactly the order they already had.
    fn dead_symbols_page_in(
        snapshot: &Connection,
        gen: u32,
        limit: usize,
    ) -> Result<Vec<DeadSymbolReport>> {
        let mut stmt = snapshot.prepare(
            "SELECT symbol_name, file_path, confidence, is_exempt, exemption_reason
             FROM generation_dead_symbols
             WHERE generation_id = ?1 AND is_exempt = 0
             ORDER BY confidence DESC, file_path, symbol_name, ordinal
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![gen, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                Ok(DeadSymbolReport {
                    symbol_name: row.get(0)?,
                    file_path: row.get(1)?,
                    confidence: row.get(2)?,
                    is_exempt: row.get::<_, i64>(3)? != 0,
                    exemption_reason: row.get(4)?,
                })
            },
        )?;
        rows.collect()
    }

    fn count_dead_non_exempt_in(snapshot: &Connection, gen: u32) -> Result<usize> {
        snapshot.query_row(
            "SELECT COUNT(*) FROM generation_dead_symbols
             WHERE generation_id = ?1 AND is_exempt = 0",
            params![gen],
            |row| row.get::<_, i64>(0).map(|count| count as usize),
        )
    }

    pub fn latest_dead_symbols(&self) -> Result<Vec<DeadSymbolReport>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(Vec::new());
        };
        Self::dead_symbols_in(&snapshot, gen)
    }

    /// Every dead-symbol row of a generation, exempt ones included.
    ///
    /// Deliberately unbounded and unfiltered: its callers compare whole
    /// generations for incremental-vs-cold equivalence, where an omitted row is
    /// the failure they exist to detect. The bounded, non-exempt read the query
    /// engine wants is [`Store::dead_symbols_page_in`].
    fn dead_symbols_in(snapshot: &Connection, gen: u32) -> Result<Vec<DeadSymbolReport>> {
        let mut stmt = snapshot.prepare(
            "SELECT symbol_name, file_path, confidence, is_exempt, exemption_reason
             FROM generation_dead_symbols
             WHERE generation_id = ?1
             ORDER BY is_exempt, confidence DESC, file_path, symbol_name, ordinal",
        )?;
        let rows = stmt.query_map(params![gen], |row| {
            Ok(DeadSymbolReport {
                symbol_name: row.get(0)?,
                file_path: row.get(1)?,
                confidence: row.get(2)?,
                is_exempt: row.get::<_, i64>(3)? != 0,
                exemption_reason: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Rebuild clone candidates from the latest generation's symbol rows.
    ///
    /// Returns the candidates and the number of symbols with no signature. The
    /// second half is not decoration: `group_clones` needs it to report a
    /// denominator, and a caller that assumed zero would turn "most of this
    /// tree was never examined" into "this tree is clean".
    ///
    /// Reads every symbol row of one generation. `generation_nodes` is
    /// `WITHOUT ROWID` keyed on `(generation_id, ordinal)`, so this is a
    /// primary-key range scan rather than a table scan of every generation.
    pub fn latest_clone_candidates(&self) -> Result<(Vec<CloneCandidate>, usize)> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok((Vec::new(), 0));
        };
        let mut stmt = snapshot.prepare(
            "SELECT p.path, n.name, n.qualified_name, n.kind, n.span_start, n.span_end,
                    n.body_exact, n.body_structural, n.body_nodes
             FROM generation_nodes n
             JOIN paths p ON p.id = n.file_id
             WHERE n.generation_id = ?1
             ORDER BY n.ordinal",
        )?;
        let rows = stmt.query_map(params![gen], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
            ))
        })?;

        let mut candidates = Vec::new();
        let mut unsigned = 0usize;
        for row in rows {
            let (path, name, qn, kind, start, end, exact, structural, nodes) = row?;
            // All three or none. A row missing any part carries no usable
            // signature, and half a signature must not be grouped on.
            let (Some(exact), Some(structural), Some(nodes)) = (exact, structural, nodes) else {
                unsigned += 1;
                continue;
            };
            // A kind this binary does not know cannot be grouped: the Type-2
            // rule is stated in terms of kinds, and applying it to an
            // uninterpretable one would be a guess.
            let Some(kind) = SymbolKind::from_persisted(&kind) else {
                unsigned += 1;
                continue;
            };
            let (span_start, span_end) = checked_span(&path, &name, start, end)?;
            candidates.push(CloneCandidate {
                file_path: path,
                symbol_name: name,
                qualified_name: qn,
                span_start,
                span_end,
                kind,
                // Reverses the bit-preserving cast made on write.
                exact: exact as u64,
                structural: structural as u64,
                nodes: nodes.clamp(0, i64::from(u32::MAX)) as u32,
            });
        }
        Ok((candidates, unsigned))
    }

    /// Count dead-symbol rows whose persisted confidence is at least `min`
    /// in milliconfidence space, so `0.9` matches HIGH rows SQLite REAL
    /// cannot round-trip from `f32`.
    pub fn count_dead_at_least(&self, min: f32) -> Result<u32> {
        let min = checked_min_confidence(min)?;
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, gen)) = Self::latest_snapshot(&conn)? else {
            return Ok(0);
        };
        snapshot.query_row(
            "SELECT COUNT(*) FROM generation_dead_symbols
             WHERE generation_id = ?1
               AND CAST(ROUND(confidence * 1000) AS INTEGER)
                   >= CAST(ROUND(?2 * 1000) AS INTEGER)",
            params![gen, min],
            |row| row.get(0),
        )
    }

    pub(super) fn latest_generation_id_locked(conn: &Connection) -> Result<Option<u32>> {
        conn.query_row(
            "SELECT id FROM generations ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
    }

    /// The latest generation, and a read snapshot its rows are still in.
    ///
    /// Every reader here resolved "the latest generation" with one statement
    /// and then read that generation's rows with another. Those are two
    /// statements in SQLite's autocommit mode, which means **two snapshots**:
    /// `lock_conn` is a Rust mutex and serialises this process's own threads,
    /// it does not hold a database read. A second *process* — the daemon,
    /// which is designed to commit while clients query — could therefore
    /// commit and prune between them, and a reader that had pinned a
    /// now-deleted generation read zero rows out of it and returned them as
    /// the answer.
    ///
    /// Measured with a real second process committing and pruning in a loop
    /// against a store that always held twelve files, 20 s per run, release
    /// build:
    ///
    /// | retention | reads | false-empty answers |
    /// |---|---|---|
    /// | `prune(1)` | 182,781 | 31 (18 `search_page`, 6 `latest_edges_for_file`, 4 `latest_extractions`, 3 `all_symbols`) |
    /// | `prune(GENERATION_RETENTION)` | 192,970 | 1 (`latest_edges_for_file`) |
    ///
    /// Every one of those is a Class A failure and not merely a stale answer:
    /// `search_page` returned `total: 0, rows: []`, which is byte-identical to
    /// a query that ran and matched nothing, and an empty `callers_of` is read
    /// by `dev verify` as proof a symbol has no callers.
    ///
    /// A `DEFERRED` transaction takes its snapshot at its first statement,
    /// which is the generation lookup below, and holds it for every later read
    /// — so the generation a reader pins is still there, with its rows, for as
    /// long as it is reading. It takes no write lock and blocks no writer; the
    /// only thing it defers is WAL truncation, for the microseconds to
    /// milliseconds a read takes. Rolled back on drop, which for a read
    /// transaction is free.
    ///
    /// `None` means the store holds no generation at all, which is a different
    /// answer from a generation that matched nothing.
    pub(super) fn latest_snapshot(
        conn: &Connection,
    ) -> Result<Option<(rusqlite::Transaction<'_>, u32)>> {
        let snapshot = conn.unchecked_transaction()?;
        match Self::latest_generation_id_locked(&snapshot)? {
            Some(generation) => Ok(Some((snapshot, generation))),
            None => Ok(None),
        }
    }

    /// Refuse a search whose index is not there, instead of reporting that the
    /// corpus does not contain the query.
    ///
    /// The full-text index lives in `nodes_fts`/`nodes_fts_map`, structures
    /// separate from `generation_nodes`, and the store already assumes they can
    /// be lost on their own: [`Store::repair_fts`] and `devmap repair --fts`
    /// exist for that state and nothing else. Nothing *detected* it. Measured
    /// on a store whose four symbol rows were intact and whose index rows had
    /// been removed:
    ///
    /// ```text
    /// all_symbols  = 4
    /// search_page  = Some(SearchPage { generation: 1, total: 0, rows: [] })
    /// status       = node_count 4, degraded_reason: None
    /// ```
    ///
    /// `total: 0, rows: []` is byte-identical to a healthy index that matched
    /// nothing, so every `search` against that store answered "this repository
    /// does not contain that symbol" — permanently, and without ever naming the
    /// one command that fixes it.
    ///
    /// **What this detects, and what it does not.** It answers "does this
    /// generation have any searchable row at all", not "is the index complete".
    /// A *partially* lost index is not caught: with half of one generation's
    /// postings deleted, search returned 10 of 40 symbols and reported the 10
    /// as the whole answer, and this check passes that store. Catching a
    /// partial loss means counting the generation's index rows against its
    /// symbol rows on every query, which is O(symbols) on a path that is
    /// otherwise a bounded FTS lookup. `status` makes that count instead, once
    /// per generation (`fts_health_locked`), and reports a partial loss there;
    /// `devmap repair --fts` rebuilds the index unconditionally and is the
    /// complete answer. This is the cheap one that turns the total loss from
    /// silence into a refusal at the moment a search would have hidden it.
    ///
    /// Called only when a search came back empty, so a query that matched
    /// nothing pays two `EXISTS` probes — both primary-key range lookups on
    /// `WITHOUT ROWID` tables — and a query that matched pays nothing.
    fn require_searchable_index(conn: &Connection, gen: u32) -> Result<()> {
        let has_symbols: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM generation_nodes WHERE generation_id = ?1)",
            params![gen],
            |row| row.get::<_, i64>(0).map(|found| found != 0),
        )?;
        // A generation that indexed no symbols has nothing for the index to
        // hold, so its empty answer is the truth rather than a missing check.
        if !has_symbols {
            return Ok(());
        }
        // Both halves of the desync fail here, and they fail differently: the
        // map can be lost while the postings survive (no row matches the
        // generation), and the postings can be lost while the map survives (the
        // rowid join finds nothing). One query covers both.
        let searchable: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM nodes_fts_map m
                JOIN nodes_fts f ON f.rowid = m.rowid_ref
                WHERE m.generation_id = ?1
             )",
            params![gen],
            |row| row.get::<_, i64>(0).map(|found| found != 0),
        )?;
        if searchable {
            return Ok(());
        }
        Err(refusal(format!(
            "generation {gen} has symbol rows but no full-text index rows, so \
             this search could not run and its empty result is not an answer \
             about the repository; rebuild the index with `devmap repair --fts`"
        )))
    }
}
