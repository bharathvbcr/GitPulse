import { describe, expect, it } from "vitest";
import { planeTerminals, repoAgentPill } from "./repoAttention";
import { listing, PATHS, probe, project, snapshot, terminal, worktree } from "./__tests__/fixtures";

const pill = (
  repoPath: string,
  fields: Parameters<typeof project>[0] = {},
) => {
  const plane = project(fields);
  return repoAgentPill({ plane, probes: fields.probes ?? [probe()], repoPath, paths: PATHS });
};

describe("repoAgentPill", () => {
  it("names a repository whose agent needs the reader, with the most urgent reason first", () => {
    const answer = pill("/repo", {
      terminals: [
        terminal({ key: "a", sessionId: "a", attention: "finished" }),
        terminal({ key: "b", sessionId: "b", attention: "needs-you" }),
      ],
    });
    expect(answer.state).toBe("attention");
    expect(answer.reason).toBe("needs-you");
    expect(answer.tone).toBe("warn");
    expect(answer.live).toBe(2);
  });

  it("ranks an error above a finished run and paints it as danger", () => {
    const answer = pill("/repo", {
      terminals: [
        terminal({ key: "a", sessionId: "a", attention: "finished" }),
        terminal({ key: "b", sessionId: "b", attention: "error" }),
      ],
    });
    expect(answer.reason).toBe("error");
    expect(answer.tone).toBe("danger");
  });

  it("does not count another repository's agents", () => {
    const answer = pill("/repo", {
      terminals: [terminal({ key: "x", repoPath: "/other", sessionId: "x", attention: "needs-you" })],
    });
    expect(answer.state).not.toBe("attention");
    expect(answer.live).toBe(0);
  });

  it("does not mark a repository with only finished or informational agents as attention", () => {
    const answer = pill("/repo", {
      terminals: [
        terminal({
          key: "a",
          sessionId: "a",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "turn_finished",
        }),
      ],
    });
    expect(answer.state).toBe("active");
    expect(answer.reason).toBeNull();
    expect(answer.tone).toBeNull();
    expect(answer.live).toBe(1);
  });

  it("is active, with no reason, when agents run and none asks", () => {
    const answer = pill("/repo", { terminals: [terminal({ attention: null })] });
    expect(answer).toMatchObject({ state: "active", live: 1, reason: null, tone: null });
  });

  it("reports active with live count for checkout-attached live terminals carrying informational or no attention", () => {
    const probes = [probe({
      snapshot: snapshot({
        worktrees: listing([
          worktree({ path: "/repo/.claude/worktrees/alpha", name: "alpha", session_slug: "alpha" }),
          worktree({ path: "/repo/.claude/worktrees/beta", name: "beta", session_slug: "beta" }),
        ]),
      }),
    })];
    const answer = pill("/repo", {
      probes,
      terminals: [
        terminal({
          key: "a",
          sessionId: "a",
          cwd: "/repo/.claude/worktrees/alpha",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "turn_finished",
        }),
        terminal({
          key: "b",
          sessionId: "b",
          cwd: "/repo/.claude/worktrees/beta",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "idle_prompt",
        }),
      ],
    });
    expect(answer).toMatchObject({
      state: "active",
      live: 2,
      reason: null,
      tone: null,
      phrase: "Running",
    });

    const quiet = pill("/repo", {
      probes,
      terminals: [
        terminal({ key: "a", sessionId: "a", cwd: "/repo/.claude/worktrees/alpha", attention: null }),
        terminal({ key: "b", sessionId: "b", cwd: "/repo/.claude/worktrees/beta", attention: null }),
      ],
    });
    expect(quiet).toMatchObject({
      state: "active",
      live: 2,
      reason: null,
      tone: null,
      phrase: "Running",
    });
  });

  it("is none, not unknown, for a repository the sweep read and found quiet", () => {
    expect(pill("/repo").state).toBe("none");
  });

  it("is unknown, never none, when the plane has not read the repository", () => {
    const plane = project({ probes: [] });
    const answer = repoAgentPill({ plane, probes: [], repoPath: "/repo", paths: PATHS });
    expect(answer.state).toBe("unknown");
    expect(answer.phrase).toBe("Not read");
  });

  it("is unknown when the read of this repository failed or was skipped", () => {
    for (const fields of [{ error: "boom", snapshot: null }, { skipped: true, skipReason: "deadline" as const }]) {
      const probes = [probe(fields)];
      const plane = project({ probes });
      expect(repoAgentPill({ plane, probes, repoPath: "/repo", paths: PATHS }).state).toBe("unknown");
    }
  });
});

describe("planeTerminals", () => {
  const record = { key: "k", repoPath: "/repo", label: "Claude", status: "running", sessionId: "s1" };

  it("takes the attention kind and rich metadata from the session's own activity", () => {
    const rows = planeTerminals(
      [{ ...record, launcher: "agy" }],
      new Map([
        [
          "s1",
          {
            title: "Fix it",
            fullyIdle: true,
            providerSessionId: "11111111-2222-4333-8444-555555555555",
            attention: {
              kind: "needs-you" as const,
              urgency: "blocking" as const,
              event: "permission_prompt",
              channel: "hook",
              label: "Needs your permission",
              detail: "Allow Bash: cargo test",
              at: 12_345,
            },
          },
        ],
      ]),
      new Map(),
    );
    expect(rows[0]).toMatchObject({
      title: "Fix it",
      launcher: "agy",
      attention: "needs-you",
      attentionUrgency: "blocking",
      attentionEvent: "permission_prompt",
      attentionChannel: "hook",
      attentionLabel: "Needs your permission",
      attentionDetail: "Allow Bash: cargo test",
      attentionAt: 12_345,
      fullyIdle: true,
      providerSessionId: "11111111-2222-4333-8444-555555555555",
    });
    expect((rows[0] as unknown as Record<string, unknown>).lastOutputAt).toBeUndefined();
  });

  it("supports minimal legacy activity fixtures without optional metadata", () => {
    const rows = planeTerminals(
      [record],
      new Map([["s1", { title: "Fix it", attention: { kind: "needs-you" as const } }]]),
      new Map(),
    );
    expect(rows[0]).toMatchObject({ title: "Fix it", attention: "needs-you", launcher: null, attentionDetail: null });
  });

  it("keeps an unread directory unknown instead of filling in the repository root", () => {
    expect(planeTerminals([record], new Map(), new Map())[0]?.cwd).toBeNull();
    expect(planeTerminals([record], new Map(), new Map([["s1", "/repo/wt"]]))[0]?.cwd).toBe("/repo/wt");
  });

  it("reads a record with no backend session as having no activity", () => {
    const rows = planeTerminals([{ ...record, sessionId: undefined }], new Map(), new Map());
    expect(rows[0]).toMatchObject({ sessionId: "", attention: null, cwd: null });
  });
});
