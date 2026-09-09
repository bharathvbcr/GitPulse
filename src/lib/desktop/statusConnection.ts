import type { MenuState } from "./menuState";

type StatusBridge = {
  read: () => Promise<MenuState>;
  subscribe: (apply: (snapshot: MenuState) => void) => Promise<() => void>;
  apply: (snapshot: MenuState) => void;
  failed: (error: unknown) => void;
};

/** Subscribe before reading; live updates win over a delayed initial response. */
export function createStatusConnection(bridge: StatusBridge) {
  let disposed = false;
  let revision = 0;
  let stop: (() => void) | null = null;
  let inflight: Promise<void> | null = null;
  return {
    connect(): Promise<void> {
      if (disposed) return Promise.resolve();
      if (inflight) return inflight;
      inflight = (async () => {
        let startedAt = revision;
        try {
          if (!stop) {
            const unsubscribe = await bridge.subscribe((next) => {
              if (!disposed) { revision++; bridge.apply(next); }
            });
            if (disposed) { unsubscribe(); return; }
            stop = unsubscribe;
          }
          startedAt = revision;
          const next = await bridge.read();
          if (!disposed && startedAt === revision) bridge.apply(next);
        } catch (error) {
          if (!disposed && startedAt === revision) bridge.failed(error);
        }
      })().finally(() => { inflight = null; });
      return inflight;
    },
    dispose() { disposed = true; stop?.(); stop = null; },
  };
}
