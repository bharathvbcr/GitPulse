import { describe, expect, it, vi, beforeEach } from "vitest";
import {
  docsRefresh,
  docsStatus,
  docsSearch,
  docsBrokenLinks,
  docsBacklinks,
  docsGraph,
  docsRename,
} from "./client";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

beforeEach(() => invoke.mockReset());

describe("docs client", () => {
  it("invokes refresh / status / search / broken / backlinks / graph / rename", async () => {
    invoke.mockResolvedValue({});
    await docsRefresh("/r");
    expect(invoke).toHaveBeenCalledWith("cmd_docs_refresh", { repoPath: "/r" });

    await docsStatus("/r");
    expect(invoke).toHaveBeenCalledWith("cmd_docs_status", { repoPath: "/r" });

    await docsSearch("/r", "q", 10);
    expect(invoke).toHaveBeenCalledWith("cmd_docs_search", {
      repoPath: "/r",
      query: "q",
      limit: 10,
    });

    await docsBrokenLinks("/r");
    expect(invoke).toHaveBeenCalledWith("cmd_docs_broken_links", { repoPath: "/r" });

    await docsBacklinks("/r", "docs/a.md");
    expect(invoke).toHaveBeenCalledWith("cmd_docs_backlinks", {
      repoPath: "/r",
      path: "docs/a.md",
    });

    await docsGraph("/r", { focus: "docs/a.md", depth: 2 });
    expect(invoke).toHaveBeenCalledWith("cmd_docs_graph", {
      repoPath: "/r",
      focus: "docs/a.md",
      depth: 2,
      tag: null,
      folder: null,
    });

    await docsRename("/r", "a.md", "b.md");
    expect(invoke).toHaveBeenCalledWith("cmd_docs_rename", {
      repoPath: "/r",
      from: "a.md",
      to: "b.md",
    });
  });
});
