/**
 * Which terminal sessions the user can actually see.
 *
 * The native notifier suppresses a banner for a session the user is already
 * watching — otherwise an agent that pauses while you are reading its output
 * would interrupt you to tell you so. Half of that judgement is native: the
 * backend asks the window whether it has focus, because a renderer that has
 * been throttled or has stopped running is precisely the case where a
 * notification matters most, and its last word would have been "focused".
 *
 * The other half only the renderer knows: which tabs are on screen. That is
 * this module. Several can be — a split terminal shows two — so it is a set,
 * and each session reports only its own membership. Nothing here decides
 * anything; it collects and pushes.
 *
 * Reports are coalesced into one call per turn. Switching tabs changes two
 * memberships at once (one leaves, one arrives) and pushing each would send a
 * list that is briefly wrong in a way the backend would act on.
 */

export interface Attendance {
  /** Records whether one session is on screen, and schedules a push. */
  report(sessionId: string, onscreen: boolean): void;
  /** Drops a session entirely, for a tab that is closing. */
  forget(sessionId: string): void;
  /** The list as it stands, in insertion order. */
  visible(): string[];
  /** Sends the current list now, bypassing the coalescing turn. */
  flush(): Promise<void>;
}

/**
 * The upper bound the backend also applies. Kept here so a runaway caller is
 * refused where the mistake is, rather than silently truncated on arrival.
 */
export const MAX_VISIBLE_SESSIONS = 8;

export function createAttendance(
  push: (sessionIds: string[]) => Promise<void>,
  schedule: (run: () => void) => void = queueMicrotask,
): Attendance {
  const onscreen = new Map<string, boolean>();
  let queued = false;
  /**
   * The last list actually sent. A repeat is dropped rather than pushed: a
   * terminal repaints far more often than its visibility changes, and every
   * push is an IPC round trip.
   */
  let sent: string | null = null;

  const visible = () =>
    [...onscreen.entries()]
      .filter(([, on]) => on)
      .map(([id]) => id)
      .slice(0, MAX_VISIBLE_SESSIONS);

  const send = async () => {
    const list = visible();
    const key = JSON.stringify(list);
    if (key === sent) return;
    // Recorded before awaiting, so two flushes in the same turn cannot both
    // decide they are the first. A failed push clears it so the next attempt
    // retries rather than assuming the backend agrees with us.
    sent = key;
    try {
      await push(list);
    } catch {
      sent = null;
    }
  };

  const later = () => {
    if (queued) return;
    queued = true;
    schedule(() => {
      queued = false;
      void send();
    });
  };

  return {
    report(sessionId, on) {
      if (!sessionId) return;
      if (onscreen.get(sessionId) === on) return;
      onscreen.set(sessionId, on);
      later();
    },
    forget(sessionId) {
      if (!onscreen.delete(sessionId)) return;
      later();
    },
    visible,
    flush: send,
  };
}
