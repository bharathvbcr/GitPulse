/**
 * The Storage panel's user-clicked commit-graph write (Owner Decision 3).
 *
 * The flow is a small state machine, kept out of the component so the rules are
 * testable without a DOM: nothing is written until the person has seen the
 * exact command and the directory it writes under, and has confirmed it.
 *
 *   idle -> previewing -> confirming -> running -> done | failed
 *
 * `confirm` is the only call that reaches `cmd_commit_graph_write`, and it only
 * does so from `confirming`, echoing back the previewed argv. The backend
 * refuses anything that is not that argv, and the command gate judges it.
 * Nothing here runs on a timer, on mount, or from a store subscription.
 */
import { writable, type Readable } from "svelte/store";
import type {
  CommitGraphReport,
  CommitGraphState,
  CommitGraphWriteOutcome,
  CommitGraphWritePlan,
} from "../storage/types";
import type { Guarded } from "../stores/harnessStore";

/** States in which a write would improve the graph; the button exists only then. */
const OFFERED: readonly CommitGraphState[] = ["missing", "stale", "no_bloom"];

/**
 * Whether the Storage panel offers the write button: a missing, stale or
 * Bloom-less graph. A present graph, an empty repository, a report from before
 * the detector, and a state that could not be checked all hide it.
 */
export function offersCommitGraphWrite(report: CommitGraphReport | null | undefined): boolean {
  return !!report && OFFERED.includes(report.state);
}

export type CommitGraphWriteStep =
  | { phase: "idle" }
  | { phase: "previewing" }
  | { phase: "confirming"; plan: CommitGraphWritePlan }
  | { phase: "running"; plan: CommitGraphWritePlan; cancelling: boolean }
  | { phase: "done"; outcome: CommitGraphWriteOutcome }
  | { phase: "failed"; message: string };

export type CommitGraphInvoke = <T>(command: string, args: Record<string, unknown>) => Promise<T>;

export interface CommitGraphWriteFlow extends Readable<CommitGraphWriteStep> {
  /** Asks the backend what the write would run, then waits for confirmation. */
  open(): Promise<void>;
  /** Runs the previewed argv. Does nothing unless a preview is awaiting it. */
  confirm(): Promise<void>;
  /** Backs out of a preview, or asks a running write to stop. */
  cancel(): Promise<void>;
  /** Returns to idle after a finished or failed run. */
  dismiss(): void;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createCommitGraphWriteFlow(options: {
  repoPath: () => string;
  invoke: CommitGraphInvoke;
  /** Called after a write ran to completion, so the panel can scan again. */
  onWritten?: (outcome: CommitGraphWriteOutcome) => void;
}): CommitGraphWriteFlow {
  const { subscribe, set } = writable<CommitGraphWriteStep>({ phase: "idle" });
  let step: CommitGraphWriteStep = { phase: "idle" };
  /** The phase right now; a read through a call, since `await` may have moved it. */
  const phaseNow = (): CommitGraphWriteStep["phase"] => step.phase;
  const move = (next: CommitGraphWriteStep) => {
    step = next;
    set(next);
  };

  return {
    subscribe,
    async open() {
      if (step.phase !== "idle" && step.phase !== "failed" && step.phase !== "done") return;
      move({ phase: "previewing" });
      try {
        const plan = await options.invoke<CommitGraphWritePlan>("cmd_commit_graph_write_plan", {
          repoPath: options.repoPath(),
        });
        // Backed out while the preview was loading: it is no longer wanted.
        if (phaseNow() === "previewing") move({ phase: "confirming", plan });
      } catch (error) {
        if (phaseNow() === "previewing") move({ phase: "failed", message: messageOf(error) });
      }
    },
    async confirm() {
      if (step.phase !== "confirming") return;
      const plan = step.plan;
      move({ phase: "running", plan, cancelling: false });
      try {
        const result = await options.invoke<Guarded<CommitGraphWriteOutcome>>(
          "cmd_commit_graph_write",
          { repoPath: options.repoPath(), confirmedArgv: plan.argv },
        );
        move({ phase: "done", outcome: result.output });
        options.onWritten?.(result.output);
      } catch (error) {
        move({ phase: "failed", message: messageOf(error) });
      }
    },
    async cancel() {
      if (step.phase === "confirming" || step.phase === "previewing") {
        move({ phase: "idle" });
        return;
      }
      if (step.phase !== "running" || step.cancelling) return;
      move({ ...step, cancelling: true });
      try {
        await options.invoke<boolean>("cmd_commit_graph_write_cancel", {
          repoPath: options.repoPath(),
        });
      } catch (error) {
        move({ phase: "failed", message: messageOf(error) });
      }
    },
    dismiss() {
      if (step.phase === "done" || step.phase === "failed") move({ phase: "idle" });
    },
  };
}
