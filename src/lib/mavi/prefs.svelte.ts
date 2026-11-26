/**
 * Persistent UI preferences for Mavi: panel widths, conversation layout,
 * timeline visibility toggles, and composer send-key behavior.
 *
 * Stored defensively in localStorage so corrupt or out-of-range values
 * never poison layout geometry or throw when storage is unavailable.
 */

export const MAVI_PREFS_STORAGE_KEY = "gitpulse.mavi.prefs.v1";

export const MAVI_RAIL_WIDTH_BOUNDS = {
  min: 180,
  max: 380,
  default: 248,
} as const;

export const MAVI_CHANGES_WIDTH_BOUNDS = {
  min: 280,
  max: 640,
  default: 380,
} as const;

export const MAVI_RESIZE_STEP = 16;

export type MaviChatWidth = "compact" | "standard" | "wide" | "full";
export type MaviDensity = "compact" | "comfortable" | "spacious";
export type MaviTextSize = "sm" | "md" | "lg";
export type MaviSendKey = "enter" | "mod_enter";
export type MaviMessageStyle = "bubbles" | "editorial";
export type MaviDefaultAgent = "claude" | "agy";
export type MaviSummaryEngine = "claude" | "ai";

export interface MaviPrefs {
  railWidth: number;
  changesWidth: number;
  chatWidth: MaviChatWidth;
  density: MaviDensity;
  textSize: MaviTextSize;
  autoScroll: boolean;
  autoOpenChanges: boolean;
  showThinking: boolean;
  expandTools: boolean;
  showTurnMeta: boolean;
  sendKey: MaviSendKey;
  messageStyle: MaviMessageStyle;
  defaultAgent: MaviDefaultAgent;
  summarizeOnEnd: boolean;
  summaryEngine: MaviSummaryEngine;
  summaryModel: string;
}

export const MAVI_PREF_DEFAULTS: Readonly<MaviPrefs> = {
  railWidth: MAVI_RAIL_WIDTH_BOUNDS.default,
  changesWidth: MAVI_CHANGES_WIDTH_BOUNDS.default,
  chatWidth: "standard",
  density: "comfortable",
  textSize: "md",
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
  summaryModel: "haiku",
};

export const MAVI_CHAT_WIDTH_CSS: Readonly<Record<MaviChatWidth, string>> = {
  compact: "44rem",
  standard: "52rem",
  wide: "66rem",
  full: "100%",
};

export const MAVI_DENSITY_CSS: Readonly<
  Record<MaviDensity, { readonly turnGap: string; readonly rowGap: string; readonly bubblePad: string }>
> = {
  compact: { turnGap: "16px", rowGap: "6px", bubblePad: "7px 11px" },
  comfortable: { turnGap: "24px", rowGap: "10px", bubblePad: "10px 14px" },
  spacious: { turnGap: "32px", rowGap: "14px", bubblePad: "13px 18px" },
};

export type StorageLike = Pick<Storage, "getItem" | "setItem">;

function defaultStorage(): StorageLike | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function clampWidth(px: number, min: number, max: number, fallback: number): number {
  if (typeof px !== "number" || !Number.isFinite(px)) return fallback;
  if (px < min) return min;
  if (px > max) return max;
  return Math.round(px);
}

export function clampRailWidth(px: number): number {
  return clampWidth(px, MAVI_RAIL_WIDTH_BOUNDS.min, MAVI_RAIL_WIDTH_BOUNDS.max, MAVI_RAIL_WIDTH_BOUNDS.default);
}

export function clampChangesWidth(px: number): number {
  return clampWidth(
    px,
    MAVI_CHANGES_WIDTH_BOUNDS.min,
    MAVI_CHANGES_WIDTH_BOUNDS.max,
    MAVI_CHANGES_WIDTH_BOUNDS.default,
  );
}

function isChatWidth(value: unknown): value is MaviChatWidth {
  return value === "compact" || value === "standard" || value === "wide" || value === "full";
}

function isDensity(value: unknown): value is MaviDensity {
  return value === "compact" || value === "comfortable" || value === "spacious";
}

function isTextSize(value: unknown): value is MaviTextSize {
  return value === "sm" || value === "md" || value === "lg";
}

function isSendKey(value: unknown): value is MaviSendKey {
  return value === "enter" || value === "mod_enter";
}

function isMessageStyle(value: unknown): value is MaviMessageStyle {
  return value === "bubbles" || value === "editorial";
}

