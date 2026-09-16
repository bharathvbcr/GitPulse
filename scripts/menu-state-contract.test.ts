import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  MENU_ACTION_IDS,
  MENU_LIMITS,
  MENU_TONES,
  MENU_WATCH_STATUSES,
  RECENT_PREFIX,
  REPOSITORY_PREFIX,
  TRAY_SUMMARY_IDS,
  isCheckable,
  menuStateProblem,
} from "../src/lib/desktop/menuContract";
import type { MenuState } from "../src/lib/desktop/menuState";

/**
 * `menuContract.ts` restates, in TypeScript, the rules `MenuState::validate`
 * enforces in Rust. That mirror is what lets the frontend suite prove
 * `buildMenuState` is total — but a mirror nobody checks is worse than none,
 * because it keeps passing after the original moves and hands back false
 * confidence about the exact payload the app is wedging itself on.
 *
 * So the Rust source is the input here: every ceiling, enum, action id and
 * refusal message is read out of state.rs and actions.rs and compared with the
 * mirror. Drift on either side fails, and the failure names the value.
 */
const state = readFileSync(new URL("../src-tauri/src/desktop/state.rs", import.meta.url), "utf8");
const actions = readFileSync(new URL("../src-tauri/src/desktop/actions.rs", import.meta.url), "utf8");
const mirror = readFileSync(new URL("../src/lib/desktop/menuContract.ts", import.meta.url), "utf8");

/** `pub const NAME: &str = "value";` — the vocabulary both halves share. */
const CONSTANTS = new Map(
  [...actions.matchAll(/pub const ([A-Z][A-Z0-9_]*): &str = "([^"]*)";/g)].map(
    (match) => [match[1] as string, match[2] as string] as const,
  ),
);

function constant(name: string): string {
  const value = CONSTANTS.get(name);
  expect(value, `actions.rs has no const ${name}`).toBeDefined();
  return value as string;
}

