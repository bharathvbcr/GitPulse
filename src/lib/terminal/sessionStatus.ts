export const DETACHED_STATUS = "Still running — not shown in any tab";
/** A session whose terminal closed but whose process has not exited: stoppable, never shown. */
export const ENDING_STATUS = "Ending — its process has not exited";

/** A session that still holds a process, from the status string alone. */
export function sessionIsLive(status: string): boolean {
  return (
    status === "running" ||
    status === "starting" ||
    status === "closing" ||
    status === DETACHED_STATUS ||
    status === ENDING_STATUS
  );
}
