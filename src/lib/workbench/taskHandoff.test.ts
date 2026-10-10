import { afterEach, describe, expect, it } from "vitest";
import type { RunPreparation } from "./client";
import {
  MAX_CHECKOUT_LENGTH,
  checkoutBesideCommonDir,
  checkoutCandidates,
  defaultHandoff,
  describeHandoff,
  handoffGate,
  holdPendingPreparation,
  MAX_PENDING_PREPARATIONS,
  pendingPreparationFor,
  pendingPreparationsChanged,
  releasePendingPreparation,
  isAgentProvider,
  launchModelOverride,
  modelChoiceLabel,
  modelOverrideChoices,
  modelOverrideFields,
  NO_MODEL_OVERRIDE,
  normalizeCheckout,
  preferredCheckout,
  reconcileHandoff,
  attemptHolding,
  canRelease,
  runExpired,
  runHoldsCheckout,
  runStatusLabel,
  runStateLabel,
  sanitizeHandoff,
  supportsManaged,
} from "./taskHandoff";
import { wallClockBudget } from "../testing/wallClock";

const options = { caseInsensitive: false };
const repositories = [
  { id: "r0", identity_key: "local:/work/GitPulse/.git" },
  { id: "r1", identity_key: "local:/work/bare.git" },
  { id: "r2", identity_key: "weird:not-a-path" },
];

describe("sanitizeHandoff", () => {
  it("falls back to the default for anything unrecognizable", () => {
    expect(sanitizeHandoff(undefined)).toEqual(defaultHandoff());
    expect(sanitizeHandoff(null)).toEqual(defaultHandoff());
    expect(sanitizeHandoff("codex")).toEqual(defaultHandoff());
    expect(sanitizeHandoff({ provider: "gemini", kind: "orbital", permission: "yolo" })).toEqual(defaultHandoff());
  });

  it("restores a valid remembered handoff", () => {
    expect(sanitizeHandoff({ provider: "claude", kind: "external_terminal", permission: "edit" }))
      .toEqual({ provider: "claude", kind: "external_terminal", permission: "edit" });
    expect(sanitizeHandoff({ provider: "codex", kind: "managed", permission: "inspect" }))
      .toEqual({ provider: "codex", kind: "managed", permission: "inspect" });
  });

  it("never restores bypass, whatever was stored", () => {
    // Bypass turns off the agent's permission checks and its sandbox. A
    // preference must not be able to widen access on the next launch.
    expect(sanitizeHandoff({ provider: "codex", kind: "managed", permission: "bypass" }).permission).toBe("ask");
    expect(sanitizeHandoff({ permission: "bypass" }).permission).toBe("ask");
  });

  it("refuses a managed connection for a provider that has none", () => {
    // Grok has no managed adapter, so a stored `{grok, managed}` must come
    // back as a terminal handoff rather than a launch that cannot succeed.
    expect(sanitizeHandoff({ provider: "grok", kind: "managed", permission: "ask" }).kind)
      .toBe("external_terminal");
    expect(supportsManaged("grok")).toBe(false);
    expect(supportsManaged("agy")).toBe(false);
    // Both adapters that exist are restored as managed.
    expect(supportsManaged("codex")).toBe(true);
    expect(supportsManaged("claude")).toBe(true);
    expect(sanitizeHandoff({ provider: "claude", kind: "managed", permission: "ask" }).kind)
      .toBe("managed");
  });

  it("treats Grok as a first-class terminal-only provider", () => {
    expect(isAgentProvider("grok")).toBe(true);
    expect(supportsManaged("grok")).toBe(false);
    expect(sanitizeHandoff({ provider: "grok", kind: "managed", permission: "edit" }))
      .toEqual({ provider: "grok", kind: "external_terminal", permission: "edit" });
    expect(describeHandoff({ provider: "grok", kind: "external_terminal", permission: "ask" }))
      .toBe("Grok · terminal");
    expect(handoffGate({
      checkout: "/work/GitPulse",
      settings: { provider: "grok", kind: "managed", permission: "ask" },
      acknowledgedBypass: false,
      dirty: false,
      busy: false,
    }).reason).toMatch(/Grok supports terminal handoffs only/);
  });

  it("treats Antigravity as a first-class terminal-only provider", () => {
    expect(isAgentProvider("agy")).toBe(true);
    expect(supportsManaged("agy")).toBe(false);
    expect(sanitizeHandoff({ provider: "agy", kind: "managed", permission: "edit" }))
      .toEqual({ provider: "agy", kind: "external_terminal", permission: "edit" });
    expect(describeHandoff({ provider: "agy", kind: "external_terminal", permission: "ask" }))
      .toBe("Antigravity · terminal");
    expect(handoffGate({
      checkout: "/work/GitPulse",
      settings: { provider: "agy", kind: "managed", permission: "ask" },
      acknowledgedBypass: false,
      dirty: false,
      busy: false,
    }).reason).toMatch(/Antigravity supports terminal handoffs only/);
  });

  it("keeps a settings object coherent when the provider changes under it", () => {
    expect(reconcileHandoff({ provider: "grok", kind: "managed", permission: "ask" }))
      .toEqual({ provider: "grok", kind: "external_terminal", permission: "ask" });
    expect(reconcileHandoff({ provider: "codex", kind: "managed", permission: "ask" }).kind).toBe("managed");
    expect(reconcileHandoff({ provider: "claude", kind: "managed", permission: "ask" }).kind).toBe("managed");
  });

  it("describes a handoff in one readable phrase", () => {
    expect(describeHandoff({ provider: "claude", kind: "external_terminal", permission: "ask" }))
      .toBe("Claude Code · terminal");
    expect(describeHandoff({ provider: "codex", kind: "managed", permission: "ask" }))
      .toBe("Codex · managed");
  });
});