describe("the frontend mirror matches MenuState::validate", () => {
  it("reads a state.rs that still has the shapes this test parses", () => {
    // Guards every regex below against passing vacuously on a renamed file.
    expect(CONSTANTS.size).toBeGreaterThan(40);
    expect(state).toContain("pub fn validate(&self) -> Result<(), String>");
    expect(state).toContain("pub fn checkable(id: &str) -> bool");
  });

  it("agrees on every entry-count ceiling", () => {
    const ceiling = (field: string) => {
      const found = state.match(new RegExp(`self\\.${field}\\.len\\(\\) > (\\d[\\d_]*)`));
      expect(found, `state.rs no longer bounds ${field}`).not.toBeNull();
      return Number((found as RegExpMatchArray)[1].replace(/_/g, ""));
    };
    expect(MENU_LIMITS.enabled).toBe(ceiling("enabled"));
    expect(MENU_LIMITS.checked).toBe(ceiling("checked"));
    expect(MENU_LIMITS.labels).toBe(ceiling("labels"));
    expect(MENU_LIMITS.repositories).toBe(ceiling("repositories"));
    expect(MENU_LIMITS.trayDetails).toBe(ceiling("tray_details"));
  });

  it("agrees on every text and count ceiling", () => {
    // 2048 bytes is spelled once per field; the mirror applies one constant, so
    // it is only correct while every spelling in Rust says the same number.
    const texts = [...state.matchAll(/\.len\(\) > (\d[\d_]*)/g)]
      .map((match) => Number(match[1].replace(/_/g, "")))
      .filter((value) => value === MENU_LIMITS.text || value === MENU_LIMITS.path);
    expect(texts.filter((value) => value === MENU_LIMITS.text).length).toBeGreaterThanOrEqual(5);
    expect(texts.filter((value) => value === MENU_LIMITS.path).length).toBeGreaterThanOrEqual(2);
    expect(state).toContain(`chars().count() > ${MENU_LIMITS.trayTitleChars}`);
    const counts = [...state.matchAll(/is_some_and\(\|n\| n > (\d[\d_]*)\)/g)].map((match) =>
      Number(match[1].replace(/_/g, "")),
    );
    expect(counts).toEqual([MENU_LIMITS.count, MENU_LIMITS.count]);
  });

  it("agrees on the status-card enums", () => {
    const variants = (field: string) => {
      const found = state.match(new RegExp(`self\\.status\\.${field}\\.as_str\\(\\),([^)]*)\\)`));
      expect(found, `state.rs no longer constrains status.${field}`).not.toBeNull();
      return [...(found as RegExpMatchArray)[1].matchAll(/"([^"]*)"/g)].map((match) => match[1]);
    };
    expect([...MENU_TONES]).toEqual(variants("tone"));
    expect([...MENU_WATCH_STATUSES]).toEqual(variants("watch_status"));
  });

  it("agrees on the actions the tray summary may invoke", () => {
    // The alternatives follow `as_str(),` one per line, until the line that
    // closes the `matches!`.
    const lines = state.slice(state.indexOf("self.tray_summary.id.as_str(),")).split("\n").slice(1);
    const end = lines.findIndex((line) => line.trim() === ")");
    expect(end, "the tray-summary matches! no longer closes on its own line").toBeGreaterThan(0);
    const listed = [...lines.slice(0, end).join("\n").matchAll(/"([^"]*)"|actions::([A-Z_]+)/g)].map(
      (match) => (match[1] !== undefined ? match[1] : constant(match[2] as string)),
    );
    expect(listed).toHaveLength(TRAY_SUMMARY_IDS.length);
    expect([...TRAY_SUMMARY_IDS].sort()).toEqual(listed.sort());
  });

  it("agrees on the path-carrying prefixes", () => {
    expect(REPOSITORY_PREFIX).toBe(constant("REPOSITORY_PREFIX"));
    expect(RECENT_PREFIX).toBe(constant("RECENT_PREFIX"));
  });

  it("lists exactly the actions NativeAction::parse resolves without a path", () => {
    // `known` in validate() accepts an id only when it parses AND carries no
    // path, so the mirror's vocabulary has to be the parse table minus the two
    // path-carrying arms and minus the ids that deliberately parse to None.
    const parse = actions.slice(actions.indexOf("pub fn parse(id: &str)"), actions.indexOf("pub fn event_id"));
    const arms = [...parse.matchAll(/^\s{12}([A-Z][A-Z0-9_]*) => Self::/gm)].map((match) =>
      constant(match[1] as string),
    );
    expect(arms.length).toBeGreaterThan(40);
    expect([...MENU_ACTION_IDS].sort()).toEqual(arms.sort());
    // The arms the mirror must not carry, for the reasons validate() excludes them.
    expect(parse).toContain("RECENT_EMPTY => return None");
    expect(MENU_ACTION_IDS).not.toContain(constant("RECENT_EMPTY"));
    expect(MENU_ACTION_IDS).not.toContain(constant("ACTIVATE_REPO"));
    expect(MENU_ACTION_IDS).not.toContain(constant("OPEN_RECENT"));
  });

  it("agrees on which ids carry a checkmark", () => {
    const body = state.slice(state.indexOf("pub fn checkable(id: &str) -> bool"));
    const prefixes = [...body.slice(0, body.indexOf("matches!")).matchAll(/starts_with\((?:"([^"]*)"|actions::([A-Z_]+))\)/g)]
      .map((match) => (match[1] !== undefined ? match[1] : constant(match[2] as string)));
    expect(prefixes).toEqual(["tab-", "section:", REPOSITORY_PREFIX]);
    for (const prefix of prefixes) expect(isCheckable(`${prefix}anything`)).toBe(true);

    const listed = [...body.slice(body.indexOf("matches!"), body.indexOf("}\n")).matchAll(/actions::([A-Z_]+)/g)]
      .map((match) => constant(match[1] as string));
    expect(listed).toEqual(["theme-system", "theme-light", "theme-dark", "fleet", "terminal-dock"]);
    for (const id of listed) expect(isCheckable(id)).toBe(true);
    for (const id of ["fetch", "push", "open", "refresh", "check-updates"]) {
      expect(isCheckable(id), `${id} must not be checkable`).toBe(false);
    }
  });

  it("returns the same refusal messages Rust does", () => {
    const rust = [...state.slice(state.indexOf("pub fn validate")).matchAll(/return Err\("([^"]+)"\.into\(\)\)/g)]
      .map((match) => match[1] as string);
    const typescript = [...mirror.matchAll(/^\s*return "([^"]+)";$/gm)].map((match) => match[1] as string);
    expect(rust.length).toBeGreaterThanOrEqual(6);
    expect(new Set(typescript)).toEqual(new Set(rust));
    // The message the shipped build reported, kept spelled the same on both
    // sides so a diagnostic can still be traced to the branch that raised it.
    expect(rust).toContain("Native menu active repository does not match its path");
  });

  it("still refuses what Rust refuses", () => {
    const valid: MenuState = {
      enabled: ["open"],
      checked: ["theme-system"],
      labels: [],
      repositories: [{ path: "/r/a", label: "a", active: true, changed: 1, conflicts: null, busy: false }],
      activePath: "/r/a",
      showStatusIcon: false,
      hideDockWhenClosed: true,
      trayTitle: null,
      trayDetails: [],
      traySummary: { id: "open", text: "Open a repository…" },
      trayDetail: "GitPulse",
      status: {
        repository: "GitPulse", branch: "", changed: null, staged: null, conflicts: null,
        ahead: null, behind: null, upstream: null, headline: "Your work, at a glance",
        tone: "neutral", primaryLabel: "Open repository", watchStatus: "unknown",
        reduceMotion: false, stashes: null, operation: null, activity: null,
        elsewhere: 0, fetchedAt: null,
      },
    };
    expect(menuStateProblem(structuredClone(valid))).toBeNull();

    const broken = <T>(mutate: (draft: MenuState) => T) => {
      const draft = structuredClone(valid);
      mutate(draft);
      return menuStateProblem(draft);
    };
    expect(broken((draft) => (draft.repositories[0].active = false))).toBe(
      "Native menu active repository does not match its path",
    );
    // Rust checks each row before it checks the pointer, so a pointer at a
    // repository the switcher does not list is only reported as "missing" once
    // no row claims to be the active one.
    expect(
      broken((draft) => {
        draft.repositories[0].active = false;
        draft.activePath = "/r/missing";
      }),
    ).toBe("Native menu active repository is missing from the switcher");
    expect(broken((draft) => draft.repositories.push({ ...draft.repositories[0], active: false })))
      .toBe("Native menu state contains an invalid repository");
    expect(broken((draft) => (draft.repositories[0].changed = MENU_LIMITS.count + 1))).toBe(
      "Native menu repository counts exceed their limit",
    );
    expect(broken((draft) => draft.enabled.push("not-an-action"))).toBe(
      "Native menu state contains an unknown action",
    );
    expect(broken((draft) => draft.checked.push("fetch"))).toBe(
      "Native menu state contains an unknown action",
    );
    expect(broken((draft) => (draft.traySummary.id = "fetch"))).toBe(
      "Native menu state contains an unknown action",
    );
    expect(broken((draft) => draft.labels.push({ id: "fetch", text: "a" }, { id: "fetch", text: "b" }))).toBe(
      "Native menu state contains an invalid label",
    );
    expect(broken((draft) => (draft.status.tone = "excited"))).toBe("Invalid status card presentation");
    expect(broken((draft) => (draft.status.watchStatus = "wedged"))).toBe("Invalid status card presentation");
    expect(broken((draft) => (draft.trayDetails = Array(MENU_LIMITS.trayDetails + 1).fill("row")))).toBe(
      "Native menu text exceeds its limit",
    );
    expect(broken((draft) => (draft.trayTitle = "x".repeat(MENU_LIMITS.trayTitleChars + 1)))).toBe(
      "Native menu text exceeds its limit",
    );
    // Rust counts bytes, not UTF-16 units: 2048 astral characters are 2048
    // JavaScript lengths and 8192 bytes, and only the byte count is refused.
    expect(broken((draft) => (draft.status.headline = "\u{1f600}".repeat(MENU_LIMITS.text / 2)))).toBe(
      "Invalid status card presentation",
    );
  });
});
