use super::*;

thread_local! {
    pub(super) static SCOPE_LOCALS: RefCell<HashMap<usize, HashSet<String>>> =
        RefCell::new(HashMap::new());
    /// The deadline for the current file's extraction, readable by helpers that
    /// the walk calls but does not pass arguments to.
    pub(super) static WALK_DEADLINE: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    /// Set once a helper has stopped early because that deadline passed.
    pub(super) static WALK_OVERRAN: Cell<bool> = const { Cell::new(false) };
    /// Ancestor steps taken since the clock was last read. See `bounded_parent`.
    pub(super) static PARENT_STEPS: Cell<u32> = const { Cell::new(0) };
    /// Type walkers that stopped at [`TYPE_NESTING_MAX_DEPTH`] in this file,
    /// and how often. See [`note_nesting_cap`].
    pub(super) static NESTING_CAPS: RefCell<BTreeMap<&'static str, usize>> =
        const { RefCell::new(BTreeMap::new()) };
    #[cfg(test)]
    pub(super) static NATIVE_PARENT_READS: Cell<usize> = const { Cell::new(0) };
}

/// Arms the extraction deadline for exactly as long as one file is being
/// extracted, and disarms it on the way out of *every* path — including the
/// early returns that refuse a file.
///
/// The deadline used to be armed inside `walk_tree` and never cleared, so it
/// outlived the walk it belonged to: every phase that runs afterwards
/// (`go_method_sets`, the Python passes, `clonesig`) saw a live deadline
/// belonging to the *previous* file, and the next file's pre-walk phases saw
/// one that had already expired. Nothing consulted it there yet, which is the
/// only reason it was invisible; extending the bound past the walk — the point
/// of this type — makes that stale value load-bearing. Tying arm and disarm to
/// a scope means the invariant cannot be broken by adding a `return`.
pub(super) struct BudgetGuard;

impl BudgetGuard {
    pub(super) fn arm(deadline: std::time::Instant) -> Self {
        reset_scope_locals();
        WALK_DEADLINE.with(|slot| slot.set(Some(deadline)));
        WALK_OVERRAN.with(|slot| slot.set(false));
        PARENT_STEPS.with(|slot| slot.set(0));
        NESTING_CAPS.with(|caps| caps.borrow_mut().clear());
        BudgetGuard
    }
}

impl Drop for BudgetGuard {
    fn drop(&mut self) {
        WALK_DEADLINE.with(|slot| slot.set(None));
        // Retain the incomplete-walk signal until the next extraction arms
        // its budget. The publication gate must not lose a helper's refusal.
    }
}

