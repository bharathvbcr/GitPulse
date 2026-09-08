import { loadWork } from "./load";
import { getCollisionRisk } from "../insights/client";
import { validateWorkResponse } from "./contracts";
import { withinDeadline, WORK_TIMEOUT_MS } from "./request";
import { formatError } from "../ui/formatError";
import type { WorkProjection } from "./projection";
import type { CollisionRisk } from "../insights/types";

export interface WorkRefreshObserver {
  projection(value: WorkProjection, checkedAt: number): void;
  collisions(value: CollisionRisk | null, error: string | null): void;
  finished(error: string | null): void;
}

/** One running refresh plus the latest requested refresh, shared across remounts. */
export function createWorkRefresh(deps = { load: loadWork, collisions: getCollisionRisk }) {
  type Job = { repo: string; observer: WorkRefreshObserver; controller: AbortController };
  let running: Job | null = null;
  let pending: Job | null = null;

  async function drain(): Promise<void> {
    if (running) return;
    while (pending) {
      const job: Job = pending;
      pending = null;
      running = job;
      let error: string | null = null;
      try {
        const result = await withinDeadline(() => deps.load(job.repo, { signal: job.controller.signal }), Date.now() + WORK_TIMEOUT_MS);
        if (!job.controller.signal.aborted) {
          job.observer.projection(result, Date.now());
          // Finish this scan even when status notifications queued another load.
          // Otherwise steady file changes could starve overlap detection forever.
          try {
            const risk = await withinDeadline(() => deps.collisions(job.repo), Date.now() + WORK_TIMEOUT_MS);
            validateWorkResponse("cmd_collision_risk", risk);
            if (!job.controller.signal.aborted) job.observer.collisions(risk, risk.ok ? null : risk.error || "collision check failed");
          } catch (failure) {
            if (!job.controller.signal.aborted) job.observer.collisions(null, formatError(failure));
          }
        }
      } catch (failure) { error = formatError(failure); }
      if (!job.controller.signal.aborted) job.observer.finished(error);
      running = null;
    }
  }

  return {
    cancel(): void {
      running?.controller.abort();
      pending?.controller.abort();
      pending = null;
    },
    request(repo: string, observer: WorkRefreshObserver): () => void {
      pending?.controller.abort();
      if (running && running.repo !== repo) running.controller.abort();
      const job: Job = { repo, observer, controller: new AbortController() };
      pending = job;
      void drain();
      return () => {
        job.controller.abort();
        if (pending === job) pending = null;
      };
    },
  };
}
