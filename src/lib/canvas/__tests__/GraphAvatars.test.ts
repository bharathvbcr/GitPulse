import { describe, expect, it } from "vitest";
import {
  AVATAR_STYLES,
  AVATAR_HIT_SLOP,
  GraphRenderer,
} from "../GraphRenderer";
import { authorIdentity } from "../../authors/authorIdentity";
import { makeRecordingCtx, row } from "./recordingCtx";

const THEME = {
  background: "#ffffff",
  nodeStroke: "#dddddd",
  selection: "#0000ff",
  head: "#111111",
  muted: "#888888",
};

describe("author-avatar column rendering", () => {
  const AVATAR_X = 200;

  it("draws disc + ring + initials per visible row when enabled", () => {
    const renderer = new GraphRenderer();
    const rows = [row({ id: "a" }), row({ id: "b" })];
    const { ctx, calls } = makeRecordingCtx();
    renderer.render(ctx, rows, 0, 2, 0, undefined, {
      theme: THEME,
      viewportHeight: 800,
      showAvatars: true,
      avatarX: AVATAR_X,
    });

    const identity = authorIdentity("Ada Lovelace", "ada@example.com");
    const discs = calls.filter((c) => c.op === "arc" && Math.abs((c.x as number) - AVATAR_X) < 0.01);
    // Outer ring + body per row.
    expect(discs.length).toBe(rows.length * 2);
    const labels = calls.filter((c) => c.op === "fillText");
    expect(labels.length).toBe(rows.length);
    for (const label of labels) {
      expect(label.text).toBe(identity.initials);
      expect(label.x).toBe(AVATAR_X);
    }
  });

  it("draws nothing when disabled or avatarX is missing/non-finite", () => {
    const renderer = new GraphRenderer();
    const rows = [row({ id: "a" })];

    const off = makeRecordingCtx();
    renderer.render(off.ctx, rows, 0, 1, 0, undefined, { theme: THEME, viewportHeight: 800, showAvatars: false, avatarX: AVATAR_X });
    expect(off.calls.filter((c) => c.op === "fillText")).toHaveLength(0);

    const noX = makeRecordingCtx();
    renderer.render(noX.ctx, rows, 0, 1, 0, undefined, { theme: THEME, viewportHeight: 800, showAvatars: true });
    expect(noX.calls.filter((c) => c.op === "fillText")).toHaveLength(0);

    const nanX = makeRecordingCtx();
    renderer.render(nanX.ctx, rows, 0, 1, 0, undefined, { theme: THEME, viewportHeight: 800, showAvatars: true, avatarX: Number.NaN });
    expect(nanX.calls.filter((c) => c.op === "fillText")).toHaveLength(0);
  });

  it("emphasis-only passes never stamp avatars over the static layer", () => {
    const renderer = new GraphRenderer();
    const rows = [row({ id: "a" }), row({ id: "b" })];
    const { ctx, calls } = makeRecordingCtx();
    renderer.render(ctx, rows, 0, 2, 0, "a", {
      theme: THEME,
      viewportHeight: 800,
      emphasisOnly: true,
      showAvatars: true,
      avatarX: AVATAR_X,
    });
    expect(calls.filter((c) => c.op === "fillText")).toHaveLength(0);
  });

  it("culls avatars outside the viewport band like nodes", () => {
    const renderer = new GraphRenderer({ rowHeight: 30 });
    const rows = Array.from({ length: 100 }, (_, i) => row({ id: `r${i}` }));
    const { ctx, calls } = makeRecordingCtx(90);
    renderer.render(ctx, rows, 0, rows.length, 0, undefined, {
      theme: THEME,
      viewportHeight: 90,
      showAvatars: true,
      avatarX: AVATAR_X,
    });
    const labels = calls.filter((c) => c.op === "fillText") as Array<{ y: number }>;
    expect(labels.length).toBeGreaterThan(0);
    expect(labels.length).toBeLessThan(rows.length);
    for (const label of labels) {
      expect(label.y).toBeLessThanOrEqual(90 + 30);
      expect(label.y).toBeGreaterThanOrEqual(-30);
    }
  });

  it("compact density shrinks the disc", () => {
    const spacious = new GraphRenderer();
    const compact = new GraphRenderer();
    compact.setDensity("compact");
    expect(compact.getAvatarStyle().radius).toBeLessThan(spacious.getAvatarStyle().radius);
    expect(AVATAR_STYLES.compact.radius).toBe(6);
    expect(AVATAR_STYLES.spacious.radius).toBe(8);
  });
});

describe("avatar hit-testing", () => {
  it("returns the row when the point lands inside the avatar disc (+ slop)", () => {
    const renderer = new GraphRenderer();
    const rows = [row({ id: "a" })];
    const avatarX = 200;
    const y = renderer.getRowY(0, 0);
    expect(renderer.getCommitAtPoint(avatarX, y, rows, 0, 1, 0, avatarX)?.id).toBe("a");
  });

  it("respects the slop boundary exactly", () => {
    const renderer = new GraphRenderer();
    const rows = [row({ id: "a" })];
    const avatarX = 200;
    const r = AVATAR_STYLES.spacious.radius;
    const y = renderer.getRowY(0, 0);
    const edge = renderer.getCommitAtPoint(avatarX + r + AVATAR_HIT_SLOP - 0.5, y, rows, 0, 1, 0, avatarX);
    expect(edge?.id).toBe("a");
    const outside = renderer.getCommitAtPoint(avatarX + r + AVATAR_HIT_SLOP + 6, y, rows, 0, 1, 0, avatarX);
    expect(outside).toBeNull();
  });

  it("ignores non-finite or null avatar columns", () => {
    const renderer = new GraphRenderer();
    const rows = [row({ id: "a" })];
    const y = renderer.getRowY(0, 0);
    expect(renderer.getCommitAtPoint(200, y, rows, 0, 1, 0, null)).toBeNull();
    expect(renderer.getCommitAtPoint(Number.NaN, y, rows, 0, 1, 0, Number.NaN)).toBeNull();
  });

  it("prefers the node when both discs overlap the pointer", () => {
    const renderer = new GraphRenderer({ originX: 20, laneWidth: 18 });
    const rows = [row({ id: "a", lane: 9 })];
    const nodeX = renderer.getLaneX(9);
    const y = renderer.getRowY(0, 0);
    expect(renderer.getCommitAtPoint(nodeX, y, rows, 0, 1, 0, nodeX + 4)?.id).toBe("a");
  });
});
