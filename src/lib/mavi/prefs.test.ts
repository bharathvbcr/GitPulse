import { describe, expect, it } from "vitest";
import {
  clampChangesWidth,
  clampRailWidth,
  createMaviPrefs,
  isValidSummaryModel,
  MAVI_CHANGES_WIDTH_BOUNDS,
  MAVI_CHAT_WIDTH_CSS,
  MAVI_DENSITY_CSS,
  MAVI_PREF_DEFAULTS,
  MAVI_PREFS_STORAGE_KEY,
  MAVI_RAIL_WIDTH_BOUNDS,
  parseMaviPrefs,
  readMaviPrefs,
  writeMaviPrefs,
  type StorageLike,
} from "./prefs.svelte";

class MemoryStorage implements StorageLike {
  map = new Map<string, string>();
  writes = 0;

  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.writes += 1;
    this.map.set(key, value);
  }
}

describe("mavi prefs clamping and parsing", () => {
  it("clamps railWidth to 180–380 and changesWidth to 280–640", () => {
    expect(clampRailWidth(50)).toBe(MAVI_RAIL_WIDTH_BOUNDS.min);
    expect(clampRailWidth(999)).toBe(MAVI_RAIL_WIDTH_BOUNDS.max);
    expect(clampRailWidth(260.6)).toBe(261);
    expect(clampRailWidth(Number.NaN)).toBe(MAVI_RAIL_WIDTH_BOUNDS.default);
    expect(clampRailWidth(Number.POSITIVE_INFINITY)).toBe(MAVI_RAIL_WIDTH_BOUNDS.default);

    expect(clampChangesWidth(100)).toBe(MAVI_CHANGES_WIDTH_BOUNDS.min);
    expect(clampChangesWidth(900)).toBe(MAVI_CHANGES_WIDTH_BOUNDS.max);
    expect(clampChangesWidth(412.4)).toBe(412);
    expect(clampChangesWidth(Number.NaN)).toBe(MAVI_CHANGES_WIDTH_BOUNDS.default);
    expect(clampChangesWidth(Number.NEGATIVE_INFINITY)).toBe(MAVI_CHANGES_WIDTH_BOUNDS.default);
  });

  it("returns defaults for non-object or malformed payloads", () => {
    expect(parseMaviPrefs(null)).toEqual(MAVI_PREF_DEFAULTS);
    expect(parseMaviPrefs(undefined)).toEqual(MAVI_PREF_DEFAULTS);
    expect(parseMaviPrefs("bad")).toEqual(MAVI_PREF_DEFAULTS);
    expect(parseMaviPrefs([1, 2, 3])).toEqual(MAVI_PREF_DEFAULTS);
  });

  it("defensively validates every field and preserves valid ones", () => {
    expect(
      parseMaviPrefs({
        railWidth: 420,
        changesWidth: 120,
        chatWidth: "wide",
        density: "spacious",
        textSize: "lg",
        autoScroll: false,
        autoOpenChanges: "yes",
        showThinking: 0,
        expandTools: true,
        showTurnMeta: false,
        sendKey: "mod_enter",
        messageStyle: "editorial",
        defaultAgent: "agy",
        summarizeOnEnd: true,
        summaryEngine: "ai",
        summaryModel: "claude-haiku-5-5",
      }),
    ).toEqual({
      railWidth: 380,
      changesWidth: 280,
      chatWidth: "wide",
      density: "spacious",
      textSize: "lg",
      autoScroll: false,
      autoOpenChanges: true,
      showThinking: true,
      expandTools: true,
      showTurnMeta: false,
      sendKey: "mod_enter",
      messageStyle: "editorial",
      defaultAgent: "agy",
      summarizeOnEnd: true,
      summaryEngine: "ai",
      summaryModel: "claude-haiku-5-5",
    });

    expect(
      parseMaviPrefs({
        railWidth: "248",
        changesWidth: null,
        chatWidth: "ultra",
        density: "tiny",
        textSize: "xl",
        sendKey: "tab",
        messageStyle: "cards",
        defaultAgent: "codex",
        summarizeOnEnd: "yes",
        summaryEngine: "local",
        summaryModel: "-bad flag",
      }),
    ).toEqual(MAVI_PREF_DEFAULTS);

    expect(isValidSummaryModel("haiku")).toBe(true);
    expect(isValidSummaryModel("claude-haiku-5-5")).toBe(true);
    expect(isValidSummaryModel("")).toBe(false);
    expect(isValidSummaryModel("bad model")).toBe(false);
    expect(isValidSummaryModel("-bad")).toBe(false);
    expect(isValidSummaryModel("x".repeat(121))).toBe(false);
  });

  it("maps chatWidth and density CSS tokens", () => {
    expect(MAVI_CHAT_WIDTH_CSS).toEqual({
      compact: "44rem",
      standard: "52rem",
      wide: "66rem",
      full: "100%",
    });
    expect(MAVI_DENSITY_CSS.compact).toEqual({ turnGap: "16px", rowGap: "6px", bubblePad: "7px 11px" });
    expect(MAVI_DENSITY_CSS.comfortable).toEqual({ turnGap: "24px", rowGap: "10px", bubblePad: "10px 14px" });
    expect(MAVI_DENSITY_CSS.spacious).toEqual({ turnGap: "32px", rowGap: "14px", bubblePad: "13px 18px" });
  });
});