describe("normalizeCheckout", () => {
  it("accepts a plain path unchanged", () => {
    expect(normalizeCheckout("/work/GitPulse")).toBe("/work/GitPulse");
    expect(normalizeCheckout("  /work/GitPulse  ")).toBe("/work/GitPulse");
  });

  it("returns the original spelling rather than a normalized one", () => {
    // The normalizer rewrites backslashes for comparison; what is sent to the
    // backend has to stay the path the reader actually chose.
    expect(normalizeCheckout("C:\\work\\GitPulse")).toBe("C:\\work\\GitPulse");
  });

  it("refuses empty, oversized, control-laden and bare-separator input", () => {
    expect(normalizeCheckout("")).toBe("");
    expect(normalizeCheckout("   ")).toBe("");
    expect(normalizeCheckout("/work\u0000/evil")).toBe("");
    expect(normalizeCheckout("/work\u001b[2J")).toBe("");
    expect(normalizeCheckout("/")).toBe("");
    expect(normalizeCheckout("x".repeat(MAX_CHECKOUT_LENGTH + 1))).toBe("");
    expect(normalizeCheckout(42)).toBe("");
    expect(normalizeCheckout(null)).toBe("");
  });

  it("refuses what the host refuses: a relative path, and a path over its byte budget", () => {
    // The host requires an absolute path of at most 4096 bytes
    // (`terminal_launch::valid_path`); the gate passing anything else only
    // moved the refusal to a generic error after the reader pressed Launch.
    for (const relative of ["repo", "./repo", "../repo", "~/code/repo", "C:repo", "work/GitPulse"]) {
      expect(normalizeCheckout(relative), relative).toBe("");
      expect(handoffGate({ checkout: relative, settings: defaultHandoff(), acknowledgedBypass: false, dirty: false, busy: false }).ok, relative).toBe(false);
    }
    for (const absolute of ["/work/GitPulse", "C:\\work\\GitPulse", "C:/work/GitPulse", "\\\\server\\share\\repo"]) {
      expect(normalizeCheckout(absolute), absolute).toBe(absolute);
    }
    // 2048 two-byte characters plus "/" is 2049 UTF-16 units but 4097 bytes.
    expect(normalizeCheckout("/" + "é".repeat(2048))).toBe("");
    expect(normalizeCheckout("/" + "é".repeat(2047) + "x")).toBe("/" + "é".repeat(2047) + "x");
    expect(normalizeCheckout("/" + "x".repeat(MAX_CHECKOUT_LENGTH - 1))).toBe("/" + "x".repeat(MAX_CHECKOUT_LENGTH - 1));
  });
});

