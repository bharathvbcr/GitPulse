//! Relations the SQL grammar drops inside a dialect `ERROR` span.
//!
//! tree-sitter-sequel has no production for `CREATE VIRTUAL TABLE ... USING
//! fts5` or for a SQLite trigger body. The error is not confined to that
//! clause: the node that survives can wear the following table's name and a
//! span that starts at the broken statement, or the following `CREATE TABLE`
//! is absent entirely. Both are the same fact — the statement text is a
//! relation the grammar accepts when it is parsed alone.
//!
//! Recovery reparses that statement and installs the symbol the grammar
//! itself emits for it, shifted back into the file. A virtual table never
//! parses, so its name and span come from the statement text and it is a
//! `Struct`, the kind every other relation already has. Indexes and triggers
//! stay non-symbols. Nothing is invented for an unterminated statement, or
//! for a `CREATE` inside a string, a comment, a dollar quote, a parenthesised
//! expression, or a `BEGIN`/`END` body, and a name that cannot be read in full
//! is not recovered at all.
//!
//! The text scan is a second reader of a file the grammar already read, so it
//! is never trusted over the grammar where the grammar had no complaint: on a
//! file that parsed `Clean` recovery removes nothing, and on a `Partial` file
//! it removes only a symbol whose start lies strictly inside a skipped body
//! (a trigger, routine or dollar-quoted body with a matched terminator) and
//! that overlaps a grammar error range. A scan that loses sync is evidence the
//! scanner is wrong, not that the grammar invented declarations: recovery
//! stops at that point and everything after it keeps the grammar's output,
//! with no removal and no synthesis. The scan is bounded by the file's own
//! deadline and by caps, and a cap or deadline leaves the grammar's output
//! exactly as it was.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::model::{ExtractedSymbol, Extraction, ParseOutcome, Span, SymbolKind};

/// Reparsed statements per file. Past this the file is left as the grammar
/// read it: a cap is not a finished scan, and a half-applied recovery would
/// read like one.
const MAX_REPARSES: usize = 256;

/// Relation statements read from one file. A file at the 1 MiB source ceiling
/// cannot hold more, so reaching this means the scan is not reading a schema.
const MAX_STATEMENTS: usize = 65_536;

/// Non-relation statements and blocks recorded from one file.
const MAX_SKIPPED: usize = 65_536;

/// Scan steps between clock reads.
const DEADLINE_STRIDE: usize = 256;

/// Why recovery stopped without changing the extraction.
enum Abandon {
    Deadline,
    Cap(&'static str),
    Reparse,
}

impl Abandon {
    fn diagnostic(&self) -> String {
        let why = match self {
            Abandon::Deadline => "the file's parse budget ran out".to_string(),
            Abandon::Cap(what) => format!("the {what} cap was reached"),
            Abandon::Reparse => "a statement reparse was refused".to_string(),
        };
        format!(
            "SQL relation recovery abandoned because {why}; the relations are as the grammar read them"
        )
    }
}

fn past(deadline: Instant) -> bool {
    Instant::now() >= deadline
}

/// Parse error ranges merged into sorted, disjoint spans, so a symbol is
/// checked against them in `O(log n)`.
#[derive(Default)]
struct ErrorRanges {
    merged: Vec<(usize, usize)>,
}

impl ErrorRanges {
    fn new(ranges: impl Iterator<Item = (usize, usize)>) -> Self {
        let mut sorted: Vec<(usize, usize)> = ranges.collect();
        sorted.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(sorted.len());
        for (start, end) in sorted {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        Self { merged }
    }

    fn is_empty(&self) -> bool {
        self.merged.is_empty()
    }

    /// `[start, end)` shares a byte with a range.
    fn overlaps(&self, start: usize, end: usize) -> bool {
        let at = self
            .merged
            .partition_point(|&(_, range_end)| range_end <= start);
        self.merged
            .get(at)
            .is_some_and(|&(range_start, _)| range_start < end)
    }
}

/// Put back relations whose statement overlaps a parse error and whose symbol
/// is missing or wears the wrong span.
///
/// A no-op for every other language and for a file that did not parse. Existing
/// error ranges are left in place: a `CHECK(json_valid(...))` the grammar
/// already confined stays a recorded error, and a trigger body the grammar
/// cannot parse stays one too.
///
/// A `CREATE TABLE` the grammar saw inside a trigger body or a routine body is
/// not a relation in the schema. Such a symbol is removed only when its start
/// lies strictly inside a skipped body that ended at a terminator and it
/// overlaps a grammar error range; on a file the grammar parsed `Clean`
/// nothing is removed. Where the scan loses sync (an unclosed quote, comment
/// or block) recovery stops: nothing after that point is removed or added. A
/// stored function itself stays: it is the callable the call graph already
/// attributes. Quoted identifiers are stored as the name inside the quotes,
/// unless another symbol already holds that qualified name.
///
/// `deadline` is the instant the file's parse budget ends. Recovery runs after
/// the grammar pass has disarmed its own clock, so it carries the deadline
/// itself. Everything is planned first and applied only if the plan finished,
/// so a deadline or a cap leaves the extraction as the grammar produced it.
pub(crate) fn recover_lost_relations(extraction: &mut Extraction, source: &str, deadline: Instant) {
    if extraction.language != "sql" || source.is_empty() {
        return;
    }
    let errors = match &extraction.parse_outcome {
        ParseOutcome::Clean => ErrorRanges::default(),
        ParseOutcome::Partial { error_ranges, .. } => ErrorRanges::new(
            error_ranges
                .iter()
                .map(|range| (range.start_byte, range.end_byte)),
        ),
        _ => return,
    };
    match plan(extraction, source, &errors, deadline) {
        // Applying is linear but not interruptible, so the last look at the
        // clock is here: a hit leaves the grammar's output as it was.
        Ok(_) if past(deadline) => extraction.diagnostics.push(Abandon::Deadline.diagnostic()),
        Ok(plan) => {
            let stopped = plan.scan.unterminated_at;
            apply(extraction, plan, &errors);
            if let Some(at) = stopped {
                extraction.diagnostics.push(format!(
                    "SQL relation recovery stopped at byte {at}, where the text scan lost sync; what follows is as the grammar read it"
                ));
            }
        }
        Err(abandon) => extraction.diagnostics.push(abandon.diagnostic()),
    }
}

struct Plan {
    scan: Scan,
    installs: Vec<Install>,
}

struct Install {
    start: usize,
    end: usize,
    desired: ExtractedSymbol,
}

fn plan(
    extraction: &Extraction,
    source: &str,
    errors: &ErrorRanges,
    deadline: Instant,
) -> Result<Plan, Abandon> {
    let scan = scan_source(source, deadline)?;
    // Symbols by start byte, so asking "did the grammar anchor a table here" is
    // a lookup and not a walk over every symbol per statement.
    let mut anchors: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, symbol) in extraction.symbols.iter().enumerate() {
        if symbol.kind != SymbolKind::File {
            anchors
                .entry(symbol.span.start_byte)
                .or_default()
                .push(index);
        }
    }
    let mut installs = Vec::new();
    let mut reparses = 0usize;
    for statement in &scan.relations {
        if !errors.overlaps(statement.start, statement.end) {
            continue;
        }
        if past(deadline) {
            return Err(Abandon::Deadline);
        }
        let desired = symbol_for(
            extraction,
            &anchors,
            source,
            statement,
            &mut reparses,
            deadline,
        )?;
        if let Some(desired) = desired {
            installs.push(Install {
                start: statement.start,
                end: statement.end,
                desired,
            });
        }
    }
    if past(deadline) {
        return Err(Abandon::Deadline);
    }
    Ok(Plan { scan, installs })
}

/// Qualified names that changed, old to new, and the symbols that hold each
/// qualified name. One pass over the edges applies the renames; a rename per
/// symbol would walk every edge once per symbol.
///
/// An edge names its enclosing symbol by qualified name alone, so a rename that
/// moves the edges of `old` moves them for every symbol that holds `old`. That
/// is why [`Names::try_move`] is the only way a qualified name changes: it
/// allows a move only for a set that is every holder of the old name, and only
/// onto a name nobody holds.
#[derive(Default)]
struct Names {
    /// Every qualified name that was left, to the name it ended on. A name
    /// renamed twice (`A` to `B` to `C`) maps both `A` and `B` to `C`, so an
    /// edge is moved in one hop to where its symbol finally is.
    renames: HashMap<String, String>,
    /// The inverse: a qualified name to every earlier name that ended on it.
    renamed_from: HashMap<String, Vec<String>>,
    held: HashMap<String, Vec<usize>>,
}

impl Names {
    fn hold(&mut self, qualified: &str, index: usize) {
        self.held
            .entry(qualified.to_string())
            .or_default()
            .push(index);
    }

