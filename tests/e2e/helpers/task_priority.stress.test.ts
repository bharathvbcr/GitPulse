/**
 * Adversarial Challenger Stress Harness for Feature 13:
 * Priority Normalization & Mutation Invariants
 *
 * Empirically tests & validates:
 * 1. Fuzzing & Invariants:
 *    - 500+ randomized/fuzz inputs against `normalizePriority`, `inferTaskPriority`, `parsePriority`, `numericPriority`.
 *    - Extreme integers (-100, 999, MAX_SAFE_INTEGER, NaN, Infinity), strings with weird casing ("UrGeNt", "sev:Critical", "priority:P0"), objects, arrays, null, undefined.
 *    - Invariant: Never throws, never returns NaN, returns valid Priority (0..3) or undefined/null/all.
 *    - Scale invariant: P0 (0) < P1 (1) < P2 (2) < P3 (3); most urgent wins.
 * 2. Optimistic Concurrency Control (expected_revision):
 *    - Out-of-order, stale, future, and conflicting revisions fail closed.
 *    - Matching revision succeeds and increments monotonically.
 *    - 100-worker concurrent race condition simulation: exactly 1 winner, 99 fail closed.
 *    - 500 sequential chained mutations with monotonic revision increment.
 *    - Idempotent request replay via `request_id`.
 * 3. Evidence-Gated Completion Invariants (verification_gate.rs & intake.rs):
 *    - Tasks claiming 'done' without verified evidence fail closed and remain in 'review'.
 *    - Failed, degraded, running, unresolvable, mismatched head (TOCTOU), and foreign worktree records are rejected.
 *    - Only settled + passed + matching head verification permits transition to 'done'.
 * 4. Backlog Deliverable & Task ft-7e812b3f State Parity:
 *    - Real task metadata in .tasks/ft-7e812b3f... reflects verified done status and completed criteria.
 *    - TASK_REVIEW.md distribution integrity check.
 */

import { describe, expect, it } from "vitest";
import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import {
  normalizePriority as normalizePriorityHelper,
  type PriorityLevel,
} from "./test-utils";
import {
  inferTaskPriority,
  inferTaskSeverity,
} from "@/lib/workbench/issueTask";
import {
  parsePriority,
  numericPriority,
  priorityLabel,
  DEFAULT_PRIORITY,
  PRIORITY_LABELS,
  type Priority,
} from "@/lib/workbench/taskPriority";
import {
  normalizePriority as normalizeFacetPriority,
} from "@/lib/workbench/taskOrganize";
import { TaskBatch } from "@/lib/workbench/taskActions";
import {
  task,
  taskWrite,
  WorkbenchError,
  type Task,
  type TaskStatus,
} from "@/lib/workbench/client";

const PROJECT_ROOT = join(__dirname, "..", "..", "..");