describe("checkoutBesideCommonDir", () => {
  it("returns the working folder beside a .git directory", () => {
    expect(checkoutBesideCommonDir("/work/GitPulse/.git")).toBe("/work/GitPulse");
    expect(checkoutBesideCommonDir("C:\\work\\GitPulse\\.git")).toBe("C:\\work\\GitPulse");
  });

  it("returns nothing for a bare repository, which has no working tree", () => {
    expect(checkoutBesideCommonDir("/work/bare.git")).toBeNull();
    expect(checkoutBesideCommonDir("/.git")).toBeNull();
    expect(checkoutBesideCommonDir("")).toBeNull();
    expect(checkoutBesideCommonDir("/work/GitPulse")).toBeNull();
  });
});

describe("checkoutCandidates", () => {
  it("offers the open tabs for this repository first, labelled as open", () => {
    const candidates = checkoutCandidates(
      "r0",
      repositories,
      [
        { path: "/work/GitPulse", label: "GitPulse" },
        { path: "/work/Manvi", label: "Manvi" },
      ],
      options,
    );
    expect(candidates).toEqual([{ path: "/work/GitPulse", label: "GitPulse", source: "open" }]);
    expect(preferredCheckout(candidates)).toBe("/work/GitPulse");
  });

  it("falls back to the folder beside the common dir, marked as derived", () => {
    const candidates = checkoutCandidates("r0", repositories, [], options);
    expect(candidates).toEqual([{ path: "/work/GitPulse", label: "/work/GitPulse", source: "derived" }]);
  });

  it("never lists the same checkout twice", () => {
    const candidates = checkoutCandidates(
      "r0",
      repositories,
      [
        { path: "/work/GitPulse", label: "GitPulse" },
        { path: "/work/GitPulse/", label: "GitPulse again" },
      ],
      options,
    );
    expect(candidates).toHaveLength(1);
    expect(candidates[0].source).toBe("open");
  });

  it("offers nothing for a bare repository or an unknown identity", () => {
    expect(checkoutCandidates("r1", repositories, [], options)).toEqual([]);
    expect(checkoutCandidates("r2", repositories, [], options)).toEqual([]);
    expect(checkoutCandidates("missing", repositories, [], options)).toEqual([]);
    expect(preferredCheckout([])).toBe("");
  });

  it("matches a tab opened at the .git directory itself", () => {
    const candidates = checkoutCandidates("r0", repositories, [{ path: "/work/GitPulse/.git", label: "dotgit" }], options);
    expect(candidates[0]).toMatchObject({ path: "/work/GitPulse/.git", source: "open" });
  });

  it("honours case-insensitive filesystems without duplicating a checkout", () => {
    const candidates = checkoutCandidates(
      "r0",
      repositories,
      [
        { path: "/work/GitPulse", label: "GitPulse" },
        { path: "/WORK/gitpulse", label: "shouted" },
      ],
      { caseInsensitive: true },
    );
    expect(candidates).toHaveLength(1);
  });

  it("skips a tab whose path is not usable at all", () => {
    const candidates = checkoutCandidates("r0", repositories, [{ path: "/work\u0000/GitPulse", label: "bad" }], options);
    expect(candidates.every((entry) => entry.source === "derived")).toBe(true);
  });

  it("stays bounded with many open tabs", () => {
    const tabs = Array.from({ length: 5_000 }, (_, i) => ({ path: `/work/other-${i}`, label: `o${i}` }));
    const started = performance.now();
    const candidates = checkoutCandidates("r0", repositories, tabs, options);
    // Catches an accidental O(n^2) over the tab list, which at 5,000 tabs is
    // 25M comparisons and lands in seconds; the bound is sized to separate
    // that from linear work on a machine that is also running a build.
    expect(performance.now() - started).toBeLessThan(wallClockBudget(2_000));
    expect(candidates).toHaveLength(1);
  });
});

