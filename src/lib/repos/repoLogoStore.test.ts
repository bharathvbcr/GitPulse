import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";
import {
  createRepoLogoStore,
  loadRememberedLogos,
  MAX_PREVIEW_CHARS,
  NO_LOGO_TTL_MS,
  REPO_LOGO_STORAGE_KEY,
  UNAVAILABLE_RETRY_MS,
  type LogoTarget,
  type RepoLogoDeps,
} from "./repoLogoStore";
import type { LogoBlob } from "./repoLogo";

const PNG = btoa(String.fromCharCode(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d));

function memoryStorage(seed: Record<string, string> = {}) {
  const data = new Map(Object.entries(seed));
  return {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => void data.set(key, value),
    data,
  };
}

async function flush() {
  for (let i = 0; i < 20; i++) await Promise.resolve();
}

interface Harness {
  store: ReturnType<typeof createRepoLogoStore>;
  list: ReturnType<typeof vi.fn>;
  blob: ReturnType<typeof vi.fn>;
  storage: ReturnType<typeof memoryStorage>;
  clock: { now: number };
}

function harness(options: {
  files?: Record<string, readonly string[]>;
  blobs?: Record<string, LogoBlob | "refuse">;
  listRefused?: boolean;
  storage?: ReturnType<typeof memoryStorage>;
  concurrency?: number;
  clock?: { now: number };
} = {}): Harness {
  const clock = options.clock ?? { now: 1_000_000 };
  const list = vi.fn(async (repoPath: string) => {
    if (options.listRefused) throw new Error("Repository not trusted");
    return options.files?.[repoPath] ?? [];
  });
  const blob = vi.fn(async (_repo: string, path: string) => {
    const answer = options.blobs?.[path];
    if (answer === undefined || answer === "refuse") throw new Error(`cannot read ${path}`);
    return answer;
  });
  const storage = options.storage ?? memoryStorage();
  const deps: RepoLogoDeps = { list, blob, storage, now: () => clock.now, concurrency: options.concurrency };
  return { store: createRepoLogoStore(deps), list, blob, storage, clock };
}

const target: LogoTarget = { key: "family:/r/.git", repoPath: "/r" };