function isDefaultAgent(value: unknown): value is MaviDefaultAgent {
  return value === "claude" || value === "agy";
}

function isSummaryEngine(value: unknown): value is MaviSummaryEngine {
  return value === "claude" || value === "ai";
}

export function isValidSummaryModel(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= 120 &&
    !/\s/.test(value) &&
    !value.startsWith("-")
  );
}

function parseBool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

export function parseMaviPrefs(raw: unknown): MaviPrefs {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return { ...MAVI_PREF_DEFAULTS };
  }
  const obj = raw as Record<string, unknown>;
  return {
    railWidth: clampRailWidth(obj.railWidth as number),
    changesWidth: clampChangesWidth(obj.changesWidth as number),
    chatWidth: isChatWidth(obj.chatWidth) ? obj.chatWidth : MAVI_PREF_DEFAULTS.chatWidth,
    density: isDensity(obj.density) ? obj.density : MAVI_PREF_DEFAULTS.density,
    textSize: isTextSize(obj.textSize) ? obj.textSize : MAVI_PREF_DEFAULTS.textSize,
    autoScroll: parseBool(obj.autoScroll, MAVI_PREF_DEFAULTS.autoScroll),
    autoOpenChanges: parseBool(obj.autoOpenChanges, MAVI_PREF_DEFAULTS.autoOpenChanges),
    showThinking: parseBool(obj.showThinking, MAVI_PREF_DEFAULTS.showThinking),
    expandTools: parseBool(obj.expandTools, MAVI_PREF_DEFAULTS.expandTools),
    showTurnMeta: parseBool(obj.showTurnMeta, MAVI_PREF_DEFAULTS.showTurnMeta),
    sendKey: isSendKey(obj.sendKey) ? obj.sendKey : MAVI_PREF_DEFAULTS.sendKey,
    messageStyle: isMessageStyle(obj.messageStyle) ? obj.messageStyle : MAVI_PREF_DEFAULTS.messageStyle,
    defaultAgent: isDefaultAgent(obj.defaultAgent) ? obj.defaultAgent : MAVI_PREF_DEFAULTS.defaultAgent,
    summarizeOnEnd: parseBool(obj.summarizeOnEnd, MAVI_PREF_DEFAULTS.summarizeOnEnd),
    summaryEngine: isSummaryEngine(obj.summaryEngine) ? obj.summaryEngine : MAVI_PREF_DEFAULTS.summaryEngine,
    summaryModel: isValidSummaryModel(obj.summaryModel) ? obj.summaryModel : MAVI_PREF_DEFAULTS.summaryModel,
  };
}

export function readMaviPrefs(storage: StorageLike | null): MaviPrefs {
  if (!storage) return { ...MAVI_PREF_DEFAULTS };
  try {
    const text = storage.getItem(MAVI_PREFS_STORAGE_KEY);
    if (!text) return { ...MAVI_PREF_DEFAULTS };
    return parseMaviPrefs(JSON.parse(text));
  } catch {
    return { ...MAVI_PREF_DEFAULTS };
  }
}

