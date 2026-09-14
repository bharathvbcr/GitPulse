/**
 * The reporting half of a browser harness: rows in, verdict out.
 *
 * Harnesses used to evaluate each assertion at the call site and hand the
 * helper a boolean, which makes two failures indistinguishable from silence.
 *
 *  - An assertion that *throws* — `getComputedStyle(x)` after `x` came back
 *    null — never reaches the helper, so no row is written for it. The outer
 *    catch records the raw error, which names a type and a built-in, not the
 *    check that produced it.
 *  - The rows after the stopping point are simply absent. Absent reads the
 *    same as never written: the verdict is a short list of passes, and the
 *    checks that could not run look like checks nobody wrote.
 *
 * Both are fixed by moving evaluation inside the helper and writing the
 * truncation down. `check` takes the assertion as a thunk, so a throw is
 * attributed to the check that caused it and recorded as that check failing;
 * `stopped` appends the row that says where the run ended and that everything
 * after it did not run.
 */

export interface CheckRow {
  name: string;
  pass: boolean;
}

/** Thrown by a failed check, so `stopped` can tell "this check failed" from
 * "something outside the checks threw" and avoid naming the failure twice. */
export class CheckFailed extends Error {}

export function createChecks() {
  const results: CheckRow[] = [];

  /** Evaluate `assertion` here, never at the call site: a throw is this
   * check's failure, with the error kept beside its name. */
  function check(name: string, assertion: () => unknown): void {
    let pass = false;
    let threw = "";
    try {
      pass = !!assertion();
    } catch (error) {
      threw = ` — threw ${error}`;
    }
    results.push({ name: `${name}${threw}`, pass });
    if (!pass) throw new CheckFailed(`${name}${threw}`);
  }

  /** Close the run out after any throw — a failed check, or a helper that gave
   * up before one could be reached. Returns the row's text so the page can
   * show a reader the same sentence the verdict carries. */
  function stopped(error: unknown): string {
    const last = results.at(-1)?.name ?? "the start of the run";
    const reason = error instanceof CheckFailed ? "" : `: ${error}`;
    const name = `run stopped after "${last}"${reason} — the checks after it did not run`;
    results.push({ name, pass: false });
    return name;
  }

  return { results, check, stopped };
}