    /// Some symbol holds `qualified`.
    fn holds(&self, qualified: &str) -> bool {
        self.held
            .get(qualified)
            .is_some_and(|holders| !holders.is_empty())
    }

    /// Move `group` from `old` to `new`. Refused (and nothing recorded) unless
    /// the group is exactly the set of symbols that hold `old` and nothing holds
    /// `new`. A name that earlier renames ended on is composed: they now end on
    /// `new`. Linear in the group.
    fn try_move(&mut self, group: &[usize], old: &str, new: &str) -> bool {
        if old == new {
            return true;
        }
        let members: HashSet<usize> = group.iter().copied().collect();
        let is_every_holder = self
            .held
            .get(old)
            .is_some_and(|holders| holders.iter().copied().collect::<HashSet<usize>>() == members);
        if !is_every_holder || self.holds(new) {
            return false;
        }
        self.held.remove(old);
        self.held.insert(new.to_string(), group.to_vec());
        let mut earlier = self.renamed_from.remove(old).unwrap_or_default();
        for name in &earlier {
            self.renames.insert(name.clone(), new.to_string());
        }
        earlier.push(old.to_string());
        self.renames.insert(old.to_string(), new.to_string());
        self.renamed_from
            .entry(new.to_string())
            .or_default()
            .extend(earlier);
        true
    }
}

fn apply(extraction: &mut Extraction, plan: Plan, errors: &ErrorRanges) {
    let file = extraction.file_path.clone();
    let mut names = Names::default();
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, symbol) in extraction.symbols.iter().enumerate() {
        names.hold(&symbol.qualified_name, index);
        if symbol.kind != SymbolKind::File {
            by_name
                .entry(bare_name(&symbol.name))
                .or_default()
                .push(index);
        }
    }
    for install in plan.installs {
        install_symbol(extraction, &mut by_name, &mut names, install);
    }
    apply_scanned_names(extraction, &plan.scan.relations, &file, &mut names);
    normalize_quoted_names(extraction, &file, &mut names);
    retarget(extraction, &names.renames);
    drop_spurious(extraction, &plan.scan, errors);
}

/// The grammar's name node ends at the first quote or space inside a delimited
/// identifier (`"a""b"` becomes `a`, `[Order Details]` becomes `Details`).
/// The statement scan already read the identifier. Write that name onto the
/// declaration that starts at the statement's own `CREATE`, and only when the
/// grammar's name is a fragment of the scanned one. A scan that reads a string
/// or comment differently from the grammar can invent a statement whose range
/// covers a real table; that table starts elsewhere, or is named something
/// else, and keeps the grammar's name. An undelimited name is the grammar's own
/// to keep.
fn apply_scanned_names(
    extraction: &mut Extraction,
    relations: &[Statement],
    file: &str,
    names: &mut Names,
) {
    let mut order: Vec<usize> = extraction
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, symbol)| symbol.kind != SymbolKind::File)
        .map(|(index, _)| index)
        .collect();
    order.sort_by_key(|&index| extraction.symbols[index].span.start_byte);
    // Symbols to rename, grouped by the qualified name they hold and the name
    // they are to take, so a rename that has to move every holder can.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut group_of: HashMap<(String, String), usize> = HashMap::new();
    for statement in relations.iter().filter(|statement| statement.delimited) {
        let first = order
            .partition_point(|&index| extraction.symbols[index].span.start_byte < statement.start);
        let anchored = order[first..]
            .iter()
            .copied()
            .take_while(|&index| extraction.symbols[index].span.start_byte == statement.start)
            .find(|&index| is_fragment_of(&extraction.symbols[index].name, &statement.name));
        let Some(index) = anchored else {
            continue;
        };
        let key = (
            extraction.symbols[index].qualified_name.clone(),
            statement.name.clone(),
        );
        match group_of.get(&key) {
            Some(&group) => groups[group].1.push(index),
            None => {
                group_of.insert(key, groups.len());
                groups.push((statement.name.clone(), vec![index]));
            }
        }
    }
    for (name, group) in groups {
        rename_group(&mut extraction.symbols, &group, &name, file, names);
    }
}

/// The grammar's (possibly quoted, possibly cut short) name is part of the
/// scanned name.
fn is_fragment_of(grammar_name: &str, scanned: &str) -> bool {
    let bare = bare_name(grammar_name);
    !bare.is_empty() && scanned.contains(&bare)
}

struct Statement {
    start: usize,
    end: usize,
    name: String,
    /// The name was written between quotes or brackets.
    delimited: bool,
    /// `CREATE VIRTUAL TABLE` — the grammar has no node for it.
    virtual_table: bool,
}

fn symbol_for(
    extraction: &Extraction,
    anchors: &HashMap<usize, Vec<usize>>,
    source: &str,
    statement: &Statement,
    reparses: &mut usize,
    deadline: Instant,
) -> Result<Option<ExtractedSymbol>, Abandon> {
    if statement.virtual_table {
        return Ok(Some(synthesized(extraction, statement)));
    }
    // The grammar already anchored this table at its own `CREATE`. A confined
    // error inside it (`CHECK(abs(a)>0)`) is not a reason to read it again.
    let anchored = anchors.get(&statement.start).is_some_and(|indices| {
        indices.iter().any(|&index| {
            let symbol = &extraction.symbols[index];
            symbol.span.end_byte <= statement.end && same_name(&symbol.name, &statement.name)
        })
    });
    if anchored {
        return Ok(None);
    }
    if *reparses >= MAX_REPARSES {
        return Err(Abandon::Cap("reparse"));
    }
    *reparses += 1;
    let Some(snippet) = source.get(statement.start..statement.end) else {
        return Ok(None);
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Abandon::Deadline);
    }
    let parsed = crate::treesitter::extract_treesitter_with_budget(
        &extraction.file_path,
        "sql",
        snippet,
        remaining,
    );
    if !matches!(
        parsed.parse_outcome,
        ParseOutcome::Clean | ParseOutcome::Partial { .. }
    ) {
        return Err(Abandon::Reparse);
    }
    let Some(mut symbol) = parsed
        .symbols
        .into_iter()
        .find(|symbol| symbol.kind != SymbolKind::File && same_name(&symbol.name, &statement.name))
    else {
        return Ok(None);
    };
    symbol.span.start_byte = symbol.span.start_byte.saturating_add(statement.start);
    symbol.span.end_byte = symbol.span.end_byte.saturating_add(statement.start);
    // The grammar's name node includes the quote characters. The statement
    // scanner already stored the identifier.
    symbol.qualified_name = format!("{}::{}", extraction.file_path, statement.name);
    symbol.name = statement.name.clone();
    Ok(Some(symbol))
}

fn synthesized(extraction: &Extraction, statement: &Statement) -> ExtractedSymbol {
    let file = extraction.file_path.clone();
    ExtractedSymbol {
        qualified_name: format!("{file}::{}", statement.name),
        name: statement.name.clone(),
        kind: SymbolKind::Struct,
        span: Span {
            start_byte: statement.start,
            end_byte: statement.end,
        },
        is_exported: !statement.name.starts_with('_'),
        docstring: None,
        signature: None,
        parent_symbol: Some(file),
        body_signature: None,
        declaration_hash: None,
        return_type: None,
    }
}

/// The name a symbol is looked up by: the identifier inside its quotes.
fn bare_name(name: &str) -> String {
    unquote(name).unwrap_or_else(|| name.to_string())
}