describe("createRepoLogoStore", () => {
  it("shows the best candidate that draws and remembers which file it was", async () => {
    const h = harness({
      files: { "/r": ["assets/logo.png", "src-tauri/icons/128x128.png", "README.md"] },
      blobs: { "assets/logo.png": { base64: "bm90IGEgcG5n" }, "src-tauri/icons/128x128.png": { base64: PNG } },
    });
    h.store.request(target);
    await flush();
    const state = get(h.store)[target.key];
    // The logo.png ranks first but is not a PNG; the next candidate wins.
    expect(state).toMatchObject({ status: "ready", path: "src-tauri/icons/128x128.png", source: "auto" });
    expect(state.url).toBe(`data:image/png;base64,${PNG}`);

    // The next launch reads the remembered file and lists nothing.
    const again = harness({ storage: h.storage, blobs: { "src-tauri/icons/128x128.png": { base64: PNG } } });
    again.store.request(target);
    await flush();
    expect(again.list).not.toHaveBeenCalled();
    expect(again.blob).toHaveBeenCalledTimes(1);
    expect(get(again.store)[target.key].status).toBe("ready");
  });

  it("remembers a repository with no logo for a while, then looks again", async () => {
    const h = harness({ files: { "/r": ["src/main.rs"] } });
    h.store.request(target);
    await flush();
    expect(get(h.store)[target.key].status).toBe("none");

    const soon = harness({ storage: h.storage, clock: { now: h.clock.now + 1000 }, files: { "/r": ["logo.svg"] } });
    soon.store.request(target);
    await flush();
    expect(soon.list).not.toHaveBeenCalled();
    expect(get(soon.store)[target.key].status).toBe("none");

    const later = harness({
      storage: h.storage,
      clock: { now: h.clock.now + NO_LOGO_TTL_MS + 1 },
      files: { "/r": ["logo.svg"] },
      blobs: { "logo.svg": { text: "<svg></svg>" } },
    });
    later.store.request(target);
    await flush();
    expect(later.list).toHaveBeenCalledTimes(1);
    expect(get(later.store)[target.key].status).toBe("ready");
  });

  it("calls a refused listing unavailable, never 'no logo', and does not remember it", async () => {
    const h = harness({ listRefused: true });
    h.store.request(target);
    await flush();
    expect(get(h.store)[target.key].status).toBe("unavailable");
    expect(h.storage.data.get(REPO_LOGO_STORAGE_KEY) ?? null).toBeNull();

    // Not asked again until the retry delay has passed.
    h.store.request(target);
    await flush();
    expect(h.list).toHaveBeenCalledTimes(1);
    h.clock.now += UNAVAILABLE_RETRY_MS + 1;
    h.store.request(target);
    await flush();
    expect(h.list).toHaveBeenCalledTimes(2);
  });

  it("backs off a repository that keeps refusing, rather than listing it every minute", async () => {
    const h = harness({ listRefused: true });
    h.store.request(target);
    await flush();
    h.clock.now += UNAVAILABLE_RETRY_MS + 1;
    h.store.request(target);
    await flush();
    expect(h.list).toHaveBeenCalledTimes(2);
    // The second refusal doubles the wait: one minute more is not enough.
    h.clock.now += UNAVAILABLE_RETRY_MS + 1;
    h.store.request(target);
    await flush();
    expect(h.list).toHaveBeenCalledTimes(2);
    h.clock.now += UNAVAILABLE_RETRY_MS;
    h.store.request(target);
    await flush();
    expect(h.list).toHaveBeenCalledTimes(3);
  });

  it("calls every-read-refused unavailable rather than 'no logo'", async () => {
    const h = harness({ files: { "/r": ["logo.png", "icon.png"] }, blobs: { "logo.png": "refuse", "icon.png": "refuse" } });
    h.store.request(target);
    await flush();
    expect(get(h.store)[target.key].status).toBe("unavailable");
    expect(loadRememberedLogos(h.storage).size).toBe(0);
  });

  it("asks nothing for a checkout that cannot be read", async () => {
    const h = harness({ files: { "/r": ["logo.png"] } });
    h.store.request({ ...target, blocked: true });
    await flush();
    expect(h.list).not.toHaveBeenCalled();
    expect(get(h.store)[target.key]).toBeUndefined();
  });

  it("is single-flight per repository and bounded across repositories", async () => {
    let inFlight = 0;
    let peak = 0;
    const list = vi.fn(async () => {
      inFlight++;
      peak = Math.max(peak, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 5));
      inFlight--;
      return [] as string[];
    });
    const store = createRepoLogoStore({ list, blob: vi.fn(), storage: null, concurrency: 2 });
    store.request(target);
    store.request(target);
    for (let i = 0; i < 5; i++) store.request({ key: `path:/r${i}`, repoPath: `/r${i}` });
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(list).toHaveBeenCalledTimes(6);
    expect(peak).toBe(2);
  });

  it("puts a priority request ahead of the queue", async () => {
    const order: string[] = [];
    const list = vi.fn(async (repoPath: string) => {
      order.push(repoPath);
      await new Promise((resolve) => setTimeout(resolve, 2));
      return [] as string[];
    });
    const store = createRepoLogoStore({ list, blob: vi.fn(), storage: null, concurrency: 1 });
    store.request({ key: "a", repoPath: "/a" });
    store.request({ key: "b", repoPath: "/b" });
    store.request({ key: "c", repoPath: "/c" }, { priority: true });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(order).toEqual(["/a", "/c", "/b"]);
  });

  it("keeps a person's choice of initials or a file, and 'automatic' forgets it", async () => {
    const h = harness({
      files: { "/r": ["logo.png", "art/mark.png"] },
      blobs: { "logo.png": { base64: PNG }, "art/mark.png": { base64: PNG } },
    });
    await h.store.choose(target, { kind: "initials" });
    expect(get(h.store)[target.key]).toMatchObject({ status: "none", source: "initials" });
    expect(h.store.choiceFor(target.key)).toEqual({ kind: "initials" });

    await h.store.choose(target, { kind: "file", path: "art/mark.png" });
    expect(get(h.store)[target.key]).toMatchObject({ status: "ready", path: "art/mark.png", source: "file" });
    const reloaded = loadRememberedLogos(h.storage).get(target.key);
    expect(reloaded).toMatchObject({ path: "art/mark.png", choice: "file" });

    await h.store.choose(target, { kind: "auto" });
    expect(get(h.store)[target.key]).toMatchObject({ status: "ready", path: "logo.png", source: "auto" });
    expect(h.store.choiceFor(target.key)).toEqual({ kind: "auto" });
  });

  it("falls back to the automatic pick when a chosen file is gone, without losing the choice", async () => {
    const seed = memoryStorage({
      [REPO_LOGO_STORAGE_KEY]: JSON.stringify([[target.key, { path: "gone.png", at: 1, choice: "file" }]]),
    });
    const h = harness({ storage: seed, files: { "/r": ["logo.png"] }, blobs: { "logo.png": { base64: PNG } } });
    h.store.request(target);
    await flush();
    expect(get(h.store)[target.key]).toMatchObject({ status: "ready", path: "logo.png", source: "auto" });
    expect(h.store.choiceFor(target.key)).toEqual({ kind: "file", path: "gone.png" });
  });

  it("moves to the next candidate when the shown one does not decode", async () => {
    const h = harness({
      files: { "/r": ["logo.png", "icon.png"] },
      blobs: { "logo.png": { base64: PNG }, "icon.png": { base64: PNG } },
    });
    h.store.request(target);
    await flush();
    const first = get(h.store)[target.key];
    expect(first.path).toBe("logo.png");
    h.store.reportBroken(target, first.url!);
    await flush();
    expect(get(h.store)[target.key]).toMatchObject({ status: "ready", path: "icon.png" });
    h.store.reportBroken(target, get(h.store)[target.key].url!);
    await flush();
    expect(get(h.store)[target.key].status).toBe("none");
  });

  it("calls a listing that is not a list unavailable instead of hanging in loading", async () => {
    const list = vi.fn(async () => null as unknown as string[]);
    const store = createRepoLogoStore({ list, blob: vi.fn(), storage: null });
    store.request(target);
    await flush();
    expect(get(store)[target.key].status).toBe("unavailable");
  });

  it("reuses the picker's images when the menu opens again, and asks again after a refusal", async () => {
    const h = harness({ files: { "/r": ["logo.png"] }, blobs: { "logo.png": { base64: PNG } } });
    await h.store.candidates(target);
    await h.store.candidates(target);
    expect(h.list).toHaveBeenCalledTimes(1);

    const refused = harness({ listRefused: true });
    await expect(refused.store.candidates(target)).rejects.toThrow();
    await expect(refused.store.candidates(target)).rejects.toThrow();
    expect(refused.list).toHaveBeenCalledTimes(2);
  });

  it("offers readable images for a person to choose from", async () => {
    const h = harness({
      files: { "/r": ["logo.png", "docs/screenshot.png", "broken.png", "main.rs"] },
      blobs: { "logo.png": { base64: PNG }, "docs/screenshot.png": { base64: PNG }, "broken.png": { base64: "AAAA" } },
    });
    const found = await h.store.candidates(target);
    expect(found.map((c) => c.path)).toEqual(["logo.png", "docs/screenshot.png"]);
  });
});

