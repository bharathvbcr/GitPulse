import { invoke } from "../ipc/invoke";
import { harnessStore } from "../stores/harnessStore";
import { runPassed, type TerminalRunResult } from "./runResult";

/**
 * The backend's own limit for one Manvi action, in seconds (it clamps the
 * request to [1s, 30min]). 900 is long enough for a cold install, build and
 * test cycle, which is the slowest thing a coverage generator or a Health
 * remediation step is asked to do. Coverage used 900 and Health 600 before
 * this was shared; a remediation step that is an install-then-build is the
 * same work, so Health does not get a shorter fuse than coverage.
 */
export const MANVI_ACTION_TIMEOUT_SECS = 900;

/**
 * How long past the backend's limit the UI keeps waiting. The backend answers
 * a timeout with `timed_out: true`; only a native call that never answers at
 * all reaches this bound, and it ends the wait rather than the command.
 */
export const MANVI_ACTION_GRACE_MS = 30_000;

export type ManviActionRequest = {
  repoPath: string;
  argv: string[];
  /** The backend's policy bucket: `coverage`, `coverage_generator`, `health`. */
  actionKind: string;
  /** The journal entry's kind (`coverage-step`, `remediation-step`, ...). */
  journalKind: string;
  /** The journal entry's label. */
  label: string;
};

/**
 * The single place a Manvi step crosses IPC from a panel. It runs the command
 * under one bounded wait, and files exactly one journal entry: the outcome
 * with its policy verdict when the command answered, a failure when it threw
 * or the wait ran out. The caller decides what the answer means for its own
 * state; a rejection is rethrown after being recorded.
 */
export async function runManviAction(request: ManviActionRequest): Promise<TerminalRunResult> {
  const { repoPath, argv, actionKind, journalKind, label } = request;
  let res: TerminalRunResult;
  try {
    res = await invoke<TerminalRunResult>(
      "cmd_manvi_run_action",
      {
        repoPath,
        args: argv,
        actionKind,
        timeoutSecs: MANVI_ACTION_TIMEOUT_SECS,
      },
      { deadlineMs: MANVI_ACTION_TIMEOUT_SECS * 1000 + MANVI_ACTION_GRACE_MS },
    );
  } catch (err) {
    harnessStore.recordAction({ repoPath, kind: journalKind, label, ok: false });
    throw err;
  }
  harnessStore.recordAction({
    repoPath,
    kind: journalKind,
    label,
    ok: runPassed(res),
    verdict: res.policy ?? null,
  });
  return res;
}