/// Replace a same-named symbol whose span overlaps this statement, or append
/// one. "Same-named" ignores quoting: the grammar names `"work_events"` with
/// its quotes and the statement scan names it without, and comparing the raw
/// strings pushed a second copy. A symbol that already has the grammar's span
/// is left as it is, so a table the grammar parsed — including one whose
/// `CHECK(...)` is an error inside it — is not rewritten. A qualified name
/// another symbol already holds is never taken: the install is skipped.
fn install_symbol(
    extraction: &mut Extraction,
    by_name: &mut HashMap<String, Vec<usize>>,
    names: &mut Names,
    install: Install,
) {
    let Install {
        start,
        end,
        desired,
    } = install;
    let key = bare_name(&desired.name);
    let candidates = by_name.get(&key).map(Vec::as_slice).unwrap_or(&[]);
    let overlapping = candidates.iter().copied().find(|&index| {
        let symbol = &extraction.symbols[index];
        overlaps(symbol.span.start_byte, symbol.span.end_byte, start, end)
    });
    if let Some(index) = overlapping {
        let existing = &mut extraction.symbols[index];
        if !names.try_move(&[index], &existing.qualified_name, &desired.qualified_name) {
            return;
        }
        existing.name = desired.name;
        existing.span = desired.span;
        existing.kind = desired.kind;
        existing.qualified_name = desired.qualified_name;
        existing.is_exported = desired.is_exported;
        existing.parent_symbol = desired.parent_symbol;
        return;
    }
    let duplicate = candidates.iter().any(|&index| {
        let symbol = &extraction.symbols[index];
        symbol.span.start_byte == desired.span.start_byte
            && symbol.span.end_byte == desired.span.end_byte
    });
    if duplicate || names.holds(&desired.qualified_name) {
        return;
    }
    let index = extraction.symbols.len();
    names.hold(&desired.qualified_name, index);
    by_name.entry(key).or_default().push(index);
    extraction.symbols.push(desired);
}

fn overlaps(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start < b_end && b_start < a_end
}

struct Scan {
    relations: Vec<Statement>,
    /// Non-relation statements (`CREATE FUNCTION`, `CREATE TRIGGER`, …) that
    /// ended at a terminator, start inclusive and end exclusive, in file order
    /// and disjoint. A symbol that starts strictly inside one of these is a
    /// declaration the body text mentioned, not a schema object. A statement
    /// the end of the file closed is not listed: nothing positively ended it.
    skipped: Vec<(usize, usize)>,
    /// Where the scan lost sync: a quote, comment or block that never closed.
    /// Nothing from here on is read, removed or added.
    unterminated_at: Option<usize>,
}

/// Scan `source` once as written, and once more reading a backslash as an
/// escape when the first reading left a quote open and the text has a `\'` or
/// `\"` that could be one. MySQL escapes quotes that way; standard SQL does
/// not, and the file does not say which it is.
fn scan_source(source: &str, deadline: Instant) -> Result<Scan, Abandon> {
    let first = scan(source, false, deadline)?;
    let Some(first_stop) = first.unterminated_at else {
        return Ok(first);
    };
    if !source.contains("\\'") && !source.contains("\\\"") {
        return Ok(first);
    }
    let second = scan(source, true, deadline)?;
    match second.unterminated_at {
        None => Ok(second),
        Some(stop) if stop > first_stop => Ok(second),
        Some(_) => Ok(first),
    }
}

fn scan(source: &str, backslash: bool, deadline: Instant) -> Result<Scan, Abandon> {
    let bytes = source.as_bytes();
    let mut relations = Vec::new();
    let mut skipped = Vec::new();
    let mut unterminated_at = None;
    let mut i = 0usize;
    let mut steps = 0usize;
    let mut paren = 0i32;
    let mut block = 0i32;
    // MySQL's `DELIMITER $$` changes what ends a statement until the next
    // `DELIMITER`. It is a client directive, not SQL.
    let mut delimiter = ";";
    let timed_out = Cell::new(false);
    while i < bytes.len() {
        steps += 1;
        if steps % DEADLINE_STRIDE == 0 && past(deadline) {
            return Err(Abandon::Deadline);
        }
        let ctx = Ctx {
            backslash,
            delimiter,
            deadline,
            timed_out: &timed_out,
        };
        match eat_noise(source, i, ctx) {
            Eat::Unclosed => {
                unterminated_at = Some(i);
                break;
            }
            Eat::Ate(next) if next > i => {
                i = next;
                continue;
            }
            Eat::Ate(_) | Eat::No => {}
        }
        if bytes[i] == b'(' {
            paren += 1;
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            paren = paren.saturating_sub(1);
            i += 1;
            continue;
        }
        // `GO` is T-SQL's batch separator and T-SQL forbids it inside a
        // `BEGIN`; a lone `go` in a body elsewhere is a name.
        if paren == 0 && block == 0 {
            if let Some(next) = go_line(source, i) {
                i = next;
                continue;
            }
        }
        if paren == 0 && block == 0 {
            if let Some((token, next)) = delimiter_directive(source, i) {
                delimiter = token;
                i = next;
                continue;
            }
        }
        if paren == 0 {
            if let Some((next, delta)) = block_token(source, i, block > 0) {
                block += delta;
                i = next;
                continue;
            }
        }
        if paren == 0 && block == 0 && keyword(source, i, "CREATE") {
            let classified = classify(source, i, ctx);
            if timed_out.get() {
                return Err(Abandon::Deadline);
            }
            match classified {
                Classified::Relation(statement) => {
                    if relations.len() >= MAX_STATEMENTS {
                        return Err(Abandon::Cap("relation statement"));
                    }
                    i = statement.end;
                    relations.push(statement);
                }
                Classified::Skip(next, closed) if next > i => {
                    if closed {
                        if skipped.len() >= MAX_SKIPPED {
                            return Err(Abandon::Cap("skipped statement"));
                        }
                        skipped.push((i, next));
                    }
                    i = next;
                }
                Classified::Skip(..) | Classified::Stop => {
                    unterminated_at = Some(i);
                    break;
                }
            }
            continue;
        }
        i = next_char(source, i);
    }
    Ok(Scan {
        relations,
        skipped,
        unterminated_at,
    })
}

/// How the text around a statement is read: whether a backslash escapes a
/// quote, and what ends a statement.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    backslash: bool,
    delimiter: &'a str,
    /// The file's deadline, read inside a long statement. `timed_out` records
    /// that it was hit, since the reader answers `None` either way.
    deadline: Instant,
    timed_out: &'a Cell<bool>,
}

impl Ctx<'_> {
    fn custom_delimiter(&self) -> bool {
        self.delimiter != ";"
    }

    /// A `$$` that is the delimiter is not the start of a dollar quote.
    fn dollar_quotes(&self) -> bool {
        !self.delimiter.contains('$')
    }
}

/// T-SQL's batch separator: a line holding only `GO`, with an optional repeat
/// count. It ends the batch whatever is open, so it ends a statement. Returns
/// the byte after the line.
fn go_line(source: &str, i: usize) -> Option<usize> {
    if !keyword(source, i, "GO") || !at_line_start(source, i) {
        return None;
    }
    let line_end = source[i..]
        .find('\n')
        .map_or(source.len(), |offset| i + offset);
    let rest = source[i + "GO".len()..line_end].trim();
    rest.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then_some((line_end + 1).min(source.len()))
}

/// Only blanks stand between `i` and the start of its line. Walks back over the
/// blanks, not the line, so many `#` or `GO` on one long line cost nothing.
fn at_line_start(source: &str, i: usize) -> bool {
    let bytes = source.as_bytes();
    let mut at = i;
    while at > 0 && matches!(bytes[at - 1], b' ' | b'\t' | b'\r') {
        at -= 1;
    }
    at == 0 || bytes[at - 1] == b'\n'
}

/// A `DELIMITER <token>` line at the start of a line: the token and the byte
/// after the line.
fn delimiter_directive(source: &str, i: usize) -> Option<(&str, usize)> {
    if !keyword(source, i, "DELIMITER") || !at_line_start(source, i) {
        return None;
    }
    let line_end = source[i..]
        .find('\n')
        .map_or(source.len(), |offset| i + offset);
    let token = source[i + "DELIMITER".len()..line_end]
        .split_whitespace()
        .next()?;
    Some((token, (line_end + 1).min(source.len())))
}

