import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { noticeSummary } from "./GraphNotices.svelte";

const source = readFileSync(new URL("./GraphNotices.svelte", import.meta.url), "utf8");
const history = readFileSync(new URL("./HistoryView.svelte", import.meta.url), "utf8");
const store = readFileSync(
  new URL("../stores/graphStore.ts", import.meta.url),
  "utf8",
);

/**
 * The defect this component exists to close: `graphStore` carried the
 * backend's completeness disclosures in `state.warnings`, no component ever
 * read that field, and the only surface they reached was the diagnostics ring
 * — the app's crash log. A true statement about the repository therefore
 * arrived looking like a malfunction, once per repository per launch.
 */
describe("GraphNotices", () => {
  it("renders nothing when the graph is drawing everything", () => {
    // Silence is correct here and only here: a strip that appears on every
    // repository is one nobody reads, which is how the real one gets missed.
    expect(source).toContain("{#if notices.length > 0}");
    expect(noticeSummary([])).toEqual({ lead: "", overflow: "" });
  });

  it("leads with the first notice and counts the rest", () => {
    const first = "36 commit(s) ... are not drawn.";
    expect(noticeSummary([first])).toEqual({ lead: first, overflow: "" });
    expect(noticeSummary([first, "Ref labels are capped ..."])).toEqual({
      lead: first,
      overflow: "+1 more",
    });
    expect(noticeSummary([first, "b", "c"])).toEqual({ lead: first, overflow: "+2 more" });
  });

  it("keeps the overflow count out of the truncating box", () => {
    // Appended to the lead it is the first thing the ellipsis eats, and the
    // reader is told there is one notice when there are three.
    expect(source).toContain('<span class="min-w-0 flex-1 truncate"');
    expect(source).toContain("{#if !expanded && summary.overflow}");
  });

  it("keeps every notice reachable rather than only the summarised one", () => {
    // A count with no way to read what it counts is a worse disclosure than
    // none: the reader learns something is hidden and cannot learn what.
    expect(source).toContain("{#each notices as notice");
    expect(source).toContain('title={notices.join("\\n\\n")}');
  });

  it("offers the way out the notice names", () => {
    // The sentence says to change the ref scope; that control lives in
    // Settings, and the diagnostics log it used to land in cannot link there.
    expect(source).toContain('new CustomEvent("gitpulse:settings")');
  });

  it("is mounted above the graph it describes", () => {
    expect(history).toContain("<GraphNotices notices={$graphStore.notices} />");
  });

  it("is fed by a field the store keeps separate from its warnings", () => {
    // The split is the fix. If `notices` were folded back into `warnings`,
    // the diagnostics push in graphStore would refile every disclosure as a
    // fault and this component would be showing the fault log again.
    expect(store).toContain("notices: payload.notices ?? []");
    expect(store).not.toContain('reportFailure("graph", `${repoPath}: ${n}`)');
    const diagnosticPush = store.slice(store.indexOf("const warningList"));
    expect(diagnosticPush.slice(0, 400)).not.toContain("notices");
  });
});