describe("browsing a repository's images", () => {
  it("lists every image from the same listing the picker reads", async () => {
    const h = harness({ files: { "/r": ["logo.png", "tests/a.png", "main.rs"] }, blobs: { "logo.png": { base64: PNG } } });
    await h.store.candidates(target);
    const list = await h.store.images(target);
    expect(list).toEqual({ paths: ["logo.png", "tests/a.png"], total: 2 });
    expect(h.list).toHaveBeenCalledTimes(1);
  });

  it("rejects a refused or blocked listing rather than reporting no images", async () => {
    const refused = harness({ listRefused: true });
    await expect(refused.store.images(target)).rejects.toThrow();
    await expect(refused.store.images(target)).rejects.toThrow();
    expect(refused.list).toHaveBeenCalledTimes(2);
    const h = harness({ files: { "/r": ["logo.png"] } });
    await expect(h.store.images({ ...target, blocked: true })).rejects.toThrow();
    expect(h.list).not.toHaveBeenCalled();
  });

  it("previews an image once, null for one that is not drawable, and rejects a refused read", async () => {
    const h = harness({
      blobs: { "logo.png": { base64: PNG }, "fake.png": { base64: "AAAA" }, "huge.svg": { text: "x" }, "gone.png": "refuse" },
    });
    expect(await h.store.preview(target, "logo.png")).toMatch(/^data:image\/png;base64,/);
    expect(await h.store.preview(target, "logo.png")).toMatch(/^data:image\/png;base64,/);
    expect(h.blob.mock.calls.filter(([, path]) => path === "logo.png")).toHaveLength(1);
    expect(await h.store.preview(target, "fake.png")).toBeNull();
    expect(await h.store.preview(target, "huge.svg")).toBeNull();
    await expect(h.store.preview(target, "gone.png")).rejects.toThrow();
    // A refusal is not kept: the next look asks again.
    await expect(h.store.preview(target, "gone.png")).rejects.toThrow();
    expect(h.blob.mock.calls.filter(([, path]) => path === "gone.png")).toHaveLength(2);
    expect(await h.store.preview(target, "bad\0path.png")).toBeNull();
  });

  it("reads thumbnails a few at a time and drops ones cancelled before they start", async () => {
    const releases: (() => void)[] = [];
    let running = 0;
    let peak = 0;
    const h = harness();
    h.blob.mockImplementation(async () => {
      running++;
      peak = Math.max(peak, running);
      await new Promise<void>((release) => releases.push(release));
      running--;
      return { base64: PNG };
    });
    const abort = new AbortController();
    const reads = Array.from({ length: 8 }, (_, i) =>
      h.store.preview(target, `i${i}.png`, i >= 5 ? abort.signal : undefined).then(
        (url) => (url ? "ok" : "none"),
        () => "rejected",
      ),
    );
    await flush();
    expect(running).toBe(3);
    abort.abort();
    while (releases.length > 0) {
      releases.shift()?.();
      await flush();
    }
    expect(peak).toBe(3);
    expect(await Promise.all(reads)).toEqual(["ok", "ok", "ok", "ok", "ok", "rejected", "rejected", "rejected"]);
    expect(h.blob).toHaveBeenCalledTimes(5);
  });

  it("does not hold up the tabs' own lookups while thumbnails load", async () => {
    const h = harness({ files: { "/r": ["logo.png"] }, blobs: { "logo.png": { base64: PNG } }, concurrency: 1 });
    const stall: (() => void)[] = [];
    h.blob.mockImplementation(async (_repo: string, path: string) => {
      if (path.startsWith("slow")) await new Promise<void>((release) => stall.push(release));
      return { base64: PNG };
    });
    for (let i = 0; i < 6; i++) void h.store.preview(target, `slow${i}.png`).catch(() => undefined);
    h.store.request(target);
    await flush();
    expect(get(h.store)[target.key]).toMatchObject({ status: "ready", path: "logo.png" });
    stall.forEach((release) => release());
  });

  it("bounds the thumbnails it holds by size and lets a closed browser release them", async () => {
    const big = (n: number) => btoa(String.fromCharCode(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a) + "x".repeat(n));
    // 300 KiB of base64 each: well under the per-file cap, so all draw.
    const blobs: Record<string, LogoBlob> = {};
    for (let i = 0; i < 60; i++) blobs[`shot${i}.png`] = { base64: big(225 * 1024) };
    const h = harness({ blobs });
    for (let i = 0; i < 60; i++) expect(await h.store.preview(target, `shot${i}.png`)).not.toBeNull();
    const held = h.store.previewFootprint();
    expect(held.chars).toBeLessThanOrEqual(MAX_PREVIEW_CHARS);
    expect(held.count).toBeLessThan(60);
    // The newest is kept, the oldest went first.
    h.blob.mockClear();
    await h.store.preview(target, "shot59.png");
    expect(h.blob).not.toHaveBeenCalled();
    await h.store.preview(target, "shot0.png");
    expect(h.blob).toHaveBeenCalledTimes(1);

    const other: LogoTarget = { key: "family:/o/.git", repoPath: "/o" };
    const o = harness({ blobs: { "logo.png": { base64: PNG } } });
    await o.store.preview(target, "logo.png");
    await o.store.preview(other, "logo.png");
    o.store.releasePreviews(target);
    expect(o.store.previewFootprint().count).toBe(1);
    o.blob.mockClear();
    await o.store.preview(other, "logo.png");
    expect(o.blob).not.toHaveBeenCalled();
    await o.store.preview(target, "logo.png");
    expect(o.blob).toHaveBeenCalledTimes(1);
    o.store.releasePreviews(target);
    o.store.releasePreviews(other);
    expect(o.store.previewFootprint()).toEqual({ count: 0, chars: 0 });
  });

  it("shows a file chosen from the browser and keeps it", async () => {
    const h = harness({
      files: { "/r": ["logo.png", "art/mark.png"] },
      blobs: { "logo.png": { base64: PNG }, "art/mark.png": { base64: PNG } },
    });
    await h.store.choose(target, { kind: "file", path: "art/mark.png" });
    expect(get(h.store)[target.key]).toMatchObject({ status: "ready", path: "art/mark.png", source: "file" });
    expect(h.store.choiceFor(target.key)).toEqual({ kind: "file", path: "art/mark.png" });
  });
});