/// What the block keyword at `i` does to the nesting depth, and where the
/// scan resumes: `+1` for a block opener, `-1` for a closer, `0` for a token
/// that is consumed and changes nothing.
///
/// * `BEGIN` opens a block unless it starts a transaction (followed by `;`,
///   end of file, or `TRANSACTION`, `TRAN`, `WORK`, `DEFERRED`, `IMMEDIATE`,
///   `EXCLUSIVE`, `ISOLATION` or `READ`) or is a name inside an expression
///   (`SELECT begin FROM r`, `begin = 1`).
/// * `CASE` opens one. `END` closes one — but `END IF`, `END LOOP`, `END
///   WHILE`, `END REPEAT` and `END FOR` close a construct the scan never
///   opened, so they change nothing, and `END CASE` closes the `CASE` and must
///   not let the `CASE` after it open another. Those forms are recognised only
///   when a `;` follows (with an optional label), so a T-SQL `END` followed by
///   an `IF` statement is still an `END`.
/// * `END` outside a block (`END;` after a Postgres `BEGIN;`) is not a token.
fn block_token(source: &str, i: usize, in_block: bool) -> Option<(usize, i32)> {
    if keyword(source, i, "BEGIN") {
        let after = i + "BEGIN".len();
        return Some((after, i32::from(begins_block(source, after))));
    }
    if keyword(source, i, "CASE") {
        return Some((i + "CASE".len(), 1));
    }
    if in_block && keyword(source, i, "END") {
        return Some(end_token(source, i + "END".len()));
    }
    None
}

fn begins_block(source: &str, after_begin: usize) -> bool {
    let Some(at) = skip_sep(source, after_begin) else {
        return true;
    };
    if at >= source.len() || source.as_bytes()[at] == b';' {
        return false;
    }
    let first = source.as_bytes()[at];
    // `begin` used as a name: `SELECT begin, end FROM t`, `begin = 1`, `r.begin`.
    if matches!(
        first,
        b',' | b'.'
            | b')'
            | b'='
            | b'<'
            | b'>'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'|'
            | b'&'
            | b'^'
            | b'!'
            | b'~'
            | b':'
            | b'['
    ) {
        return false;
    }
    let Some((word, _)) = read_word(source, at) else {
        return true;
    };
    ![
        "TRANSACTION",
        "TRAN",
        "WORK",
        "DEFERRED",
        "IMMEDIATE",
        "EXCLUSIVE",
        "ISOLATION",
        "READ",
        // `SELECT begin FROM r`: the next word continues an expression.
        "FROM",
        "AS",
        "AND",
        "OR",
        "WHERE",
        "ON",
        "IS",
        "IN",
        "BETWEEN",
        "LIKE",
        "ILIKE",
        "ASC",
        "DESC",
        "GROUP",
        "ORDER",
        "HAVING",
        "LIMIT",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "JOIN",
        "LEFT",
        "RIGHT",
        "INNER",
        "OUTER",
        "CROSS",
        "USING",
        "THEN",
        "ELSE",
    ]
    .iter()
    .any(|transaction| word.eq_ignore_ascii_case(transaction))
}

fn end_token(source: &str, after_end: usize) -> (usize, i32) {
    let plain = (after_end, -1);
    let Some(at) = skip_sep(source, after_end) else {
        return plain;
    };
    let Some((word, word_end)) = read_word(source, at) else {
        return plain;
    };
    let delta = if word.eq_ignore_ascii_case("CASE") {
        -1
    } else if ["IF", "LOOP", "WHILE", "REPEAT", "FOR"]
        .iter()
        .any(|closer| word.eq_ignore_ascii_case(closer))
    {
        0
    } else {
        return plain;
    };
    let Some(mut after) = skip_sep(source, word_end) else {
        return plain;
    };
    // An optional label: `END LOOP lp;`.
    if !word.eq_ignore_ascii_case("IF") {
        if let Some((_, label_end)) = read_word(source, after) {
            match skip_sep(source, label_end) {
                Some(next) => after = next,
                None => return plain,
            }
        }
    }
    if after >= source.len() || source.as_bytes()[after] == b';' {
        (word_end, delta)
    } else {
        plain
    }
}

enum Classified {
    Relation(Statement),
    /// A `CREATE` that is not a relation, consumed through its end, and
    /// whether a terminator (rather than the end of the file) ended it.
    Skip(usize, bool),
    /// No terminator. The tail of the file is not a statement.
    Stop,
}

fn classify(source: &str, create_at: usize, ctx: Ctx) -> Classified {
    let Some(mut cursor) = skip_sep(source, create_at + "CREATE".len()) else {
        return Classified::Stop;
    };
    let mut virtual_table = false;
    let kind = loop {
        let Some((word, next)) = read_word(source, cursor) else {
            return finish_non_relation(source, create_at, ctx);
        };
        let upper = word.to_ascii_uppercase();
        match upper.as_str() {
            "OR" => {
                let Some(after) = skip_sep(source, next) else {
                    return Classified::Stop;
                };
                let Some((replace, after_replace)) = read_word(source, after) else {
                    return finish_non_relation(source, create_at, ctx);
                };
                if !replace.eq_ignore_ascii_case("REPLACE") {
                    return finish_non_relation(source, create_at, ctx);
                }
                let Some(after_sep) = skip_sep(source, after_replace) else {
                    return Classified::Stop;
                };
                cursor = after_sep;
            }
            "TEMP" | "TEMPORARY" | "UNLOGGED" | "GLOBAL" | "LOCAL" | "UNIQUE" => {
                let Some(after) = skip_sep(source, next) else {
                    return Classified::Stop;
                };
                cursor = after;
            }
            "VIRTUAL" => {
                virtual_table = true;
                let Some(after) = skip_sep(source, next) else {
                    return Classified::Stop;
                };
                cursor = after;
            }
            "MATERIALIZED" => {
                let Some(after) = skip_sep(source, next) else {
                    return Classified::Stop;
                };
                let Some((view, _)) = read_word(source, after) else {
                    return finish_non_relation(source, create_at, ctx);
                };
                if !view.eq_ignore_ascii_case("VIEW") {
                    return finish_non_relation(source, create_at, ctx);
                }
                // Leave the cursor on VIEW so the step below consumes it,
                // the same way TABLE and VIEW already sit under the cursor.
                cursor = after;
                break RelationKind::View;
            }
            "TABLE" => break RelationKind::Table,
            "VIEW" => break RelationKind::View,
            _ => return finish_non_relation(source, create_at, ctx),
        }
    };
    let Some(mut cursor) = skip_sep(
        source,
        match kind {
            RelationKind::Table | RelationKind::View => {
                // `cursor` still points at the kind word. Step over it.
                let Some((_, next)) = read_word(source, cursor) else {
                    return Classified::Stop;
                };
                next
            }
        },
    ) else {
        return Classified::Stop;
    };
    if let Some((word, next)) = read_word(source, cursor) {
        if word.eq_ignore_ascii_case("IF") {
            let Some(after_if) = skip_sep(source, next) else {
                return Classified::Stop;
            };
            let Some((not, after_not)) = read_word(source, after_if) else {
                return finish_non_relation(source, create_at, ctx);
            };
            if !not.eq_ignore_ascii_case("NOT") {
                return finish_non_relation(source, create_at, ctx);
            }
            let Some(after_not_sep) = skip_sep(source, after_not) else {
                return Classified::Stop;
            };
            let Some((exists, after_exists)) = read_word(source, after_not_sep) else {
                return finish_non_relation(source, create_at, ctx);
            };
            if !exists.eq_ignore_ascii_case("EXISTS") {
                return finish_non_relation(source, create_at, ctx);
            }
            let Some(after) = skip_sep(source, after_exists) else {
                return Classified::Stop;
            };
            cursor = after;
        }
    }
    let Some((name, delimited)) = read_qualified_name(source, cursor) else {
        return finish_non_relation(source, create_at, ctx);
    };
    let Some((end, _)) = statement_end(source, create_at, ctx) else {
        return Classified::Stop;
    };
    if name.is_empty() || end <= create_at {
        return finish_non_relation(source, create_at, ctx);
    }
    Classified::Relation(Statement {
        start: create_at,
        end,
        name,
        delimited,
        virtual_table: virtual_table && matches!(kind, RelationKind::Table),
    })
}

enum RelationKind {
    Table,
    View,
}

fn finish_non_relation(source: &str, create_at: usize, ctx: Ctx) -> Classified {
    match statement_end(source, create_at, ctx) {
        Some((end, closed)) if end > create_at => Classified::Skip(end, closed),
        _ => Classified::Stop,
    }
}

