import { describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { readFileSync } from "node:fs";

const wave = vi.hoisted(() => ({ record: null as unknown }));
vi.mock("../workbench/waveHost", () => ({
  beginWave: vi.fn(), resumeActiveWave: vi.fn(async () => ({ runner: null, complete: true })),
  currentWave: () => (wave.record ? { record: () => wave.record, finished: new Promise(() => {}), cancel: async () => {} } : null),
}));
const { default: WavePanel } = await import("./WavePanel.svelte");

const html = (repositoryId: string | null) => render(WavePanel, { props: { repositoryId } }).body;
const running = (members: unknown[], over: Record<string, unknown> = {}) => ({ v: 1, id: "w", repositoryId: "repo-1", n: 3, batchesLeft: 1, batch: 1, state: "running", members, attempted: [], note: "", seq: 1, ...over });
const member = (phase: string, detail = "") => ({ taskId: "a", title: "Fix the crash", phase, runId: "r", requestId: "q", cwd: null, head: null, detail, startedAt: 1 });

describe("WavePanel", () => {
  it("offers Start with the default size, bounded, and says why it is off without a repository board", () => {
    wave.record = null;
    const on = html("repo-1");
    expect(on).toContain('data-testid="wave-start"');
    expect(on).toMatch(/data-testid="wave-size"[^>]*max="5"|max="5"[^>]*data-testid="wave-size"/);
    expect(on).not.toMatch(/data-testid="wave-start"[^>]*disabled/);
    const off = html(null);
    expect(off).toMatch(/disabled[^>]*data-testid="wave-start"|data-testid="wave-start"[^>]*disabled/);
    expect(off).toContain("Choose a repository board");
  });

  it("shows Stop, not Start, while a wave runs, and says nothing is pushed", () => {
    wave.record = running([member("running")]);
    const body = html("repo-1");
    expect(body).toContain('data-testid="wave-stop"');
    expect(body).not.toContain('data-testid="wave-start"');
    expect(body).toContain("Nothing is pushed.");
  });

  it("shows a held task with its reason, so a degraded verdict or a conflict is never hidden", () => {
    wave.record = running([member("held", "Verification degraded; this is not a pass, so it is left for review.")], { state: "done" });
    const body = html("repo-1");
    expect(body).toContain('data-phase="held"');
    expect(body).toContain("Held for review");
    expect(body).toContain("this is not a pass");
  });

  it("names the repository it is for, and never shows another repository's wave as this one's", () => {
    wave.record = running([member("running")], { repositoryId: "repo-other" });
    const body = html("repo-1");
    expect(body).toContain('data-testid="wave-repo"');
    expect(body).toContain("in repo-1");
    expect(body).toContain('data-testid="wave-foreign"');
    expect(body).toMatch(/running in repo-other, not in repo-1/);
    expect(body).not.toContain('data-testid="wave-member"');
    expect(body).not.toContain('data-testid="wave-state"');
    // Its own repository still shows it plainly.
    expect(html("repo-other")).toContain('data-testid="wave-member"');
    // With no repository board chosen, nothing is claimed for a wave elsewhere.
    expect(html(null)).toContain('data-testid="wave-foreign"');
  });

  it("shows why a wave refused to start from a partial board, and offers the unchecked-ranking opt-in", () => {
    wave.record = running([], { state: "refused", batch: 0, note: "Nothing was launched: 385 open tasks' links were not read, so a blocker may be among what was not read." });
    const body = html("repo-1");
    expect(body).toContain("Not started");
    expect(body).toMatch(/385 open tasks(&#39;|') links were not read/);
    expect(body).toContain('data-testid="wave-accept-unchecked"');
    expect(body).not.toMatch(/data-testid="wave-accept-unchecked"[^>]*checked/);
  });

  it("is mounted on the board beside Suggest next, scoped to the repository board only", () => {
    const board = readFileSync(new URL("./TaskBoard.svelte", import.meta.url), "utf8");
    expect(board).toContain('<WavePanel repositoryId={scope.kind === "repository" ? scope.id : null} />');
  });
});