export function writeMaviPrefs(storage: StorageLike | null, prefs: MaviPrefs): void {
  if (!storage) return;
  try {
    storage.setItem(MAVI_PREFS_STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    /* storage full or unavailable: keep in-memory prefs */
  }
}

export class MaviPrefsStore {
  #storage: StorageLike | null;
  #state = $state<MaviPrefs>({ ...MAVI_PREF_DEFAULTS });

  constructor(storage: StorageLike | null = defaultStorage()) {
    this.#storage = storage;
    this.#state = readMaviPrefs(storage);
  }

  #persist(): void {
    writeMaviPrefs(this.#storage, { ...this.#state });
  }

  get railWidth(): number {
    return this.#state.railWidth;
  }
  set railWidth(value: number) {
    this.setRailWidth(value);
  }

  get changesWidth(): number {
    return this.#state.changesWidth;
  }
  set changesWidth(value: number) {
    this.setChangesWidth(value);
  }

  get chatWidth(): MaviChatWidth {
    return this.#state.chatWidth;
  }
  set chatWidth(value: MaviChatWidth) {
    this.#state.chatWidth = isChatWidth(value) ? value : MAVI_PREF_DEFAULTS.chatWidth;
    this.#persist();
  }

  get density(): MaviDensity {
    return this.#state.density;
  }
  set density(value: MaviDensity) {
    this.#state.density = isDensity(value) ? value : MAVI_PREF_DEFAULTS.density;
    this.#persist();
  }

  get textSize(): MaviTextSize {
    return this.#state.textSize;
  }
  set textSize(value: MaviTextSize) {
    this.#state.textSize = isTextSize(value) ? value : MAVI_PREF_DEFAULTS.textSize;
    this.#persist();
  }

  get autoScroll(): boolean {
    return this.#state.autoScroll;
  }
  set autoScroll(value: boolean) {
    this.#state.autoScroll = Boolean(value);
    this.#persist();
  }

  get autoOpenChanges(): boolean {
    return this.#state.autoOpenChanges;
  }
  set autoOpenChanges(value: boolean) {
    this.#state.autoOpenChanges = Boolean(value);
    this.#persist();
  }

  get showThinking(): boolean {
    return this.#state.showThinking;
  }
  set showThinking(value: boolean) {
    this.#state.showThinking = Boolean(value);
    this.#persist();
  }

  get expandTools(): boolean {
    return this.#state.expandTools;
  }
  set expandTools(value: boolean) {
    this.#state.expandTools = Boolean(value);
    this.#persist();
  }

  get showTurnMeta(): boolean {
    return this.#state.showTurnMeta;
  }
  set showTurnMeta(value: boolean) {
    this.#state.showTurnMeta = Boolean(value);
    this.#persist();
  }

  get sendKey(): MaviSendKey {
    return this.#state.sendKey;
  }
  set sendKey(value: MaviSendKey) {
    this.#state.sendKey = isSendKey(value) ? value : MAVI_PREF_DEFAULTS.sendKey;
    this.#persist();
  }

  get messageStyle(): MaviMessageStyle {
    return this.#state.messageStyle;
  }
  set messageStyle(value: MaviMessageStyle) {
    this.#state.messageStyle = isMessageStyle(value) ? value : MAVI_PREF_DEFAULTS.messageStyle;
    this.#persist();
  }

  get defaultAgent(): MaviDefaultAgent {
    return this.#state.defaultAgent;
  }
  set defaultAgent(value: MaviDefaultAgent) {
    this.#state.defaultAgent = isDefaultAgent(value) ? value : MAVI_PREF_DEFAULTS.defaultAgent;
    this.#persist();
  }

  get summarizeOnEnd(): boolean {
    return this.#state.summarizeOnEnd;
  }
  set summarizeOnEnd(value: boolean) {
    this.#state.summarizeOnEnd = Boolean(value);
    this.#persist();
  }

  get summaryEngine(): MaviSummaryEngine {
    return this.#state.summaryEngine;
  }
  set summaryEngine(value: MaviSummaryEngine) {
    this.#state.summaryEngine = isSummaryEngine(value) ? value : MAVI_PREF_DEFAULTS.summaryEngine;
    this.#persist();
  }

  get summaryModel(): string {
    return this.#state.summaryModel;
  }
  set summaryModel(value: string) {
    this.#state.summaryModel = isValidSummaryModel(value) ? value : MAVI_PREF_DEFAULTS.summaryModel;
    this.#persist();
  }

  /** Live resize preview during pointer drag without hammering localStorage. */
  setRailWidthLive(px: number): void {
    this.#state.railWidth = clampRailWidth(px);
  }

  setRailWidth(px: number): void {
    this.#state.railWidth = clampRailWidth(px);
    this.#persist();
  }

  /** Live resize preview during pointer drag without hammering localStorage. */
  setChangesWidthLive(px: number): void {
    this.#state.changesWidth = clampChangesWidth(px);
  }

  setChangesWidth(px: number): void {
    this.#state.changesWidth = clampChangesWidth(px);
    this.#persist();
  }

  resetWidths(): void {
    this.#state.railWidth = MAVI_PREF_DEFAULTS.railWidth;
    this.#state.changesWidth = MAVI_PREF_DEFAULTS.changesWidth;
    this.#persist();
  }

  reset(): void {
    this.#state = { ...MAVI_PREF_DEFAULTS };
    this.#persist();
  }

  snapshot(): MaviPrefs {
    return { ...this.#state };
  }
}

export function createMaviPrefs(storage: StorageLike | null = defaultStorage()): MaviPrefsStore {
  return new MaviPrefsStore(storage);
}

export const maviPrefs = createMaviPrefs();