/// Byte after the statement's terminator and whether a terminator ended it.
/// The terminator is a semicolon at parenthesis and `BEGIN`/`CASE` depth zero,
/// or, after a `DELIMITER` directive, that delimiter wherever it stands outside
/// a quote or comment. The end of the file ends a statement too — the grammar
/// accepts a last statement without its `;` — when nothing is open: no
/// parenthesis, no block, no quote or comment. `None` when the statement never
/// ends.
fn statement_end(source: &str, start: usize, ctx: Ctx) -> Option<(usize, bool)> {
    let bytes = source.as_bytes();
    let custom = ctx.custom_delimiter();
    let mut i = start;
    let mut steps = 0usize;
    let mut paren = 0i32;
    let mut block = 0i32;
    while i < bytes.len() {
        steps += 1;
        if steps % DEADLINE_STRIDE == 0 && past(ctx.deadline) {
            ctx.timed_out.set(true);
            return None;
        }
        match eat_noise(source, i, ctx) {
            Eat::Unclosed => return None,
            Eat::Ate(next) if next > i => {
                i = next;
                continue;
            }
            Eat::Ate(_) | Eat::No => {}
        }
        if custom && source[i..].starts_with(ctx.delimiter) {
            return Some((i + ctx.delimiter.len(), true));
        }
        // A `GO` line ends a T-SQL batch, and so the statement before it.
        if paren == 0 && block == 0 && go_line(source, i).is_some() {
            let end = source[start..i].trim_end().len() + start;
            return (end > start).then_some((end, true));
        }
        match bytes[i] {
            b'(' => {
                paren += 1;
                i += 1;
            }
            b')' => {
                paren = paren.saturating_sub(1);
                i += 1;
            }
            b';' if !custom && paren == 0 && block == 0 => return Some((i + 1, true)),
            _ => {
                let token = if paren == 0 && !custom {
                    block_token(source, i, block > 0)
                } else {
                    None
                };
                match token {
                    Some((next, delta)) => {
                        block += delta;
                        i = next;
                    }
                    None => i = next_char(source, i),
                }
            }
        }
    }
    if !custom && (paren != 0 || block != 0) {
        return None;
    }
    let end = source[start..].trim_end().len() + start;
    (end > start).then_some((end, false))
}

/// The last component of a possibly qualified name, and whether it was written
/// between quotes or brackets.
fn read_qualified_name(source: &str, start: usize) -> Option<(String, bool)> {
    let (mut name, mut cursor) = read_ident(source, start)?;
    let mut delimited = is_delimited_at(source, start);
    loop {
        let Some(after) = skip_sep(source, cursor) else {
            return None;
        };
        if after >= source.len() || source.as_bytes()[after] != b'.' {
            break;
        }
        let Some(after_dot) = skip_sep(source, after + 1) else {
            return None;
        };
        let (next, next_cursor) = read_ident(source, after_dot)?;
        name = next;
        delimited = is_delimited_at(source, after_dot);
        cursor = next_cursor;
    }
    (!name.is_empty()).then_some((name, delimited))
}

fn is_delimited_at(source: &str, at: usize) -> bool {
    matches!(source.as_bytes().get(at), Some(b'"' | b'`' | b'['))
}

/// An identifier at `start`. An unquoted name is read as Unicode letters,
/// digits, `_` and `$`; if the character after it is a non-ASCII one that is not
/// part of a word (a combining mark, a symbol), the name cannot be read in full
/// and this abstains rather than return a prefix.
fn read_ident(source: &str, start: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    if start >= bytes.len() {
        return None;
    }
    match bytes[start] {
        b'"' => read_quoted(source, start, b'"', b'"'),
        b'`' => read_quoted(source, start, b'`', b'`'),
        b'[' => read_quoted(source, start, b']', b']'),
        _ => {
            let end = word_end(source, start)?;
            let next = source[end..].chars().next();
            if next.is_some_and(|ch| !ch.is_ascii() && !ch.is_whitespace()) {
                return None;
            }
            Some((source[start..end].to_string(), end))
        }
    }
}

/// The interior of a quoted ident. A doubled closer is the closer itself.
fn read_quoted(source: &str, start: usize, closer: u8, escape: u8) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    let mut i = start + 1;
    let mut name = String::new();
    while i < bytes.len() {
        if bytes[i] == closer {
            let doubled = i + 1 < bytes.len() && bytes[i + 1] == escape && closer == escape;
            if doubled || (closer == b']' && i + 1 < bytes.len() && bytes[i + 1] == b']') {
                name.push(bytes[i] as char);
                i += 2;
                continue;
            }
            return Some((name, i + 1));
        }
        let ch = source[i..].chars().next()?;
        name.push(ch);
        i += ch.len_utf8();
    }
    None
}

fn read_word(source: &str, start: usize) -> Option<(&str, usize)> {
    let end = word_end(source, start)?;
    Some((&source[start..end], end))
}

/// End of the word that starts at `start`: a letter or `_`, then letters,
/// digits, `_` and `$`, in any script.
fn word_end(source: &str, start: usize) -> Option<usize> {
    let mut chars = source.get(start..)?.char_indices();
    let (_, first) = chars.next()?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    let mut end = start + first.len_utf8();
    for (offset, ch) in chars {
        if !is_ident_char(ch) {
            break;
        }
        end = start + offset + ch.len_utf8();
    }
    Some(end)
}

fn skip_sep(source: &str, mut i: usize) -> Option<usize> {
    loop {
        if i >= source.len() {
            return Some(i);
        }
        match eat_separator(source, i) {
            Eat::Unclosed => return None,
            Eat::Ate(next) if next > i => i = next,
            Eat::Ate(_) | Eat::No => return Some(i),
        }
    }
}

enum Eat {
    No,
    Ate(usize),
    Unclosed,
}

/// Whitespace, comments, strings, dollar quotes and bracketed names. Used
/// inside a statement, where a quoted semicolon must not end it.
fn eat_noise(source: &str, i: usize, ctx: Ctx) -> Eat {
    match eat_separator(source, i) {
        Eat::Unclosed => return Eat::Unclosed,
        Eat::Ate(next) if next > i => return Eat::Ate(next),
        Eat::Ate(_) | Eat::No => {}
    }
    if i < source.len() && source.as_bytes()[i] == b'[' {
        if let Some(end) = eat_brackets(source.as_bytes(), i) {
            return Eat::Ate(end);
        }
        return Eat::No;
    }
    if ctx.dollar_quotes() {
        if let Some(eaten) = eat_dollar(source, i) {
            return eaten;
        }
    }
    if let Some(eaten) = eat_string(source, i, ctx.backslash) {
        return eaten;
    }
    Eat::No
}

/// Longest bracket run read as one token. A T-SQL name is at most 128
/// characters; a longer one is read as ordinary text, which only matters if it
/// hides a quote or a semicolon.
const MAX_BRACKET_RUN: usize = 256;