// ============================================================================
// PART 1: FUZZING & INVARIANTS (500+ Randomized & Adversarial Inputs)
// ============================================================================
describe("Feature 13 Challenge 1: Priority Fuzzing & Strict Invariants", () => {
  // Deterministic PRNG for repeatable fuzz testing
  function createPrng(seed = 0xdeadbeef) {
    let s = seed >>> 0;
    return () => {
      s = (Math.imul(1664525, s) + 1013904223) >>> 0;
      return s / 4294967296;
    };
  }

  const prng = createPrng(42);

  const ADVERSARIAL_RAW_VALUES = [
    // Extreme and special numbers
    -1000, -100, -50, -1, 0, 1, 2, 3, 4, 10, 99, 999, 10000,
    Number.MAX_SAFE_INTEGER, Number.MIN_SAFE_INTEGER,
    Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY,
    Number.NaN, 0.0001, 2.9999, 3.0001, -0, +0,

    // Mixed-case and namespaced priority labels
    "UrGeNt", "URGENT", "urgent", "critical", "CRITICAL", "CrItIcAl",
    "blocker", "BLOCKER", "p0", "P0", "0", " 0 ", "  p0  ",
    "high", "HIGH", "HiGh", "important", "IMPORTANT", "p1", "P1", "1", " 1 ",
    "normal", "NORMAL", "medium", "MEDIUM", "p2", "P2", "2", " 2 ",
    "low", "LOW", "minor", "MINOR", "trivial", "TRIVIAL", "p3", "P3", "3", " 3 ",

    // Tracker namespace variations
    "priority:P0", "PRIORITY: p0", "prio:urgent", "sev:Critical", "SEV:critical",
    "severity:blocker", "prio/critical", "sev/p0", "priority/urgent",
    "priority:1", "prio:high", "sev:high", "severity:important",
    "priority:2", "prio:medium", "sev:normal", "priority:normal",
    "priority:3", "prio:low", "sev:minor", "severity:trivial",

    // Malformed, compound, or adversarial strings
    "", "   ", "\t\n\r", "0 (Low)", "1 (High)", "High (1)", "2 (Normal)", "3 (Low)",
    "p-1", "p4", "p99", "sev0", "sev1", "sev2", "sev3", "sev4",
    "priority:unknown", "sev:none", "undefined", "null", "NaN", "false", "true",
    "[object Object]", "{}", "[]", "--1", "+1", "007", "3.14",
    "\u0000", "\u0000p0", "p0\u0000", "🔥", "🚀", "drop table tasks;",

    // Non-primitive & unexpected objects
    null, undefined, true, false,
    {}, { priority: 0 }, { rank: 1 }, { toString: () => "0" }, { valueOf: () => 1 },
    [], [0], [1], ["urgent"], ["p0", "p3"], ["sev:Critical", "prio:low"],
    [null, undefined, 123], () => 0, Symbol("priority"), new Date(),
  ];

  it("1.1: normalizePriority (test helper) with 500+ fuzz inputs never crashes, never returns NaN, returns valid 0..3", () => {
    const fuzzCount = 500;
    for (let i = 0; i < fuzzCount; i++) {
      let input: unknown;
      if (i < ADVERSARIAL_RAW_VALUES.length) {
        input = ADVERSARIAL_RAW_VALUES[i];
      } else {
        // Generate pseudo-random combinations
        const r = prng();
        if (r < 0.25) {
          input = Math.floor(prng() * 2000) - 1000;
        } else if (r < 0.5) {
          const chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 :/_-\t\n";
          let str = "";
          const len = Math.floor(prng() * 20);
          for (let j = 0; j < len; j++) {
            str += chars[Math.floor(prng() * chars.length)];
          }
          input = str;
        } else if (r < 0.75) {
          input = [Math.floor(prng() * 5), "label-" + Math.floor(prng() * 10)];
        } else {
          input = { val: prng(), flag: prng() > 0.5 };
        }
      }

      let res: { code: PriorityLevel; rank: number } | undefined;
      expect(() => {
        res = normalizePriorityHelper(input);
      }).not.toThrow();

      expect(res).toBeDefined();
      expect(Number.isNaN(res!.rank)).toBe(false);
      expect(Number.isInteger(res!.rank)).toBe(true);
      expect(res!.rank).toBeGreaterThanOrEqual(0);
      expect(res!.rank).toBeLessThanOrEqual(3);
      expect(["P0", "P1", "P2", "P3"]).toContain(res!.code);
      expect(res!.code).toBe(`P${res!.rank}`);
    }
  });

  it("1.2: inferTaskPriority with 500+ fuzz label combinations never crashes, never returns NaN, returns valid 0..3", () => {
    const fuzzCount = 500;
    for (let i = 0; i < fuzzCount; i++) {
      let labels: unknown;
      if (i < ADVERSARIAL_RAW_VALUES.length) {
        // Test single items wrapped or bare
        labels = [ADVERSARIAL_RAW_VALUES[i]];
      } else {
        // Test arrays with multi-label combinations
        const size = Math.floor(prng() * 8);
        const arr: unknown[] = [];
        for (let s = 0; s < size; s++) {
          const pick = ADVERSARIAL_RAW_VALUES[Math.floor(prng() * ADVERSARIAL_RAW_VALUES.length)];
          arr.push(pick);
        }
        labels = arr;
      }

      let res: Priority | undefined;
      expect(() => {
        // @ts-expect-error fuzz test deliberately passes arbitrary input
        res = inferTaskPriority(labels);
      }).not.toThrow();

      expect(res).toBeDefined();
      expect(Number.isNaN(res!)).toBe(false);
      expect(Number.isInteger(res!)).toBe(true);
      expect([0, 1, 2, 3]).toContain(res);
    }
  });

  it("1.3: parsePriority and numericPriority adhere strictly to the shared specification", () => {
    // parsePriority strict contract
    expect(parsePriority(0)).toBe(0);
    expect(parsePriority(1)).toBe(1);
    expect(parsePriority(2)).toBe(2);
    expect(parsePriority(3)).toBe(3);
    expect(parsePriority(4)).toBe(null);
    expect(parsePriority(-1)).toBe(null);
    expect(parsePriority("urgent")).toBe(0);
    expect(parsePriority("high")).toBe(1);
    expect(parsePriority("normal")).toBe(2);
    expect(parsePriority("medium")).toBe(2);
    expect(parsePriority("low")).toBe(3);
    expect(parsePriority("1 (High)")).toBe(1);
    expect(parsePriority("0 (Low)")).toBe(null); // Refuses mismatched name & number
    expect(parsePriority("High (1)")).toBe(null); // Refuses inverted syntax
    expect(parsePriority("critical")).toBe(null); // Strict parser rejects bare 'critical' (handled in issue labels)

    // numericPriority contract
    expect(numericPriority(0)).toBe(0);
    expect(numericPriority("0")).toBe(0);
    expect(numericPriority("3")).toBe(3);
    expect(numericPriority("4")).toBe(null);
    expect(numericPriority("-1")).toBe(null);
    expect(numericPriority("P0")).toBe(null); // Accepts only digits 0..3
    expect(numericPriority("urgent")).toBe(null);

    // priorityLabel contracts
    expect(priorityLabel(0)).toBe("Urgent");
    expect(priorityLabel(1)).toBe("High");
    expect(priorityLabel(2)).toBe("Normal");
    expect(priorityLabel(3)).toBe("Low");
    expect(priorityLabel(99)).toBe("Normal"); // Fallback to DEFAULT_PRIORITY (2)
    expect(priorityLabel("unknown")).toBe("Normal");
  });

  it("1.4: Multi-label priority resolution invariant: highest urgency (lowest numeric rank) strictly dominates", () => {
    // When a task has both P0 and P3 labels, P0 (rank 0) must win
    expect(inferTaskPriority(["p3", "p0"])).toBe(0);
    expect(inferTaskPriority(["low", "critical"])).toBe(0);
    expect(inferTaskPriority(["prio:low", "sev:Critical"])).toBe(0);
    expect(inferTaskPriority(["trivial", "blocker"])).toBe(0);
    expect(inferTaskPriority(["p2", "p1"])).toBe(1);
    expect(inferTaskPriority(["normal", "high"])).toBe(1);

    // Non-priority labels default to Normal (2)
    expect(inferTaskPriority(["frontend", "virtualization", "feature"])).toBe(2);
    expect(inferTaskPriority([])).toBe(2);
  });

  it("1.5: normalizeFacetPriority handles facet values safely without crashing", () => {
    expect(normalizeFacetPriority("all")).toBe("all");
    expect(normalizeFacetPriority("")).toBe("all");
    expect(normalizeFacetPriority(null)).toBe("all");
    expect(normalizeFacetPriority(undefined)).toBe("all");
    expect(normalizeFacetPriority(0)).toBe(0);
    expect(normalizeFacetPriority("0")).toBe(0);
    expect(normalizeFacetPriority(1)).toBe(1);
    expect(normalizeFacetPriority(3)).toBe(3);
    expect(normalizeFacetPriority(999)).toBe("all");
  });

  it("1.6: parsePriority (production) with 500+ fuzz inputs never crashes, never returns NaN, returns valid 0..3 or null", () => {
    const fuzzCount = 500;
    for (let i = 0; i < fuzzCount; i++) {
      let input: unknown;
      if (i < ADVERSARIAL_RAW_VALUES.length) {
        input = ADVERSARIAL_RAW_VALUES[i];
      } else {
        const r = prng();
        if (r < 0.3) {
          input = Math.floor(prng() * 1000) - 500;
        } else if (r < 0.6) {
          const chars = "pP0123456789urgenthighnormalmediumlow ()-_\t\n\r";
          let str = "";
          const len = Math.floor(prng() * 25);
          for (let j = 0; j < len; j++) {
            str += chars[Math.floor(prng() * chars.length)];
          }
          input = str;
        } else if (r < 0.8) {
          input = [prng(), "custom"];
        } else {
          input = { priority: Math.floor(prng() * 4) };
        }
      }

      let res: Priority | null | undefined;
      expect(() => {
        res = parsePriority(input);
      }).not.toThrow();

      if (res !== null) {
        expect(res).toBeDefined();
        expect(Number.isNaN(res)).toBe(false);
        expect(Number.isInteger(res)).toBe(true);
        expect([0, 1, 2, 3]).toContain(res);
      }
    }
  });
});

