import type { VisualCommitRow } from "../GraphRenderer";

/** Recording 2D context capturing the ops these regressions assert on. */
export function makeRecordingCtx(height = 800) {
  const calls: Array<Record<string, unknown>> = [];
  const ctx: Record<string, unknown> = {
    canvas: { height: height, width: 400 },
    save() {
      calls.push({ op: "save" });
    },
    restore() {
      calls.push({ op: "restore" });
    },
    beginPath() {
      calls.push({ op: "beginPath" });
    },
    arc(x: number, y: number, r: number) {
      calls.push({ op: "arc", x, y, r });
    },
    fill() {
      calls.push({ op: "fill", style: ctx.fillStyle });
    },
    stroke() {
      calls.push({ op: "stroke", style: ctx.strokeStyle, width: ctx.lineWidth });
    },
    fillRect(x: number, y: number, w: number, h: number) {
      calls.push({ op: "fillRect", x, y, w, h, style: ctx.fillStyle });
    },
    moveTo(x: number, y: number) {
      calls.push({ op: "moveTo", x, y });
    },
    lineTo(x: number, y: number) {
      calls.push({ op: "lineTo", x, y });
    },
    bezierCurveTo(a: number, b: number, c: number, d: number) {
      calls.push({ op: "bezier", a, b, c, d });
    },
    fillText(text: string, x: number, y: number) {
      calls.push({ op: "fillText", text, x, y, font: ctx.font, style: ctx.fillStyle });
    },
    setTransform() {},
    globalAlpha: 1,
    lineWidth: 1,
    strokeStyle: "#000",
    fillStyle: "#000",
    font: "",
    textAlign: "left",
    textBaseline: "alphabetic",
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, calls };
}

export function row(overrides: Partial<VisualCommitRow> & { id: string }): VisualCommitRow {
  return {
    parent_ids: [],
    summary: "s",
    author_name: "Ada Lovelace",
    author_email: "ada@example.com",
    timestamp: 100,
    lane: 0,
    color_index: 0,
    active_lanes: [0],
    active_lane_colors: [0],
    connections: [],
    is_merge: false,
    is_root: false,
    ...overrides,
  };
}

