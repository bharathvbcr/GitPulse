import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { previewDevmapEdits } from "./client";
import { CODEINTEL_FANOUT_CAP } from "./fanout";
import { previewStore } from "./previewStore";
import type { DevmapPreviewOutcome } from "./types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./client", () => ({ previewDevmapEdits: vi.fn() }));

function emptyOutcome(overrides: Partial<DevmapPreviewOutcome> = {}): DevmapPreviewOutcome {
  return {
    available: true,
    binary: "/bin/devmap",
    lookup: "path_search",
    reason: null,
    files: [],
    cancelled: false,
    ...overrides,
  };
}

describe("previewStore.refresh", () => {
  beforeEach(() => {
    previewStore.reset();
    vi.mocked(invoke).mockReset();
    vi.mocked(previewDevmapEdits).mockReset();
    vi.mocked(invoke).mockResolvedValue("fn main() {}\n");
    vi.mocked(previewDevmapEdits).mockResolvedValue(emptyOutcome());
  });

  afterEach(() => {
    previewStore.reset();
  });

  it("does not re-read or re-preview when the same repo and path key are already loaded", async () => {
    const paths = ["src/a.ts", "src/b.ts"];
    await previewStore.refresh("/repo", paths);
    await previewStore.refresh("/repo", [...paths].reverse());
    expect(invoke).toHaveBeenCalledTimes(paths.length);
    expect(previewDevmapEdits).toHaveBeenCalledTimes(1);
  });

  it("does not start a second in-flight preview for the same key", async () => {
    let release!: (value: string) => void;
    vi.mocked(invoke).mockImplementation(
      () => new Promise((resolve) => { release = resolve; }),
    );
    const first = previewStore.refresh("/repo", ["src/a.ts"]);
    await Promise.resolve();
    expect(get(previewStore).loading).toBe(true);
    const second = previewStore.refresh("/repo", ["src/a.ts"]);
    expect(invoke).toHaveBeenCalledTimes(1);
    release("fn main() {}\n");
    await first;
    await second;
  });

  it("caps working-tree reads and reports omitted files instead of walking the whole staged set", async () => {
    const paths = Array.from({ length: CODEINTEL_FANOUT_CAP + 4 }, (_, i) => `src/f${i}.ts`);
    await previewStore.refresh("/repo", paths);
    expect(invoke).toHaveBeenCalledTimes(CODEINTEL_FANOUT_CAP);
    const state = get(previewStore);
    expect(state.outcome?.truncated).toBe(true);
    expect(state.outcome?.files_omitted).toBe(4);
    expect(state.outcome?.files_total).toBe(paths.length);
    const omitted = state.files.filter((file) => !file.available);
    expect(omitted).toHaveLength(4);
    expect(omitted.every((file) => (file.reason ?? "").includes("fan-out capped"))).toBe(true);
  });
});