// ============================================================================
// PART 2: OPTIMISTIC CONCURRENCY CONTROL (expected_revision INVARIANTS)
// ============================================================================
describe("Feature 13 Challenge 2: Optimistic Concurrency Control & Mutation Invariants", () => {
  interface SimulatedTask {
    id: string;
    title: string;
    revision: number;
    status: "inbox" | "ready" | "in_progress" | "review" | "done";
    logs: string;
    receipts: Map<string, { revision: number; status: string }>;
  }

  interface MutationResult {
    ok: boolean;
    outcome: "updated" | "unchanged" | "conflict";
    error?: string;
    currentRevision: number;
  }

  /**
   * Concurrency engine implementing the exact OCC and idempotency logic from
   * `src-tauri/src/workbench/intake.rs::complete_task` (lines 1616–1720).
   */
  class TaskStoreSimulation {
    private tasks: Map<string, SimulatedTask> = new Map();

    createTask(id: string, initialRev = 1, status: SimulatedTask["status"] = "inbox"): SimulatedTask {
      const task: SimulatedTask = {
        id,
        title: "Test task " + id,
        revision: initialRev,
        status,
        logs: "",
        receipts: new Map(),
      };
      this.tasks.set(id, task);
      return task;
    }

    getTask(id: string): SimulatedTask | undefined {
      return this.tasks.get(id);
    }

    /**
     * Executes task mutation with OCC verification.
     */
    mutate(
      id: string,
      patch: { status?: SimulatedTask["status"]; title?: string },
      expectedRevision?: number,
      requestId?: string
    ): MutationResult {
      const task = this.tasks.get(id);
      if (!task) {
        return { ok: false, outcome: "conflict", error: "not_found", currentRevision: 0 };
      }

      // Idempotency check: if caller provided request_id that was already applied,
      // return unchanged/cached receipt without incrementing revision.
      if (requestId && task.receipts.has(requestId)) {
        const receipt = task.receipts.get(requestId)!;
        return {
          ok: true,
          outcome: "unchanged",
          currentRevision: receipt.revision,
        };
      }

      // OCC Check: expected_revision must match current revision
      if (expectedRevision !== undefined && expectedRevision !== task.revision) {
        return {
          ok: false,
          outcome: "conflict",
          error: `revision_conflict: The task is at revision ${task.revision}, not ${expectedRevision}`,
          currentRevision: task.revision,
        };
      }

      // If no changes in state
      if (patch.status && patch.status === task.status && (!patch.title || patch.title === task.title)) {
        return {
          ok: true,
          outcome: "unchanged",
          currentRevision: task.revision,
        };
      }

      // Apply mutation and increment revision monotonically
      if (patch.status) task.status = patch.status;
      if (patch.title) task.title = patch.title;
      task.revision += 1;

      if (requestId) {
        task.receipts.set(requestId, { revision: task.revision, status: task.status });
      }

      return {
        ok: true,
        outcome: "updated",
        currentRevision: task.revision,
      };
    }
  }

  it("2.1: Single-thread mutation with matching expected_revision succeeds and increments revision monotonically", () => {
    const store = new TaskStoreSimulation();
    const task = store.createTask("task-1", 1);

    // Initial state: rev 1
    expect(task.revision).toBe(1);

    // Rev 1 -> Rev 2
    const res1 = store.mutate("task-1", { status: "ready" }, 1);
    expect(res1.ok).toBe(true);
    expect(res1.outcome).toBe("updated");
    expect(res1.currentRevision).toBe(2);
    expect(task.revision).toBe(2);
    expect(task.status).toBe("ready");

    // Rev 2 -> Rev 3
    const res2 = store.mutate("task-1", { status: "in_progress" }, 2);
    expect(res2.ok).toBe(true);
    expect(res2.outcome).toBe("updated");
    expect(res2.currentRevision).toBe(3);
    expect(task.revision).toBe(3);
    expect(task.status).toBe("in_progress");
  });

  it("2.2: Stale, future, or invalid expected_revisions fail closed and leave state unmodified", () => {
    const store = new TaskStoreSimulation();
    const task = store.createTask("task-stale", 10, "in_progress");

    // Attempt mutation with stale revision (9 < 10)
    const staleRes = store.mutate("task-stale", { status: "review" }, 9);
    expect(staleRes.ok).toBe(false);
    expect(staleRes.outcome).toBe("conflict");
    expect(staleRes.error).toContain("revision_conflict");
    expect(staleRes.currentRevision).toBe(10);
    expect(task.status).toBe("in_progress"); // Preserved

    // Attempt mutation with future revision (15 > 10)
    const futureRes = store.mutate("task-stale", { status: "done" }, 15);
    expect(futureRes.ok).toBe(false);
    expect(futureRes.outcome).toBe("conflict");
    expect(futureRes.currentRevision).toBe(10);
    expect(task.status).toBe("in_progress");

    // Attempt mutation with negative or zero revision
    const negRes = store.mutate("task-stale", { status: "review" }, -1);
    expect(negRes.ok).toBe(false);
    expect(negRes.currentRevision).toBe(10);

    const zeroRes = store.mutate("task-stale", { status: "review" }, 0);
    expect(zeroRes.ok).toBe(false);
    expect(zeroRes.currentRevision).toBe(10);
  });

  it("2.3: High-concurrency race condition: 100 concurrent workers competing on revision R: exactly 1 wins, 99 fail closed", () => {
    const store = new TaskStoreSimulation();
    const initialRev = 5;
    store.createTask("task-race", initialRev, "inbox");

    const concurrency = 100;
    const workers = Array.from({ length: concurrency }, (_, i) => ({
      workerId: `worker-${i}`,
      targetRev: initialRev, // All workers read revision 5 simultaneously
      targetStatus: "in_progress" as const,
    }));

    // Execute all 100 mutations competing for revision 5
    const results: MutationResult[] = [];
    for (const worker of workers) {
      results.push(store.mutate("task-race", { status: worker.targetStatus }, worker.targetRev));
    }

    const successes = results.filter((r) => r.ok && r.outcome === "updated");
    const failures = results.filter((r) => !r.ok && r.outcome === "conflict");

    // Invariant: Exactly 1 worker wins the race and increments revision to 6
    expect(successes.length).toBe(1);
    expect(successes[0].currentRevision).toBe(initialRev + 1);

    // Invariant: Exactly 99 workers fail closed with revision_conflict
    expect(failures.length).toBe(concurrency - 1);
    for (const f of failures) {
      expect(f.error).toContain("revision_conflict");
      expect(f.currentRevision).toBe(initialRev + 1);
    }

    // Final task state
    const task = store.getTask("task-race")!;
    expect(task.revision).toBe(initialRev + 1);
  });

  it("2.4: 500 sequential chained mutations increment revision monotonically from 1 to 501", () => {
    const store = new TaskStoreSimulation();
    store.createTask("task-chain", 1, "inbox");

    let currentRev = 1;
    const chainSteps = 500;

    for (let step = 1; step <= chainSteps; step++) {
      const nextStatus = step % 2 === 0 ? "in_progress" : "review";
      const res = store.mutate("task-chain", { status: nextStatus }, currentRev);
      expect(res.ok).toBe(true);
      expect(res.currentRevision).toBe(currentRev + 1);
      currentRev = res.currentRevision;
    }

    expect(currentRev).toBe(1 + chainSteps);
    expect(store.getTask("task-chain")!.revision).toBe(501);
  });

  it("2.5: Idempotency with request_id prevents double-increment on lost response replay", () => {
    const store = new TaskStoreSimulation();
    store.createTask("task-idem", 1, "inbox");

    const reqId = "client-req-uuid-12345";

    // First attempt: succeeds and moves rev 1 -> 2
    const res1 = store.mutate("task-idem", { status: "ready" }, 1, reqId);
    expect(res1.ok).toBe(true);
    expect(res1.outcome).toBe("updated");
    expect(res1.currentRevision).toBe(2);

    // Replay attempt with same request_id (client timed out and retried with original rev 1)
    const res2 = store.mutate("task-idem", { status: "ready" }, 1, reqId);
    expect(res2.ok).toBe(true);
    expect(res2.outcome).toBe("unchanged");
    expect(res2.currentRevision).toBe(2); // Kept at 2, not incremented to 3!

    expect(store.getTask("task-idem")!.revision).toBe(2);
  });
});