/// `bounded_parent(node)`, bounded by the extraction deadline.
///
/// **Every ancestor walk in this crate must go through this**, because
/// `Node::parent()` is not O(1): tree-sitter reconstructs the parent by
/// descending from the tree root, so one call costs O(depth) and climbing to
/// the root costs **O(depth^2)**. Measured 2026-09-05 (release, macOS) on
/// `func (r *…*T) M() {}`: one `parent()` from the deepest node takes 27.8 us
/// at depth 1,000 and 434 us at depth 16,000, and a full ancestor walk takes
/// 13.1 ms / 52.7 ms / 204.7 ms / 821.7 ms / 3.46 s at depth 1k / 2k / 4k / 8k
/// / 16k — four times the cost for twice the depth, which is the signature.
///
/// That quadratic is why the stride in `walk_tree` was not a bound. The whole
/// 13.15 s debug extraction of a 10 KB file was **two ancestor walks on a
/// single node** — `is_inside_import_or_export` at 1.166 s and
/// `enclosing_callable_qualified` at 1.145 s in release — so the walk's
/// per-node clock check had no opportunity to fire between them, and the file
/// was published `Clean` after running 2.6x past its 5 s budget. A stride is
/// not a bound if one step is unbounded; this is where that step is bounded.
///
/// Returning `None` early reports "no further ancestors", which is a *wrong*
/// answer, and that is safe only because it is never published: the same check
/// latches `WALK_OVERRAN`, and `extract_treesitter_with_budget` turns that latch
/// into a refusal for the whole file. This is the contract
/// `collect_non_symbol_locals` already relies on.
///
/// The clock is read every `PARENT_CHECK_STRIDE` steps rather than every step
/// because `Instant::now()` costs more than an ordinary ancestor hop on the
/// happy path, where files nest tens of levels deep, not thousands.
pub(crate) fn bounded_parent(node: Node<'_>) -> Option<Node<'_>> {
    let steps = PARENT_STEPS.with(|slot| {
        let next = slot.get() + 1;
        slot.set(next);
        next
    });
    if steps >= PARENT_CHECK_STRIDE {
        PARENT_STEPS.with(|slot| slot.set(0));
        if walk_deadline_passed() {
            return None;
        }
    }
    // A walk that has already overrun must not start another climb: the latch
    // is set for the rest of the file, so every remaining ancestor walk stops
    // at its first step instead of paying O(depth) apiece to reach the same
    // refusal.
    if walk_overran() {
        return None;
    }
    crate::parent_index::parent(node).unwrap_or_else(|| {
        #[cfg(test)]
        NATIVE_PARENT_READS.with(|count| count.set(count.get() + 1));
        node.parent()
    })
}

/// Ancestor steps taken between clock reads. See `bounded_parent`.
///
/// Smaller than `DEADLINE_CHECK_STRIDE` because the steps are not comparable: a
/// node visit is O(1) while an ancestor hop is O(depth). At the 1 MiB
/// `MAX_SOURCE_BYTES` ceiling the deepest reachable nesting is around a million
/// levels, where one hop measures ~27 ms, so 64 steps bounds the overshoot at
/// roughly 1.7 s — inside the 5 s `DEFAULT_PARSE_BUDGET` and far inside the 10x
/// tolerance `tests/budget_is_a_real_bound.rs` allows. A stride of 256 would put
/// it at 6.9 s, which is past the budget it exists to enforce.
pub(super) const PARENT_CHECK_STRIDE: u32 = 64;

/// True once a bounded helper gave up because the walk's deadline passed.
///
/// `walk_tree` checks the clock every `DEADLINE_CHECK_STRIDE` nodes, which
/// bounds the *number* of nodes between checks but not the *work* inside any
/// one of them. `collect_non_symbol_locals` walks a whole scope subtree per
/// call, so a single `extract_node` could run for a minute while the stride
/// counter sat at 1 — a stride is not a bound if one step is unbounded. This
/// flag is how a helper that stopped early tells the walk to stop too, and it
/// is a plain `Cell` read so the loop can consult it on every node.
pub(super) fn walk_overran() -> bool {
    WALK_OVERRAN.with(|slot| slot.get())
}

/// Whether this file's extraction may still publish what it has.
///
/// Two ways to fail, and they are not the same question: a helper may have
/// stopped early and latched `WALK_OVERRAN` — in which case the answers it
/// returned are incomplete even though the clock may since have been reset —
/// or the phase just finished may simply have run past the deadline while
/// checking nothing. Either one means the result is not a complete read of the
/// file, and a check that could not run must never report what a check that ran
/// and passed reports.
pub(crate) fn extraction_overran(deadline: std::time::Instant) -> bool {
    walk_overran() || std::time::Instant::now() >= deadline
}

/// Deadline check for a helper running inside the walk. Latches the flag.
pub(crate) fn walk_deadline_passed() -> bool {
    match WALK_DEADLINE.with(|slot| slot.get()) {
        Some(deadline) if std::time::Instant::now() >= deadline => {
            WALK_OVERRAN.with(|slot| slot.set(true));
            true
        }
        _ => false,
    }
}

/// Deepest type or operand nesting a type walker follows.
///
/// The walkers that read a written type — an operand under parentheses, an
/// array type's element — used to recurse once per level with no bound, and a
/// 1 MiB source holds a few hundred thousand levels: 20,000 `(` aborted the
/// process on a 256 KiB stack, and extraction runs on rayon's 2 MiB worker
/// stacks, where a stack overflow is not a panic anyone can catch. No real
/// type nests anywhere near this; the sibling walkers (`rust_type_name`,
/// `receiver_identity`) stop at the same depth.
pub(crate) const TYPE_NESTING_MAX_DEPTH: usize = 16;

/// Record that `walker` stopped at [`TYPE_NESTING_MAX_DEPTH`] and abstained.
///
/// The walker's answer past the cap is "no type stated", which is the
/// conservative one for every caller — an operand with no type records no
/// implicit call, a binding with no type binds nothing — but it is not what the
/// text says, so the file says it was not read that far:
/// [`take_nesting_cap_notes`] turns these into the extraction's diagnostics.
pub(crate) fn note_nesting_cap(walker: &'static str) {
    NESTING_CAPS.with(|caps| *caps.borrow_mut().entry(walker).or_default() += 1);
}

/// One diagnostic per walker that hit [`TYPE_NESTING_MAX_DEPTH`] in this file.
pub(super) fn take_nesting_cap_notes() -> Vec<String> {
    NESTING_CAPS.with(|caps| {
        std::mem::take(&mut *caps.borrow_mut())
            .into_iter()
            .map(|(walker, count)| {
                format!(
                    "{walker} stopped at {TYPE_NESTING_MAX_DEPTH} levels of nesting {count} \
                     time(s) and stated no type there; what those types would have bound is \
                     not recorded"
                )
            })
            .collect()
    })
}

/// Drop the per-scope local-name cache between files.
///
/// The cache is keyed by `Node::id()`, which is the node's address in its
/// tree's arena — those addresses are reused once a tree is dropped, so a key
/// from one file can collide with an unrelated scope in the next. Without the
/// clear, that serves one file's local-variable set to another file, silently
/// suppressing genuine references there. Extraction of a file must never depend
/// on what was extracted before it.
pub(super) fn reset_scope_locals() {
    crate::swiftscope::reset();
    SCOPE_LOCALS.with(|cache| cache.borrow_mut().clear());
    PYTHON_FILE_FACTS.with(|cache| *cache.borrow_mut() = None);
}
