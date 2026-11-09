import { readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { filesUnder } from "../stores/sourceScan";
import { writable } from "svelte/store";
import { createRepoListing, MAX_CACHED_LISTINGS, releaseListingsOfClosedTabs } from "./repoListing";

function rig(initialKey: string | null = "k1") {
  const calls: string[] = [];
  const state = { key: initialKey as string | null, fail: false, files: ["a.ts", "b.ts"] };
  const listing = createRepoListing({
    fetch: async (repoPath) => {
      calls.push(repoPath);
      if (state.fail) throw new Error("listing refused");
      return state.files;
    },
    keyFor: () => state.key,
  });
  return { listing, calls, state };
}

describe("repository file listing owner", () => {
  it("serves a held listing without asking the backend again", async () => {
    const { listing, calls } = rig();
    const first = await listing.list("/r/a");
    expect(listing.peek("/r/a")).toBe(first);
    expect(await listing.list("/r/a")).toBe(first);
    expect(calls).toEqual(["/r/a"]);
  });

  it("joins a caller that asks while the read is in flight", async () => {
    const { listing, calls } = rig();
    const [one, two] = await Promise.all([listing.list("/r/a"), listing.list("/r/a")]);
    expect(one).toBe(two);
    expect(calls).toHaveLength(1);
  });

  it("reads again when what the listing depends on changed, and not before", async () => {
    const { listing, calls, state } = rig();
    await listing.list("/r/a");
    state.key = "k2";
    expect(listing.peek("/r/a")).toBeNull();
    state.files = ["a.ts", "b.ts", "new.ts"];
    expect(await listing.list("/r/a")).toEqual(["a.ts", "b.ts", "new.ts"]);
    expect(calls).toHaveLength(2);
  });

  it("never keeps a failed read: the error reaches the caller and the next ask reads again", async () => {
    const { listing, calls, state } = rig();
    state.fail = true;
    await expect(listing.list("/r/a")).rejects.toThrow("listing refused");
    expect(listing.peek("/r/a")).toBeNull();
    state.fail = false;
    expect(await listing.list("/r/a")).toEqual(["a.ts", "b.ts"]);
    expect(calls).toHaveLength(2);
  });

  it("force reads again under the same key", async () => {
    const { listing, calls } = rig();
    await listing.list("/r/a");
    await listing.list("/r/a", { force: true });
    expect(calls).toHaveLength(2);
  });

  it("does not hold a repository it cannot key", async () => {
    const { listing, calls } = rig(null);
    await listing.list("/r/other");
    await listing.list("/r/other");
    expect(calls).toHaveLength(2);
    expect(listing.peek("/r/other")).toBeNull();
  });

  it("invalidate forgets one repository or all", async () => {
    const { listing } = rig();
    await listing.list("/r/a");
    await listing.list("/r/b");
    listing.invalidate("/r/a");
    expect(listing.peek("/r/a")).toBeNull();
    expect(listing.peek("/r/b")).not.toBeNull();
    listing.invalidate();
    expect(listing.peek("/r/b")).toBeNull();
  });

  it("holds at most MAX_CACHED_LISTINGS repositories, dropping the oldest", async () => {
    const { listing } = rig();
    const repos = Array.from({ length: MAX_CACHED_LISTINGS + 1 }, (_, i) => `/r/${i}`);
    for (const repo of repos) await listing.list(repo);
    expect(listing.peek(repos[0])).toBeNull();
    expect(listing.peek(repos[repos.length - 1])).not.toBeNull();
  });
});

describe("listings of closed tabs", () => {
  it("are dropped when the tab closes; open tabs keep theirs", async () => {
    const { listing } = rig();
    const tabs = writable({ openTabs: [{ path: "/r/a" }, { path: "/r/b" }] });
    const stop = releaseListingsOfClosedTabs(listing, tabs);
    await listing.list("/r/a");
    await listing.list("/r/b");
    tabs.set({ openTabs: [{ path: "/r/a" }] });
    expect(listing.peek("/r/b")).toBeNull();
    expect(listing.peek("/r/a")).not.toBeNull();
    stop();
  });

  it("are left alone by a publish that did not change the tab list", async () => {
    const { listing } = rig();
    const openTabs = [{ path: "/r/a" }];
    const tabs = writable({ openTabs });
    const stop = releaseListingsOfClosedTabs(listing, tabs);
    await listing.list("/r/gone");
    tabs.set({ openTabs });
    expect(listing.peek("/r/gone")).not.toBeNull();
    stop();
  });
});

describe("one owner for cmd_list_repo_files", () => {
  it("no other source file asks the backend for the listing", () => {
    const src = join(dirname(fileURLToPath(import.meta.url)), "..");
    const askers = filesUnder(src, (name) => (name.endsWith(".ts") || name.endsWith(".svelte")) && !name.endsWith(".test.ts"))
      .filter((file) => readFileSync(file, "utf8").includes("cmd_list_repo_files"))
      .map((file) => relative(src, file));
    expect(askers).toEqual([join("files", "repoListing.ts")]);
  });
});