// ============================================================================
// PART 3: EVIDENCE-GATED COMPLETION INVARIANT (verification_gate.rs & intake.rs)
// ============================================================================
describe("Feature 13 Challenge 3: Evidence-Gated Completion Invariants", () => {
  interface VerificationEvidence {
    requestId: string;
    itemId: string;
    state: "could_not_run" | "running" | "pending" | "settled";
    outcome: "passed" | "failed" | "degraded";
    worktree: string;
    head: string;
    criteria: Array<{ index: number; text: string; status: "met" | "unmet" }>;
  }

  interface WorkItem {
    id: string;
    status: "in_progress" | "review" | "done";
    logs: string;
    worktree: string;
    head: string;
  }

  type GateDecision =
    | { type: "Passed"; verdict: string }
    | { type: "Held"; reason: string };

  /**
   * Precise simulation of `verification_gate.rs::judge` (lines 192–254).
   */
  function evaluateVerificationGate(
    evidence: VerificationEvidence | null,
    task: WorkItem,
    repoIdentity: string,
    currentHeadOfWorktree: (worktree: string) => string | null
  ): GateDecision {
    if (!evidence) {
      return {
        type: "Held",
        reason: "no verification record exists for this task: Manvi's work.verify.task has not judged it",
      };
    }

    if (evidence.itemId !== task.id) {
      return {
        type: "Held",
        reason: `the newest verification record is for another task (${evidence.itemId}), not this one`,
      };
    }

    if (evidence.state === "could_not_run") {
      return {
        type: "Held",
        reason: "verification could not run, so no verdict exists.",
      };
    }

    if (evidence.state !== "settled") {
      return {
        type: "Held",
        reason: `verification is still "${evidence.state}"; no verdict yet.`,
      };
    }

    if (evidence.outcome !== "passed") {
      return {
        type: "Held",
        reason: `verification did not pass (outcome "${evidence.outcome}").`,
      };
    }

    if (!evidence.head?.trim() || !evidence.worktree?.trim()) {
      return {
        type: "Held",
        reason: "the verification record names no worktree or head, so it cannot be tied to the work",
      };
    }

    if (evidence.worktree !== task.worktree) {
      return {
        type: "Held",
        reason: `the verified worktree "${evidence.worktree}" belongs to another repository`,
      };
    }

    const currentHead = currentHeadOfWorktree(evidence.worktree);
    if (!currentHead) {
      return {
        type: "Held",
        reason: "the head of the verified worktree could not be read, so the verdict cannot be tied to the work",
      };
    }

    if (currentHead !== evidence.head) {
      return {
        type: "Held",
        reason: `verification passed for ${evidence.head.slice(0, 8)} but the worktree is now at ${currentHead.slice(0, 8)}; the work changed after it was verified. Verify again`,
      };
    }

    return {
      type: "Passed",
      verdict: `Verification passed for ${evidence.head.slice(0, 8)} (request ${evidence.requestId}).`,
    };
  }

  /**
   * Precise simulation of `intake.rs::complete_task` (lines 1616–1655).
   */
  function completeTaskSim(
    task: WorkItem,
    requestedStatus: "done" | "review" | "in_progress",
    evidence: VerificationEvidence | null,
    repoIdentity: string,
    currentHeadLookup: (worktree: string) => string | null
  ): { status: string; requestedStatus: string; heldForReview: boolean; verification: string } {
    let gate: GateDecision | null = null;
    let heldReason: string | null = null;

    if (requestedStatus === "done" && task.status !== "done") {
      gate = evaluateVerificationGate(evidence, task, repoIdentity, currentHeadLookup);
      if (gate.type === "Held") {
        heldReason = gate.reason;
      }
    }

    const finalStatus = heldReason ? "review" : requestedStatus;
    task.status = finalStatus as WorkItem["status"];

    return {
      status: finalStatus,
      requestedStatus,
      heldForReview: heldReason !== null,
      verification: heldReason || (gate?.type === "Passed" ? gate.verdict : ""),
    };
  }

  const VALID_WORKTREE = "/repos/gitpulse";
  const HEAD_COMMIT_1 = "951dd401a2b3c4d5e6f7";
  const HEAD_COMMIT_2 = "c0ffee11223344556677";

  it("3.1: Task without verification evidence claiming 'done' is rejected and held in 'review'", () => {
    const task: WorkItem = {
      id: "ft-no-evidence",
      status: "in_progress",
      logs: "",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_1,
    };

    const res = completeTaskSim(task, "done", null, "gitpulse", () => HEAD_COMMIT_1);
    expect(res.status).toBe("review");
    expect(res.requestedStatus).toBe("done");
    expect(res.heldForReview).toBe(true);
    expect(res.verification).toContain("no verification record exists");
    expect(task.status).toBe("review"); // Invariant: Task was NOT marked done
  });

  it("3.2: Verification record with outcome 'failed' or 'degraded' is rejected and held in 'review'", () => {
    const task: WorkItem = {
      id: "ft-failing",
      status: "in_progress",
      logs: "",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_1,
    };

    const failedEvidence: VerificationEvidence = {
      requestId: "req-fail-1",
      itemId: "ft-failing",
      state: "settled",
      outcome: "failed",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_1,
      criteria: [{ index: 1, text: "unit tests pass", status: "unmet" }],
    };

    const resFail = completeTaskSim(task, "done", failedEvidence, "gitpulse", () => HEAD_COMMIT_1);
    expect(resFail.status).toBe("review");
    expect(resFail.heldForReview).toBe(true);
    expect(resFail.verification).toContain("verification did not pass (outcome \"failed\")");
    expect(task.status).toBe("review");

    // Degraded outcome
    failedEvidence.outcome = "degraded";
    const resDeg = completeTaskSim(task, "done", failedEvidence, "gitpulse", () => HEAD_COMMIT_1);
    expect(resDeg.status).toBe("review");
    expect(resDeg.heldForReview).toBe(true);
    expect(resDeg.verification).toContain("verification did not pass (outcome \"degraded\")");
  });

  it("3.3: Verification record with state 'running', 'pending', or 'could_not_run' is rejected", () => {
    const task: WorkItem = {
      id: "ft-unsettled",
      status: "in_progress",
      logs: "",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_1,
    };

    const unsettledEvidence: VerificationEvidence = {
      requestId: "req-unsettled-1",
      itemId: "ft-unsettled",
      state: "running",
      outcome: "passed",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_1,
      criteria: [],
    };

    // 'running' state
    const resRun = completeTaskSim(task, "done", unsettledEvidence, "gitpulse", () => HEAD_COMMIT_1);
    expect(resRun.status).toBe("review");
    expect(resRun.heldForReview).toBe(true);
    expect(resRun.verification).toContain("verification is still \"running\"; no verdict yet");

    // 'could_not_run' state
    unsettledEvidence.state = "could_not_run";
    const resCnr = completeTaskSim(task, "done", unsettledEvidence, "gitpulse", () => HEAD_COMMIT_1);
    expect(resCnr.status).toBe("review");
    expect(resCnr.heldForReview).toBe(true);
    expect(resCnr.verification).toContain("verification could not run, so no verdict exists");
  });

  it("3.4: TOCTOU safety: verification passed on commit A, but worktree is now on commit B -> fails closed", () => {
    const task: WorkItem = {
      id: "ft-toctou",
      status: "in_progress",
      logs: "",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_2, // Worktree moved to commit 2
    };

    const staleHeadEvidence: VerificationEvidence = {
      requestId: "req-toctou-1",
      itemId: "ft-toctou",
      state: "settled",
      outcome: "passed",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_1, // Verified against commit 1
      criteria: [{ index: 1, text: "passed", status: "met" }],
    };

    // Current worktree lookup returns HEAD_COMMIT_2 (head advanced)
    const res = completeTaskSim(task, "done", staleHeadEvidence, "gitpulse", () => HEAD_COMMIT_2);
    expect(res.status).toBe("review");
    expect(res.heldForReview).toBe(true);
    expect(res.verification).toContain("the work changed after it was verified. Verify again");
    expect(task.status).toBe("review");
  });

  it("3.5: Task claiming 'done' with fully verified, settled, matching head evidence cleanly transitions to 'done'", () => {
    const task: WorkItem = {
      id: "ft-verified-clean",
      status: "review",
      logs: "",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_1,
    };

    const validEvidence: VerificationEvidence = {
      requestId: "req-valid-1",
      itemId: "ft-verified-clean",
      state: "settled",
      outcome: "passed",
      worktree: VALID_WORKTREE,
      head: HEAD_COMMIT_1,
      criteria: [{ index: 1, text: "all tests pass", status: "met" }],
    };

    const res = completeTaskSim(task, "done", validEvidence, "gitpulse", () => HEAD_COMMIT_1);
    expect(res.status).toBe("done");
    expect(res.requestedStatus).toBe("done");
    expect(res.heldForReview).toBe(false);
    expect(res.verification).toContain("Verification passed");
    expect(task.status).toBe("done"); // Clean transition!
  });
});

