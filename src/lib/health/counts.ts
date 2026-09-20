/**
 * How a bounded section states its size.
 *
 * Every counting defect this view has had is one shape: a collection that was
 * cut by a budget, headlined by the number of rows that survived the cut. The
 * dead-symbol heading counted the rows that fitted the token budget; the
 * direct-vulnerability filter counted the rows that survived the scan cap;
 * the ecosystem row printed the first four of an already-capped list. Each was
 * found and fixed on its own, and the next section written by hand could
 * reintroduce it.
 *
 * The guard against it used to be a regex over the panel's `<h3>` text,
 * asserting that a heading mentioning `.length` also mentioned a total or a
 * cap word. That checks the shape of the sentence, not the arithmetic: a new
 * heading only had to *look* compliant. Here the arithmetic has one owner, so
 * a section cannot render a count without also rendering what the count is
 * short of — there is no code path that prints `shown` alone.
 */

export interface SectionCount {
  /** Rows actually rendered on screen. */
  shown: number;
  /**
   * Rows the scan observed before any display cap. Never below `shown`: a
   * backend that reports only what it returned must not make a heading claim
   * fewer items than it is listing.
   */
  total: number;
  /**
   * The observed total is itself a floor — the query stopped at a budget
   * before it finished counting, so even `total` understates the truth.
   */
  atLeast?: boolean;
  /**
   * Narrowing applied to the rows, e.g. "direct". A filtered view has no
   * total of its own, so it is reported as a floor rather than a count.
   */
  qualifier?: string;
}

/**
 * Render a count, always disclosing what it is short of.
 *
 * - `{shown: 3, total: 3}` → `3`
 * - `{shown: 3, total: 12}` → `12; showing 3`
 * - `{shown: 3, total: 12, atLeast: true}` → `at least 12; showing 3`
 * - `{shown: 3, total: 3, qualifier: "direct"}` → `3 direct`
 *
 * `total` is clamped up to `shown`, never down: the rendered rows are
 * evidence, and a heading may not contradict the table beneath it.
 */
/**
 * Both numbers cross an IPC boundary from scanner output GitPulse does not
 * control, so `NaN` and `Infinity` are live inputs rather than impossible
 * ones.
 *
 * A non-finite `total` is resolved as *unknown*, not as zero: it becomes the
 * row count marked as a floor ("at least 3"). Coercing it to 0 would let a
 * count nobody could read be printed as a complete, smaller one — the exact
 * shape this module exists to make impossible — and printing "NaN" tells the
 * reader nothing they can act on.
 */
function resolve(count: SectionCount): { shown: number; observed: number; atLeast: boolean } {
  const rawShown = Math.trunc(count.shown);
  const shown = Number.isFinite(rawShown) ? Math.max(0, rawShown) : 0;
  const rawTotal = Math.trunc(count.total);
  if (!Number.isFinite(rawTotal)) {
    return { shown, observed: shown, atLeast: true };
  }
  return {
    shown,
    observed: Math.max(shown, rawTotal),
    atLeast: count.atLeast === true,
  };
}

export function formatSectionCount(count: SectionCount): string {
  const { shown, observed, atLeast } = resolve(count);
  const head = atLeast ? `at least ${observed}` : `${observed}`;
  const qualified = count.qualifier ? `${head} ${count.qualifier}` : head;
  return observed > shown ? `${qualified}; showing ${shown}` : qualified;
}

/**
 * Whether this count is bounded — i.e. the section is not showing everything
 * the scan saw, or does not know what it saw.
 *
 * The section shell uses it to decide whether the heading needs its "this is
 * not the complete set" affordance, so the decision is made once rather than
 * re-derived per section.
 */
export function isBounded(count: SectionCount): boolean {
  const { shown, observed, atLeast } = resolve(count);
  return atLeast || observed > shown;
}