describe("handoffGate", () => {
  const base = {
    checkout: "/work/GitPulse",
    settings: defaultHandoff(),
    acknowledgedBypass: false,
    dirty: false,
    busy: false,
  };

  it("passes a complete, saved, idle handoff", () => {
    expect(handoffGate(base)).toEqual({ ok: true, reason: "" });
  });

  it("names the one thing to fix, in the order a reader would fix it", () => {
    expect(handoffGate({ ...base, busy: true }).reason).toMatch(/already in progress/);
    expect(handoffGate({ ...base, dirty: true }).reason).toMatch(/Save your task edits/);
    expect(handoffGate({ ...base, checkout: "" }).reason).toMatch(/working checkout/);
    expect(handoffGate({ ...base, checkout: "  " }).reason).toMatch(/working checkout/);
  });

  it("reports a busy launch ahead of an unsaved edit", () => {
    // Both are true; the one that will clear on its own is named first.
    expect(handoffGate({ ...base, busy: true, dirty: true }).reason).toMatch(/already in progress/);
  });

  it("refuses a managed connection the provider does not offer", () => {
    const gate = handoffGate({ ...base, settings: { provider: "grok", kind: "managed", permission: "ask" } });
    expect(gate.ok).toBe(false);
    expect(gate.reason).toMatch(/Grok supports terminal handoffs only/);
    // And lets through the two that do offer one.
    for (const provider of ["codex", "claude"] as const) {
      expect(handoffGate({ ...base, settings: { provider, kind: "managed", permission: "ask" } }).ok).toBe(true);
    }
  });

  it("requires an explicit authorization for every bypass attempt", () => {
    const bypass = { ...base, settings: { ...base.settings, permission: "bypass" as const } };
    expect(handoffGate(bypass).ok).toBe(false);
    expect(handoffGate(bypass).reason).toMatch(/explicit authorization/);
    expect(handoffGate({ ...bypass, acknowledgedBypass: true }).ok).toBe(true);
  });
});

describe("runStateLabel", () => {
  it("names every state the wire declares, and passes an unknown one through", () => {
    expect(runStateLabel("prepared")).toBe("Prepared");
    expect(runStateLabel("exited")).toBe("Exited");
    expect(runStateLabel("failed")).toBe("Failed to start");
    expect(runStateLabel("unresolved")).toBe("Unresolved");
    // An unknown state from a newer backend is shown, not swallowed.
    expect(runStateLabel("quantum")).toBe("quantum");
  });
});

describe("runExpired", () => {
  // Seconds on the wire, milliseconds in the browser. Reading `expires_at` as
  // milliseconds would put every expiry ~55 millennia out, so nothing would
  // ever expire and the phantom row this predicate exists to catch would stay
  // exactly as invisible as it was.
  const at = (seconds: number) => ({ state: "prepared", expires_at: seconds });

  it("expires a prepared attempt at its second, not before", () => {
    expect(runExpired(at(1_000), 999_999)).toBe(false);
    // The boundary belongs to the store, whose own predicate keeps a prepared
    // row only while `expires_at > now`; at equality it is already history.
    expect(runExpired(at(1_000), 1_000_000)).toBe(true);
    expect(runExpired(at(1_000), 1_000_001)).toBe(true);
  });

  it("calls no other state expired, however old", () => {
    // Only `prepared` carries an expiry the store acts on. A running attempt
    // whose `expires_at` has long passed is still a live process, and calling
    // it expired would hide the only control that can stop it.
    for (const state of ["starting", "running", "unresolved", "exited", "cancelled", "failed"]) {
      expect(runExpired({ state, expires_at: 0 }, Date.now())).toBe(false);
    }
  });
});

describe("runHoldsCheckout", () => {
  // The oracle is the store's own predicate, restated once here so the test
  // cannot drift into agreeing with the implementation instead.
  const store = (state: string, expiresAt: number, now: number) =>
    ["starting", "running", "unresolved"].includes(state) || (state === "prepared" && expiresAt * 1000 > now);

  it("agrees with the store for every state, either side of an expiry", () => {
    const states = ["prepared", "starting", "running", "unresolved", "exited", "failed", "cancelled", "quantum"];
    for (const state of states) {
      for (const [expires, now] of [[1_000, 999_999], [1_000, 1_000_000], [1_000, 5_000_000], [0, 0], [Number.NaN, 1]]) {
        expect(runHoldsCheckout({ state, expires_at: expires }, now), `${state} ${expires} ${now}`).toBe(store(state, expires, now));
      }
    }
  });

  it("counts an unresolved attempt as holding its checkout, which the old filter did not", () => {
    // The pre-change filter was prepared/starting/running: an unresolved row
    // rendered as history while the store refused every launch because of it.
    expect(runHoldsCheckout({ state: "unresolved", expires_at: 0 }, Date.now())).toBe(true);
    expect(canRelease({ state: "unresolved" })).toBe(true);
  });

  it("offers Release exactly where the host can act, never on history or a mere preparation", () => {
    for (const state of ["starting", "running", "unresolved"]) expect(canRelease({ state }), state).toBe(true);
    for (const state of ["prepared", "exited", "failed", "cancelled"]) expect(canRelease({ state }), state).toBe(false);
  });
});