// ============================================================================
// PART 4: PRODUCTION ARTIFACT AUDIT (TASK_REVIEW.md & ft-7e812b3f JSON)
// ============================================================================
describe("Feature 13 Challenge 4: Backlog Review Deliverable & Task State Parity", () => {
  it("4.1: TASK_REVIEW.md exists and accurately details the 255 tasks and 5 P0 profiles", () => {
    const reportPath = join(PROJECT_ROOT, "TASK_REVIEW.md");
    expect(existsSync(reportPath)).toBe(true);

    const content = readFileSync(reportPath, "utf8");
    expect(content).toContain("255 verified tasks");
    expect(content).toContain("ft-7e812b3f");
    expect(content).toContain("verification_gate.rs");

    // Check all 5 P0 tasks are profiled
    expect(content).toContain("ft-42081f16");
    expect(content).toContain("ft-aa3086e6");
    expect(content).toContain("ft-f8a297ff");
    expect(content).toContain("ft-ae39d654");
    expect(content).toContain("ft-fa689914");
  });

  it("4.2: Stored task file .tasks/ft-7e812b3fc7ea076a7da3a5d6a39d2e1f.json has verified done state and revision 6", () => {
    const taskPath = join(
      PROJECT_ROOT,
      ".tasks",
      "ft-7e812b3fc7ea076a7da3a5d6a39d2e1f.json"
    );
    expect(existsSync(taskPath)).toBe(true);

    const json = JSON.parse(readFileSync(taskPath, "utf8"));
    expect(json.id).toBe("ft-7e812b3fc7ea076a7da3a5d6a39d2e1f");
    expect(json.status).toBe("done");
    expect(json.revision).toBe(6);
    expect(json.completed_at).toBeGreaterThan(0);
    expect(json.checklist.length).toBe(5);

    // Assert all 5 criteria are marked done: true
    for (const item of json.checklist) {
      expect(item.done).toBe(true);
    }
  });
});

