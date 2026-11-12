import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { MAX_QUEUED_BROWSE, createBrowseQueue, parseBrowseRequest, type BrowseDeps } from "./browseRequests";
import { TERMINAL_SESSION_PENDING } from "../terminal/sessionMessages";
import { isLoopbackUrl } from "./browserSession";
import { createTerminalLaunchRequests } from "../terminal/launchRequests";
import { browserTabFor, initialState, openBrowserTab, setTabUrl } from "../terminal/tabs";

function fakeDeps(over: Partial<BrowseDeps> = {}) {
  const log: string[] = [];
  const failures: string[] = [];
  let busy = false;
  const deps: BrowseDeps = {
    open: async (repo) => (log.push(`open ${repo}`), repo),
    showDock: () => void log.push("dock"),
    busy: () => busy,
    launch: async (repo, url) => void log.push(`launch ${repo} ${url}`),
    fail: (message) => void failures.push(message),
    sleep: async () => {},
    ...over,
  };
  return { deps, log, failures, setBusy: (next: boolean) => (busy = next) };
}

describe("agents' browser session requests", () => {
  it("reads only a well-formed event", () => {
    expect(parseBrowseRequest({ repo: "/r", url: "http://localhost:3000/" })).toEqual({ repo: "/r", url: "http://localhost:3000/" });
    for (const bad of [null, "x", {}, { repo: "/r" }, { repo: 1, url: "u" }, { repo: "", url: "u" }, { repo: "/r", url: "x".repeat(2049) }]) {
      expect(parseBrowseRequest(bad)).toBeNull();
    }
  });

  it("opens the repository, shows its dock, then asks for the session, one request at a time in order", async () => {
    const { deps, log } = fakeDeps();
    const queue = createBrowseQueue(deps);
    queue.push({ repo: "/a", url: "http://localhost:1/" });
    queue.push({ repo: "/b", url: "http://localhost:2/" });
    await queue.settled();
    expect(log).toEqual(["open /a", "dock", "launch /a http://localhost:1/", "open /b", "dock", "launch /b http://localhost:2/"]);
  });

  it("waits for the launch channel instead of failing on a pending launch", async () => {
    const fake = fakeDeps();
    let checks = 0;
    fake.deps.busy = () => ++checks < 4;
    const queue = createBrowseQueue(fake.deps);
    queue.push({ repo: "/a", url: "http://localhost:1/" });
    await queue.settled();
    expect(fake.log.at(-1)).toBe("launch /a http://localhost:1/");
    expect(fake.failures).toEqual([]);
  });

  it("says when a request could not be carried out, and goes on to the next", async () => {
    const { deps, log, failures } = fakeDeps({
      open: async (repo) => (repo === "/gone" ? null : repo),
      launch: async (_repo, url) => {
        if (url.includes("refuse")) throw new Error("8 browser sessions are open");
        log.push(`launch ${url}`);
      },
    });
    const queue = createBrowseQueue(deps);
    queue.push({ repo: "/gone", url: "http://localhost:1/" });
    queue.push({ repo: "/a", url: "http://localhost:2/refuse" });
    queue.push({ repo: "/a", url: "http://localhost:3/" });
    await queue.settled();
    expect(failures).toHaveLength(2);
    expect(failures[0]).toContain("Couldn't open /gone.");
    expect(failures[1]).toContain("8 browser sessions are open");
    expect(log.at(-1)).toBe("launch http://localhost:3/");
  });

  it("gives up on a channel that never frees, with a notice", async () => {
    const fake = fakeDeps();
    fake.setBusy(true);
    const queue = createBrowseQueue(fake.deps);
    queue.push({ repo: "/a", url: "http://localhost:1/" });
    await queue.settled();
    expect(fake.failures[0]).toContain(TERMINAL_SESSION_PENDING);
  });

  it("holds at most a bounded number of requests, and says so past it", async () => {
    let release = () => {};
    const blocked = new Promise<void>((done) => (release = done));
    const fake = fakeDeps({ launch: () => blocked });
    const queue = createBrowseQueue(fake.deps);
    // One is taken at once; the rest wait.
    const accepted = Array.from({ length: MAX_QUEUED_BROWSE + 2 }, (_, i) => queue.push({ repo: "/a", url: `http://localhost:${i + 1}/` }));
    expect(accepted.filter(Boolean)).toHaveLength(MAX_QUEUED_BROWSE + 1);
    expect(fake.failures).toHaveLength(1);
    expect(fake.failures[0]).toContain("already waiting");
    release();
    await queue.settled();
  });
});

describe("the launch channel's browser requests", () => {
  it("carries the address to the panel and refuses an empty or oversized one", async () => {
    const requests = createTerminalLaunchRequests();
    const pending = requests.requestBrowser("/repo", " http://localhost:5173/ ");
    expect(requests.busy).toBe(true);
    expect(get(requests)).toMatchObject({ launcher: "browser", url: "http://localhost:5173/" });
    requests.take("/repo")?.complete();
    await pending;
    expect(requests.busy).toBe(false);
    await expect(requests.requestBrowser("/repo", "  ")).rejects.toThrow(/address/);
    await expect(requests.requestBrowser("/repo", `http://localhost/${"x".repeat(2048)}`)).rejects.toThrow(/2048/);
  });
});

describe("browser tabs by address", () => {
  it("reuses the tab already on an address, spelled either way, instead of opening another", () => {
    const first = openBrowserTab(initialState(), "http://localhost:3000");
    expect(first.opened).toBe(true);
    const again = openBrowserTab(first.state, "http://localhost:3000/");
    expect(again.opened).toBe(false);
    expect(again.id).toBe(first.id);
    expect(again.state.tabs).toHaveLength(first.state.tabs.length);
    expect(again.state.activeId).toBe(first.id);
    const other = openBrowserTab(first.state, "http://localhost:4000/");
    expect(other.opened).toBe(true);
    expect(other.id).not.toBe(first.id);
  });

  it("follows the page: a tab found by where it is now, not where it started", () => {
    const opened = openBrowserTab(initialState(), "http://localhost:3000/");
    const moved = setTabUrl(opened.state, opened.id!, "http://localhost:3000/settings");
    expect(browserTabFor(moved, "http://localhost:3000/settings")?.id).toBe(opened.id);
    expect(browserTabFor(moved, "http://localhost:3000/")).toBeUndefined();
    // Only browser tabs carry an address.
    const shell = moved.tabs.find((tab) => tab.launcher === "shell");
    expect(shell && setTabUrl(moved, shell.id, "http://localhost:1/")).toBe(moved);
  });

  it("opens a tab with no address when none is given", () => {
    const opened = openBrowserTab(initialState());
    expect(opened.opened).toBe(true);
    expect(opened.state.tabs.find((tab) => tab.id === opened.id)?.url).toBeUndefined();
  });
});

describe("which links open in a browser session", () => {
  it.each([
    ["http://localhost:3000/", true],
    ["https://LOCALHOST/", true],
    ["http://127.0.0.1:8080/x", true],
    ["http://127.9.9.9/", true],
    ["http://[::1]:4000/", true],
    ["https://example.com/", false],
    ["http://localhost.example.com/", false],
    ["http://10.0.0.1/", false],
    ["ftp://localhost/", false],
    ["file:///etc/passwd", false],
    ["not a url", false],
  ])("%s → %s", (url, expected) => {
    expect(isLoopbackUrl(url)).toBe(expected);
  });
});