describe("attemptHolding", () => {
  const insensitive = { caseInsensitive: true };
  const held = (cwd: string, state = "running", expires_at = 0) => ({ cwd, state, expires_at, id: cwd });

  it("names the attempt in this checkout, by identity, and ignores other checkouts", () => {
    const runs = [held("/work/Repo/.gitpulse/worktrees/a-1"), held("/work/Repo")];
    expect(attemptHolding(runs, { path: "/work/repo/" }, insensitive)?.cwd).toBe("/work/Repo");
    expect(attemptHolding(runs, { path: "/work/repo/.gitpulse/worktrees/a-1" }, insensitive)?.id).toBe("/work/Repo/.gitpulse/worktrees/a-1");
    expect(attemptHolding(runs, { path: "/work/repo/.gitpulse/worktrees/b-2" }, insensitive)).toBeNull();
    expect(attemptHolding(runs, { path: "" }, insensitive)).toBeNull();
  });

  it("does not count history or an expired preparation as occupying the checkout", () => {
    for (const state of ["exited", "failed", "cancelled"]) {
      expect(attemptHolding([held("/r", state)], { path: "/r" }, insensitive), state).toBeNull();
    }
    expect(attemptHolding([held("/r", "prepared", 1)], { path: "/r" }, insensitive, 5_000)).toBeNull();
    expect(attemptHolding([held("/r", "prepared", 10)], { path: "/r" }, insensitive, 5_000)?.state).toBe("prepared");
    // Unresolved holds it: that is the row that refused launches before.
    expect(attemptHolding([held("/r", "unresolved")], { path: "/r" }, insensitive)?.state).toBe("unresolved");
  });

  describe("by the store's own key, the checkout's Git directory", () => {
    const sensitive = { caseInsensitive: false };
    const inRepo = (cwd: string, git_dir?: string) => ({ ...held(cwd), ...(git_dir === undefined ? {} : { git_dir }) });

    it("sees an agent working in a subfolder of the chosen checkout", () => {
      // The store refuses by `git_dir`; comparing `cwd` missed this agent,
      // the store refused the launch, and only a second press ran it.
      const runs = [inRepo("/r/pkg/web", "/r/.git")];
      expect(attemptHolding(runs, { path: "/r", git_dir: "/r/.git" }, sensitive)?.cwd).toBe("/r/pkg/web");
    });

    it("keeps a worktree nested inside the checkout apart from the checkout", () => {
      // A path-prefix rule would mark /r busy for every attempt in one of
      // its own `.gitpulse/worktrees` — each has a Git directory of its own.
      const nested = [inRepo("/r/.gitpulse/worktrees/a", "/r/.git/worktrees/a")];
      expect(attemptHolding(nested, { path: "/r", git_dir: "/r/.git" }, sensitive)).toBeNull();
      expect(attemptHolding([inRepo("/r", "/r/.git")], { path: "/r/.gitpulse/worktrees/a", git_dir: "/r/.git/worktrees/a" }, sensitive)).toBeNull();
    });

    it("compares Git directories by the platform's path identity", () => {
      expect(attemptHolding([inRepo("/R", "/R/.git")], { path: "/r", git_dir: "/r/.git" }, insensitive)).not.toBeNull();
      expect(attemptHolding([inRepo("/R", "/R/.git")], { path: "/r", git_dir: "/r/.git" }, sensitive)).toBeNull();
      expect(attemptHolding([inRepo("C:\\r", "C:\\r\\.git")], { path: "C:/r", git_dir: "C:/r/.git" }, insensitive)).not.toBeNull();
    });

    it("falls back to the directory where either side has no Git directory", () => {
      expect(attemptHolding([inRepo("/r")], { path: "/r", git_dir: "/r/.git" }, sensitive)).not.toBeNull();
      expect(attemptHolding([inRepo("/r/pkg", "/r/.git")], { path: "/r" }, sensitive)).toBeNull();
      expect(attemptHolding([inRepo("/r", "/r/.git")], { path: "/r", git_dir: null }, sensitive)).not.toBeNull();
    });

    it("fuzz: any spelling of one Git directory matches, and no two worktrees do", () => {
      let seed = 7;
      const rand = (n: number) => { seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648; return seed % n; };
      const spell = (path: string) => {
        let out = rand(2) ? path.toUpperCase() : path;
        if (rand(2)) out = out.replaceAll("/", "\\");
        if (rand(2)) out += rand(2) ? "/" : "\\";
        return out;
      };
      for (let n = 0; n < 2_000; n += 1) {
        const name = `w${rand(50)}`, other = `w${50 + rand(50)}`;
        const gitDir = `/work/repo/.git/worktrees/${name}`;
        const runs = [inRepo(`/work/repo/.gitpulse/worktrees/${name}/sub`, spell(gitDir))];
        expect(attemptHolding(runs, { path: `/work/repo/.gitpulse/worktrees/${name}`, git_dir: spell(gitDir) }, insensitive), gitDir).not.toBeNull();
        expect(attemptHolding(runs, { path: `/work/repo/.gitpulse/worktrees/${other}`, git_dir: `/work/repo/.git/worktrees/${other}` }, insensitive)).toBeNull();
      }
    });
  });
});

