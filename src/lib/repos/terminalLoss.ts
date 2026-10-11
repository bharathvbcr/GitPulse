import { askConfirm } from "../stores/modalStore";

/** The slice of the terminal registry the close guard asks (`RepoStoreDeps.terminals`). */
export type TerminalCounts = {
  countFor(repoPath: string): number;
  hostedFor?(repoPath: string): number;
};

/**
 * True when these tabs may be closed: either nothing is running in any of
 * them, or the user said to end it anyway.
 *
 * Takes a LIST because "Close Other Tabs" and "Close Tabs to the Right"
 * discard several repositories at once and never route through `closeTab`.
 * Guarding only the single close would leave the two paths that can lose
 * the most work unguarded.
 *
 * Fails OPEN on a broken counter. A registry that threw would otherwise
 * make every repository tab unclosable, which is a worse failure than a
 * missing warning — the count is a courtesy, not a safety interlock.
 */
export async function confirmTerminalLoss(
  terminals: TerminalCounts,
  paths: readonly string[],
): Promise<boolean> {
  let running = 0;
  let repos = 0;
  let hosted = 0;
  try {
    for (const path of paths) {
      const count = terminals.countFor(path);
      if (!Number.isFinite(count) || count <= 0) continue;
      running += count;
      repos += 1;
      const elsewhere = terminals.hostedFor?.(path) ?? 0;
      if (Number.isFinite(elsewhere) && elsewhere > 0) hosted += Math.min(elsewhere, count);
    }
  } catch {
    return true;
  }
  if (running <= 0) return true;
  const shells = running === 1 ? "the shell" : `all ${running} shells`;
  const subject = repos === 1 ? "this repository tab" : `these ${repos} repository tabs`;
  const elsewhere = hosted === 0 ? ""
    : hosted === running
      ? `${hosted === 1 ? "It is an agent" : "They are agents"} working in another checkout of this repository, hosted here; ${hosted === 1 ? "it keeps" : "they keep"} running. `
      : `${hosted} of them ${hosted === 1 ? "is an agent" : "are agents"} working in another checkout of this repository, hosted here; ${hosted === 1 ? "it keeps" : "they keep"} running. `;
  return askConfirm({
    title: running === 1 ? "End the terminal session?" : `End ${running} terminal sessions?`,
    message:
      `${paths.length === 1 ? `${paths[0]}\n\n` : ""}` +
      `Closing ${subject} closes ${shells} in ${repos === 1 ? "it" : "them"} and their scrollback. ` +
      elsewhere +
      "A shell waiting at its prompt ends. Anything still running — a build, a test run, an agent — keeps running, " +
      "listed under Sessions in the terminal dock, where it can be shown again or stopped.\n\n" +
      "Hiding the terminal instead (⌃`) keeps the scrollback too.",
    confirmLabel: running === 1 ? "Close and End Session" : "Close and End Sessions",
    destructive: true,
  });
}
