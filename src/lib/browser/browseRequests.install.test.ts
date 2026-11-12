import { beforeEach, describe, expect, it, vi } from "vitest";

// The boot wiring of agents' browser requests: the real queue over the real
// event name, with the stores it hands each step to standing in.

type Handler = (event: { payload: unknown }) => void;
type OpenOptions = { activate?: boolean; onReady?: (path: string) => void };

const h = vi.hoisted(() => ({
  handlers: new Map<string, Handler>(),
  unlisten: () => {},
  openRepo: async (_path: string, _options: OpenOptions): Promise<boolean> => true,
  calls: [] as string[],
  errors: [] as string[],
  busy: false,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (channel: string, handler: Handler) => {
    h.handlers.set(channel, handler);
    return h.unlisten;
  },
}));

vi.mock("../stores/repoStore", () => ({
  repoStore: {
    openRepo: (path: string, options: OpenOptions) => {
      h.calls.push(`open ${path} activate=${String(options.activate)}`);
      return h.openRepo(path, options);
    },
    setTerminalOpen: (open: boolean) => void h.calls.push(`dock ${String(open)}`),
  },
}));

vi.mock("../stores/toastStore", () => ({
  toastStore: { error: (message: string) => void h.errors.push(message) },
}));

vi.mock("../terminal/launchRequests", () => ({
  terminalLaunchRequests: {
    get busy() {
      return h.busy;
    },
    requestBrowser: async (repo: string, url: string) => void h.calls.push(`browser ${repo} ${url}`),
  },
}));

const { installBrowseRequests } = await import("./browseRequests");

function emit(payload: unknown): void {
  const handler = h.handlers.get("gitpulse-browse");
  if (!handler) throw new Error("installBrowseRequests did not listen on gitpulse-browse");
  handler({ payload });
}

beforeEach(() => {
  h.handlers.clear();
  h.calls.length = 0;
  h.errors.length = 0;
  h.busy = false;
  h.openRepo = async () => true;
});

describe("installing agents' browser requests at boot", () => {
  it("listens on gitpulse-browse and returns that listener's unlisten", async () => {
    const unlisten = await installBrowseRequests();
    expect(h.handlers.has("gitpulse-browse")).toBe(true);
    expect(unlisten).toBe(h.unlisten);
  });

  it("opens the repository active, shows its dock, then asks for the session at the canonical path", async () => {
    h.openRepo = async (_path, options) => {
      options.onReady?.("/canonical/repo");
      return true;
    };
    await installBrowseRequests();
    emit({ repo: "/link/repo", url: "http://localhost:5173/" });
    await vi.waitFor(() => expect(h.calls).toHaveLength(3));
    expect(h.calls).toEqual([
      "open /link/repo activate=true",
      "dock true",
      "browser /canonical/repo http://localhost:5173/",
    ]);
    expect(h.errors).toEqual([]);
  });

  it("uses the path it was given when the repository opened without naming a canonical one", async () => {
    await installBrowseRequests();
    emit({ repo: "/r", url: "http://127.0.0.1:8080/" });
    await vi.waitFor(() => expect(h.calls.at(-1)).toBe("browser /r http://127.0.0.1:8080/"));
  });

  it("ignores a payload that is not a request", async () => {
    await installBrowseRequests();
    emit({ repo: "/r" });
    emit("http://localhost:1/");
    await new Promise((done) => setTimeout(done, 20));
    expect(h.calls).toEqual([]);
    expect(h.errors).toEqual([]);
  });

  it("says, as an error toast, when the repository could not be opened", async () => {
    h.openRepo = async () => false;
    await installBrowseRequests();
    emit({ repo: "/gone", url: "http://localhost:1/" });
    await vi.waitFor(() => expect(h.errors).toHaveLength(1));
    expect(h.errors[0]).toContain("Couldn't open /gone.");
    expect(h.calls).toEqual(["open /gone activate=true"]);
  });

  it("reads the launch channel's busy flag before asking for the session", async () => {
    h.busy = true;
    await installBrowseRequests();
    emit({ repo: "/r", url: "http://localhost:2/" });
    await vi.waitFor(() => expect(h.calls).toContain("dock true"));
    await new Promise((done) => setTimeout(done, 150));
    expect(h.calls.some((call) => call.startsWith("browser"))).toBe(false);
    h.busy = false;
    await vi.waitFor(() => expect(h.calls.at(-1)).toBe("browser /r http://localhost:2/"));
  });
});
