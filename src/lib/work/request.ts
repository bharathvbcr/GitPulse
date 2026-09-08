/** Overview has one overall load deadline, including every secondary probe. */
export const WORK_TIMEOUT_MS = 20_000;

export function workTimeout(value = WORK_TIMEOUT_MS): number {
  return Number.isFinite(value) && value > 0 ? Math.min(WORK_TIMEOUT_MS, Math.max(1, value)) : WORK_TIMEOUT_MS;
}

/** Stops waiting, not the native operation. Late resolutions/rejections are consumed. */
export function withinDeadline<T>(request: () => Promise<T>, deadline: number): Promise<T> {
  const remaining = deadline - Date.now();
  if (remaining <= 0) return Promise.reject(new Error("Overview refresh deadline exceeded"));
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("Overview refresh deadline exceeded")), remaining);
    Promise.resolve().then(request).then(resolve, reject).finally(() => clearTimeout(timer));
  });
}