describe("runStatusLabel", () => {
  const row = (state: string, outcome_uncertain = false, expires_at = 4_000_000_000) => ({ state, outcome_uncertain, expires_at });

  it("never calls an unobserved end a process exit", () => {
    expect(runStatusLabel(row("exited", true))).toBe("Ended — outcome unknown");
    expect(runStatusLabel(row("exited", false))).toBe("Process exited");
  });

  it("names an expired preparation and defers everything else to the state label", () => {
    expect(runStatusLabel(row("prepared", false, 1), 5_000)).toBe("Preparation expired");
    expect(runStatusLabel(row("prepared"), 5_000)).toBe("Prepared");
    expect(runStatusLabel(row("unresolved", true))).toBe("Unresolved");
  });
});

describe("a model for one launch", () => {
  const terminal = (provider: "claude" | "codex" | "grok" | "agy") => ({ provider, kind: "external_terminal" as const });
  const typed = (model = "", effort = "", advisor = "") => ({ model, effort, advisor });

  it("sends nothing when nothing is typed, so the saved default applies untouched", () => {
    expect(launchModelOverride(terminal("claude"), NO_MODEL_OVERRIDE)).toEqual({ ok: true, choice: undefined });
    expect(launchModelOverride(terminal("claude"), typed("  ", "", " "))).toEqual({ ok: true, choice: undefined });
  });

  it("sends only the fields typed, and only those this launcher takes", () => {
    expect(launchModelOverride(terminal("claude"), typed(" opus ", "high", "fable"))).toEqual({ ok: true, choice: { model: "opus", effort: "high", advisor: "fable" } });
    expect(launchModelOverride(terminal("claude"), typed("", "max"))).toEqual({ ok: true, choice: { effort: "max" } });
    // Codex takes a model and nothing else: an effort left in the state is not sent.
    expect(launchModelOverride(terminal("codex"), typed("gpt-6", "high", "fable"))).toEqual({ ok: true, choice: { model: "gpt-6" } });
    expect(modelOverrideFields("agy")).toEqual(["model", "effort"]);
    expect(modelOverrideFields("grok")).toEqual(["model"]);
  });

  it("refuses a value it would otherwise have dropped, and says which", () => {
    expect(launchModelOverride(terminal("claude"), typed("opus 4"))).toMatchObject({ ok: false, reason: expect.stringContaining("opus 4") });
    expect(launchModelOverride(terminal("claude"), typed("", "extreme"))).toMatchObject({ ok: false, reason: expect.stringContaining("effort") });
    expect(launchModelOverride(terminal("claude"), typed("", "", "-x"))).toMatchObject({ ok: false, reason: expect.stringContaining("advisor") });
    const gate = handoffGate({ checkout: "/work/a", settings: { ...defaultHandoff(), provider: "claude", kind: "external_terminal" }, acknowledgedBypass: false, dirty: false, busy: false, model: typed("opus 4") });
    expect(gate).toMatchObject({ ok: false, reason: expect.stringContaining("opus 4") });
  });

  it("offers Claude Code's family aliases as choices, and leaves other launchers' fields typed", () => {
    expect(modelOverrideChoices("claude", "model")).toEqual(["fable", "opus", "sonnet", "haiku"]);
    expect(modelOverrideChoices("claude", "advisor")).toEqual(["fable", "opus", "sonnet"]);
    for (const provider of ["codex", "grok", "agy"] as const) expect(modelOverrideChoices(provider, "model"), provider).toBeNull();
    // Every choice is one a launch sends as chosen, never one the gate refuses.
    for (const model of modelOverrideChoices("claude", "model") ?? []) {
      expect(launchModelOverride(terminal("claude"), typed(model))).toEqual({ ok: true, choice: { model } });
    }
    for (const advisor of modelOverrideChoices("claude", "advisor") ?? []) {
      expect(launchModelOverride(terminal("claude"), typed("", "", advisor))).toEqual({ ok: true, choice: { advisor } });
    }
  });

  it("refuses any model for a managed attempt, whose model is Manvi's", () => {
    expect(launchModelOverride({ provider: "claude", kind: "managed" }, typed("opus"))).toMatchObject({ ok: false, reason: expect.stringContaining("Manvi") });
    expect(launchModelOverride({ provider: "claude", kind: "managed" }, NO_MODEL_OVERRIDE)).toEqual({ ok: true, choice: undefined });
  });

  it("reads a recorded run model in settings order, whatever order the record keeps", () => {
    expect(modelChoiceLabel({ advisor: "fable", effort: "high", model: "opus" })).toBe("opus · high effort · advisor fable");
    expect(modelChoiceLabel({ fallback: "sonnet,haiku", model: "opus" })).toBe("opus · fallback sonnet,haiku");
    expect(modelChoiceLabel({ zeta: "1", model: "m" })).toBe("m · zeta 1");
  });
});

