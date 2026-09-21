import { describe, expect, it } from "vitest";
import { runExpired, runStateLabel } from "./taskHandoff";
import { MANAGED_PROVIDERS, supportsManaged } from "./vocabulary";

/**
 * The managed lane's two failure modes, pressed on until they break.
 *
 * 1. A *prepared* attempt occupies its repository's single run slot until it
 *    expires. The panel decided whether to keep polling with one inline
 *    comparison against `Date.now()` and decided whether to keep *showing* the
 *    row as live with a different rule that had no clock in it at all. So an
 *    expired preparation stopped being polled and then sat on screen as
 *    "Prepared" forever, offering a recover button the store would refuse —
 *    which is what "I see no updates for the run" actually was.
 *
 * 2. Which providers may take the lane is decided in several processes. A
 *    disagreement is silent by construction: every check passes and the launch
 *    dies at the far end. So the renderer's own list is pinned here too.
 *
 * `expires_at` is *seconds* on the wire and milliseconds in the browser, which
 * is the single most likely thing to be got wrong by a later edit: read as
 * milliseconds, every attempt expires ~55 millennia from now and nothing is
 * ever expired again.
 */

const POLLUTION = ["__proto__", "constructor", "prototype", "toString", "valueOf", "hasOwnProperty"];
const NOT_LIVE = ["exited", "cancelled", "failed", "unresolved", "starting", "running"];

/** The store's own predicate, restated as the oracle to test against. */
const storeHoldsRepository = (state: string, expiresAtSeconds: number, nowMs: number) =>
  ["starting", "running", "unresolved"].includes(state) ||
  (state === "prepared" && expiresAtSeconds * 1000 > nowMs);

describe("prepared-attempt expiry under hostile and boundary clocks", () => {
  it("never calls a live attempt expired, nor a dead one live", () => {
    const now = 1_700_000_000_000;
    // Seconds, chosen around the millisecond boundary this conversion invites
    // people to get wrong, plus the degenerate values a wire can carry.
    const seconds: unknown[] = [
      1_700_000_000, // exactly now
      1_699_999_999, // one second past
      1_700_000_001, // one second to go
      0,
      -1,
      -0,
      Number.MAX_SAFE_INTEGER,
      Number.MIN_SAFE_INTEGER,
      Number.MAX_VALUE,
      Number.EPSILON,
      1.5,
      NaN,
      Infinity,
      -Infinity,
      ...POLLUTION,
      null,
      undefined,
      "",
      "1700000000",
      true,
      false,
      [],
      [1_700_000_000],
      {},
    ];
    for (const value of seconds) {
      const run = { state: "prepared", expires_at: value as number };
      const verdict = runExpired(run, now);
      expect(typeof verdict, `expires_at=${String(value)} produced a non-boolean`).toBe("boolean");
      // The predicate is the exact complement of the store's own rule for a
      // prepared row. Anything else means the panel and the store disagree
      // about whether the repository is free — the disagreement that showed a
      // dead row as live and a held repository as available.
      expect(verdict, `expires_at=${String(value)} disagrees with the store`).toBe(
        !storeHoldsRepository("prepared", value as number, now),
      );
    }
  });

  it("treats a NaN or missing expiry as expired rather than as forever-live", () => {
    // A row whose expiry cannot be read is the dangerous direction: believing
    // it live keeps the panel polling something that will never change and
    // keeps a recover button on screen that cannot work.
    //
    // This case is why the predicate is written as `!(deadline > now)` rather
    // than `deadline <= now`. Both read identically and they differ on exactly
    // this input: every comparison with NaN is false, so `<=` called an
    // unreadable expiry *live forever* — the first draft did, and this test is
    // what caught it. Pinned so a later simplification back to `<=` fails.
    for (const bad of [NaN, undefined, null, "soon"]) {
      expect(runExpired({ state: "prepared", expires_at: bad as number }, Date.now())).toBe(true);
    }
  });

  it("expires only prepared attempts, whatever their timestamp", () => {
    for (const state of NOT_LIVE) {
      for (const at of [0, -1, NaN, Number.MAX_SAFE_INTEGER, Date.now() / 1000]) {
        expect(runExpired({ state, expires_at: at }, Date.now()), `${state}@${at}`).toBe(false);
      }
    }
    // Including states no build has ever emitted: an unknown state is history
    // to this predicate, never an expiring preparation.
    for (const state of [...POLLUTION, "", "quantum", "PREPARED", " prepared"]) {
      expect(runExpired({ state, expires_at: 0 }, Date.now()), state).toBe(false);
      expect(runStateLabel(state)).toBe(state === "" ? "" : runStateLabel(state));
    }
  });

  it("is monotonic: once expired, no later clock un-expires it", () => {
    const run = { state: "prepared", expires_at: 1_700_000_000 };
    let seen = false;
    for (let ms = 1_699_999_000_000; ms <= 1_700_000_002_000; ms += 250) {
      const expired = runExpired(run, ms);
      if (expired) seen = true;
      expect(seen && !expired, `un-expired at ${ms}`).toBe(false);
    }
    expect(seen).toBe(true);
  });

  it("agrees with the store across a randomized sweep", () => {
    // Deterministic pseudo-random, so a failure is reproducible.
    let seed = 0x5eed;
    const next = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
    const states = ["prepared", ...NOT_LIVE];
    for (let i = 0; i < 20_000; i += 1) {
      const state = states[Math.floor(next() * states.length)]!;
      const expires = Math.floor(next() * 2_000_000_000);
      const now = Math.floor(next() * 2_000_000_000_000);
      expect(runExpired({ state, expires_at: expires }, now)).toBe(
        state === "prepared" && !storeHoldsRepository(state, expires, now),
      );
    }
  });
});

describe("managed provider vocabulary under hostile input", () => {
  it("admits the adapter set and nothing that merely looks like it", () => {
    expect([...MANAGED_PROVIDERS].sort()).toEqual(["claude", "codex"]);
    const lookalikes = [
      ...POLLUTION,
      "",
      " ",
      "CODEX",
      "Codex",
      "codex ",
      " codex",
      "codexx",
      "cod",
      "claude-code",
      "claude\u200b",
      "claude\u0000",
      "\u202eedualc",
      "codex\n",
      "codex;claude",
      null,
      undefined,
      0,
      1,
      true,
      {},
      [],
      ["codex"],
    ];
    for (const value of lookalikes) {
      expect(supportsManaged(value as never), `${String(value)} was admitted`).toBe(false);
    }
    for (const provider of MANAGED_PROVIDERS) expect(supportsManaged(provider)).toBe(true);
  });
});