describe("mavi prefs storage round-trip", () => {
  it("handles null storage, empty keys, corrupt JSON, and throwing setItem", () => {
    const storage = new MemoryStorage();
    expect(readMaviPrefs(null)).toEqual(MAVI_PREF_DEFAULTS);
    expect(readMaviPrefs(storage)).toEqual(MAVI_PREF_DEFAULTS);

    storage.setItem(MAVI_PREFS_STORAGE_KEY, "{broken");
    expect(readMaviPrefs(storage)).toEqual(MAVI_PREF_DEFAULTS);

    writeMaviPrefs(null, { ...MAVI_PREF_DEFAULTS });
    writeMaviPrefs(
      {
        getItem: () => null,
        setItem: () => {
          throw new Error("QuotaExceededError");
        },
      },
      { ...MAVI_PREF_DEFAULTS },
    );
  });

  it("loads stored prefs and persists updates via setters", () => {
    const storage = new MemoryStorage();
    storage.setItem(
      MAVI_PREFS_STORAGE_KEY,
      JSON.stringify({
        railWidth: 300,
        changesWidth: 440,
        chatWidth: "full",
        density: "compact",
        textSize: "sm",
        autoScroll: false,
        autoOpenChanges: false,
        showThinking: false,
        expandTools: true,
        showTurnMeta: false,
        sendKey: "mod_enter",
        messageStyle: "editorial",
        defaultAgent: "agy",
        summarizeOnEnd: true,
        summaryEngine: "ai",
        summaryModel: "sonnet",
      }),
    );

    const prefs = createMaviPrefs(storage);
    expect(prefs.snapshot()).toEqual({
      railWidth: 300,
      changesWidth: 440,
      chatWidth: "full",
      density: "compact",
      textSize: "sm",
      autoScroll: false,
      autoOpenChanges: false,
      showThinking: false,
      expandTools: true,
      showTurnMeta: false,
      sendKey: "mod_enter",
      messageStyle: "editorial",
      defaultAgent: "agy",
      summarizeOnEnd: true,
      summaryEngine: "ai",
      summaryModel: "sonnet",
    });

    const writesBefore = storage.writes;
    prefs.railWidth = 999;
    expect(prefs.railWidth).toBe(380);
    prefs.changesWidth = 10;
    expect(prefs.changesWidth).toBe(280);
    prefs.chatWidth = "compact";
    prefs.density = "spacious";
    prefs.textSize = "lg";
    prefs.autoScroll = true;
    prefs.autoOpenChanges = true;
    prefs.showThinking = true;
    prefs.expandTools = false;
    prefs.showTurnMeta = true;
    prefs.sendKey = "enter";
    prefs.messageStyle = "bubbles";
    prefs.defaultAgent = "claude";
    prefs.summarizeOnEnd = false;
    prefs.summaryEngine = "claude";
    prefs.summaryModel = "claude-haiku-5-5";

    expect(prefs.chatWidth).toBe("compact");
    expect(prefs.density).toBe("spacious");
    expect(prefs.textSize).toBe("lg");
    expect(prefs.autoScroll).toBe(true);
    expect(prefs.autoOpenChanges).toBe(true);
    expect(prefs.showThinking).toBe(true);
    expect(prefs.expandTools).toBe(false);
    expect(prefs.showTurnMeta).toBe(true);
    expect(prefs.sendKey).toBe("enter");
    expect(prefs.messageStyle).toBe("bubbles");
    expect(prefs.defaultAgent).toBe("claude");
    expect(prefs.summarizeOnEnd).toBe(false);
    expect(prefs.summaryEngine).toBe("claude");
    expect(prefs.summaryModel).toBe("claude-haiku-5-5");

    expect(storage.writes).toBe(writesBefore + 16);
    const reloaded = createMaviPrefs(storage);
    expect(reloaded.snapshot()).toEqual({
      railWidth: 380,
      changesWidth: 280,
      chatWidth: "compact",
      density: "spacious",
      textSize: "lg",
      autoScroll: true,
      autoOpenChanges: true,
      showThinking: true,
      expandTools: false,
      showTurnMeta: true,
      sendKey: "enter",
      messageStyle: "bubbles",
      defaultAgent: "claude",
      summarizeOnEnd: false,
      summaryEngine: "claude",
      summaryModel: "claude-haiku-5-5",
    });
  });

  it("rejects invalid enum assignments defensively", () => {
    const storage = new MemoryStorage();
    const prefs = createMaviPrefs(storage);
    prefs.chatWidth = "invalid" as never;
    prefs.density = "invalid" as never;
    prefs.textSize = "invalid" as never;
    prefs.sendKey = "invalid" as never;
    prefs.messageStyle = "invalid" as never;
    prefs.defaultAgent = "invalid" as never;
    prefs.summaryEngine = "invalid" as never;
    prefs.summaryModel = "bad model";
    expect(prefs.chatWidth).toBe("standard");
    expect(prefs.density).toBe("comfortable");
    expect(prefs.textSize).toBe("md");
    expect(prefs.sendKey).toBe("enter");
    expect(prefs.messageStyle).toBe("bubbles");
    expect(prefs.defaultAgent).toBe("claude");
    expect(prefs.summaryEngine).toBe("claude");
    expect(prefs.summaryModel).toBe("haiku");
  });

  it("supports live drag updates without writing until settled, plus resetWidths and reset", () => {
    const storage = new MemoryStorage();
    const prefs = createMaviPrefs(storage);
    prefs.chatWidth = "wide";
    prefs.autoScroll = false;
    const writesAfterPrefs = storage.writes;

    prefs.setRailWidthLive(310);
    prefs.setChangesWidthLive(500);
    expect(prefs.railWidth).toBe(310);
    expect(prefs.changesWidth).toBe(500);
    expect(storage.writes).toBe(writesAfterPrefs);

    prefs.setRailWidth(312);
    prefs.setChangesWidth(504);
    expect(storage.writes).toBe(writesAfterPrefs + 2);

    prefs.resetWidths();
    expect(prefs.railWidth).toBe(MAVI_PREF_DEFAULTS.railWidth);
    expect(prefs.changesWidth).toBe(MAVI_PREF_DEFAULTS.changesWidth);
    expect(prefs.chatWidth).toBe("wide");
    expect(prefs.autoScroll).toBe(false);

    prefs.reset();
    expect(prefs.snapshot()).toEqual(MAVI_PREF_DEFAULTS);
    expect(readMaviPrefs(storage)).toEqual(MAVI_PREF_DEFAULTS);
  });

  it("handles a throwing global localStorage accessor gracefully", () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      get() {
        throw new Error("SecurityError");
      },
    });
    try {
      const prefs = createMaviPrefs();
      expect(prefs.snapshot()).toEqual(MAVI_PREF_DEFAULTS);
      prefs.autoScroll = false;
      expect(prefs.autoScroll).toBe(false);
    } finally {
      if (original) Object.defineProperty(globalThis, "localStorage", original);
      else delete (globalThis as Record<string, unknown>).localStorage;
    }
  });
});
