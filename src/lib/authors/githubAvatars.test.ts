import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { diagnostics } from "../diagnostics/diagnostics";
import { interfaceStore } from "../stores/interfaceStore";
import {
  GITHUB_AUTHOR_AVATARS_COMMAND,
  LOOKUP_RETRY_AFTER_MS,
  MAX_CACHED_AVATARS,
  MAX_EMAILS_PER_REQUEST,
  avatarUrlFor,
  githubAvatarsVersion,
  noreplyAvatarUrl,
  parseAuthorAvatarReport,
  requestAuthorAvatars,
  resetGithubAvatarsForTests,
  type AuthorAvatarReport,
} from "./githubAvatars";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const mocked = vi.mocked(invoke);
const REPO = "/work/repo";
const oid = (n: number) => n.toString(16).padStart(40, "0");

function report(over: Partial<AuthorAvatarReport> = {}): AuthorAvatarReport {
  return { available: true, avatars: [], no_account: [], unresolved: [], truncated: false, error: null, ...over };
}

let remotes: unknown;
let avatarHandler: (args: { repoPath: string; authors: { email: string; oids: string[] }[] }) => unknown;

function avatarCalls() {
  return mocked.mock.calls.filter((c) => c[0] === GITHUB_AUTHOR_AVATARS_COMMAND);
}

beforeEach(() => {
  resetGithubAvatarsForTests();
  interfaceStore.reset();
  mocked.mockReset();
  remotes = {
    remotes: [{ name: "origin", fetch_url: "https://github.com/acme/repo.git", push_url: null, has_credentials: false, tracking_branches: 1, is_default: true }],
    truncated: false,
  };
  avatarHandler = () => report();
  mocked.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "cmd_list_remotes") return remotes;
    if (cmd === GITHUB_AUTHOR_AVATARS_COMMAND) return avatarHandler(args as never);
    throw new Error(`unexpected command ${cmd}`);
  });
});

describe("noreplyAvatarUrl", () => {
  it("resolves the id form by numeric id", () => {
    expect(noreplyAvatarUrl("12345+octocat@users.noreply.github.com")).toBe(
      "https://avatars.githubusercontent.com/u/12345?s=64&v=4",
    );
  });
  it("resolves the login form by name, domain case-insensitively", () => {
    expect(noreplyAvatarUrl("octo-cat@Users.NoReply.GitHub.com")).toBe(
      "https://avatars.githubusercontent.com/octo-cat?s=64",
    );
  });
  it("accepts bots", () => {
    expect(noreplyAvatarUrl("41898282+github-actions[bot]@users.noreply.github.com")).toBe(
      "https://avatars.githubusercontent.com/u/41898282?s=64&v=4",
    );
    expect(noreplyAvatarUrl("dependabot[bot]@users.noreply.github.com")).toBe(
      "https://avatars.githubusercontent.com/dependabot%5Bbot%5D?s=64",
    );
  });
  it("rejects other domains and junk logins", () => {
    expect(noreplyAvatarUrl("octocat@users.noreply.gitlab.com")).toBeNull();
    expect(noreplyAvatarUrl("octocat@example.com")).toBeNull();
    expect(noreplyAvatarUrl("evil/../x@users.noreply.github.com")).toBeNull();
    expect(noreplyAvatarUrl("a b@users.noreply.github.com")).toBeNull();
    expect(noreplyAvatarUrl("-lead@users.noreply.github.com")).toBeNull();
    expect(noreplyAvatarUrl("x?y=1@users.noreply.github.com")).toBeNull();
    expect(noreplyAvatarUrl("evil@users.noreply.github.com.attacker.io")).toBeNull();
    expect(noreplyAvatarUrl("")).toBeNull();
  });
});

describe("parseAuthorAvatarReport", () => {
  it("accepts a well-formed report", () => {
    const r = report({ avatars: [{ email: "a@b.c", login: "a", avatar_url: "https://avatars.githubusercontent.com/u/1" }] });
    expect(parseAuthorAvatarReport(r)).toEqual(r);
  });
  it("fails closed to unavailable on anything malformed", () => {
    for (const bad of [null, [], "x", {}, { ...report(), available: "yes" }, { ...report(), avatars: [{ email: 1 }] }, { ...report(), no_account: [1] }, { ...report(), error: 5 }]) {
      const parsed = parseAuthorAvatarReport(bad);
      expect(parsed.available).toBe(false);
      expect(parsed.avatars).toEqual([]);
    }
  });
});

