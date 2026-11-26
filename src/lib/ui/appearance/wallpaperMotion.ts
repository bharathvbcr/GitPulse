/**
 * The moving wallpaper: whether it may move, and the one layer that moves it.
 *
 * A wallpaper moves only when the reader asked for it (`wallpaperMotion`) AND
 * the host has room for it: on mains power, thermally nominal or fair, no
 * memory pressure, and a one-minute load under {@link QUIET_LOAD} per core for
 * {@link QUIET_PULSES} sampler pulses running. It stops at once on battery, on
 * heat, on pressure, above {@link BUSY_LOAD}, or when the readings go missing or
 * stale. A reading that could not be taken is "unknown", which never moves:
 * this gate fails closed, and says which of its reasons held it.
 *
 * Reduce Motion (system or app), reduced transparency and a hidden window win
 * over the host: they stop the movement whatever the readings say.
 *
 * The readings are the sysload sampler's pulse (`hostLoadStore.ts`), counted
 * per pulse rather than per millisecond, and expired when pulses stop
 * arriving, so a last "plugged in, quiet" pulse cannot keep a video playing
 * after the sampler went away.
 *
 * Drawing: while it may move, one fixed layer under `body` (app.css,
 * `.gp-wallpaper-motion`) holds either the built-in's static textures, which
 * app.css moves with transform and opacity only, or the reader's video,
 * muted and looping, whose poster is the stored still. `data-backdrop-motion`
 * on the root hands the picture from `body` to the layer; the layer's first
 * frame is the still, so the hand-over is not visible. When movement stops the
 * video is paused and its decoder released, and the layer is removed.
 */
import { get, type Readable } from "svelte/store";
import { browserAdaptiveHost, createAdaptiveTimer, type AdaptiveHost } from "../../runtime/adaptiveTimer";
import { readHiddenDocument } from "../../runtime/foreground";
import type { HostLoadPulse } from "../../stores/hostLoadStore";
import { BACKDROP_MOTION, builtinBackdropUrl, isBuiltinBackdrop, motionTextureUrl, type BackdropId, type MotionKind } from "./backdrops";
import type { WallpaperState } from "./wallpaperStore";

/** Load per logical core at or under which the host counts as quiet. */
export const QUIET_LOAD = 0.5;
/** Load per logical core above which movement stops at once. */
export const BUSY_LOAD = 0.75;
/** Consecutive quiet pulses (about 2 s apart) before movement starts. */
export const QUIET_PULSES = 3;
/** A pulse older than this is no reading at all (five missed sampler ticks). */
export const PULSE_EXPIRY_MS = 10_000;
/** How often the controller re-checks for expired readings while movement is wanted. */
export const EXPIRY_CHECK_MS = 2_500;
/** The longest the re-check may stretch under event-loop load. */
export const EXPIRY_CHECK_MAX_MS = 5_000;

/**
 * The re-check's host: the shared adaptive timer, background only while the
 * window is HIDDEN. Unfocused but visible still paints the layer, so readings
 * must keep expiring then; hidden draws nothing (the gate removes the layer).
 */
function expiryHost(): AdaptiveHost | null {
  const base = browserAdaptiveHost();
  return base && { ...base, isHidden: () => readHiddenDocument() };
}

export type MotionReason =
  | "playing"
  | "settling"
  | "off"
  | "still"
  | "host"
  | "reduce-transparency"
  | "reduce-motion"
  | "hidden"
  | "refused"
  | "unknown"
  | "sensing-off"
  | "battery"
  | "thermal"
  | "pressure"
  | "busy";

export interface MotionInputs {
  /** The reader's switch. */
  readonly wanted: boolean;
  /** Whether the chosen wallpaper has motion: a built-in, or a custom video. */
  readonly moves: boolean;
  /** Whether wallpapers draw at all here (`native_material`). */
  readonly nativeMaterial: boolean;
  readonly reduceMotion: boolean;
  readonly reduceTransparency: boolean;
  readonly visible: boolean;
  /** This window refused to play the chosen video (an autoplay policy, an unplayable file). */
  readonly refused: boolean;
  readonly pulse: HostLoadPulse | null;
  /** Milliseconds since the last pulse arrived, or null when none has. */
  readonly pulseAgeMs: number | null;
  /** Increments once per pulse that arrived; hysteresis counts these. */
  readonly pulseSeq: number;
}

export interface MotionState {
  readonly play: boolean;
  readonly reason: MotionReason;
  /** Whether the host had room as of the last pulse counted. */
  readonly room: boolean;
  /** Consecutive quiet pulses seen while the host did not yet have room. */
  readonly quiet: number;
  /** The `pulseSeq` last counted, so a re-check between pulses counts nothing. */
  readonly seq: number;
  /** The load per core the decision saw, for the reason line. */
  readonly load: number | null;
}

