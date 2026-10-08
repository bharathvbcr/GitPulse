use super::Store;
#[cfg(feature = "parse")]
use super::{
    bucket_identities, charge, claim_matching_candidate, decode_stored_outcome, edge_tuple,
    lock_conn, refusal, stored_is_parse_failure, validate_head_sha, EdgeTuple, GenerationWriteOpts,
    RowSetDigest, StoredPayload, UnresolvedTuple, WriteBreakdown,
};
#[cfg(feature = "parse")]
use crate::schema::BUILD_HISTORY_RETENTION;
#[cfg(feature = "parse")]
use devmap_analyze::model::AnalysisSummary;
#[cfg(feature = "parse")]
use devmap_extract::model::{confidence_millis, EdgeKind, Extraction};
#[cfg(feature = "parse")]
use devmap_resolve::model::ResolutionResult;
#[cfg(feature = "parse")]
use rusqlite::OptionalExtension;
#[cfg(feature = "parse")]
use rusqlite::{params, Connection, Result};
#[cfg(feature = "parse")]
use std::path::Path;

impl Store {
    /// The id of the stored payload with this identity, inserting it if new.
    ///
    /// Keyed by the **file** plus the four fields the extraction cache keys on.
    ///
    /// `file_id` is in the key and must be: a payload is a serialized
    /// `Extraction`, which carries its own `file_path`, so content-addressing
    /// alone collapses two byte-identical files into one payload and makes both
    /// report the same path. A symlink and its target are byte-identical by
    /// construction, and the end-to-end symlink test caught exactly that on the
    /// first run.
    ///
    /// What B3 deduplicates is the same file, unchanged, across generations —
    /// 1,530 of the 1,530 duplicate rows measured on this repository — so
    /// nothing real is lost by narrowing the key.
    ///
    /// SELECT-then-INSERT rather than an upsert because the unique index is on
    /// COALESCE expressions — `grammar_version` and `analyzer_version` are
    /// nullable and SQLite treats NULLs as distinct inside a UNIQUE index, so a
    /// plain constraint would let identical NULL-version payloads both insert.
    /// The probe uses the same expressions the index does. Safe without a
    /// retry loop: every caller holds the generation write transaction, and the
    /// store has one writer.
    #[cfg(feature = "parse")]
    fn ensure_payload_id(tx: &Connection, payload: StoredPayload<'_>) -> Result<i64> {
        let StoredPayload {
            file_id,
            content_hash,
            language,
            grammar_version,
            analyzer_version,
            parse_outcome_json,
            engine_json,
            extraction_json,
        } = payload;
        if let Some(id) = tx
            .prepare_cached(
                "SELECT payload_id FROM file_payloads
                  WHERE file_id = ?1 AND content_hash = ?2 AND language = ?3
                    AND COALESCE(grammar_version, '') = ?4
                    AND COALESCE(analyzer_version, '') = ?5",
            )?
            .query_row(
                params![
                    file_id,
                    content_hash,
                    language,
                    grammar_version,
                    analyzer_version
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        tx.prepare_cached(
            "INSERT INTO file_payloads
             (file_id, content_hash, language, grammar_version, analyzer_version,
              parse_outcome_json, engine_json, extraction_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(params![
            file_id,
            content_hash,
            language,
            grammar_version,
            analyzer_version,
            parse_outcome_json,
            engine_json,
            extraction_json
        ])?;
        Ok(tx.last_insert_rowid())
    }

    /// Generation-scoped FTS rowid: high 32 bits = generation, low 32 = ordinal.
    pub(super) fn fts_rowid(gen_id: u32, node_ord: u32) -> i64 {
        ((gen_id as i64) << 32) | (node_ord as i64)
    }

    /// Writes a generation.
    ///
    /// Gated on `parse` by scope, not by need: a build without grammars answers
    /// questions about a persisted map and never builds one, so it does not
    /// link the write path. Nothing here calls a grammar. The identity each
    /// payload is stamped with comes from
    /// `devmap_extract::cache::current_payload_identity`, which answers in both
    /// configurations, and with these gates and the `CacheKey` constructors' gate
    /// removed the store compiles feature-off without a warning (checked
    /// 2026-10-06). Ungating is therefore a decision about what an
    /// embedder links, and it should come with a feature-off test that writes a
    /// generation, not just with the gates removed.
    #[cfg(feature = "parse")]
    pub fn save_generation(
        &self,
        extractions: &[Extraction],
        resolution: &ResolutionResult,
        analysis: &AnalysisSummary,
    ) -> Result<u32> {
        self.save_generation_with_opts(
            extractions,
            resolution,
            analysis,
            GenerationWriteOpts::default(),
        )
    }

    /// Differential membership write with deletion reconciliation (B3 + N2).
    ///
    /// Steps:
    /// 1. Carry forward prior-generation rows whose source file is not in affected∪deleted
    /// 2. Insert freshly resolved rows for affected (from `extractions`)
    /// 3. Deleted paths contribute zero rows (explicit absence — N2)
    #[cfg(feature = "parse")]
    pub fn save_generation_with_opts(
        &self,
        extractions: &[Extraction],
        resolution: &ResolutionResult,
        analysis: &AnalysisSummary,
        opts: GenerationWriteOpts,
    ) -> Result<u32> {
        self.save_generation_with_metadata(extractions, resolution, analysis, opts, "unknown")
    }

    #[cfg(feature = "parse")]
    pub fn save_generation_with_metadata(
        &self,
        extractions: &[Extraction],
        resolution: &ResolutionResult,
        analysis: &AnalysisSummary,
        opts: GenerationWriteOpts,
        head_sha: &str,
    ) -> Result<u32> {
        self.save_generation_timed(extractions, resolution, analysis, opts, head_sha)
            .map(|(gen_id, _)| gen_id)
    }

    /// [`save_generation_with_metadata`](Self::save_generation_with_metadata),
    /// and what the write spent on each relation.
    ///
    /// The split lives here rather than in a profiler beside the store because
    /// two of the relations cannot be separated from outside: the node and
    /// full-text writes are one interleaved loop, an FTS rowid being derived
    /// from the node ordinal the loop just produced. See [`WriteBreakdown`] for
    /// what the numbers do and do not account for.
    ///
    /// Every existing caller keeps the `u32` it had; the breakdown is a second
    /// return value on a second entry point, so the sixty-odd call sites of
    /// `save_generation*` are untouched by a change none of them asked for.
    #[cfg(feature = "parse")]
    pub fn save_generation_timed(
        &self,
        extractions: &[Extraction],
        resolution: &ResolutionResult,
        analysis: &AnalysisSummary,
        opts: GenerationWriteOpts,
        head_sha: &str,
    ) -> Result<(u32, WriteBreakdown)> {
        let mut spent = WriteBreakdown::default();
        // Started before the first thing the write does, so the residual
        // `attribute_residual` computes below covers the whole call and not
        // just the part after some later landmark.
        let write_started = std::time::Instant::now();
        self.refuse_if_read_only()?;
        validate_head_sha(head_sha)?;
        let mut unique_paths = std::collections::BTreeSet::new();
        for extraction in extractions {
            if !unique_paths.insert(extraction.file_path.as_str()) {
                return Err(refusal(format!(
                    "duplicate extraction path in generation input: {}",
                    extraction.file_path
                )));
            }
        }
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(Self::GENERATION_TX_BEHAVIOR)?;
        if let Some(root) = &opts.repo_root {
            Self::bind_repo_root_in(&tx, &Self::normalized_repo_root(Path::new(root))?)?;
        }
        let repo_root: Option<String> = tx.query_row(
            "SELECT repo_root FROM pending_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        // One path-id memo for the whole generation write. See
        // `ensure_path_id_cached`: the edge loop alone asks for two ids per
        // edge drawn from a file set two orders of magnitude smaller.
        let mut path_ids: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        // Keep the summary semantically complete even though dead rows also
        // have a normalized table. An authoritative-looking empty list makes
        // latest_analysis() disagree with latest_dead_symbols().
        //
        // Serialized straight from the borrow. This used to clone the summary
        // first and serialize the clone, for no reason the three readers
        // needed: the clone was only ever read — once here and twice for the
        // `dead_confident`/`dead_ambiguous` counts far below — and a summary of
        // this repository carries 4,927 dead-symbol rows, so the copy was
        // several thousand string allocations that existed to be dropped.
        let analysis_json = serde_json::to_string(analysis)
            .map_err(|error| refusal(format!("analysis serialization failed: {error}")))?;

        tx.execute(
            "INSERT INTO generations (created_at, head_sha, analysis_json, repo_root)
             VALUES (?1, ?2, ?3, ?4)",
            params![now, head_sha, analysis_json, repo_root],
        )?;
        let gen_id = u32::try_from(tx.last_insert_rowid()).map_err(|_| {
            refusal("generation ID space exhausted; cannot represent another generation")
        })?;

        let prev_gen: Option<u32> = tx
            .query_row(
                "SELECT id FROM generations WHERE id < ?1 ORDER BY id DESC LIMIT 1",
                params![gen_id],
                |row| row.get(0),
            )
            .optional()?;

        let affected: std::collections::HashSet<String> =
            opts.affected_paths.iter().cloned().collect();
        let deleted: std::collections::HashSet<String> =
            opts.deleted_paths.iter().cloned().collect();
        let full_rewrite = affected.is_empty() && deleted.is_empty();

        // Which prior rows may be reused at all.
        //
        // "Unaffected" used to be the whole test, and unaffected meant only
        // "content hash unchanged". That is not enough to make a stored payload
        // reusable: it must also have been produced by the extractor and
        // grammar this build is running. The extraction *cache* has always
        // known that — its key carries both versions — but the generation
        // carry-forward did not, so after two schema bumps DevCouncil's store
        // still held 1,152 `extract-v23` rows under a `v25` binary, and the
        // first changed build was refused by the edge/analysis equality below
        // (65,615 stored against 65,798 analysed) with no way forward but
        // deleting the database.
        //
        // Same three fields the cache keys on, asked of the same owner, so the
        // two cannot drift: content hash, grammar version, analyzer version.
        // A NULL version is a row from before those columns existed — unknown
        // identity is not a matching identity, so it is not reused.
        let current_hashes: std::collections::HashMap<&str, i64> = extractions
            .iter()
            .map(|ext| (ext.file_path.as_str(), ext.content_hash as i64))
            .collect();
        let mut carry: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut stale_identity: Vec<String> = Vec::new();
        if let Some(prev) = prev_gen {
            if !full_rewrite {
                let mut stmt = tx.prepare(
                    "SELECT p.path, f.language, f.content_hash, f.grammar_version, f.analyzer_version
                     FROM generation_files f
                     JOIN paths p ON p.id = f.file_id
                     WHERE f.generation_id = ?1",
                )?;
                let rows = stmt.query_map(params![prev], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })?;
                for row in rows {
                    let (path, language, content_hash, grammar, analyzer) = row?;
                    if deleted.contains(&path) || affected.contains(&path) {
                        continue;
                    }
                    let (current_grammar, current_analyzer) =
                        devmap_extract::cache::current_payload_identity(&language);
                    let identity_matches = grammar.as_deref() == Some(current_grammar.as_str())
                        && analyzer.as_deref() == Some(current_analyzer.as_str());
                    // A content hash that moved without the path being declared
                    // affected means the caller's affected set is wrong; the
                    // stored payload describes different bytes either way.
                    let content_matches = current_hashes
                        .get(path.as_str())
                        .is_none_or(|hash| *hash == content_hash);
                    if identity_matches && content_matches {
                        carry.insert(path);
                    } else {
                        stale_identity.push(path);
                    }
                }
            }
        }
        // A stale path this write cannot replace would simply vanish from the
        // generation — the file silently absent from the map rather than out of
        // date. Refused loudly instead, naming the remedy, because the callers
        // that can rebuild it (the CLI's cold-build closure, the daemon's
        // full resync) both check the identity first and never reach here.
        let unreplaceable: Vec<&String> = stale_identity
            .iter()
            .filter(|path| !current_hashes.contains_key(path.as_str()))
            .collect();
        if !unreplaceable.is_empty() {
            return Err(refusal(format!(
                "cannot carry forward {} file(s) whose stored payload was produced by a different \
                 extractor or grammar (for example {}); rebuild this generation from a full \
                 extraction rather than a differential write",
                unreplaceable.len(),
                unreplaceable[0]
            )));
        }

        // Carrying a payload forward is now a `payload_id`, not a payload.
        //
        // This block used to SELECT each unaffected file's row — language,
        // hashes, and a `parse_outcome_json`, `engine_json` and
        // `extraction_json` averaging 53.7 KB together — into Rust and INSERT
        // it back under the new generation id. Measured on this repository, a
        // one-line edit to one file moved **~82 MB of JSON** that way, and left
        // 1,530 byte-identical duplicate rows behind. Since v17 the bytes live
        // once in `file_payloads` keyed by the identity the extraction cache
        // already uses, and a carried file is a 16-byte membership row.
        //
        // One statement, executed inside SQLite, rather than a loop: there is
        // nothing for Rust to decide here — `carry` has already decided it —
        // and a round trip per file was the whole cost.
        if let Some(prev) = prev_gen {
            if !full_rewrite && !carry.is_empty() {
                let mut stmt = tx.prepare(
                    "INSERT INTO generation_file_rows (generation_id, file_id, payload_id)
                     SELECT ?1, m.file_id, m.payload_id
                       FROM generation_file_rows m
                       JOIN paths p ON p.id = m.file_id
                      WHERE m.generation_id = ?2 AND p.path = ?3",
                )?;
                let _charge = charge(&mut spent.file_rows);
                for path in &carry {
                    stmt.execute(params![gen_id, prev, path])?;
                }
            }
        }

        for extraction in extractions {
            // Not "is it affected" but "was it carried". They differ exactly
            // when a prior payload failed the identity gate: the file is
            // unaffected, nothing was carried for it, and its fresh rows are
            // the only ones this generation will have.
            if !full_rewrite && carry.contains(&extraction.file_path) {
                continue;
            }
            if deleted.contains(&extraction.file_path) {
                continue;
            }
            // SQLite has no unsigned integer type. Preserve all 64 bits using
            // the same two's-complement representation as the extraction cache.
            let content_hash = extraction.content_hash as i64;
            let parse_json = serde_json::to_string(&extraction.parse_outcome).map_err(|error| {
                refusal(format!(
                    "parse outcome serialization failed for {}: {error}",
                    extraction.file_path
                ))
            })?;
            let engine_json = serde_json::to_string(&extraction.engine).map_err(|error| {
                refusal(format!(
                    "extraction engine serialization failed for {}: {error}",
                    extraction.file_path
                ))
            })?;
            let mut durable_extraction = extraction.for_durable_store();
            durable_extraction.source_code = None;
            let extraction_json = serde_json::to_string(&durable_extraction).map_err(|error| {
                refusal(format!(
                    "extraction serialization failed for {}: {error}",
                    extraction.file_path
                ))
            })?;
            let file_id = Self::ensure_path_id_cached(&tx, &mut path_ids, &extraction.file_path)?;
            // The identity this payload was produced with, so a stored row is
            // usable as a cache fallback without discarding the staleness
            // guarantee the cache key exists to enforce (SC8). Since v17 it is
            // also the payload's own key.
            let identity = devmap_extract::cache::CacheKey::for_extraction(extraction);
            let payload_id = Self::ensure_payload_id(
                &tx,
                StoredPayload {
                    file_id,
                    content_hash,
                    language: &extraction.language,
                    grammar_version: &identity.grammar_version,
                    analyzer_version: &identity.analyzer_version,
                    parse_outcome_json: &parse_json,
                    engine_json: &engine_json,
                    extraction_json: &extraction_json,
                },
            )?;
            {
                let _charge = charge(&mut spent.file_rows);
                tx.execute(
                    "INSERT INTO generation_file_rows (generation_id, file_id, payload_id)
                     VALUES (?1, ?2, ?3)",
                    params![gen_id, file_id, payload_id],
                )?;
            }
        }

        let mut node_ord: u32 = 0;

        // Carry forward unchanged files from previous generation (differential).
        if let Some(prev) = prev_gen {
            if !full_rewrite {
                // The signature columns are carried with the row. Dropping
                // them here would make every unchanged file look unsigned after
                // one incremental build, and a clone report reads unsigned as
                // "not examined" — so the whole tree would quietly go dark
                // except the handful of files that happened to be edited.
                let mut stmt = tx.prepare(
                    "SELECT p.path, n.name, n.qualified_name, n.kind, n.span_start, n.span_end, n.is_exported,
                            n.body_exact, n.body_structural, n.body_nodes
                     FROM generation_nodes n
                     JOIN paths p ON p.id = n.file_id
                     WHERE n.generation_id = ?1",
                )?;
                let rows = stmt.query_map(params![prev], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ))
                })?;
                for row in rows {
                    // The decode is charged to `nodes` with the insert it feeds:
                    // reading the previous generation's 18,501 rows back out is
                    // the carry-forward's cost as much as writing them is, and
                    // splitting the two would leave the larger half unnamed.
                    let (path, name, qn, kind, start, end, exported, b_exact, b_struct, b_nodes) = {
                        let _charge = charge(&mut spent.nodes);
                        row?
                    };
                    if !carry.contains(&path) {
                        continue;
                    }
                    let file_id = Self::ensure_path_id_cached(&tx, &mut path_ids, &path)?;
                    {
                        let _charge = charge(&mut spent.nodes);
                        tx.prepare_cached(
                            "INSERT INTO generation_nodes (generation_id, ordinal, file_id, name, qualified_name, kind, span_start, span_end, is_exported, body_exact, body_structural, body_nodes)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        )?
                        .execute(params![
                            gen_id, node_ord, file_id, name, qn, kind, start, end, exported,
                            b_exact, b_struct, b_nodes
                        ])?;
                    }
                    let fts_rowid = Self::fts_rowid(gen_id, node_ord);
                    {
                        let _charge = charge(&mut spent.fts);
                        tx.prepare_cached(
                            "INSERT INTO nodes_fts (rowid, name, qualified_name, path) VALUES (?1, ?2, ?3, ?4)",
                        )?
                        .execute(params![fts_rowid, name, qn, path])?;
                        tx.prepare_cached(
                            "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, ?2)",
                        )?
                        .execute(params![fts_rowid, gen_id])?;
                    }
                    node_ord += 1;
                }
            }
            if !carry.is_empty() {
                let mut literal_carry = tx.prepare(
                    "INSERT INTO generation_literals
                        (generation_id, file_id, line, span_start, value, qualified_name, symbol_name)
                     SELECT ?1, l.file_id, l.line, l.span_start, l.value, l.qualified_name, l.symbol_name
                       FROM generation_literals l
                       JOIN paths p ON p.id = l.file_id
                      WHERE l.generation_id = ?2 AND p.path = ?3",
                )?;
                for path in &carry {
                    literal_carry.execute(params![gen_id, prev, path])?;
                }
            }
        }

        // Insert fresh rows for every extraction whose file was not carried.
        for ext in extractions {
            if !full_rewrite && carry.contains(&ext.file_path) {
                continue;
            }
            if deleted.contains(&ext.file_path) {
                continue;
            }
            let file_id = Self::ensure_path_id_cached(&tx, &mut path_ids, &ext.file_path)?;
            for sym in &ext.symbols {
                {
                    let _charge = charge(&mut spent.nodes);
                    tx.execute(
                        "INSERT INTO generation_nodes (generation_id, ordinal, file_id, name, qualified_name, kind, span_start, span_end, is_exported, body_exact, body_structural, body_nodes)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            gen_id,
                            node_ord,
                            file_id,
                            sym.name,
                            sym.qualified_name,
                            sym.kind.as_str(),
                            i64::try_from(sym.span.start_byte).map_err(|error| {
                                rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                            })?,
                            i64::try_from(sym.span.end_byte).map_err(|error| {
                                rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                            })?,
                            sym.is_exported as i32,
                            // SQLite integers are signed. The cast is
                            // bit-preserving and reversed on read, so the stored
                            // value round-trips even though half the hash space
                            // reads back negative.
                            sym.body_signature.map(|s| s.exact as i64),
                            sym.body_signature.map(|s| s.structural as i64),
                            sym.body_signature.map(|s| i64::from(s.nodes))
                        ],
                    )?;
                }
                let fts_rowid = Self::fts_rowid(gen_id, node_ord);
                {
                    let _charge = charge(&mut spent.fts);
                    tx.prepare_cached(
                        "INSERT INTO nodes_fts (rowid, name, qualified_name, path) VALUES (?1, ?2, ?3, ?4)",
                    )?
                    .execute(params![fts_rowid, sym.name, sym.qualified_name, ext.file_path])?;
                    tx.prepare_cached(
                        "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, ?2)",
                    )?
                    .execute(params![fts_rowid, gen_id])?;
                }
                node_ord += 1;
            }
            for lit in &ext.literals {
                if lit.value.is_empty() || lit.value.as_bytes().contains(&0) {
                    continue;
                }
                tx.execute(
                    "INSERT OR IGNORE INTO generation_literals
                        (generation_id, file_id, line, span_start, value, qualified_name, symbol_name)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        gen_id,
                        file_id,
                        i64::from(lit.line),
                        i64::try_from(lit.start_byte).unwrap_or(i64::MAX),
                        lit.value,
                        lit.enclosing_qualified_name,
                        lit.enclosing_name,
                    ],
                )?;
            }
        }

        // Edges come from this build's resolution, always — never from the
        // previous generation.
        //
        // Carrying them forward was sound only while a changed build resolved
        // just the changed files. It no longer does: the build resolves the
        // whole tree so that the analysis means the same thing on both paths,
        // which means `resolution.edges` already holds the current, correct
        // edge for every file, unaffected ones included. Copying the prior
        // generation's rows over the top of that was not a saving — it read
        // rows and re-inserted the same number — it was only a way to keep an
        // older answer.
        //
        // And the answer did drift, in two ways the affected-set closure
        // cannot see. A payload produced by an older extractor stayed until its
        // file's bytes changed. An edge from an unchanged file into a target
        // whose *identity* moved without its name changing — a Go package
        // renamed, an import alias repointed — resolves differently today while
        // the source file itself never entered the affected set. Both showed up
        // as the same symptom: the equality below refusing the write.
        //
        // Writing every resolved edge makes that equality true by construction
        // rather than by argument. It stays below as a regression check.
        // Since v18 the write is the *difference* between the freshly resolved
        // tuple multiset and the one already valid, not the whole set.
        //
        // Measured on this repository: two consecutive builds one appended line
        // apart held 101,446 and 101,447 distinct edge tuples, one appeared and
        // none disappeared — and the store wrote all 102,083 rows again anyway,
        // because the relation was keyed by generation. The comparison below is
        // ~100k in-memory tuple compares; the write that follows is the delta.
        //
        // Nothing above changes: `resolution.edges` is still the whole tree's
        // resolution, so a carried row is one this build re-derived and found
        // identical, not one it declined to look at. That is the distinction the
        // paragraph above is about, and it is why the equality below is still
        // structural.
        //
        // The multiset is built in one pass over `resolution.edges` and the
        // inserts walk that same slice again, so the rows land in the resolver's
        // emission order. Iterating the map instead would have been shorter and
        // was measurably wrong: a `HashMap`'s order is arbitrary and varies per
        // process, so the inserts landed in no order at all, and the read path's
        // sort — which is handed the rows in stored order — lost the nearly
        // sorted input it had been getting for free. A cold `devmap impact` on
        // this repository went 115 ms to 150 ms for **the same instruction
        // count** (1.192 G against 1.188 G) and 32% more cycles: pure memory
        // stalls in a sort with a worse starting order.
        //
        // [`edge_tuple`] is the one owner of what an edge's identity is, called
        // by both passes, so the pass that decides what to write and the pass
        // that writes it cannot come to disagree about which rows they mean.
        //
        // Every field borrows, and the kinds are formatted once each into
        // `kind_labels` rather than once per edge: `format!("{:?}", kind)` for
        // 102,083 edges is 102,083 heap allocations held for the length of the
        // write. It is the same string by construction, because it is the same
        // expression.
        //
        // Since v19 the comparison is itself a difference. An edge belongs to
        // its source file and so does an unresolved call, so a file whose
        // freshly resolved rows digest to what the previous generation recorded
        // holds exactly the rows already stored: nothing of it is read back,
        // nothing of it is compared, and nothing of it is written. The measured
        // shape this addresses is a build that stores one row and reads two
        // hundred thousand -- 107,257 edge rows and 91,703 ledger rows on this
        // repository, 66% of `persist:write`.
        //
        // The digest is over what the *resolver produced*, never over what the
        // caller said was affected. Those differ in exactly the case the
        // paragraphs above describe: an edge from an unchanged file into a
        // target whose identity moved resolves differently today while its
        // source file never enters the affected set. Its digest moves with it
        // and its comparison runs. Scoping on the affected set instead would
        // reintroduce the staleness this loop refuses.
        let scope_by_digest = !opts.verify_every_row && !full_rewrite && prev_gen.is_some();
        let mut stored_edge_digests: std::collections::HashMap<u32, RowSetDigest> =
            std::collections::HashMap::new();
        let mut stored_unresolved_digests: std::collections::HashMap<u32, RowSetDigest> =
            std::collections::HashMap::new();
        if let (true, Some(prev)) = (scope_by_digest, prev_gen) {
            let _charge = charge(&mut spent.digests);
            // Both sides keyed by `paths.id`, and the join to `paths` gone with
            // the reason for it.
            //
            // Until v22 the ledger stored `source_file` as text while
            // `edge_rows` stored an id, so this statement had to fetch the path
            // as well and the two scans below compared against different keys
            // for the same file. The ledger stores `source_file_id` now, so
            // there is one key, one column, and no per-row translation on
            // either side.
            let mut stmt = tx.prepare(
                "SELECT file_id, edge_rows, edge_lo, edge_hi,
                        unresolved_rows, unresolved_lo, unresolved_hi
                   FROM generation_file_digests
                  WHERE generation_id = ?1",
            )?;
            let mut rows = stmt.query(params![prev])?;
            while let Some(row) = rows.next()? {
                let file_id: u32 = row.get(0)?;
                stored_edge_digests.insert(
                    file_id,
                    RowSetDigest::from_columns(row.get(1)?, row.get(2)?, row.get(3)?),
                );
                stored_unresolved_digests.insert(
                    file_id,
                    RowSetDigest::from_columns(row.get(4)?, row.get(5)?, row.get(6)?),
                );
            }
        }

        let mut kind_labels: std::collections::HashMap<EdgeKind, String> =
            std::collections::HashMap::new();
        let edge_charge = charge(&mut spent.edges);
        for edge in &resolution.edges {
            kind_labels
                .entry(edge.edge_kind)
                .or_insert_with(|| format!("{:?}", edge.edge_kind));
        }
        // Which edges are in this generation at all, and under which path ids.
        //
        // `None` is the one owner of "not in this generation": deleted paths are
        // not extracted, so a resolution over the current tree has no edge
        // touching one, and the guard stays for callers that pass a resolution
        // computed before the deletion. Every pass below reads this rather than
        // re-asking `deleted`, so they cannot come to disagree about which edges
        // they are talking about.
        let mut edge_ids: Vec<Option<(u32, u32)>> = Vec::with_capacity(resolution.edges.len());
        let mut edge_ord: u32 = 0;
        for edge in &resolution.edges {
            if deleted.contains(&edge.source_file) || deleted.contains(&edge.target_file) {
                edge_ids.push(None);
                continue;
            }
            let src_f_id = Self::ensure_path_id_cached(&tx, &mut path_ids, &edge.source_file)?;
            let tgt_f_id = Self::ensure_path_id_cached(&tx, &mut path_ids, &edge.target_file)?;
            edge_ids.push(Some((src_f_id, tgt_f_id)));
            edge_ord += 1;
        }

        // What this build resolved, per source file, as one comparable value
        // each. Computed on every build and not only on scoped ones: it is what
        // the *next* build compares against, so a build that skipped it would
        // cost the following one the whole saving.
        let mut fresh_edge_digests: std::collections::HashMap<u32, RowSetDigest> =
            std::collections::HashMap::new();
        for (index, edge) in resolution.edges.iter().enumerate() {
            let Some((src_f_id, tgt_f_id)) = edge_ids[index] else {
                continue;
            };
            fresh_edge_digests
                .entry(src_f_id)
                .or_default()
                .absorb(&edge_tuple(edge, &kind_labels, src_f_id, tgt_f_id));
        }
        // How many rows each of those files *actually* has live, asked of the
        // rows rather than of the record.
        //
        // A digest is a claim a previous build recorded about what it wrote,
        // and a claim is not the store. `incremental_equivalence.rs` is built
        // on the case where the two part company: `drop_stored_edges` deletes
        // live rows behind the write path, standing in for an older kernel that
        // recorded fewer of them, and the build is required to commit the cold
        // answer anyway. A delta that trusted the digest alone would read
        // "unchanged", skip the file, and leave those rows missing for ever —
        // which is the class
        // `stored_edges_that_disagree_with_a_fresh_resolution_are_replaced_not_carried`
        // exists to refuse, and which this loop's own comment refuses in the
        // paragraph above.
        //
        // So a file is skipped only when the rows agree with the record as well
        // as with this build: one integer column per live row, no allocation
        // and no comparison, against the four string allocations and the
        // field-by-field compare the skip avoids.
        //
        // **What it covers, stated because the gap is the safety argument.**
        // Every row added to or removed from a file by anything other than this
        // write path — a repair, an older kernel, a hand-edited database. Not a
        // content column overwritten in place with the row count preserved, and
        // nothing outside a test does that: the only `UPDATE` either ranged
        // table takes in this crate sets `valid_to`, twice, in this function.
        // A row's content is written by its `INSERT` and never again.
        let mut live_edge_rows: std::collections::HashMap<u32, u64> = fresh_edge_digests
            .keys()
            .map(|file_id| (*file_id, 0))
            .collect();
        if scope_by_digest {
            let mut stmt =
                tx.prepare("SELECT source_file_id FROM edge_rows WHERE valid_to IS NULL")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                // A file this build resolved nothing for is not a candidate to
                // skip, so its live rows need no count — the scan below
                // compares and closes them either way.
                if let Some(count) = live_edge_rows.get_mut(&row.get::<_, u32>(0)?) {
                    *count += 1;
                }
            }
        }
        // A file is unchanged only when a digest was *found* and matched. The
        // three ways there can be no entry — a v18 store that migrated with an
        // empty table, a file this generation resolved for the first time, a
        // file whose rows the previous build wrote under `verify_every_row` —
        // all land on "compare it", which is v18's behaviour exactly. Absence
        // is never equality.
        let unchanged_edge_files: std::collections::HashSet<u32> = if scope_by_digest {
            fresh_edge_digests
                .iter()
                .filter(|(file_id, fresh)| {
                    stored_edge_digests.get(file_id) == Some(*fresh)
                        && live_edge_rows.get(file_id) == Some(&fresh.rows)
                })
                .map(|(file_id, _)| *file_id)
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        // A *multiset*, not a set. 475 edge tuples of this repository occur more
        // than once in one generation (1,111 rows); collapsing them would drop
        // rows the analysis counted and make the equality below refuse the
        // build. The multiset is `matched` — one bit per resolved edge — rather
        // than a count per distinct tuple, so two identical edges are two
        // entries that are consumed one at a time.
        //
        // One closure, named and handed to both the bucketing and the search,
        // rather than the same body written out twice. `bucket_identities`'
        // doc says why the two must agree about what an identity *is*; since
        // v19 they must also agree about which indexes are offered at all, and
        // a second copy of the `unchanged_edge_files` test is exactly the drift
        // that doc describes — a structure built under one rule and searched
        // under another, whose symptom is not a crash but a build that keeps
        // rows it should have closed.
        let edge_identity = |index: usize| -> Option<EdgeTuple<'_>> {
            let (src_f_id, tgt_f_id) = edge_ids[index]?;
            // Not a candidate for anything: this file's live rows are not read
            // back, so nothing can claim them, and its fresh rows are already
            // stored, so nothing may insert them.
            if unchanged_edge_files.contains(&src_f_id) {
                return None;
            }
            Some(edge_tuple(
                &resolution.edges[index],
                &kind_labels,
                src_f_id,
                tgt_f_id,
            ))
        };
        let (edge_buckets, edge_chain) = bucket_identities(resolution.edges.len(), edge_identity);
        // Pre-claimed rather than left false: an unchanged file's rows are
        // already valid, so the insert loop below must not write them again,
        // and it skips exactly what is marked here.
        let mut edge_matched: Vec<bool> = (0..resolution.edges.len())
            .map(|index| {
                edge_ids[index]
                    .is_some_and(|(src_f_id, _)| unchanged_edge_files.contains(&src_f_id))
            })
            .collect();

