import { get, type Readable } from "svelte/store";

type Activity = Record<string, string[]>;
export function gitActionCount(activity: Activity): number {
  return Object.values(activity).reduce((count, entries) => count + entries.length, 0);
}

/** A quit wait has a deadline and always releases its subscription. */
export function waitForGitIdle(activity: Readable<Activity>, timeoutMs = 120_000): Promise<void> {
  if (gitActionCount(get(activity)) === 0) return Promise.resolve();
  return new Promise((resolve, reject) => {
    let stop: (() => void) | undefined;
    let settled = false;
    const timer = setTimeout(() => {
      settled = true;
      stop?.();
      reject(new Error("Git actions are still running. GitPulse will remain open; try quitting when they finish."));
    }, timeoutMs);
    stop = activity.subscribe((value) => {
      if (gitActionCount(value) !== 0) return;
      settled = true;
      clearTimeout(timer);
      stop?.();
      resolve();
    });
    if (settled) stop();
  });
}