export const MOTION_IDLE: MotionState = { play: false, reason: "off", room: false, quiet: 0, seq: 0, load: null };

type RoomReason = Extract<MotionReason, "unknown" | "sensing-off" | "battery" | "thermal" | "pressure" | "busy" | "settling" | "playing">;

/** The host half of the decision, with its hysteresis. Pure. */
function hostRoom(prev: MotionState, input: MotionInputs): { room: boolean; reason: RoomReason; quiet: number; load: number | null } {
  const pulse = input.pulse;
  const load = pulse?.load_per_core ?? null;
  const stop = (reason: RoomReason) => ({ room: false, reason, quiet: 0, load });
  if (pulse === null || input.pulseAgeMs === null || input.pulseAgeMs > PULSE_EXPIRY_MS) return stop("unknown");
  // Switched off by the reader: a different fix from a reading that failed.
  if (!pulse.enabled) return stop("sensing-off");
  if (pulse.state !== "fresh" && pulse.state !== "stale_recent") return stop("unknown");
  if (pulse.power == null) return stop("unknown");
  if (pulse.power !== "ac") return stop("battery");
  if (pulse.thermal == null) return stop("unknown");
  if (pulse.thermal === "serious" || pulse.thermal === "critical") return stop("thermal");
  if (pulse.pressure === "unavailable") return stop("unknown");
  if (pulse.pressure !== "normal" || pulse.compressor_thrash) return stop("pressure");
  if (load === null) return stop("unknown");
  if (load > BUSY_LOAD) return stop("busy");
  if (prev.room) return { room: true, reason: "playing", quiet: 0, load };
  if (load > QUIET_LOAD) return stop("busy");
  const quiet = input.pulseSeq !== prev.seq ? prev.quiet + 1 : prev.quiet;
  return quiet >= QUIET_PULSES ? { room: true, reason: "playing", quiet: 0, load } : { room: false, reason: "settling", quiet, load };
}

/**
 * The next state from the last one and what holds now. Pure: the controller
 * calls it on every input change and every expiry check.
 */
export function decideMotion(prev: MotionState, input: MotionInputs): MotionState {
  const host = hostRoom(prev, input);
  const base = { room: host.room, quiet: host.quiet, seq: input.pulseSeq, load: host.load };
  const held = (reason: MotionReason): MotionState => ({ ...base, play: false, reason });
  if (!input.wanted) return held("off");
  if (!input.nativeMaterial) return held("host");
  if (!input.moves) return held("still");
  if (input.reduceTransparency) return held("reduce-transparency");
  if (input.reduceMotion) return held("reduce-motion");
  if (!input.visible) return held("hidden");
  if (input.refused) return held("refused");
  return host.room ? { ...base, play: true, reason: "playing" } : held(host.reason);
}

/** The settings row's line for a state; null where the row shows nothing. */
export function motionReasonText(state: MotionState): string | null {
  const load = state.load === null ? "" : ` (load ${state.load.toFixed(2)} per core)`;
  switch (state.reason) {
    case "off":
    case "host":
      return null;
    case "playing":
      return "Moving now: plugged in and the system is quiet.";
    case "settling":
      return "Plugged in and quiet: it starts moving in a few seconds.";
    case "still":
      return "This wallpaper is a still image, so it has nothing to animate.";
    case "reduce-transparency":
      return "Still: transparency is reduced, so the wallpaper is hidden.";
    case "reduce-motion":
      return "Still: Reduce Motion is on.";
    case "hidden":
      return "Still while the window is hidden.";
    case "refused":
      return "Still: this window wouldn't play the video, so its first frame is shown instead.";
    case "unknown":
      return "Still: the power source or system load can't be read right now, so it doesn't move.";
    case "sensing-off":
      return "Still: host load sampling is off, so Waylode can't tell whether the Mac has room. Turn it on in Settings, Agents, Host load.";
    case "battery":
      return "Still: running on battery. It moves again when plugged in.";
    case "thermal":
      return "Still: the Mac is running hot.";
    case "pressure":
      return "Still: memory is under pressure.";
    case "busy":
      return `Still: the system is busy${load}. It moves again once load stays under ${QUIET_LOAD} per core.`;
  }
}

// --- the layer ---------------------------------------------------------------

export type MotionPlan =
  | { readonly kind: "builtin"; readonly key: string; readonly layers: readonly MotionLayerPlan[] }
  | { readonly kind: "video"; readonly key: string; readonly src: string; readonly poster: string };

export interface MotionLayerPlan {
  readonly kind: MotionKind;
  readonly url: string;
  readonly seconds: number;
  readonly delay: number;
}

