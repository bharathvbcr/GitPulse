import { describe, expect, it } from "vitest";
import type { HostLoadPulse } from "../../stores/hostLoadStore";
import { BACKDROP_MOTION, BUILTIN_BACKDROP_IDS, builtinBackdropUrl } from "./backdrops";
import {
  BUSY_LOAD,
  MOTION_IDLE,
  PULSE_EXPIRY_MS,
  QUIET_LOAD,
  QUIET_PULSES,
  decideMotion,
  motionPlan,
  motionReasonText,
  type MotionInputs,
  type MotionState,
} from "./wallpaperMotion";

const quietPulse: HostLoadPulse = {
  state: "fresh",
  pressure: "normal",
  level: "normal",
  age_ms: 500,
  enabled: true,
  compressor_thrash: false,
  role: "writer",
  governor: {
    admit: true,
    shed_background: false,
    shed_reactive: false,
    binding: null,
    refusing_for_s: null,
    notice: false,
    overridden: false,
    largest_foreign: null,
  },
  power: "ac",
  thermal: "nominal",
  load_per_core: 0.2,
};

function inputs(over: Partial<MotionInputs> = {}): MotionInputs {
  return {
    wanted: true,
    moves: true,
    nativeMaterial: true,
    reduceMotion: false,
    reduceTransparency: false,
    visible: true,
    refused: false,
    pulse: quietPulse,
    pulseAgeMs: 100,
    pulseSeq: 1,
    ...over,
  };
}

/** Feeds `count` fresh pulses of `pulse` through the gate, as the controller would. */
function feed(state: MotionState, count: number, over: Partial<MotionInputs> = {}): MotionState {
  let next = state;
  for (let i = 0; i < count; i += 1) next = decideMotion(next, inputs({ pulseSeq: next.seq + 1, ...over }));
  return next;
}

const playing = (): MotionState => feed(MOTION_IDLE, QUIET_PULSES);

describe("decideMotion: starting", () => {
  it(`starts only after ${QUIET_PULSES} quiet pulses, settling until then`, () => {
    let state = MOTION_IDLE;
    for (let i = 1; i < QUIET_PULSES; i += 1) {
      state = feed(state, 1);
      expect(state.play).toBe(false);
      expect(state.reason).toBe("settling");
    }
    state = feed(state, 1);
    expect(state.play).toBe(true);
    expect(state.reason).toBe("playing");
  });

  it("counts pulses, not re-checks: the same pulse evaluated again adds nothing", () => {
    let state = feed(MOTION_IDLE, 1);
    for (let i = 0; i < 10; i += 1) state = decideMotion(state, inputs({ pulseSeq: state.seq }));
    expect(state.play).toBe(false);
    expect(state.quiet).toBe(1);
  });

  it("a pulse between quiet and busy resets the count rather than starting", () => {
    let state = feed(MOTION_IDLE, QUIET_PULSES - 1);
    state = feed(state, 1, { pulse: { ...quietPulse, load_per_core: (QUIET_LOAD + BUSY_LOAD) / 2 } });
    expect(state.play).toBe(false);
    expect(state.quiet).toBe(0);
    expect(state.reason).toBe("busy");
  });
});

describe("decideMotion: hysteresis once playing", () => {
  it("keeps playing between the quiet and busy marks", () => {
    const state = feed(playing(), 5, { pulse: { ...quietPulse, load_per_core: BUSY_LOAD } });
    expect(state.play).toBe(true);
  });

  it("stops at once above the busy mark, and needs the quiet run again to restart", () => {
    let state = feed(playing(), 1, { pulse: { ...quietPulse, load_per_core: BUSY_LOAD + 0.01 } });
    expect(state.play).toBe(false);
    expect(state.reason).toBe("busy");
    state = feed(state, 1);
    expect(state.play).toBe(false);
    state = feed(state, QUIET_PULSES - 1);
    expect(state.play).toBe(true);
  });
});