        // The rows already valid, streamed rather than materialised: the probe
        // key is built per row and dropped, so the peak is this map plus the
        // ids that need closing, not a second copy of the generation.
        let mut close_edges: Vec<i64> = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT edge_id, source_file_id, target_file_id, source_symbol,
                        target_symbol, edge_kind, confidence, resolution, candidate_total
                 FROM edge_rows WHERE valid_to IS NULL",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                // The partition column first, and on its own. A row belonging
                // to an unchanged file costs one integer decode here instead of
                // the four string allocations, the hash and the field-by-field
                // comparison below — measured at 51 ms of the 68 ms this loop
                // spent on 107,257 rows.
                let source_file_id: u32 = row.get(1)?;
                if unchanged_edge_files.contains(&source_file_id) {
                    continue;
                }
                let edge_id: i64 = row.get(0)?;
                let live = EdgeTuple {
                    source_file_id,
                    target_file_id: row.get(2)?,
                    source_symbol: std::borrow::Cow::Owned(row.get(3)?),
                    target_symbol: std::borrow::Cow::Owned(row.get(4)?),
                    edge_kind: std::borrow::Cow::Owned(row.get(5)?),
                    confidence: row.get::<_, f64>(6)?.to_bits(),
                    // The stored label verbatim, never round-tripped through
                    // `ResolutionKind`: a spelling this binary does not know
                    // would come back `None` from the enum and then compare
                    // equal to a row that genuinely has no resolution, which is
                    // a carried-forward row the reader would label
                    // `Reconstructed` while the writer thought it matched.
                    resolution: row
                        .get::<_, Option<String>>(7)?
                        .map(std::borrow::Cow::Owned),
                    candidate_total: row.get(8)?,
                };
                let still_valid = claim_matching_candidate(
                    &edge_buckets,
                    &edge_chain,
                    &mut edge_matched,
                    &live,
                    edge_identity,
                );
                if !still_valid {
                    close_edges.push(edge_id);
                }
            }
        }
        {
            let mut close = tx.prepare_cached(
                "UPDATE edge_rows SET valid_to = ?2 WHERE edge_id = ?1 AND valid_to IS NULL",
            )?;
            for edge_id in &close_edges {
                close.execute(params![edge_id, gen_id])?;
            }
            // `prepare_cached` so this 10-parameter INSERT is compiled once per
            // transaction rather than once per edge. It is the writer's
            // highest-frequency statement on a cold build — one execution per
            // resolved edge, 102,083 of them here — and on an incremental build
            // it now runs for the delta alone.
            let mut insert = tx.prepare_cached(
                "INSERT INTO edge_rows (source_file_id, target_file_id, source_symbol,
                                        target_symbol, edge_kind, confidence, resolution,
                                        candidate_total, valid_from, valid_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)",
            )?;
            // In emission order, and only the copies the live set did not
            // already supply: a tuple wanted three times and valid twice is
            // inserted once, at the position of its first occurrence.
            for (index, edge) in resolution.edges.iter().enumerate() {
                let Some((src_f_id, tgt_f_id)) = edge_ids[index] else {
                    continue;
                };
                if edge_matched[index] {
                    continue;
                }
                let tuple = edge_tuple(edge, &kind_labels, src_f_id, tgt_f_id);
                insert.execute(params![
                    tuple.source_file_id,
                    tuple.target_file_id,
                    tuple.source_symbol.as_ref(),
                    tuple.target_symbol.as_ref(),
                    tuple.edge_kind.as_ref(),
                    f64::from_bits(tuple.confidence),
                    tuple.resolution.as_deref(),
                    tuple.candidate_total,
                    gen_id,
                ])?;
            }
        }
        // One span from the kind labels to the last insert: the identity index,
        // the scan of live rows, the closes and the inserts are the edge delta,
        // and charging them separately would invite a reader to fix the cheapest
        // of four passes that only exist together.
        drop(edge_charge);

        // The analysis must have been computed over the edge set being stored.
        //
        // These two numbers come from different places: `edge_ord` counts the
        // rows this generation will hold, while `total_edges` is what the
        // analyser actually saw. Every consumer of `dead_symbols` and
        // `communities` assumes they are the same set. They once were not. A
        // build that resolved only the changed files handed the analyser 63 of
        // 15,017 edges and committed a generation with 433 dead-code candidates
        // instead of 14; the graph was intact and only the analysis of it was
        // wrong, so nothing failed and `devmap dead` reported plainly-called
        // symbols as callerless.
        //
        // Now that every resolved edge is stored, agreement is structural: both
        // sides count the same `resolution.edges`. The check stays because it
        // costs one comparison and it is the thing that caught the carry-forward
        // drift — a generation whose stored edges came from an older extractor
        // than its analysis. It should now be unfailable; if it ever fires
        // again, a *new* asymmetry has been introduced between what this
        // function stores and what the caller analysed.
        //
        // Deletions are covered too, rather than exempted. The worry was that
        // `--deleted` drops rows the analyser had counted, but it cannot: a
        // deleted file is not extracted, so a resolution over the current tree
        // has no edge touching it, and the carried-forward rows that did are
        // dropped on both sides of this equality. Checked as well as argued —
        // 30 randomised deletion builds (6–25 files, up to a third removed)
        // held it exactly. Exempting the case would have left the watcher, the
        // most frequent writer of all, unguarded precisely when it deletes.
        if edge_ord as usize != analysis.total_edges {
            return Err(refusal(format!(
                "generation would store {edge_ord} edges but its analysis was computed over {}; \
                 dead-code and community results would describe a different graph than the one stored",
                analysis.total_edges
            )));
        }

        // The inventory of what this generation could not read.
        //
        // Two halves with different provenance and one rule. The extraction
        // gaps are derived here, from the same `extractions` slice the caller
        // analysed, through `devmap_analyze::extraction_gaps` — the owner
        // `extraction_coverage` folds — so a stored path list and the counts in
        // `AnalysisStatus` cannot describe different files. The discovery
        // refusals cannot be derived from anything: a refused file has no
        // `Extraction` at all, so they arrive on `opts` from whoever walked the
        // tree.
        //
        // Deleted paths are excluded on both halves. A file the caller is
        // removing from the generation must not leave a coverage row behind
        // claiming the graph is missing something it no longer contains.
        let mut gap_rows: Vec<(String, String, String)> = Vec::new();
        let gap_charge = charge(&mut spent.gaps);
        // The extraction gaps carry forward exactly as the file rows above do,
        // and for the same reason: a differential write is handed only the
        // extractions it re-read, so deriving the whole inventory from them
        // would drop every gap in a file this batch did not touch. Skipping
        // `affected` and `deleted` is what lets a file that used to fail to
        // parse leave the list on the build that parses it.
        if let Some(prev) = prev_gen {
            if !full_rewrite {
                let mut stmt = tx.prepare(
                    "SELECT gap, path, reason FROM generation_coverage_gaps
                     WHERE generation_id = ?1 AND gap != ?2",
                )?;
                let rows = stmt.query_map(
                    params![prev, crate::coverage::GAP_DISCOVERY_REFUSED],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )?;
                for row in rows {
                    let (gap, path, reason) = row?;
                    if deleted.contains(&path) || affected.contains(&path) {
                        continue;
                    }
                    gap_rows.push((gap, path, reason));
                }
            }
        }
        for entry in devmap_analyze::extraction_gaps(extractions) {
            if deleted.contains(&entry.path) {
                continue;
            }
            gap_rows.push((entry.gap.label().to_string(), entry.path, entry.reason));
        }
        // The refusal half is never carried forward here. It cannot be: a
        // refused path has no `Extraction`, so this function has no way to tell
        // a path the caller re-decided from one it never looked at. The caller
        // that walked the tree owns that decision — the cold walk replaces the
        // inventory outright, the drain carries it minus its affected set — and
        // hands the whole answer down.
        //
        // Deleted paths are *not* excluded. A containment refusal deliberately
        // deletes the path's rows while charging the refusal to coverage; that
        // is the drain agreeing with `devmap build` about where the repository
        // ends, and dropping the row here would make the refusal invisible on
        // the one path that produces it most.
        drop(gap_charge);
        let measured_refusals = match &opts.discovery_refusals {
            Some(refusals) => {
                // Deduplicated by path, because the count below is checked
                // against `COUNT(*)` of the rows and the table is keyed by
                // path: a caller that names one file twice would otherwise
                // claim a refusal the inventory cannot hold.
                let unique: std::collections::BTreeMap<&str, &str> = refusals
                    .iter()
                    .map(|refusal| (refusal.path.as_str(), refusal.reason.as_str()))
                    .collect();
                for (path, reason) in &unique {
                    gap_rows.push((
                        crate::coverage::GAP_DISCOVERY_REFUSED.to_string(),
                        (*path).to_string(),
                        (*reason).to_string(),
                    ));
                }
                Some(unique.len())
            }
            None => None,
        };
        // The same guard the edge count above gets, for the same reason: the
        // number a consumer reads and the rows it is supposed to count come
        // from two places, and nothing but this obliges them to agree. Getting
        // it wrong is not a cosmetic mismatch — `discovery_refused_files` caps
        // the dead-code confidence, so a summary claiming a refusal the
        // inventory cannot name is a graph degraded for a file nobody can look
        // at, and a summary claiming none while rows exist is the over-claim
        // this whole inventory exists to end.
        if measured_refusals != analysis.discovery_refused_files {
            return Err(refusal(format!(
                "generation would store {measured_refusals:?} discovery refusal(s) but its \
                 analysis was computed over {:?}; `discovery_refused_files` is derived from \
                 the inventory and the two must be one measurement",
                analysis.discovery_refused_files
            )));
        }
        {
            let _charge = charge(&mut spent.gaps);
            let mut insert = tx.prepare(
                "INSERT OR REPLACE INTO generation_coverage_gaps
                 (generation_id, gap, path, reason)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (gap, path, reason) in &gap_rows {
                insert.execute(params![gen_id, gap, path, reason])?;
            }
        }

        let dead_charge = charge(&mut spent.dead);
        for (ordinal, dead) in analysis.dead_symbols.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).map_err(|_| {
                refusal("dead-symbol row count exceeds SQLite generation ordinal capacity")
            })?;
            tx.execute(
                "INSERT INTO generation_dead_symbols
                 (generation_id, ordinal, file_path, symbol_name, confidence, is_exempt, exemption_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    gen_id,
                    ordinal,
                    dead.file_path,
                    dead.symbol_name,
                    confidence_millis(dead.confidence) as f64 / 1000.0,
                    dead.is_exempt as i32,
                    dead.exemption_reason
                ],
            )?;
        }
        drop(dead_charge);

        // D17: the unresolved-call ledger. Written inside the same transaction
        // as everything else, so a generation can never be observable while
        // claiming a completeness it did not record.
        // One prepared statement for the whole ledger. A repository of this size
        // produces tens of thousands of unresolved calls per generation, and
        // re-preparing the INSERT for each one cost seconds of the build — the
        // self-build gate caught it as a regression the moment this table
        // landed.
        //
        // Written as a validity range since v18, exactly as the edges above
        // are, and for the same measurement: two consecutive builds one appended
        // line apart held 89,743 rows and **65,567 distinct tuples on both
        // sides, with nothing appearing and nothing disappearing** — a ledger
        // that had not changed at all and was rewritten in full every time.
        // Declared out here because the digest write below reads it, and the
        // block it is filled in is scoped to the charge it belongs to.
        let mut fresh_unresolved_digests: std::collections::HashMap<u32, RowSetDigest> =
            std::collections::HashMap::new();
        {
            let _charge = charge(&mut spent.unresolved);
            // 12,424 ledger tuples of this repository occur more than once in
            // one generation (36,600 rows), so this is a multiset too — and
            // `matched`, one bit a row, is what makes it one.
            //
            // The row's identity, as ids.
            //
            // Three of its six columns are interned since v22 — the path into
            // `paths`, the reason and the classification into
            // `unresolved_texts` — so this resolves each distinct text to its
            // id once, here, and everything downstream compares integers.
            //
            // The reason is formatted once per row rather than three times.
            // It used to be formatted on demand, which read as the frugal
            // choice: a parallel `Vec<String>` holds 181,163 owned strings for
            // the length of the write, and that is memory `verify.sh` gate 6
            // charges against the kernel's model. But "on demand" was three
            // demands — the digest pass, the bucket pass and the insert pass
            // each rebuilt every row's `format!("{:?}", …)` — so the write paid
            // half a million allocations to avoid holding 181,163.
            //
            // Interning against the *persisted* pool, not a fresh one per
            // write: the id has to mean the same text as it did in the stored
            // rows this write is about to compare against, so the pool is read
            // in once and only genuinely new texts are inserted. On this
            // repository that is 46,978 rows read and, on an incremental build,
            // a handful written.
            let mut text_ids: std::collections::HashMap<std::rc::Rc<str>, i64> = {
                let mut stmt = tx.prepare("SELECT id, text FROM unresolved_texts")?;
                let mut rows = stmt.query([])?;
                let mut pool = std::collections::HashMap::new();
                while let Some(row) = rows.next()? {
                    let id: i64 = row.get(0)?;
                    let text: String = row.get(1)?;
                    pool.insert(std::rc::Rc::from(text.as_str()), id);
                }
                pool
            };

            // Per-row ids, in `resolution.unresolved` order.
            //
            // Two passes, not one. The obvious shape — resolve each row's text
            // to an id as the row is reached, inserting on a miss — makes the
            // pool's inserts arrive in the order the rows happen to mention
            // them, which for a `UNIQUE(text)` index is random order. Measured
            // on the cold self-build, where every one of 46,978 reasons is a
            // miss, that cost 1.55 us an insert against 0.54 us for a ledger
            // row: the index is over 130-byte text, so a random insertion order
            // pays a page split and a string comparison at every level.
            //
            // The first pass therefore only *slots* each row against a distinct
            // text; the second inserts the genuinely new slots in sorted order,
            // which is the order the index wants. Per-row cost is unchanged —
            // one hash of the text either way — and the insert becomes
            // append-mostly.
            //
            // `Rc<str>` so a distinct text is stored once and the map's key is a
            // refcount bump rather than a second copy.
            let mut slot_of_text: std::collections::HashMap<std::rc::Rc<str>, u32> =
                std::collections::HashMap::new();
            let mut slot_text: Vec<std::rc::Rc<str>> = Vec::new();
            let mut reason_slots: Vec<u32> = Vec::with_capacity(resolution.unresolved.len());
            let mut class_slots: Vec<u32> = Vec::with_capacity(resolution.unresolved.len());
            let mut source_ids: Vec<u32> = Vec::with_capacity(resolution.unresolved.len());
            {
                let mut scratch = String::new();
                let slot_for =
                    |text: &str,
                     slot_of_text: &mut std::collections::HashMap<std::rc::Rc<str>, u32>,
                     slot_text: &mut Vec<std::rc::Rc<str>>|
                     -> Result<u32> {
                        if let Some(slot) = slot_of_text.get(text) {
                            return Ok(*slot);
                        }
                        let slot = u32::try_from(slot_text.len()).map_err(|_| {
                            refusal("unresolved ledger text count exceeds u32 slot capacity")
                        })?;
                        let owned: std::rc::Rc<str> = std::rc::Rc::from(text);
                        slot_text.push(std::rc::Rc::clone(&owned));
                        slot_of_text.insert(owned, slot);
                        Ok(slot)
                    };
                // `class.label()` is a `&'static str` over a handful of
                // variants, so its slot is memoised on the identity of the
                // static the enum hands back rather than re-hashed 181,163
                // times. A linear scan of at most a dozen keys beats hashing
                // even a short string.
                //
                // The key is (pointer, length) and not the pointer alone. A
                // pointer alone is only a unique key for these labels while no
                // label is a prefix of another: `&'static str` carries no
                // terminator, so nothing stops a toolchain from placing
                // `"external"` at the same address as the start of a longer
                // `"external_module"`, and then two classes would share a
                // memo entry and every row of one would be stored with the
                // other's text. That is not a bug today — the eight labels are
                // pairwise non-prefix, and no rustc release is known to merge
                // string literals this way — but it is an assumption about a
                // toolchain, held by a `match` arm in another crate, that
                // nothing would restate if a ninth label were added.
                // Comparing the length too costs one `usize` compare and owes
                // the assumption nothing.
                //
                // Both halves were classified by mutation rather than argued.
                // Reverting this key to the pointer alone leaves the suite
                // green, so the length is defensive and not load-bearing today.
                // Making the memo return one slot for every class -- what a
                // pointer collision would actually do -- fails exactly one test
                // in the crate, `every_unresolved_class_keeps_its_own_label` in
                // `tests/ledger_write.rs`, and the other six in that file pass
                // with all eight classes stored as `builtin`. That test is the
                // behavioural half of this comment, and it is load-bearing.
                let mut class_memo: Vec<((*const u8, usize), u32)> = Vec::new();
                // Consecutive rows overwhelmingly share a source file — the
                // resolver emits per file — so one remembered path answers
                // almost every lookup without hashing forty bytes again.
                let mut last_path: Option<(&str, u32)> = None;

                for unresolved in &resolution.unresolved {
                    use std::fmt::Write as _;
                    scratch.clear();
                    let _ = write!(scratch, "{:?}", unresolved.resolution);
                    reason_slots.push(slot_for(&scratch, &mut slot_of_text, &mut slot_text)?);

                    let label = unresolved.class.label();
                    let key = (label.as_ptr(), label.len());
                    let class_slot = match class_memo.iter().find(|(seen, _)| *seen == key) {
                        Some((_, slot)) => *slot,
                        None => {
                            let slot = slot_for(label, &mut slot_of_text, &mut slot_text)?;
                            class_memo.push((key, slot));
                            slot
                        }
                    };
                    class_slots.push(class_slot);

                    let path = unresolved.source_file.as_str();
                    let file_id = match last_path {
                        Some((seen, id)) if seen == path => id,
                        _ => {
                            let id = Self::ensure_path_id_cached(&tx, &mut path_ids, path)?;
                            last_path = Some((path, id));
                            id
                        }
                    };
                    source_ids.push(file_id);
                }
            }

            // The slots the pool does not hold yet, inserted in sorted order.
            let mut slot_ids: Vec<i64> = vec![0; slot_text.len()];
            {
                let mut fresh: Vec<u32> = (0..slot_text.len() as u32)
                    .filter(|slot| !text_ids.contains_key(&slot_text[*slot as usize]))
                    .collect();
                fresh.sort_unstable_by(|left, right| {
                    slot_text[*left as usize].cmp(&slot_text[*right as usize])
                });
                let mut insert =
                    tx.prepare_cached("INSERT INTO unresolved_texts (text) VALUES (?1)")?;
                for slot in fresh {
                    let text = std::rc::Rc::clone(&slot_text[slot as usize]);
                    insert.execute(params![text.as_ref()])?;
                    text_ids.insert(text, tx.last_insert_rowid());
                }
            }
            for (slot, text) in slot_text.iter().enumerate() {
                slot_ids[slot] = *text_ids.get(text).ok_or_else(|| {
                    refusal("an interned ledger text has no id after its own insert")
                })?;
            }
            let reason_ids: Vec<i64> = reason_slots
                .iter()
                .map(|slot| slot_ids[*slot as usize])
                .collect();
            let class_ids: Vec<i64> = class_slots
                .iter()
                .map(|slot| slot_ids[*slot as usize])
                .collect();

            let unresolved_tuple = |index: usize| -> UnresolvedTuple<'_> {
                let unresolved = &resolution.unresolved[index];
                UnresolvedTuple {
                    source_file_id: source_ids[index],
                    source_symbol: std::borrow::Cow::Borrowed(unresolved.source_symbol.as_str()),
                    callee_name: std::borrow::Cow::Borrowed(unresolved.callee_name.as_str()),
                    reason_id: reason_ids[index],
                    classification_id: class_ids[index],
                    receiver: unresolved
                        .receiver
                        .as_deref()
                        .map(std::borrow::Cow::Borrowed),
                }
            };
            // The same per-file digest the edges get, and keyed the same way
            // since v22. Before it, this relation stored its path as text and
            // keyed by that, so the two halves of one digest row were reached
            // by two different keys.
            for (index, file_id) in source_ids.iter().enumerate() {
                fresh_unresolved_digests
                    .entry(*file_id)
                    .or_default()
                    .absorb(&unresolved_tuple(index));
            }
            // The ledger's half of the check the edge pass documents: the rows
            // are asked how many of them there are, so a row deleted behind the
            // write path is never mistaken for a row still stored.
            //
            // The map is seeded from the fresh file ids and only ever
            // incremented through `get_mut`, so the `u32` off the row answers it
            // and nothing allocates. Since v22 the row carries the id, so this
            // no longer reads a path out of every live row to look it up.
            let mut live_unresolved_rows: std::collections::HashMap<u32, u64> =
                fresh_unresolved_digests
                    .keys()
                    .map(|file_id| (*file_id, 0))
                    .collect();
            if scope_by_digest {
                let mut stmt = tx
                    .prepare("SELECT source_file_id FROM unresolved_rows WHERE valid_to IS NULL")?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    if let Some(count) = live_unresolved_rows.get_mut(&row.get::<_, u32>(0)?) {
                        *count += 1;
                    }
                }
            }
            let unchanged_unresolved_files: std::collections::HashSet<u32> = if scope_by_digest {
                fresh_unresolved_digests
                    .iter()
                    .filter(|(file_id, fresh)| {
                        stored_unresolved_digests.get(*file_id) == Some(*fresh)
                            && live_unresolved_rows.get(*file_id) == Some(&fresh.rows)
                    })
                    .map(|(file_id, _)| *file_id)
                    .collect()
            } else {
                std::collections::HashSet::new()
            };

            // One closure for the bucketing and the search, for the reason the
            // edge pass names: the two must agree about which indexes are
            // offered, not only about what an identity is.
            let ledger_identity = |index: usize| -> Option<UnresolvedTuple<'_>> {
                if unchanged_unresolved_files.contains(&source_ids[index]) {
                    return None;
                }
                Some(unresolved_tuple(index))
            };
            // Built on the first live row that reaches the comparison, not
            // ahead of it.
            //
            // The index exists only to answer "is this stored row still one of
            // ours", so a write with no live row to ask about never needs it —
            // and that is exactly the cold build, where the table is empty and
            // every one of this repository's 180,679 identities was formatted,
            // hashed and chained into a structure nothing then queried. Measured
            // at 93 ms of a 505 ms ledger write, on the path that is already the
            // slowest one.
            //
            // Lazy rather than a `SELECT EXISTS` guard: the emptiness that
            // matters is not "are there live rows" but "does any live row
            // survive the `unchanged_unresolved_files` filter below", which a
            // count cannot answer without doing the scan twice.
            let mut ledger_index: Option<(std::collections::HashMap<u64, u32>, Vec<u32>)> = None;
            let mut ledger_matched: Vec<bool> = source_ids
                .iter()
                .map(|file_id| unchanged_unresolved_files.contains(file_id))
                .collect();

            let mut close_rows: Vec<i64> = Vec::new();
            {
                let mut stmt = tx.prepare(
                    "SELECT unresolved_id, source_file_id, source_symbol, callee_name, reason_id,
                            classification_id, receiver
                     FROM unresolved_rows WHERE valid_to IS NULL",
                )?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    // The partition column is an integer since v22, so the
                    // membership test is a `u32` compare. It used to read the
                    // path text out of every live row and hash it — 181,163
                    // string reads to decide which rows were even in scope —
                    // and needed `get_ref` to avoid owning a `String` for each.
                    let source_file_id: u32 = row.get(1)?;
                    if unchanged_unresolved_files.contains(&source_file_id) {
                        continue;
                    }
                    let unresolved_id: i64 = row.get(0)?;
                    // Two of the three remaining owned `String`s went with the
                    // same change: `reason` averaged 130 bytes a row and is now
                    // an id. What is left is the symbol and callee, which are
                    // this row's own text and belong in it.
                    let live = UnresolvedTuple {
                        source_file_id,
                        source_symbol: std::borrow::Cow::Owned(row.get(2)?),
                        callee_name: std::borrow::Cow::Owned(row.get(3)?),
                        reason_id: row.get(4)?,
                        classification_id: row.get(5)?,
                        receiver: row
                            .get::<_, Option<String>>(6)?
                            .map(std::borrow::Cow::Owned),
                    };
                    let (ledger_buckets, ledger_chain) = ledger_index.get_or_insert_with(|| {
                        bucket_identities(resolution.unresolved.len(), ledger_identity)
                    });
                    let still_valid = claim_matching_candidate(
                        ledger_buckets,
                        ledger_chain,
                        &mut ledger_matched,
                        &live,
                        ledger_identity,
                    );
                    if !still_valid {
                        close_rows.push(unresolved_id);
                    }
                }
            }
            let mut close = tx.prepare_cached(
                "UPDATE unresolved_rows SET valid_to = ?2
                  WHERE unresolved_id = ?1 AND valid_to IS NULL",
            )?;
            for unresolved_id in &close_rows {
                close.execute(params![unresolved_id, gen_id])?;
            }
            // One prepared statement for the whole ledger. A repository of this
            // size produces tens of thousands of unresolved calls per
            // generation, and re-preparing the INSERT for each one cost seconds
            // of the build — the self-build gate caught it as a regression the
            // moment this table landed.
            //
            // One row per execute, deliberately, after multi-row batching was
            // tried here and removed.
            //
            // The theory was that 180,679 executes of a cached statement pay
            // 180,679 VDBE step/reset cycles that batches of 128 would amortise.
            // Measured, in isolation, on the cold self-build: `persist:write`
            // 0.4576 s at one row per statement against 0.4568 s at 128
            // (min-of-4, interleaved) — no difference, with the ledger pass
            // itself marginally worse batched. The insert's cost is b-tree page
            // writes, not statement dispatch, which is also why dropping the
            // two unread indexes on this table moved it 2.3x and batching moved
            // it nothing.
            //
            // Not kept "in case it helps later". A batch has to build its SQL
            // for a varying arity and bind positionally by hand, so a
            // miscounted chunk or a transposed bind writes a wrong ledger with
            // no error — and the varying-arity remainder statement is a new
            // cache key every build, evicting one of the sixteen slots
            // rusqlite's LRU has and this write already fills eleven of.
            let mut insert = tx.prepare_cached(
                "INSERT INTO unresolved_rows
                 (source_file_id, source_symbol, callee_name, reason_id, classification_id,
                  receiver, valid_from, valid_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            )?;
            for (index, still_valid) in ledger_matched.iter().enumerate() {
                if *still_valid {
                    continue;
                }
                let tuple = unresolved_tuple(index);
                insert.execute(params![
                    tuple.source_file_id,
                    tuple.source_symbol.as_ref(),
                    tuple.callee_name.as_ref(),
                    tuple.reason_id,
                    tuple.classification_id,
                    tuple.receiver.as_deref(),
                    gen_id,
                ])?;
            }
        }

        // What the *next* build scopes by.
        //
        // Written for every file that has at least one row in either relation,
        // which is exactly the set that can have live rows after this write: a
        // live row is either one this build re-derived and kept or one it just
        // inserted, and both come from `resolution`. A file with no fresh rows
        // therefore needs no digest — it has none of either relation left, and
        // the next build reads its absence as "compare it" and finds nothing.
        //
        // Deleted paths are covered by the same statement rather than exempted
        // from it. `edge_ids` is `None` for every edge touching one, so a
        // deleted file contributes to no digest, is absent from this table, and
        // its stored rows are compared and closed on the next build exactly as
        // they are on this one.
        //
        // Row-per-file, not row-per-relation-per-file: the two digests share a
        // key and are read together by the one query above, and splitting them
        // would double a table whose whole purpose is to be cheap to read.
        {
            let _charge = charge(&mut spent.digests);
            let mut digests: std::collections::HashMap<u32, (RowSetDigest, RowSetDigest)> =
                std::collections::HashMap::with_capacity(fresh_edge_digests.len());
            for (file_id, digest) in &fresh_edge_digests {
                digests.entry(*file_id).or_default().0 = *digest;
            }
            // No interning left to do here. This used to translate each
            // unresolved digest's path to an id, because the ledger keyed its
            // digests by text while the edges keyed theirs by id; since v22
            // both arrive keyed the same way and the two loops are the same
            // loop over different maps.
            for (file_id, digest) in &fresh_unresolved_digests {
                digests.entry(*file_id).or_default().1 = *digest;
            }
            let mut insert = tx.prepare_cached(
                "INSERT INTO generation_file_digests
                 (generation_id, file_id, edge_rows, edge_lo, edge_hi,
                  unresolved_rows, unresolved_lo, unresolved_hi)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for (file_id, (edges, unresolved)) in &digests {
                let [edge_rows, edge_lo, edge_hi] = edges.to_columns();
                let [unresolved_rows, unresolved_lo, unresolved_hi] = unresolved.to_columns();
                insert.execute(params![
                    gen_id,
                    file_id,
                    edge_rows,
                    edge_lo,
                    edge_hi,
                    unresolved_rows,
                    unresolved_lo,
                    unresolved_hi,
                ])?;
            }
        }

        // The history row is written inside the generation's own transaction.
        // A build is therefore never observable without its history entry, and
        // a rolled-back generation leaves no phantom row behind.
        let history_charge = charge(&mut spent.history);
        let symbols: i64 = tx.query_row(
            "SELECT COUNT(*) FROM generation_nodes WHERE generation_id = ?1",
            params![gen_id],
            |row| row.get(0),
        )?;
        let edges: i64 = tx.query_row(
            "SELECT COUNT(*) FROM generation_edges WHERE generation_id = ?1",
            params![gen_id],
            |row| row.get(0),
        )?;
        let files: i64 = tx.query_row(
            "SELECT COUNT(*) FROM generation_files WHERE generation_id = ?1",
            params![gen_id],
            |row| row.get(0),
        )?;
        // "Confident" and "ambiguous" are the two tiers a reader acts on:
        // an exempt symbol is one liveness could not rule out, so counting it
        // as confidently dead is exactly the dishonesty D6 removed.
        //
        // The 0.4 tier is not only `only_ambiguous_callers`: an unresolved
        // namesake veto lands at the same confidence with a different reason,
        // and coverage-capped findings sit below 0.9 too. Counting anything
        // under the extracted floor as `dead_confident` inflated the history
        // trend with unconfirmed rows.
        let dead_confident = analysis
            .dead_symbols
            .iter()
            .filter(|dead| !dead.is_exempt && dead.confidence >= 0.9)
            .count() as i64;
        let dead_ambiguous = analysis
            .dead_symbols
            .iter()
            .filter(|dead| !dead.is_exempt && dead.confidence < 0.9)
            .count() as i64;
        // S-2: both of these are counted over the generation's own rows, like
        // `files`/`symbols`/`edges` above, and not over `extractions`.
        //
        // `extractions` is the slice this *write* carried. On an incremental
        // build that is the handful of edited files, while `files` beside it is
        // `COUNT(*)` over the whole generation — a partial numerator against a
        // whole denominator, in the one table whose entire purpose is the
        // trend. A one-line edit in a twelve-language tree wrote
        // `languages_covered: 1, parse_failed: 0` next to the real file count,
        // so `devmap history` showed the repository shedding eleven languages
        // and repairing every parse failure on each incremental build, then
        // regaining both on the next cold one.
        let languages_covered: i64 = tx.query_row(
            "SELECT COUNT(DISTINCT language) FROM generation_files WHERE generation_id = ?1",
            params![gen_id],
            |row| row.get(0),
        )?;
        // K5: ask the canonical classifier, not the raw variant. A prose or
        // data format reports `ParseOutcome::Failed` because no grammar exists
        // for it, so the raw test counted 294 of this repository's 1,310 files
        // as parse failures — all Markdown, JSON, YAML, config and HTML — and
        // buried the 16 files a grammar actually parsed and flagged. Reading it
        // off the stored columns keeps that rule and applies it to carried-
        // forward rows too, which the in-memory slice cannot see.
        let mut parse_failed: i64 = 0;
        {
            let mut stmt = tx.prepare(
                "SELECT p.path, f.parse_outcome_json, f.engine_json
                 FROM generation_files f
                 JOIN paths p ON p.id = f.file_id
                 WHERE f.generation_id = ?1",
            )?;
            let rows = stmt.query_map(params![gen_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (path, parse_json, engine_json) = row?;
                let (outcome, engine) = decode_stored_outcome(&path, &parse_json, &engine_json)?;
                if stored_is_parse_failure(&outcome, &engine) {
                    parse_failed += 1;
                }
            }
        }
        let page_count: i64 = tx.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = tx.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        let build_ms = opts
            .build_started
            .map(|started| i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX));

        tx.execute(
            "INSERT OR REPLACE INTO build_history
             (generation_id, built_at, head_sha, files, symbols, edges,
              dead_confident, dead_ambiguous, parse_failed, languages_covered,
              build_ms, db_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                gen_id,
                now,
                head_sha,
                files,
                symbols,
                edges,
                dead_confident,
                dead_ambiguous,
                parse_failed,
                languages_covered,
                build_ms,
                page_count.saturating_mul(page_size),
            ],
        )?;
        // Retention is by row count, not by surviving generation: history must
        // outlive the graphs it describes or it cannot show a trend.
        tx.execute(
            "DELETE FROM build_history WHERE generation_id NOT IN
             (SELECT generation_id FROM build_history ORDER BY built_at DESC, generation_id DESC LIMIT ?1)",
            params![BUILD_HISTORY_RETENTION as i64],
        )?;
        drop(history_charge);

        {
            let _charge = charge(&mut spent.commit);
            tx.commit()?;
        }
        spent.attribute_residual(write_started.elapsed().as_secs_f64());
        Ok((gen_id, spent))
    }
}
