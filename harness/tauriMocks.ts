import { mockIPC } from "@tauri-apps/api/mocks";

type Handler = Parameters<typeof mockIPC>[0];
type Invoke = (cmd: string, args?: unknown, options?: unknown) => Promise<unknown>;

/**
 * `mockIPC(handler, { shouldMockEvents: true })` with unlisten that removes.
 *
 * `@tauri-apps/api` 2.12.1 (the latest at the time of writing) unlistens by
 * sending `plugin:event|unlisten` with `{ event, eventId }`, while the event
 * mock removes the handler whose id is `args.id`. The lookup never matches, so
 * the handler stays registered after its callback is deleted, and every later
 * emit of that event warns `[TAURI] Couldn't find callback id …`. In the
 * task-runs harness that was 45 warnings per run, all from the terminal bus
 * releasing its listeners correctly, and each one blamed an app reload that
 * never happened.
 *
 * Real Tauri removes listeners natively, so only the mock is wrong. The
 * repair forwards `eventId` as `id`, which is what the mock reads. When an
 * upstream release fixes the mock, the "upstream mock" case in
 * `tauriMocks.test.ts` fails and this wrapper can go.
 */
export function mockIPCWithEvents(handler: Handler): void {
  mockIPC(handler, { shouldMockEvents: true });
  const internals: unknown = Reflect.get(window, "__TAURI_INTERNALS__");
  if (typeof internals !== "object" || internals === null) throw new Error("mockIPC did not install window.__TAURI_INTERNALS__");
  const mocked: unknown = Reflect.get(internals, "invoke");
  if (typeof mocked !== "function") throw new Error("mockIPC did not install window.__TAURI_INTERNALS__.invoke");
  const repaired: Invoke = async (cmd, args, options) => {
    const forwarded = cmd === "plugin:event|unlisten" && args && typeof args === "object" && "eventId" in args && !("id" in args)
      ? { ...args, id: args.eventId }
      : args;
    return Reflect.apply(mocked, internals, [cmd, forwarded, options]);
  };
  Reflect.set(internals, "invoke", repaired);
}