/** What the layer draws for a wallpaper choice, or null when it has no motion. */
export function motionPlan(backdrop: BackdropId, custom: WallpaperState): MotionPlan | null {
  if (isBuiltinBackdrop(backdrop)) {
    return {
      kind: "builtin",
      key: backdrop,
      layers: BACKDROP_MOTION[backdrop].map((layer) => ({
        kind: layer.kind,
        url: layer.texture === "base" ? builtinBackdropUrl(backdrop) : motionTextureUrl(layer.texture),
        seconds: layer.seconds,
        delay: layer.delay ?? 0,
      })),
    };
  }
  if (backdrop === "custom" && custom.status === "ready" && custom.videoUrl !== null) {
    return { kind: "video", key: custom.videoUrl, src: custom.videoUrl, poster: custom.url };
  }
  return null;
}

export const MOTION_ATTRIBUTE = "data-backdrop-motion";
export const MOTION_LAYER_CLASS = "gp-wallpaper-motion";

/** Only our own data: and blob: URLs reach a style or a src. */
const SAFE_URL = /^(?:data:image\/svg\+xml,|blob:)[^"\\\s]*$/;

function releaseVideo(layer: Element): void {
  for (const video of layer.querySelectorAll("video")) {
    video.pause();
    video.removeAttribute("src");
    video.load();
  }
}

function buildLayer(doc: Document, plan: MotionPlan): HTMLElement {
  const layer = doc.createElement("div");
  layer.className = MOTION_LAYER_CLASS;
  layer.setAttribute("aria-hidden", "true");
  layer.dataset.wallpaperMotion = plan.kind === "video" ? "video" : plan.key;
  if (plan.kind === "video") {
    const video = doc.createElement("video");
    video.muted = true;
    video.defaultMuted = true;
    video.loop = true;
    video.playsInline = true;
    video.autoplay = true;
    video.controls = false;
    video.disablePictureInPicture = true;
    video.setAttribute("muted", "");
    video.setAttribute("playsinline", "");
    if (SAFE_URL.test(plan.poster)) video.poster = plan.poster;
    if (SAFE_URL.test(plan.src)) video.src = plan.src;
    layer.append(video);
    return layer;
  }
  for (const entry of plan.layers) {
    if (!SAFE_URL.test(entry.url)) continue;
    const div = doc.createElement("div");
    div.className = entry.kind === "drift" ? "gp-wp-drift" : `gp-wp-accent gp-wp-${entry.kind}`;
    div.style.backgroundImage = `url("${entry.url}")`;
    div.style.setProperty("--gp-wp-dur", `${entry.seconds}s`);
    if (entry.delay > 0) div.style.setProperty("--gp-wp-delay", `${entry.delay}s`);
    layer.append(div);
  }
  return layer;
}

/**
 * Puts the document in step with one decision: the layer present and the root
 * attribute set while `plan` plays, both gone otherwise. Rebuilds the layer only
 * when the plan's key changes.
 */
export function renderMotion(
  doc: Document,
  plan: MotionPlan | null,
  onRefused: (src: string) => void = () => {},
): void {
  const root = doc.documentElement;
  const existing = doc.querySelector(`body > .${MOTION_LAYER_CLASS}`);
  if (plan === null) {
    root.removeAttribute(MOTION_ATTRIBUTE);
    if (existing) {
      releaseVideo(existing);
      existing.remove();
    }
    return;
  }
  const key = plan.kind === "video" ? "video" : plan.key;
  let layer = existing as HTMLElement | null;
  if (layer && (layer.dataset.wallpaperMotion !== key || (plan.kind === "video" && layer.querySelector("video")?.getAttribute("src") !== plan.src))) {
    releaseVideo(layer);
    layer.remove();
    layer = null;
  }
  if (!layer) {
    // Before the body exists there is nowhere to draw; the next input change draws it.
    if (!doc.body) return;
    layer = buildLayer(doc, plan);
    // First in body: beneath every in-flow box, outside the shell.
    doc.body.prepend(layer);
  }
  for (const video of layer.querySelectorAll("video")) {
    if (!video.paused) continue;
    const src = video.getAttribute("src") ?? "";
    void video.play().catch((error: unknown) => {
      // Removing the layer mid-start aborts the play; only a refusal is one.
      const name = error instanceof DOMException ? error.name : "";
      if (name === "NotAllowedError" || name === "NotSupportedError") onRefused(src);
    });
  }
  root.setAttribute(MOTION_ATTRIBUTE, "");
}

// --- the controller ----------------------------------------------------------

export interface MotionSources {
  readonly prefs: Readable<{
    readonly backdrop: BackdropId;
    readonly wallpaperMotion: boolean;
    /** The app's own Reduce Motion, read here so the gate needs no component to stamp it. */
    readonly reduceMotion: boolean;
  }>;
  readonly platform: Readable<{ readonly native_material: boolean }>;
  readonly custom: Readable<WallpaperState>;
  readonly pulse: Readable<HostLoadPulse | null>;
  /** Starts the sampler's pulse; called once movement is wanted. Idempotent. */
  readonly startPulse: () => void;
  readonly doc?: Document | null;
  readonly now?: () => number;
}

const stateListeners = new Set<(state: MotionState) => void>();
let current: MotionState = MOTION_IDLE;

function setState(next: MotionState): void {
  current = next;
  for (const listener of stateListeners) listener(next);
}

/** The last decision, for the settings row. */
export const wallpaperMotionState: Readable<MotionState> = {
  subscribe(run) {
    run(current);
    stateListeners.add(run);
    return () => stateListeners.delete(run);
  },
};

/**
 * Keeps the moving layer in step with the stores, the window and the host.
 * Returns the stop. Outside a document it does nothing.
 */
export function installWallpaperMotion(sources: MotionSources): () => void {
  const doc = sources.doc ?? (typeof document === "undefined" ? null : document);
  // No document, or a stand-in without a real root (a unit test's stub): nothing to draw on.
  if (!doc || typeof doc.documentElement?.getAttribute !== "function") return () => {};
  const now = sources.now ?? (() => Date.now());
  const root = doc.documentElement;
  const view = doc.defaultView;
  const media = (query: string) => (view && typeof view.matchMedia === "function" ? view.matchMedia(query) : null);
  const systemReduceMotion = media("(prefers-reduced-motion: reduce)");
  const systemReduceTransparency = media("(prefers-reduced-transparency: reduce)");

  let pulseAt: number | null = null;
  let pulseSeq = 0;
  let stopTimer: (() => void) | null = null;
  /** The video src this window refused to play; cleared when another is chosen. */
  let refusedSrc: string | null = null;
  let state = MOTION_IDLE;

  const evaluate = () => {
    const prefs = get(sources.prefs);
    const custom = get(sources.custom);
    const plan = motionPlan(prefs.backdrop, custom);
    const wanted = prefs.wallpaperMotion && prefs.backdrop !== "none";
    // Only a window that draws the wallpaper moves it: the main window carries
    // `html.macos` (main.ts); the menu-bar popover loads these stores but never
    // draws a wallpaper, and must not decode a video nobody can see.
    const drawsHere = get(sources.platform).native_material && root.classList.contains("macos");
    if (wanted && plan !== null && drawsHere) sources.startPulse();
    state = decideMotion(state, {
      wanted,
      moves: plan !== null,
      nativeMaterial: drawsHere,
      reduceMotion:
        prefs.reduceMotion || root.getAttribute("data-motion") === "reduced" || systemReduceMotion?.matches === true,
      reduceTransparency:
        root.getAttribute("data-transparency") === "reduced" || systemReduceTransparency?.matches === true,
      visible: doc.visibilityState !== "hidden",
      refused: plan?.kind === "video" && plan.src === refusedSrc,
      pulse: get(sources.pulse),
      pulseAgeMs: pulseAt === null ? null : Math.max(0, now() - pulseAt),
      pulseSeq,
    });
    renderMotion(doc, state.play ? plan : null, (src) => {
      refusedSrc = src;
      evaluate();
    });
    setState(state);
    // Expiry needs a clock only while movement is wanted.
    if (wanted && plan !== null && drawsHere && stopTimer === null) {
      stopTimer = createAdaptiveTimer(evaluate, EXPIRY_CHECK_MS, expiryHost(), { maxMs: EXPIRY_CHECK_MAX_MS });
    } else if (!(wanted && plan !== null && drawsHere) && stopTimer !== null) {
      stopTimer();
      stopTimer = null;
    }
  };

  const stops: (() => void)[] = [];
  stops.push(sources.prefs.subscribe(() => evaluate()));
  stops.push(sources.platform.subscribe(() => evaluate()));
  stops.push(sources.custom.subscribe(() => evaluate()));
  stops.push(
    sources.pulse.subscribe((pulse) => {
      if (pulse !== null) {
        pulseAt = now();
        pulseSeq += 1;
      }
      evaluate();
    }),
  );
  const onChange = () => evaluate();
  doc.addEventListener("visibilitychange", onChange);
  stops.push(() => doc.removeEventListener("visibilitychange", onChange));
  for (const query of [systemReduceMotion, systemReduceTransparency]) {
    if (!query) continue;
    query.addEventListener("change", onChange);
    stops.push(() => query.removeEventListener("change", onChange));
  }
  if (typeof MutationObserver !== "undefined") {
    const observer = new MutationObserver(onChange);
    observer.observe(root, { attributes: true, attributeFilter: ["class", "data-motion", "data-transparency"] });
    stops.push(() => observer.disconnect());
  }
  return () => {
    for (const stop of stops) stop();
    stopTimer?.();
    renderMotion(doc, null);
  };
}