describe("decideMotion: fails closed on the host", () => {
  const cases: [string, Partial<HostLoadPulse>, MotionState["reason"]][] = [
    ["on battery", { power: "battery" }, "battery"],
    ["no power reading", { power: null }, "unknown"],
    ["a backend that sends no power field", { power: undefined }, "unknown"],
    ["serious heat", { thermal: "serious" }, "thermal"],
    ["critical heat", { thermal: "critical" }, "thermal"],
    ["no thermal reading", { thermal: null }, "unknown"],
    ["memory pressure", { pressure: "warn" }, "pressure"],
    ["compressor thrash", { compressor_thrash: true }, "pressure"],
    ["pressure unreadable", { pressure: "unavailable" }, "unknown"],
    ["no load reading", { load_per_core: null }, "unknown"],
    ["a stale sensor", { state: "stale_old" }, "unknown"],
    ["a dead sensor", { state: "dead" }, "unknown"],
    ["sampling switched off by the reader", { enabled: false }, "sensing-off"],
  ];
  for (const [label, change, reason] of cases) {
    it(`${label}: stops a playing wallpaper at once (${reason})`, () => {
      const state = feed(playing(), 1, { pulse: { ...quietPulse, ...change } });
      expect(state.play).toBe(false);
      expect(state.reason).toBe(reason);
      expect(state.room).toBe(false);
    });
  }

  it("with no pulse at all it never starts", () => {
    const state = feed(MOTION_IDLE, 10, { pulse: null, pulseAgeMs: null });
    expect(state.play).toBe(false);
    expect(state.reason).toBe("unknown");
  });

  it("expires a pulse that stopped arriving: the last quiet reading does not keep it moving", () => {
    const fresh = playing();
    const stale = decideMotion(fresh, inputs({ pulseSeq: fresh.seq, pulseAgeMs: PULSE_EXPIRY_MS + 1 }));
    expect(stale.play).toBe(false);
    expect(stale.reason).toBe("unknown");
  });
});

describe("decideMotion: the reader and the window win over the host", () => {
  const cases: [string, Partial<MotionInputs>, MotionState["reason"]][] = [
    ["switched off", { wanted: false }, "off"],
    ["no glass material here", { nativeMaterial: false }, "host"],
    ["a still image", { moves: false }, "still"],
    ["Reduce Transparency", { reduceTransparency: true }, "reduce-transparency"],
    ["Reduce Motion", { reduceMotion: true }, "reduce-motion"],
    ["a hidden window", { visible: false }, "hidden"],
    ["a video this window refused to play", { refused: true }, "refused"],
  ];
  for (const [label, change, reason] of cases) {
    it(`${label} stops it on a quiet, plugged-in host`, () => {
      const state = feed(playing(), 1, change);
      expect(state.play).toBe(false);
      expect(state.reason).toBe(reason);
    });
  }

  it("keeps the host's room through a hidden spell, so it moves again on return without settling", () => {
    const hidden = feed(playing(), 3, { visible: false });
    expect(hidden.play).toBe(false);
    expect(hidden.room).toBe(true);
    expect(feed(hidden, 1).play).toBe(true);
  });
});

describe("motionReasonText", () => {
  it("names the busy load and the mark it waits for", () => {
    const state = feed(MOTION_IDLE, 1, { pulse: { ...quietPulse, load_per_core: 2.13 } });
    expect(motionReasonText(state)).toContain("load 2.13 per core");
    expect(motionReasonText(state)).toContain(String(QUIET_LOAD));
  });

  it("says nothing when the switch is off", () => {
    expect(motionReasonText(feed(MOTION_IDLE, 1, { wanted: false }))).toBeNull();
  });

  it("has a line for every reason a reader can see", () => {
    for (const reason of ["playing", "settling", "still", "reduce-transparency", "reduce-motion", "hidden", "refused", "unknown", "sensing-off", "battery", "thermal", "pressure", "busy"] as const) {
      expect(motionReasonText({ ...MOTION_IDLE, reason })?.length ?? 0, reason).toBeGreaterThan(0);
    }
  });
});

describe("motionPlan", () => {
  it.each(BUILTIN_BACKDROP_IDS.map((id) => [id] as const))("plans every layer of %s, the base being the still", (id) => {
    const plan = motionPlan(id, { status: "none" });
    expect(plan?.kind).toBe("builtin");
    if (plan?.kind !== "builtin") return;
    expect(plan.layers).toHaveLength(BACKDROP_MOTION[id].length);
    const base = BACKDROP_MOTION[id].findIndex((layer) => layer.texture === "base");
    if (base !== -1) expect(plan.layers[base].url).toBe(builtinBackdropUrl(id));
  });

  it("plays a custom video over its still, and has nothing for a custom image or none", () => {
    const ready = { status: "ready" as const, url: "blob:still", name: "loop.mp4", width: 10, height: 10 };
    expect(motionPlan("custom", { ...ready, videoUrl: "blob:video" })).toEqual({
      kind: "video",
      key: "blob:video",
      src: "blob:video",
      poster: "blob:still",
    });
    expect(motionPlan("custom", { ...ready, videoUrl: null })).toBeNull();
    expect(motionPlan("none", { status: "none" })).toBeNull();
    expect(motionPlan("custom", { status: "loading" })).toBeNull();
  });
});