describe("requestAuthorAvatars", () => {
  it("prefers the oldest loaded commits, at most three per email", async () => {
    // Rows are newest first: index 0 is newest.
    const rows = [1, 2, 3, 4, 5].map((n) => ({ id: oid(n), author_email: "Ada@Example.com " }));
    await requestAuthorAvatars(REPO, rows);
    const [, args] = avatarCalls()[0];
    expect(args).toEqual({ repoPath: REPO, authors: [{ email: "ada@example.com", oids: [oid(5), oid(4), oid(3)] }] });
  });

  it("skips noreply emails and sends nothing when nothing remains", async () => {
    await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "1+x@users.noreply.github.com" }]);
    expect(avatarCalls()).toHaveLength(0);
  });

  it("caps one call at 150 emails and asks for the rest in a follow-up", async () => {
    const rows = Array.from({ length: 160 }, (_, i) => ({ id: oid(i + 1), author_email: `u${i}@example.com` }));
    await requestAuthorAvatars(REPO, rows);
    const calls = avatarCalls();
    expect(calls).toHaveLength(2);
    expect((calls[0][1] as { authors: unknown[] }).authors).toHaveLength(MAX_EMAILS_PER_REQUEST);
    expect((calls[1][1] as { authors: unknown[] }).authors).toHaveLength(10);
  });

  it("asks each email once per repo per session, negatives included", async () => {
    avatarHandler = () => report({ no_account: ["ada@example.com"] });
    const rows = [{ id: oid(1), author_email: "ada@example.com" }];
    await requestAuthorAvatars(REPO, rows);
    await requestAuthorAvatars(REPO, rows);
    expect(avatarCalls()).toHaveLength(1);
  });

  it("does nothing while the setting is off, and asks once it is on", async () => {
    interfaceStore.setShowGithubAvatars(false);
    const rows = [{ id: oid(1), author_email: "ada@example.com" }];
    await requestAuthorAvatars(REPO, rows);
    expect(mocked).not.toHaveBeenCalled();
    interfaceStore.setShowGithubAvatars(true);
    await requestAuthorAvatars(REPO, rows);
    expect(avatarCalls()).toHaveLength(1);
  });

  it("never touches the lookup for a repository without a GitHub remote", async () => {
    remotes = { remotes: [{ name: "origin", fetch_url: "https://gitlab.com/a/b.git", push_url: null, has_credentials: false, tracking_branches: 0, is_default: true }], truncated: false };
    await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "ada@example.com" }]);
    expect(avatarCalls()).toHaveLength(0);
  });

  it("stops asking for a repo once a report says unavailable", async () => {
    avatarHandler = () => report({ available: false, error: "gh missing" });
    await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }]);
    await requestAuthorAvatars(REPO, [{ id: oid(2), author_email: "b@example.com" }]);
    expect(avatarCalls()).toHaveLength(1);
  });

  it("asks again, about the same authors, once the retry pause has passed", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000_000);
    try {
      avatarHandler = () => report({ available: false, error: "timed out" });
      await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }]);
      now.mockReturnValue(1_000_000 + LOOKUP_RETRY_AFTER_MS - 1);
      await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }]);
      expect(avatarCalls()).toHaveLength(1);
      now.mockReturnValue(1_000_000 + LOOKUP_RETRY_AFTER_MS);
      avatarHandler = () =>
        report({ avatars: [{ email: "a@example.com", login: "a", avatar_url: "https://avatars.githubusercontent.com/u/1?s=64" }] });
      await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }]);
      expect(avatarCalls()).toHaveLength(2);
      expect(avatarUrlFor("a@example.com")).toBe("https://avatars.githubusercontent.com/u/1?s=64");
    } finally {
      now.mockRestore();
    }
  });

  it("writes a failure to Diagnostics once per repository and reason", async () => {
    const warn = vi.spyOn(diagnostics, "warn");
    const now = vi.spyOn(Date, "now").mockReturnValue(5_000_000);
    try {
      avatarHandler = () => report({ available: false, error: "GitHub CLI is not authenticated" });
      await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }]);
      now.mockReturnValue(5_000_000 + LOOKUP_RETRY_AFTER_MS);
      await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }]);
      const mine = warn.mock.calls.filter((c) => c[0] === "avatars");
      expect(mine).toHaveLength(1);
      expect(String(mine[0][1])).toContain("GitHub CLI is not authenticated");
    } finally {
      now.mockRestore();
      warn.mockRestore();
    }
  });

  it("reports a partial lookup's error while keeping what resolved", async () => {
    const warn = vi.spyOn(diagnostics, "warn");
    try {
      avatarHandler = () =>
        report({
          avatars: [{ email: "a@example.com", login: "a", avatar_url: "https://avatars.githubusercontent.com/u/1?s=64" }],
          error: "Avatar lookup was partial: HTTP 502",
        });
      await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }]);
      expect(avatarUrlFor("a@example.com")).not.toBeNull();
      expect(warn.mock.calls.some((c) => c[0] === "avatars" && String(c[1]).includes("HTTP 502"))).toBe(true);
    } finally {
      warn.mockRestore();
    }
  });

  it("treats a thrown invoke like unavailable and never throws", async () => {
    avatarHandler = () => {
      throw new Error("boom");
    };
    await expect(requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }])).resolves.toBeUndefined();
    await requestAuthorAvatars(REPO, [{ id: oid(2), author_email: "b@example.com" }]);
    expect(avatarCalls()).toHaveLength(1);
  });

  it("queues one follow-up carrying the union while a request is in flight", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    let first = true;
    avatarHandler = () => {
      if (first) {
        first = false;
        return gate.then(() => report());
      }
      return report();
    };
    const p1 = requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }]);
    await vi.waitFor(() => expect(avatarCalls()).toHaveLength(1));
    const p2 = requestAuthorAvatars(REPO, [{ id: oid(2), author_email: "b@example.com" }]);
    const p3 = requestAuthorAvatars(REPO, [{ id: oid(3), author_email: "c@example.com" }, { id: oid(4), author_email: "b@example.com" }]);
    await Promise.all([p2, p3]);
    expect(avatarCalls()).toHaveLength(1);
    release();
    await p1;
    const calls = avatarCalls();
    expect(calls).toHaveLength(2);
    const followUp = (calls[1][1] as { authors: { email: string; oids: string[] }[] }).authors;
    expect(followUp.map((a) => a.email).sort()).toEqual(["b@example.com", "c@example.com"]);
    expect(followUp.find((a) => a.email === "b@example.com")?.oids.sort()).toEqual([oid(2), oid(4)].sort());
  });

  it("caches results for avatarUrlFor and bumps the version", async () => {
    avatarHandler = () =>
      report({ avatars: [{ email: "ada@example.com", login: "ada", avatar_url: "https://avatars.githubusercontent.com/u/9?v=4" }] });
    const before = get(githubAvatarsVersion);
    expect(avatarUrlFor("ada@example.com")).toBeNull();
    await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "ada@example.com" }]);
    expect(avatarUrlFor("ADA@example.com")).toBe("https://avatars.githubusercontent.com/u/9?v=4&s=64");
    expect(get(githubAvatarsVersion)).toBeGreaterThan(before);
    // Global across repositories: no second lookup elsewhere.
    await requestAuthorAvatars("/work/other", [{ id: oid(2), author_email: "ada@example.com" }]);
    expect(avatarCalls()).toHaveLength(1);
  });

  it("refuses URLs outside GitHub's avatar CDN", async () => {
    avatarHandler = () =>
      report({
        avatars: [
          { email: "a@example.com", login: "a", avatar_url: "https://evil.example/x.png" },
          { email: "b@example.com", login: "b", avatar_url: "http://avatars.githubusercontent.com/u/1" },
        ],
      });
    await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "a@example.com" }, { id: oid(2), author_email: "b@example.com" }]);
    expect(avatarUrlFor("a@example.com")).toBeNull();
    expect(avatarUrlFor("b@example.com")).toBeNull();
  });

  it("avatarUrlFor is null with the setting off, even for noreply and cached emails", async () => {
    avatarHandler = () =>
      report({ avatars: [{ email: "ada@example.com", login: "ada", avatar_url: "https://avatars.githubusercontent.com/u/9" }] });
    await requestAuthorAvatars(REPO, [{ id: oid(1), author_email: "ada@example.com" }]);
    expect(avatarUrlFor("ada@example.com")).not.toBeNull();
    expect(avatarUrlFor("1+x@users.noreply.github.com")).not.toBeNull();
    interfaceStore.setShowGithubAvatars(false);
    expect(avatarUrlFor("ada@example.com")).toBeNull();
    expect(avatarUrlFor("1+x@users.noreply.github.com")).toBeNull();
  });

  it("bounds the positive cache", async () => {
    const total = MAX_CACHED_AVATARS + 20;
    for (let start = 0; start < total; start += MAX_EMAILS_PER_REQUEST) {
      avatarHandler = ({ authors }) =>
        report({ avatars: authors.map((a) => ({ email: a.email, login: "x", avatar_url: "https://avatars.githubusercontent.com/u/1" })) });
      const rows = Array.from({ length: Math.min(MAX_EMAILS_PER_REQUEST, total - start) }, (_, i) => ({
        id: oid(start + i + 1),
        author_email: `u${start + i}@example.com`,
      }));
      await requestAuthorAvatars(REPO, rows);
    }
    const present = Array.from({ length: total }, (_, i) => avatarUrlFor(`u${i}@example.com`)).filter(Boolean).length;
    expect(present).toBe(MAX_CACHED_AVATARS);
    // The newest batch survives; the oldest insertions were evicted.
    expect(avatarUrlFor(`u${total - 1}@example.com`)).not.toBeNull();
  });
});
