import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type {
  ConflictChunk,
  ConflictDocument,
  ConflictResolutionChoice,
} from "../diff/conflict";
import { render } from "svelte/server";
import ConflictEditor, { adoptResolutions } from "./ConflictEditor.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ConflictEditor.svelte"),
  "utf8",
);

/**
 * The real wire types, not structural mirrors. The mirrors could not fail on
 * drift: they were assignable to whatever the component declared, so both
 * sides could go stale together.
 */
function chunk(index: number, resolution: ConflictResolutionChoice): ConflictChunk {
  return {
    chunk_index: index,
    start_line: index * 10,
    end_line: index * 10 + 3,
    ours_label: "HEAD",
    ours_content: `ours ${index}`,
    theirs_label: "feature",
    theirs_content: `theirs ${index}`,
    resolution,
    // Rust has always sent these; the structural mirror this file used to
    // declare simply left them out, and nothing could say so.
    ours_crlf: [],
    theirs_crlf: [],
    local_crlf: false,
  };
}

function doc(path: string, chunks: ConflictChunk[]): ConflictDocument {
  return {
    file_path: path,
    segments: chunks.map((Conflict) => ({ Conflict })),
    total_conflicts: chunks.length,
    crlf: false,
    trailing_newline: true,
    final_crlf: false,
    normal_crlf_flags: [],
  };
}

describe("adoptResolutions (stale-parse protection)", () => {
  it("carries same-file resolutions onto a fresh parse by chunk index", () => {
    const current = doc("a.txt", [chunk(0, "Unresolved"), chunk(1, "AcceptTheirs"), chunk(2, { Custom: "merged line" })]);
    const next = doc("a.txt", [chunk(0, "Unresolved"), chunk(1, "Unresolved"), chunk(2, "Unresolved")]);
    const merged = adoptResolutions(next, current);
    expect(merged.segments.map((seg) => seg.Conflict?.resolution)).toEqual([
      "Unresolved",
      "AcceptTheirs",
      { Custom: "merged line" },
    ]);
  });

  it("leaves a different file's parse untouched", () => {
    const current = doc("a.txt", [chunk(0, "AcceptOurs")]);
    const next = doc("b.txt", [chunk(0, "Unresolved")]);
    expect(adoptResolutions(next, current).segments[0].Conflict?.resolution).toBe("Unresolved");
  });

  it("returns the next parse unchanged when nothing was resolved yet", () => {
    const current = doc("a.txt", [chunk(0, "Unresolved")]);
    const next = doc("a.txt", [chunk(0, "AcceptOurs")]);
    expect(adoptResolutions(next, current).segments[0].Conflict?.resolution).toBe("AcceptOurs");
  });

  it("handles a null current doc and leaves the previous doc unmutated", () => {
    const next = doc("a.txt", [chunk(0, "AcceptTheirs")]);
    expect(adoptResolutions(next, null)).toBe(next);
    const current = doc("a.txt", [chunk(0, "AcceptOurs")]);
    adoptResolutions(next, current);
    expect(current.segments[0].Conflict?.resolution).toBe("AcceptOurs");
  });
});

describe("ConflictEditor load-effect memo guard", () => {
  it("reloads only when the conflicted file or repo actually changes", () => {
    // repoStore republishes fresh objects every ~6s status poll; reloading
    // per emission would churn IPC and replace parsedDoc mid-edit.
    expect(source).toContain("if (key === loadedKey) return;");
    expect(source).toContain("loadedKey = key;");
    // The memo key derives only from repo path + conflicted file.
    expect(source).toContain("`${repo}\\u0000${file}`");
  });

  it("does not cancel an in-flight load via effect teardown on memo hits", () => {
    // Svelte runs an effect's previous teardown before EVERY re-run, so the
    // load guard must be cancelled explicitly in the body instead.
    const loadIdx = source.indexOf("const guard = createAsyncGuard();");
    const explicitCancel = source.indexOf("loadGuard?.cancel();", source.indexOf("let loadedKey"));
    expect(explicitCancel).toBeGreaterThan(-1);
    expect(explicitCancel).toBeLessThan(loadIdx);
    expect(source.match(/return \(\) => \{\s*guard\.cancel\(\);\s*\};/g)).toBeNull();
  });

  it("routes a landed parse through adoptResolutions instead of assigning raw", () => {
    expect(source).toContain("parsedDoc = adoptResolutions(doc, parsedDoc);");
  });
});

describe("ConflictEditor rendering", () => {
  it("renders the empty state when no conflicts exist", () => {
    const { body } = render(ConflictEditor);
    expect(body).toContain("No merge conflicts");
  });
});