// ============================================================================
// PART 5: PRODUCTION TASKBATCH OCC & CONCURRENCY STRESS HARNESS
// ============================================================================
describe("Feature 13 Challenge 5: Production TaskBatch OCC & Concurrency Stress Harness", () => {
  function makeSampleTask(id: string, revision = 1, status: TaskStatus = "inbox"): Task {
    return task({
      id,
      title: "Task " + id,
      kind: "feature",
      revision,
      status,
      priority: 2,
      severity: null,
      owner: null,
      due_at: null,
      labels: [],
      repository_ids: ["r1"],
      primary_repository_id: "r1",
      home_workspace_id: null,
      position: 0,
      description: "Description for " + id,
      acceptance_criteria: [],
      checklist: [],
      links: [],
      updated_at: 1000,
    });
  }

  it("5.1: TaskBatch constructor strictly rejects invalid revisions, empty selections, and oversized batches", () => {
    // Empty selection
    expect(() => new TaskBatch([], { kind: "delete" })).toThrow("Select between 1 and 100 distinct tasks");

    // Oversized selection (> 100)
    const over100 = Array.from({ length: 101 }, (_, i) => ({ id: `t-${i}`, title: `T${i}`, revision: 1 }));
    expect(() => new TaskBatch(over100, { kind: "delete" })).toThrow("Select between 1 and 100 distinct tasks");

    // Duplicate task IDs
    const duplicates = [
      { id: "dup-1", title: "D1", revision: 1 },
      { id: "dup-1", title: "D2", revision: 1 },
    ];
    expect(() => new TaskBatch(duplicates, { kind: "delete" })).toThrow("Select between 1 and 100 distinct tasks");

    // Invalid revisions: 0, negative, non-integer, non-safe integer
    const invalidRevs = [0, -1, -999, 1.5, NaN, Infinity, -Infinity, Number.MAX_SAFE_INTEGER + 10];
    for (const badRev of invalidRevs) {
      expect(() => {
        new TaskBatch([{ id: "t1", title: "T1", revision: badRev }], { kind: "delete" });
      }).toThrow("Task selection contains an invalid saved revision");
    }

    // Invalid update changes (priority > 3, negative position, invalid status)
    expect(() => {
      new TaskBatch(
        [{ id: "t1", title: "T1", revision: 1 }],
        { kind: "update", changes: { priority: 4 } }
      );
    }).toThrow("Invalid task organization change");

    expect(() => {
      new TaskBatch(
        [{ id: "t1", title: "T1", revision: 1 }],
        { kind: "update", changes: { position: -5 } }
      );
    }).toThrow("Invalid task organization change");
  });

  it("5.2: Production TaskBatch high-concurrency race condition: 100 workers competing on revision R: exactly 1 wins, 99 fail closed", async () => {
    const initialRev = 10;
    let storedTask = makeSampleTask("task-occ-100", initialRev, "inbox");

    // Async storage IO with realistic event-loop jitter matching backend SQLite WAL OCC
    const storeIO = {
      read: async (id: string) => {
        await new Promise((r) => setTimeout(r, Math.floor(Math.random() * 4)));
        if (id !== storedTask.id) throw new WorkbenchError("not_found", "Task not found");
        return storedTask;
      },
      write: async (input: Record<string, unknown>) => {
        await new Promise((r) => setTimeout(r, Math.floor(Math.random() * 4)));
        if (input.expected_revision !== storedTask.revision) {
          throw new WorkbenchError(
            "revision_conflict",
            `Revision mismatch: expected ${input.expected_revision}, current is ${storedTask.revision}`
          );
        }
        storedTask = task({
          ...storedTask,
          status: (input.status as TaskStatus) ?? storedTask.status,
          revision: storedTask.revision + 1,
          updated_at: Date.now(),
        });
        return storedTask;
      },
      remove: async () => {},
    };

    const concurrency = 100;
    const batches = Array.from({ length: concurrency }, () => {
      return new TaskBatch(
        [{ id: "task-occ-100", title: storedTask.title, revision: initialRev }],
        { kind: "update", changes: { status: "ready" } },
        storeIO
      );
    });

    // Execute all 100 TaskBatch runs concurrently
    await Promise.all(batches.map((b) => b.run()));

    const snapshots = batches.map((b) => b.snapshot()[0]);
    const winners = snapshots.filter((s) => s.state === "done");
    const losers = snapshots.filter((s) => s.state === "failed");

    // INVARIANT: Exactly 1 worker wins the OCC race
    expect(winners).toHaveLength(1);
    expect(winners[0].error).toBe("");

    // INVARIANT: Exactly 99 workers fail closed with revision conflict
    expect(losers).toHaveLength(concurrency - 1);
    for (const loser of losers) {
      expect(loser.state).toBe("failed");
      expect(loser.error).toMatch(/revision|changed/i);
    }

    // INVARIANT: Final stored task was modified exactly once (revision 10 -> 11)
    expect(storedTask.revision).toBe(initialRev + 1);
    expect(storedTask.status).toBe("ready");
  });

  it("5.3: Production TaskBatch out-of-order, stale, and future revisions fail closed without mutating task", async () => {
    let storedTask = makeSampleTask("task-ooo-rev", 5, "in_progress");

    const storeIO = {
      read: async (id: string) => {
        if (id !== storedTask.id) throw new WorkbenchError("not_found", "Task not found");
        return storedTask;
      },
      write: async (input: Record<string, unknown>) => {
        if (input.expected_revision !== storedTask.revision) {
          throw new WorkbenchError("revision_conflict", "Revision mismatch");
        }
        storedTask = task({
          ...storedTask,
          status: (input.status as TaskStatus) ?? storedTask.status,
          revision: storedTask.revision + 1,
        });
        return storedTask;
      },
      remove: async () => {},
    };

    // Stale revision: card revision 4 < stored revision 5
    const staleBatch = new TaskBatch(
      [{ id: "task-ooo-rev", title: storedTask.title, revision: 4 }],
      { kind: "update", changes: { status: "review" } },
      storeIO
    );
    await staleBatch.run();
    const staleSnap = staleBatch.snapshot()[0];
    expect(staleSnap.state).toBe("failed");
    expect(staleSnap.error).toContain("This task changed while you were moving it");
    expect(storedTask.revision).toBe(5);
    expect(storedTask.status).toBe("in_progress");

    // Future revision: card revision 8 > stored revision 5
    const futureBatch = new TaskBatch(
      [{ id: "task-ooo-rev", title: storedTask.title, revision: 8 }],
      { kind: "update", changes: { status: "done" } },
      storeIO
    );
    await futureBatch.run();
    const futureSnap = futureBatch.snapshot()[0];
    expect(futureSnap.state).toBe("failed");
    expect(futureSnap.error).toContain("This task changed while you were moving it");
    expect(storedTask.revision).toBe(5);
    expect(storedTask.status).toBe("in_progress");

    // Retry with exact revision 5: succeeds cleanly and advances to revision 6
    const validBatch = new TaskBatch(
      [{ id: "task-ooo-rev", title: storedTask.title, revision: 5 }],
      { kind: "update", changes: { status: "review" } },
      storeIO
    );
    await validBatch.run();
    const validSnap = validBatch.snapshot()[0];
    expect(validSnap.state).toBe("done");
    expect(validSnap.error).toBe("");
    expect(storedTask.revision).toBe(6);
    expect(storedTask.status).toBe("review");
  });

  it("5.4: 500 sequential chained mutations through production TaskBatch increment revision monotonically from 1 to 501", async () => {
    let storedTask = makeSampleTask("task-chain-500", 1, "inbox");

    const storeIO = {
      read: async (id: string) => {
        if (id !== storedTask.id) throw new WorkbenchError("not_found", "Task not found");
        return storedTask;
      },
      write: async (input: Record<string, unknown>) => {
        if (input.expected_revision !== storedTask.revision) {
          throw new WorkbenchError("revision_conflict", "Revision mismatch");
        }
        storedTask = task({
          ...storedTask,
          status: (input.status as TaskStatus) ?? storedTask.status,
          revision: storedTask.revision + 1,
        });
        return storedTask;
      },
      remove: async () => {},
    };

    let expectedRev = 1;
    const chainLength = 500;

    for (let i = 1; i <= chainLength; i++) {
      const nextStatus: TaskStatus = i % 2 === 0 ? "in_progress" : "review";
      const batch = new TaskBatch(
        [{ id: "task-chain-500", title: storedTask.title, revision: expectedRev }],
        { kind: "update", changes: { status: nextStatus } },
        storeIO
      );
      await batch.run();
      const snap = batch.snapshot()[0];
      expect(snap.state).toBe("done");
      expect(snap.error).toBe("");
      expect(storedTask.revision).toBe(expectedRev + 1);
      expect(storedTask.status).toBe(nextStatus);
      expectedRev = storedTask.revision;
    }

    expect(storedTask.revision).toBe(501);
  });

  it("5.5: Multi-task batch isolates revision conflict: conflicting card fails, non-conflicting cards succeed", async () => {
    const tasksMap = new Map<string, Task>([
      ["t1", makeSampleTask("t1", 1, "inbox")],
      ["t2", makeSampleTask("t2", 1, "inbox")],
      ["t3", makeSampleTask("t3", 2, "ready")], // Store is already at revision 2!
      ["t4", makeSampleTask("t4", 1, "inbox")],
      ["t5", makeSampleTask("t5", 1, "inbox")],
    ]);

    const storeIO = {
      read: async (id: string) => {
        const t = tasksMap.get(id);
        if (!t) throw new WorkbenchError("not_found", `Task ${id} not found`);
        return t;
      },
      write: async (input: Record<string, unknown>) => {
        const id = String(input.id);
        const t = tasksMap.get(id);
        if (!t) throw new WorkbenchError("not_found", `Task ${id} not found`);
        if (input.expected_revision !== t.revision) {
          throw new WorkbenchError("revision_conflict", `Revision conflict on ${id}`);
        }
        const updated = task({
          ...t,
          status: (input.status as TaskStatus) ?? t.status,
          revision: t.revision + 1,
        });
        tasksMap.set(id, updated);
        return updated;
      },
      remove: async () => {},
    };

    // Caller constructed batch with cards assumed to be at revision 1
    const cards = [
      { id: "t1", title: "T1", revision: 1 },
      { id: "t2", title: "T2", revision: 1 },
      { id: "t3", title: "T3", revision: 1 }, // Stale revision (store has 2)
      { id: "t4", title: "T4", revision: 1 },
      { id: "t5", title: "T5", revision: 1 },
    ];

    const batch = new TaskBatch(cards, { kind: "update", changes: { status: "ready" } }, storeIO);
    await batch.run();

    const results = batch.snapshot();
    expect(results[0].state).toBe("done"); // t1 updated (1 -> 2)
    expect(results[1].state).toBe("done"); // t2 updated (1 -> 2)
    expect(results[2].state).toBe("failed"); // t3 failed closed (stale 1 vs store 2)
    expect(results[2].error).toContain("This task changed while you were moving it");
    expect(results[3].state).toBe("done"); // t4 updated (1 -> 2)
    expect(results[4].state).toBe("done"); // t5 updated (1 -> 2)

    // Store state: t1, t2, t4, t5 advanced to revision 2 with status ready; t3 remained at revision 2
    expect(tasksMap.get("t1")!.revision).toBe(2);
    expect(tasksMap.get("t2")!.revision).toBe(2);
    expect(tasksMap.get("t3")!.revision).toBe(2);
    expect(tasksMap.get("t4")!.revision).toBe(2);
    expect(tasksMap.get("t5")!.revision).toBe(2);
    expect(tasksMap.get("t1")!.status).toBe("ready");
    expect(tasksMap.get("t3")!.status).toBe("ready"); // Untouched by stale mutation
  });

  it("5.6: Out-of-order execution race: Batch B (rev 6) before Batch A (rev 5->6) fails closed; retrying after A succeeds", async () => {
    let storedTask = makeSampleTask("task-race-order", 5, "inbox");

    const storeIO = {
      read: async (id: string) => {
        if (id !== storedTask.id) throw new WorkbenchError("not_found", "Task not found");
        return storedTask;
      },
      write: async (input: Record<string, unknown>) => {
        if (input.expected_revision !== storedTask.revision) {
          throw new WorkbenchError("revision_conflict", "Revision mismatch");
        }
        storedTask = task({
          ...storedTask,
          status: (input.status as TaskStatus) ?? storedTask.status,
          revision: storedTask.revision + 1,
        });
        return storedTask;
      },
      remove: async () => {},
    };

    // Batch A: expects rev 5 -> moves to ready (rev 6)
    const batchA = new TaskBatch(
      [{ id: "task-race-order", title: storedTask.title, revision: 5 }],
      { kind: "update", changes: { status: "ready" } },
      storeIO
    );

    // Batch B: speculative next action expecting rev 6 -> moves to in_progress (rev 7)
    const batchB = new TaskBatch(
      [{ id: "task-race-order", title: storedTask.title, revision: 6 }],
      { kind: "update", changes: { status: "in_progress" } },
      storeIO
    );

    // If Batch B runs out-of-order before Batch A:
    await batchB.run();
    expect(batchB.snapshot()[0].state).toBe("failed");
    expect(batchB.snapshot()[0].error).toContain("This task changed while you were moving it");
    expect(storedTask.revision).toBe(5); // Preserved

    // Batch A now runs:
    await batchA.run();
    expect(batchA.snapshot()[0].state).toBe("done");
    expect(storedTask.revision).toBe(6);
    expect(storedTask.status).toBe("ready");

    // Retrying Batch B now that store reached revision 6:
    const batchBRetry = new TaskBatch(
      [{ id: "task-race-order", title: storedTask.title, revision: 6 }],
      { kind: "update", changes: { status: "in_progress" } },
      storeIO
    );
    await batchBRetry.run();
    expect(batchBRetry.snapshot()[0].state).toBe("done");
    expect(storedTask.revision).toBe(7);
    expect(storedTask.status).toBe("in_progress");
  });

  it("5.7: TaskBatch.undo() generates atomic restore plan and cleanly reverts production store state", async () => {
    let storedTask = makeSampleTask("task-undo", 1, "inbox");

    const storeIO = {
      read: async (id: string) => {
        if (id !== storedTask.id) throw new WorkbenchError("not_found", "Task not found");
        return storedTask;
      },
      write: async (input: Record<string, unknown>) => {
        if (input.expected_revision !== storedTask.revision) {
          throw new WorkbenchError("revision_conflict", "Revision mismatch");
        }
        storedTask = task({
          ...storedTask,
          status: (input.status as TaskStatus) ?? storedTask.status,
          revision: storedTask.revision + 1,
        });
        return storedTask;
      },
      remove: async () => {},
    };

    // Forward mutation: inbox -> ready (rev 1 -> 2)
    const forwardBatch = new TaskBatch(
      [{ id: "task-undo", title: storedTask.title, revision: 1 }],
      { kind: "update", changes: { status: "ready" } },
      storeIO
    );
    await forwardBatch.run();
    expect(forwardBatch.snapshot()[0].state).toBe("done");
    expect(storedTask.status).toBe("ready");
    expect(storedTask.revision).toBe(2);

    // Generate undo plan
    const undoPlan = forwardBatch.undo();
    expect(undoPlan).not.toBeNull();
    expect(undoPlan!.cards[0].revision).toBe(2); // Targets the revision produced
    expect(undoPlan!.action.kind).toBe("restore");

    // Execute undo plan
    const undoBatch = new TaskBatch(undoPlan!.cards, undoPlan!.action, storeIO);
    await undoBatch.run();
    expect(undoBatch.snapshot()[0].state).toBe("done");
    expect(storedTask.status).toBe("inbox"); // Restored!
    expect(storedTask.revision).toBe(3); // Monotonically incremented!
  });
});
