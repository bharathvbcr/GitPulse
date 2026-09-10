import type { MenuState } from "./menuState";
import { createListenerTracker } from "../dom/listenerTracker";

type StatusBridge = {
  read: () => Promise<MenuState>;
  subscribe: (apply: (snapshot: MenuState) => void) => Promise<() => void>;
  apply: (snapshot: MenuState) => void;
  failed: (error: unknown) => void;
};

/** Subscribe before reading; live updates win over a delayed initial response. */
export function createStatusConnection(bridge: StatusBridge) {
  const listeners = createListenerTracker();
  let revision = 0;
  let inflight: Promise<void> | null = null;
  return {
    connect(): Promise<void> {
      if (listeners.disposed) return Promise.resolve();
      if (inflight) return inflight;
      inflight = (async () => {
        let startedAt = revision;
        try {
          if (listeners.size === 0) {
            const unsubscribe = await bridge.subscribe((next) => {
              if (!listeners.disposed) { revision++; bridge.apply(next); }
            });
            listeners.track(unsubscribe);
            if (listeners.disposed) return;
          }
          startedAt = revision;
          const next = await bridge.read();
          if (!listeners.disposed && startedAt === revision) bridge.apply(next);
        } catch (error) {
          if (!listeners.disposed && startedAt === revision) bridge.failed(error);
        }
      })().finally(() => { inflight = null; });
      return inflight;
    },
    dispose() { listeners.dispose(); },
  };
}
