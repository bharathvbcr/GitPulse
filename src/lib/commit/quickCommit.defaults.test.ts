import { beforeEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";

const state = writable({
  currentPath: "/r/app",
  generation: 4,
  currentBranch: "main",
  statuses: [{ is_conflicted: false }],
});
const commitDraft = writable("feat: draft");
const fakeRepoStore = Object.assign(
  { subscribe: state.subscribe },
  {
    commitDraft,
    quickCommit: vi.fn(async (_message: string) => ({ ok: true }) as const),
    setAmending: vi.fn(),
    setError: vi.fn(),
  },
);
const askText = vi.fn(async (_options: unknown) => "feat: typed" as string | null);

vi.mock("../stores/repoStore", () => ({ repoStore: fakeRepoStore }));
vi.mock("../stores/modalStore", () => ({ askText: (options: unknown) => askText(options) }));

const { defaultQuickCommitDeps, promptQuickCommit } = await import("./quickCommit");

beforeEach(() => {
  fakeRepoStore.quickCommit.mockClear();
  fakeRepoStore.setAmending.mockClear();
  fakeRepoStore.setError.mockClear();
  askText.mockClear();
  commitDraft.set("feat: draft");
});

describe("the real dependencies behind Quick Commit", () => {
  it("reads the repository state together with the unsent draft", () => {
    const deps = defaultQuickCommitDeps();
    expect(deps.getState()).toMatchObject({
      currentPath: "/r/app",
      generation: 4,
      currentBranch: "main",
      commitDraft: "feat: draft",
    });
  });

  it("routes each side effect to the store that owns it", async () => {
    const deps = defaultQuickCommitDeps();
    await deps.askMessage({
      title: "t",
      message: "m",
      placeholder: "p",
      initialValue: "i",
      confirmLabel: "c",
    });
    expect(askText).toHaveBeenCalledWith(expect.objectContaining({ title: "t", initialValue: "i" }));
    await deps.commitAll("feat: x");
    expect(fakeRepoStore.quickCommit).toHaveBeenCalledWith("feat: x");
    deps.setDraft("draft");
    expect(get(commitDraft)).toBe("draft");
    deps.setAmending(true);
    expect(fakeRepoStore.setAmending).toHaveBeenCalledWith(true);
    deps.setError("boom");
    expect(fakeRepoStore.setError).toHaveBeenCalledWith("boom");
  });

  it("commits what was typed and clears the draft when run end to end on the real wiring", async () => {
    const outcome = await promptQuickCommit();
    expect(outcome).toEqual({ ok: true });
    expect(askText).toHaveBeenCalledWith(expect.objectContaining({ initialValue: "feat: draft" }));
    expect(fakeRepoStore.quickCommit).toHaveBeenCalledWith("feat: typed");
    expect(get(commitDraft)).toBe("");
    expect(fakeRepoStore.setAmending).toHaveBeenCalledWith(false);
  });

  it("leaves the draft alone when the dialog is cancelled", async () => {
    askText.mockResolvedValueOnce(null);
    const outcome = await promptQuickCommit();
    expect(outcome).toEqual({ ok: false });
    expect(fakeRepoStore.quickCommit).not.toHaveBeenCalled();
    expect(get(commitDraft)).toBe("feat: draft");
  });
});