describe("a preparation whose outcome is unknown", () => {
  const preparation = (task: string, id = `run-${task}`): RunPreparation => ({
    id, request_id: `req-${id}`, task_id: task, source_revision: 1, repository_id: "repo", repository_revision: 1,
    repo_path: "/work/a", provider: "claude", permission_mode: "ask", acknowledge_bypass: false,
  });
  const held: string[] = [];
  const hold = (prep: RunPreparation) => { held.push(prep.task_id); holdPendingPreparation(prep); };
  afterEach(() => { for (const task of held.splice(0)) releasePendingPreparation(task); });

  it("outlives the form that sent it, so a reopened sheet retries the same request", () => {
    expect(pendingPreparationFor("t1")).toBeNull();
    hold(preparation("t1"));
    expect(pendingPreparationFor("t1")).toMatchObject({ id: "run-t1", request_id: "req-run-t1" });
    expect(pendingPreparationFor("t2")).toBeNull();
  });

  it("hands back a copy, so a form cannot edit the request it will replay", () => {
    hold(preparation("t1"));
    const read = pendingPreparationFor("t1");
    if (!read) throw new Error("expected a held preparation");
    read.repo_path = "/elsewhere";
    expect(pendingPreparationFor("t1")?.repo_path).toBe("/work/a");
  });

  it("signals every hold and release, so a form showing a restored copy sees another form settle it", () => {
    // The pane remounts while the store is answering: the new form restores
    // the hold, and the form that sent it — gone — is the one the answer
    // reaches. Without a signal the new form keeps offering a retry of an
    // attempt that is already running.
    const seen: number[] = [];
    const stop = pendingPreparationsChanged.subscribe((revision) => seen.push(revision));
    try {
      hold(preparation("t1"));
      const restored = pendingPreparationFor("t1");
      releasePendingPreparation("t1", "someone-else");
      expect(seen).toHaveLength(2); // the subscription's first value, then the hold; a no-op release is silent
      releasePendingPreparation("t1", restored?.id);
      expect(seen).toHaveLength(3);
      expect(new Set(seen).size).toBe(3);
      expect(pendingPreparationFor("t1")).toBeNull();
    } finally {
      stop();
    }
  });

  it("is released only for the attempt it names", () => {
    hold(preparation("t1", "first"));
    releasePendingPreparation("t1", "other");
    expect(pendingPreparationFor("t1")?.id).toBe("first");
    releasePendingPreparation("t1", "first");
    expect(pendingPreparationFor("t1")).toBeNull();
  });

  it("stays bounded, dropping the oldest, and refreshes a task that is held again", () => {
    for (let n = 0; n < MAX_PENDING_PREPARATIONS; n += 1) hold(preparation(`t${n}`));
    hold(preparation("t0", "again"));
    hold(preparation("overflow"));
    expect(pendingPreparationFor("t0")?.id).toBe("again");
    expect(pendingPreparationFor("t1")).toBeNull();
    expect(pendingPreparationFor("overflow")).not.toBeNull();
    expect(pendingPreparationFor(`t${MAX_PENDING_PREPARATIONS - 1}`)).not.toBeNull();
  });
});
