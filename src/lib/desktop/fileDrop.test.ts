import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { reduceFileDrop, type FileDropGesture } from "./fileDrop";

const idle: FileDropGesture = { sawFiles: false };
const draggingFiles: FileDropGesture = { sawFiles: true };

describe("reduceFileDrop", () => {
  it("shows the overlay only when enter carries file paths", () => {
    expect(reduceFileDrop({ type: "enter", paths: ["/tmp/repo"] }, idle)).toEqual({
      sawFiles: true,
      overlay: true,
      dropped: null,
    });
    expect(reduceFileDrop({ type: "enter", paths: [] }, idle)).toEqual({
      sawFiles: false,
      overlay: false,
      dropped: null,
    });
    expect(reduceFileDrop({ type: "enter" }, idle)).toEqual({
      sawFiles: false,
      overlay: false,
      dropped: null,
    });
  });

  it("does not start the overlay from over — in-app card drags fire over with no paths", () => {
    expect(reduceFileDrop({ type: "over" }, idle)).toEqual({
      sawFiles: false,
      overlay: false,
      dropped: null,
    });
  });

  it("keeps a file-drop overlay alive across over after a spurious leave", () => {
    const afterLeave = reduceFileDrop({ type: "leave" }, draggingFiles);
    expect(afterLeave).toEqual({ sawFiles: true, overlay: false, dropped: null });
    expect(reduceFileDrop({ type: "over" }, afterLeave)).toEqual({
      sawFiles: true,
      overlay: true,
      dropped: null,
    });
  });

  it("clears the overlay on drop and only yields a path when files were dropped", () => {
    expect(reduceFileDrop({ type: "drop", paths: ["/tmp/repo", ""] }, draggingFiles)).toEqual({
      sawFiles: false,
      overlay: false,
      dropped: "/tmp/repo",
    });
    expect(reduceFileDrop({ type: "drop", paths: [] }, idle)).toEqual({
      sawFiles: false,
      overlay: false,
      dropped: null,
    });
  });

  it("treats an empty-path enter as the start of an in-app drag, cancelling a prior file gesture", () => {
    expect(reduceFileDrop({ type: "enter", paths: [] }, draggingFiles)).toEqual({
      sawFiles: false,
      overlay: false,
      dropped: null,
    });
  });
});

describe("nativeShell file-drop wiring", () => {
  it("routes window drag events through reduceFileDrop instead of treating every over as a repo drop", () => {
    const source = readFileSync(new URL("./nativeShell.ts", import.meta.url), "utf8");
    expect(source).toContain("reduceFileDrop");
    expect(source).toContain("decision.dropped");
    expect(source).not.toContain('event.payload.type === "enter" || event.payload.type === "over"');
  });
});