/// `[` to its matching `]` as a depth counter, so `a[b[1]]`, `ARRAY[[1,2],[3]]`
/// and `int[][]` close where they should. `None` when it does not close within
/// [`MAX_BRACKET_RUN`] bytes.
fn eat_brackets(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let limit = bytes.len().min(start + MAX_BRACKET_RUN);
    for (offset, byte) in bytes[start..limit].iter().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn eat_separator(source: &str, i: usize) -> Eat {
    if i >= source.len() {
        return Eat::No;
    }
    let bytes = source.as_bytes();
    if bytes[i].is_ascii_whitespace() {
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        return Eat::Ate(j);
    }
    if bytes[i] == b'#' && starts_hash_comment(source, i) {
        let end = source[i..]
            .find('\n')
            .map(|offset| i + offset + 1)
            .unwrap_or(source.len());
        return Eat::Ate(end);
    }
    if source[i..].starts_with("--") {
        let rest = i + 2;
        let end = source[rest..]
            .find('\n')
            .map(|offset| rest + offset + 1)
            .unwrap_or(source.len());
        return Eat::Ate(end);
    }
    if source[i..].starts_with("/*") {
        return nested_comment_end(bytes, i).map_or(Eat::Unclosed, Eat::Ate);
    }
    Eat::No
}

/// End of the block comment that starts at `start`. Comments nest, as in
/// Postgres and the SQL standard: a `/*` inside one needs its own `*/`. SQLite
/// and MySQL do not nest, so in those files a comment that contains `/*` reads
/// as longer than it is. That only hides text from the scan; it never adds a
/// declaration.
fn nested_comment_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b'/' && bytes[i + 1] == b'*' {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// MySQL's `#` line comment. In Postgres `#>`, `#>>` and `#=` are operators,
/// and in T-SQL `#name` is a temporary table, so a `#` that is followed by an
/// operator character or glued to a name is a comment only when it opens its
/// line.
fn starts_hash_comment(source: &str, i: usize) -> bool {
    let bytes = source.as_bytes();
    match bytes.get(i + 1) {
        Some(b'>' | b'=') => false,
        Some(next) if is_ident_start(*next) => at_line_start(source, i),
        _ => true,
    }
}

fn eat_dollar(source: &str, i: usize) -> Option<Eat> {
    if !source[i..].starts_with('$') {
        return None;
    }
    let after = i + 1;
    let tag_len = if source[after..].starts_with('$') {
        0
    } else {
        let len = source[after..]
            .bytes()
            .take_while(|byte| is_ident_continue(*byte))
            .count();
        if len == 0 || !source[after + len..].starts_with('$') {
            return None;
        }
        len
    };
    let closer_len = 1 + tag_len + 1;
    let closer = &source[i..i + closer_len];
    let body = i + closer_len;
    match source[body..].find(closer) {
        Some(offset) => Some(Eat::Ate(body + offset + closer.len())),
        None => Some(Eat::Unclosed),
    }
}

fn eat_string(source: &str, i: usize, backslash: bool) -> Option<Eat> {
    let bytes = source.as_bytes();
    if i >= bytes.len() {
        return None;
    }
    let mut quote_at = i;
    // `E'...'` is an escape string: the grammar reads a backslash in it as an
    // escape whatever the rest of the file does.
    let mut escape_string = false;
    if matches!(
        bytes[i],
        b'N' | b'n' | b'X' | b'x' | b'E' | b'e' | b'B' | b'b'
    ) && (i == 0 || !is_ident_continue(bytes[i - 1]))
        && i + 1 < bytes.len()
        && bytes[i + 1] == b'\''
    {
        quote_at = i + 1;
        escape_string = matches!(bytes[i], b'E' | b'e');
    } else if (bytes[i] == b'U' || bytes[i] == b'u')
        && (i == 0 || !is_ident_continue(bytes[i - 1]))
        && source[i + 1..].starts_with("&'")
    {
        quote_at = i + 2;
    }
    let quote = *bytes.get(quote_at)?;
    if quote != b'\'' && quote != b'"' && quote != b'`' {
        return None;
    }
    // A `"` or `` ` `` here is an identifier when the header reader is in
    // charge. Inside a statement both still hide semicolons, which is what
    // this eater is for. At the header, `read_ident` runs before the body
    // scan, so consuming the name as noise there never happens.
    Some(quoted_span(
        source,
        quote_at,
        quote,
        backslash || escape_string,
    ))
}

/// A `\'` in the middle of a word (`'O\'Brien'`) cannot end a standard string:
/// SQL has no token that starts with a letter straight after a closing quote.
/// So even when backslashes are not escapes, it is one here.
fn escapes_a_name(bytes: &[u8], backslash_at: usize, quote: u8) -> bool {
    bytes.get(backslash_at + 1) == Some(&quote)
        && bytes
            .get(backslash_at + 2)
            .is_some_and(|next| is_ident_continue(*next))
}

/// `backslash` reads `\x` as one character, so `'O\'Brien'` is one string. It
/// applies to the quotes that make a string, not to backticks.
fn quoted_span(source: &str, quote_at: usize, quote: u8, backslash: bool) -> Eat {
    let bytes = source.as_bytes();
    let mut i = quote_at + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && quote != b'`' && (backslash || escapes_a_name(bytes, i, quote)) {
            i = next_char(source, next_char(source, i));
            continue;
        }
        if bytes[i] == quote {
            if i + 1 < bytes.len() && bytes[i + 1] == quote {
                i += 2;
                continue;
            }
            return Eat::Ate(i + 1);
        }
        i = next_char(source, i);
    }
    Eat::Unclosed
}

fn keyword(source: &str, i: usize, word: &str) -> bool {
    let Some(rest) = source.get(i..) else {
        return false;
    };
    let after = i + word.len();
    // `word` is ASCII, but the source bytes under it may split a multibyte
    // character. Slicing there panics; it is also not this keyword.
    if rest.len() < word.len() || !source.is_char_boundary(after) {
        return false;
    }
    if !rest[..word.len()].eq_ignore_ascii_case(word) {
        return false;
    }
    if source[after..]
        .chars()
        .next()
        .is_some_and(|ch| is_ident_char(ch))
    {
        return false;
    }
    if i > 0
        && source[..i]
            .chars()
            .next_back()
            .is_some_and(|ch| is_ident_char(ch))
    {
        return false;
    }
    true
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

fn next_char(source: &str, i: usize) -> usize {
    let Some(rest) = source.get(i..) else {
        // Mid-character: step one byte so the scan cannot stick or panic.
        return (i + 1).min(source.len());
    };
    match rest.chars().next() {
        Some(ch) => i + ch.len_utf8(),
        None => source.len(),
    }
}

fn same_name(found: &str, wanted: &str) -> bool {
    found == wanted || unquote(found).as_deref() == Some(wanted)
}

/// Strip the quotes of a name that is one delimited identifier: `"a"`,
/// `` `a` `` or `[a]`, where a doubled closer is one character of the name.
/// `None` for anything else, including `"s"."t"` (a closing quote in the middle
/// ends the identifier early, so the whole is not one).
fn unquote(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let closer = match bytes[0] {
        b'"' | b'`' => bytes[0],
        b'[' => b']',
        _ => return None,
    };
    if *bytes.last()? != closer {
        return None;
    }
    let end = bytes.len() - 1;
    let mut out = String::new();
    let mut i = 1usize;
    while i < end {
        if bytes[i] == closer {
            // Only a doubled closer is a character of the name.
            if i + 1 < end && bytes[i + 1] == closer {
                out.push(closer as char);
                i += 2;
                continue;
            }
            return None;
        }
        let ch = name.get(i..)?.chars().next()?;
        if i + ch.len_utf8() > end {
            return None;
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    (!out.is_empty()).then_some(out)
}

/// Strip the quotes off every quoted name. Symbols that share one qualified
/// name (the overloads `"f"(int)` and `"f"(text)`) are one group: they all
/// take the unquoted name, or none of them does, so no edge is moved onto a
/// symbol that is only one of them.
fn normalize_quoted_names(extraction: &mut Extraction, file: &str, names: &mut Names) {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<(String, String), usize> = HashMap::new();
    for (index, symbol) in extraction.symbols.iter().enumerate() {
        if symbol.kind == SymbolKind::File || unquote(&symbol.name).is_none() {
            continue;
        }
        let key = (symbol.qualified_name.clone(), symbol.name.clone());
        match group_of.get(&key) {
            Some(&group) => groups[group].push(index),
            None => {
                group_of.insert(key, groups.len());
                groups.push(vec![index]);
            }
        }
    }
    for group in groups {
        let symbol = &extraction.symbols[group[0]];
        let Some(bare) = unquote(&symbol.name) else {
            continue;
        };
        rename_group(&mut extraction.symbols, &group, &bare, file, names);
    }
}

/// Give symbols that share a name and a qualified name a new name, and record
/// the qualified name they left so the edges that named them can follow. The
/// group must be every symbol that holds that qualified name, and nothing may
/// hold the new one ([`Names::try_move`]); otherwise nothing changes and the
/// symbols keep the grammar's name. This is the one place a name is rewritten.
fn rename_group(
    symbols: &mut [ExtractedSymbol],
    group: &[usize],
    name: &str,
    file: &str,
    names: &mut Names,
) {
    let Some(&lead) = group.first() else {
        return;
    };
    if symbols[lead].name == name {
        return;
    }
    let old_qualified = symbols[lead].qualified_name.clone();
    let new_qualified = renamed(&old_qualified, &symbols[lead].name, name, file);
    if !names.try_move(group, &old_qualified, &new_qualified) {
        return;
    }
    for &index in group {
        symbols[index].qualified_name = new_qualified.clone();
        symbols[index].name = name.to_string();
    }
}

/// Replace the `::{old_name}` component. A bare `strip_suffix(old_name)` would
/// also eat a shorter name off the end of a longer one (`data` / `a`).
fn renamed(qualified: &str, old_name: &str, new_name: &str, file: &str) -> String {
    let suffix = format!("::{old_name}");
    match qualified.strip_suffix(&suffix) {
        Some(prefix) => format!("{prefix}::{new_name}"),
        None => format!("{file}::{new_name}"),
    }
}

/// Point every edge whose enclosing symbol was renamed at the new name, in one
/// pass over the edges.
fn retarget(extraction: &mut Extraction, renames: &HashMap<String, String>) {
    if renames.is_empty() {
        return;
    }
    for call in &mut extraction.calls {
        if let Some(new) = call
            .caller_symbol
            .as_deref()
            .and_then(|caller| renames.get(caller))
        {
            call.caller_symbol = Some(new.clone());
        }
    }
    for reference in &mut extraction.references {
        if let Some(new) = reference
            .enclosing_symbol
            .as_deref()
            .and_then(|enclosing| renames.get(enclosing))
        {
            reference.enclosing_symbol = Some(new.clone());
        }
    }
}

/// Drop declarations that are not schema objects: a symbol whose start lies
/// strictly inside a skipped body that ended at a terminator (a table written
/// in a trigger or routine body) and that overlaps a grammar error range. The
/// start decides, because the grammar can hand a symbol inside a routine a span
/// that runs on past the routine's end. The scan is a second reader of the
/// text; where the grammar had no complaint it is not allowed to overrule it,
/// so a file that parsed `Clean` loses nothing, and a scan that lost sync
/// removes nothing. Edges that named a removed symbol as their caller go with
/// it. A symbol that begins at the skipped statement itself — the routine —
/// stays.
fn drop_spurious(extraction: &mut Extraction, scan: &Scan, errors: &ErrorRanges) {
    if errors.is_empty() || scan.skipped.is_empty() {
        return;
    }
    let mut removed: HashSet<String> = HashSet::new();
    extraction.symbols.retain(|symbol| {
        if symbol.kind == SymbolKind::File
            || !errors.overlaps(symbol.span.start_byte, symbol.span.end_byte)
        {
            return true;
        }
        let at = scan
            .skipped
            .partition_point(|&(start, _)| start < symbol.span.start_byte);
        let inside_skipped = at
            .checked_sub(1)
            .and_then(|previous| scan.skipped.get(previous))
            .is_some_and(|&(_, end)| symbol.span.start_byte < end);
        if inside_skipped {
            removed.insert(symbol.qualified_name.clone());
            return false;
        }
        true
    });
    if removed.is_empty() {
        return;
    }
    // A qualified name another symbol still carries keeps its edges.
    for symbol in &extraction.symbols {
        removed.remove(&symbol.qualified_name);
    }
    if removed.is_empty() {
        return;
    }
    extraction.calls.retain(|call| match &call.caller_symbol {
        Some(caller) => !removed.contains(caller),
        None => true,
    });
    extraction
        .references
        .retain(|reference| match &reference.enclosing_symbol {
            Some(enclosing) => !removed.contains(enclosing),
            None => true,
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::extract_treesitter;
    use std::time::Duration;

    fn grammar(source: &str) -> Extraction {
        extract_treesitter("t.sql", "sql", source)
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    /// Everything recovery can touch, rendered so two extractions compare.
    fn shape(extraction: &Extraction) -> String {
        format!(
            "{:?}\n{:?}\n{:?}\n{:?}",
            extraction.parse_outcome, extraction.symbols, extraction.calls, extraction.references
        )
    }

    const SWALLOWED: &str = "\
CREATE VIRTUAL TABLE docs_fts USING fts5(title);
CREATE TABLE work_events (id INT CHECK(json_valid(id)));
";

    #[test]
    fn recovery_changes_the_grammar_output_given_time() {
        let mut extraction = grammar(SWALLOWED);
        let before = shape(&extraction);
        recover_lost_relations(&mut extraction, SWALLOWED, far());
        assert_ne!(
            shape(&extraction),
            before,
            "the control case needs recovery"
        );
        assert!(
            extraction.diagnostics.is_empty(),
            "{:?}",
            extraction.diagnostics
        );
    }

    #[test]
    fn a_passed_deadline_leaves_the_grammar_output_untouched() {
        let mut extraction = grammar(SWALLOWED);
        let before = shape(&extraction);
        recover_lost_relations(&mut extraction, SWALLOWED, Instant::now());
        assert_eq!(shape(&extraction), before);
        assert!(
            matches!(extraction.parse_outcome, ParseOutcome::Partial { .. }),
            "an abandoned recovery must not say Clean: {:?}",
            extraction.parse_outcome
        );
        assert_eq!(
            extraction.diagnostics.len(),
            1,
            "{:?}",
            extraction.diagnostics
        );
        assert!(extraction.diagnostics[0].contains("abandoned"));
    }

    #[test]
    fn a_clean_file_is_untouched_and_stays_clean_when_the_deadline_passed() {
        let source = "CREATE TABLE `a` (id INT);\nCREATE TABLE b (id INT);\n";
        let mut extraction = grammar(source);
        assert_eq!(extraction.parse_outcome, ParseOutcome::Clean);
        let before = shape(&extraction);
        recover_lost_relations(&mut extraction, source, Instant::now());
        assert_eq!(shape(&extraction), before);
    }

    #[test]
    fn past_the_reparse_cap_the_grammar_output_is_left_alone() {
        let mut source = String::new();
        for index in 0..(MAX_REPARSES + 40) {
            source.push_str(&format!("CREATE VIRTUAL TABLE v{index} USING fts5(x);\n"));
            source.push_str(&format!("CREATE TABLE t{index} (id INT);\n"));
        }
        let mut extraction = grammar(&source);
        let before = shape(&extraction);
        recover_lost_relations(&mut extraction, &source, far());
        assert_eq!(shape(&extraction), before);
        assert!(
            extraction
                .diagnostics
                .iter()
                .any(|line| line.contains("reparse cap")),
            "{:?}",
            extraction.diagnostics
        );
    }

    #[test]
    fn error_ranges_merge_and_overlap_by_shared_bytes() {
        let ranges = ErrorRanges::new([(10, 20), (15, 25), (40, 40)].into_iter());
        assert_eq!(ranges.merged, vec![(10, 25), (40, 40)]);
        assert!(ranges.overlaps(0, 11) && ranges.overlaps(24, 30));
        assert!(!ranges.overlaps(0, 10) && !ranges.overlaps(25, 30));
        assert!(ranges.overlaps(30, 41) && !ranges.overlaps(30, 40));
        assert!(!ErrorRanges::default().overlaps(0, usize::MAX));
    }

    fn with_ctx<R>(delimiter: &str, run: impl FnOnce(Ctx) -> R) -> R {
        let timed_out = Cell::new(false);
        run(Ctx {
            backslash: false,
            delimiter,
            deadline: far(),
            timed_out: &timed_out,
        })
    }

    #[test]
    fn the_end_of_the_file_ends_a_statement_only_when_nothing_is_open() {
        let end = |text: &str| with_ctx(";", |ctx| statement_end(text, 0, ctx));
        assert_eq!(end("CREATE TABLE a(id int)"), Some((22, false)));
        assert_eq!(end("CREATE TABLE a(id int)  \n"), Some((22, false)));
        assert_eq!(end("CREATE TABLE a(id int);"), Some((23, true)));
        assert_eq!(end("CREATE TABLE a(id int"), None);
        assert_eq!(end("CREATE TABLE a(n TEXT DEFAULT 'x"), None);
        assert_eq!(end("CREATE TABLE a(id int) /* open"), None);
        assert_eq!(
            end("CREATE TRIGGER t AFTER INSERT ON a BEGIN SELECT 1;"),
            None
        );
    }

    #[test]
    fn a_delimiter_directive_ends_a_routine_where_the_client_does() {
        let source = "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; END$$\nDELIMITER ;\n";
        let (token, next) = delimiter_directive(source, 0).expect("directive");
        assert_eq!((token, next), ("$$", 13));
        let create = source.find("CREATE").unwrap();
        let (end, closed) = with_ctx("$$", |ctx| statement_end(source, create, ctx)).unwrap();
        assert!(closed);
        assert_eq!(&source[end - 5..end], "END$$");
        assert!(delimiter_directive("  x DELIMITER $$\n", 4).is_none());
    }

    #[test]
    fn a_go_line_ends_a_batch_and_a_hash_scan_costs_nothing_per_hash() {
        assert_eq!(go_line("GO\nx", 0), Some(3));
        assert_eq!(go_line("  go 5 \nx", 2), Some(8));
        assert!(go_line("GOTO x\n", 0).is_none());
        assert!(go_line("x GO\n", 2).is_none());
        assert!(go_line("GO x\n", 0).is_none());
        let source = "CREATE PROCEDURE p AS BEGIN SELECT 1 END\nGO\nCREATE TABLE t (id int)\n";
        let (end, closed) = with_ctx(";", |ctx| statement_end(source, 0, ctx)).unwrap();
        assert!(closed);
        assert_eq!(&source[..end], "CREATE PROCEDURE p AS BEGIN SELECT 1 END");
        // A `GO` alone on a line inside parentheses is a column, not a batch end.
        let table = "CREATE TABLE t (\n go\n INT)\n";
        assert_eq!(
            with_ctx(";", |ctx| statement_end(table, 0, ctx)),
            Some((table.trim_end().len(), false))
        );
        let long = format!("{}x", "a #".repeat(50_000));
        assert!(!at_line_start(&long, 2));
        let timed_out = Cell::new(false);
        let expired = Ctx {
            backslash: false,
            delimiter: ";",
            deadline: Instant::now(),
            timed_out: &timed_out,
        };
        let big = format!("CREATE TABLE t ({})", "a INT, ".repeat(4096));
        assert_eq!(statement_end(&big, 0, expired), None);
        assert!(
            timed_out.get(),
            "the deadline is checked inside a statement"
        );
    }

    #[test]
    fn an_e_string_always_escapes_a_backslash() {
        let source = "E'a\\' CREATE TABLE \"fake\" (id int) -- ';\nCREATE TABLE real_t (id int);";
        let eaten = eat_string(source, 0, false);
        assert!(matches!(eaten, Some(Eat::Ate(end)) if source[..end].ends_with("-- '")));
        // An ordinary string is not: the backslash is a literal and the second
        // quote ends it.
        let plain = "'a\\' CREATE";
        assert!(matches!(eat_string(plain, 0, false), Some(Eat::Ate(4))));
    }

    #[test]
    fn brackets_nest_as_a_depth_and_comments_nest() {
        for text in ["[[1,2],[3,4]]", "[b[1]]", "[][]", "[a]"] {
            let bytes = text.as_bytes();
            let end = eat_brackets(bytes, 0).unwrap();
            assert!(
                end == text.len() || text == "[][]" && end == 2,
                "{text}: {end}"
            );
        }
        assert!(eat_brackets(b"[[1,2]", 0).is_none());
        assert_eq!(nested_comment_end(b"/* a /* b */ c */ x", 0), Some(17));
        assert_eq!(nested_comment_end(b"/* a /* b */ c", 0), None);
    }

    #[test]
    fn a_name_is_unquoted_only_when_it_is_one_delimited_identifier() {
        assert_eq!(unquote("\"a\"").as_deref(), Some("a"));
        assert_eq!(unquote("`a`").as_deref(), Some("a"));
        assert_eq!(unquote("[a b]").as_deref(), Some("a b"));
        assert_eq!(unquote("\"a\"\"b\"").as_deref(), Some("a\"b"));
        assert_eq!(unquote("\"s\".\"t\""), None);
        assert_eq!(unquote("\"s\"x\"t\""), None);
        assert_eq!(unquote("`a`.`b`"), None);
        assert_eq!(unquote("[a].[b]"), None);
        assert_eq!(unquote("\"\""), None);
        assert_eq!(unquote("plain"), None);
    }

    #[test]
    fn a_group_moves_only_when_it_is_every_holder_of_the_old_name() {
        let mut names = Names::default();
        for index in 0..4 {
            names.hold("f.sql::old", index);
        }
        names.hold("f.sql::taken", 9);
        // Two of four holders: refused, nothing recorded.
        assert!(!names.try_move(&[0, 1], "f.sql::old", "f.sql::new"));
        assert!(names.renames.is_empty());
        assert_eq!(names.held["f.sql::old"].len(), 4);
        // As many members as holders, but not the holders: refused.
        assert!(!names.try_move(&[0, 1, 2, 7], "f.sql::old", "f.sql::new"));
        assert!(!names.try_move(&[0, 0, 1, 2], "f.sql::old", "f.sql::new"));
        // The new name is held by someone else: refused.
        assert!(!names.try_move(&[0, 1, 2, 3], "f.sql::old", "f.sql::taken"));
        // Every holder, and the new name free: moved together.
        assert!(names.try_move(&[3, 2, 1, 0], "f.sql::old", "f.sql::new"));
        assert!(!names.held.contains_key("f.sql::old"));
        assert_eq!(names.held["f.sql::new"], vec![3, 2, 1, 0]);
        assert_eq!(names.renames["f.sql::old"], "f.sql::new");
    }

    #[test]
    fn a_name_renamed_twice_ends_every_edge_on_the_last_name() {
        let mut names = Names::default();
        names.hold("f.sql::a", 0);
        assert!(names.try_move(&[0], "f.sql::a", "f.sql::b"));
        assert!(names.try_move(&[0], "f.sql::b", "f.sql::c"));
        assert_eq!(names.renames["f.sql::a"], "f.sql::c");
        assert_eq!(names.renames["f.sql::b"], "f.sql::c");
        // A third hop composes the same way.
        assert!(names.try_move(&[0], "f.sql::c", "f.sql::d"));
        for old in ["f.sql::a", "f.sql::b", "f.sql::c"] {
            assert_eq!(names.renames[old], "f.sql::d", "{old}");
        }
    }

    #[test]
    fn a_name_is_read_in_full_or_not_at_all() {
        assert_eq!(
            read_ident("données USING", 0),
            Some(("données".to_string(), "données".len()))
        );
        assert_eq!(read_ident("表 (", 0).map(|n| n.0), Some("表".to_string()));
        // `e` followed by a combining acute accent: not a word character, so the
        // name is cut short by the reader and it abstains.
        assert!(read_ident("cafe\u{301} (", 0).is_none());
        assert_eq!(read_ident("t(", 0).map(|n| n.0), Some("t".to_string()));
    }

    #[test]
    fn block_tokens_open_and_close_the_way_the_dialects_do() {
        let token = |text: &str, in_block: bool| block_token(text, 0, in_block).map(|t| t.1);
        for transaction in [
            "BEGIN;",
            "BEGIN",
            "BEGIN TRANSACTION;",
            "begin immediate;",
            "BEGIN DEFERRED TRANSACTION;",
            "BEGIN EXCLUSIVE;",
            "BEGIN WORK;",
            "BEGIN -- note\n;",
        ] {
            assert_eq!(token(transaction, false), Some(0), "{transaction}");
        }
        for name in [
            "begin FROM r;",
            "begin, end FROM r;",
            "begin = 1",
            "begin.x",
            "begin)",
            "begin AND x",
        ] {
            assert_eq!(token(name, false), Some(0), "{name}");
        }
        assert_eq!(token("BEGIN\n  SELECT 1; END;", false), Some(1));
        assert_eq!(token("BEGIN ATOMIC SELECT 1; END;", false), Some(1));
        assert_eq!(token("CASE x WHEN 1 THEN 2 END", false), Some(1));
        assert_eq!(token("END;", true), Some(-1));
        assert_eq!(token("END;", false), None);
        for non_closer in [
            "END IF;",
            "END LOOP;",
            "END LOOP lp;",
            "END WHILE;",
            "END REPEAT;",
            "END FOR;",
        ] {
            assert_eq!(token(non_closer, true), Some(0), "{non_closer}");
        }
        assert_eq!(token("END CASE;", true), Some(-1));
        // T-SQL: a block's END followed by an IF statement is still an END.
        assert_eq!(token("END IF @x = 1 SELECT 1", true), Some(-1));
        assert_eq!(token("END CASE_OF_THINGS", true), Some(-1));
    }

    #[test]
    fn a_hash_is_a_comment_unless_it_is_an_operator_or_a_temporary_table_name() {
        assert!(starts_hash_comment("# header\n", 0));
        assert!(starts_hash_comment("a; # note\n", 3));
        assert!(starts_hash_comment("#--------\n", 0));
        assert!(!starts_hash_comment("a #> b", 2));
        assert!(!starts_hash_comment("a #>> b", 2));
        assert!(!starts_hash_comment("a #= b", 2));
        assert!(!starts_hash_comment("CREATE TABLE #tmp (id INT);", 13));
        assert!(starts_hash_comment("  #tmp comment\n", 2));
    }
}