describe("loadRememberedLogos", () => {
  it("drops malformed entries one by one and cannot be steered into a prototype", () => {
    const storage = memoryStorage({
      [REPO_LOGO_STORAGE_KEY]: JSON.stringify([
        ["__proto__", { path: "logo.png", at: 1 }],
        ["ok", { path: "logo.png", at: 2 }],
        ["no-time", { path: "logo.png" }],
        ["file-without-path", { path: null, at: 3, choice: "file" }],
        ["initials", { path: "ignored.png", at: 4, choice: "initials" }],
        "junk",
      ]),
    });
    const loaded = loadRememberedLogos(storage);
    expect([...loaded.keys()]).toEqual(["__proto__", "ok", "initials"]);
    expect(loaded.get("initials")).toEqual({ path: null, at: 4, choice: "initials" });
    expect(({} as Record<string, unknown>).path).toBeUndefined();
  });

  it("reads an object-shaped or unparsable store as empty", () => {
    expect(loadRememberedLogos(memoryStorage({ [REPO_LOGO_STORAGE_KEY]: "{\"a\":1}" })).size).toBe(0);
    expect(loadRememberedLogos(memoryStorage({ [REPO_LOGO_STORAGE_KEY]: "{" })).size).toBe(0);
    expect(loadRememberedLogos(null).size).toBe(0);
  });
});
