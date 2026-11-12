import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CommitComposer from "./CommitComposer.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CommitComposer.svelte"),
  "utf8",
);

describe("CommitComposer", () => {
  it("renders the include-unstaged option and staged-only commit by default", () => {
    const { body } = render(CommitComposer);
    expect(body).toContain("Include unstaged");
    expect(body).toContain('aria-label="Include unstaged files in this commit"');
    expect(body).toContain("Amend");
    expect(body).toContain("Commit (0)");
    expect(body).not.toContain("Commit all");
  });

  it("names the message box and ties its chords to it for assistive tech", () => {
    const { body } = render(CommitComposer);
    const textarea = body.match(/<textarea[^>]*>/)?.[0] ?? "";
    expect(textarea).toContain('aria-label="Commit message"');
    const describedBy = textarea.match(/aria-describedby="([^"]+)"/)?.[1] ?? "";
    expect(describedBy).toMatch(/commit-message-keys$/);
    const hint = body.match(new RegExp(`<span id="${describedBy}"[^>]*>([^<]*)</span>`));
    expect(hint?.[0]).toContain("sr-only");
    expect(hint?.[1]).toContain("Cmd/Ctrl Enter commits staged files.");
    expect(hint?.[1]).toContain("Cmd/Ctrl Shift Enter stages everything and commits.");
  });

  it("gives each composer its own hint id, so two mounted instances never collide", () => {
    // Svelte numbers instances within one render tree, so two separate SSR
    // renders can't show a clash; assert the id is derived, not a literal.
    expect(source).toContain("const instanceId = $props.id();");
    expect(source).toContain("`${instanceId}-commit-message-keys`");
    expect(source).toContain("aria-describedby={keysId}");
    expect(source).toContain("<span id={keysId}");
    expect(source).not.toContain('id="commit-message-keys"');
    expect(source).not.toContain('aria-describedby="commit-message-keys"');
  });

  it("announces an AI-generation error but leaves the commit error to the toast", () => {
    const ai = source.slice(source.indexOf("{#if aiError}"));
    expect(ai.slice(0, ai.indexOf("{/if}"))).toContain('<div role="alert"');
    const commit = source.slice(source.indexOf("{:else if commitError}"));
    expect(commit.slice(0, commit.indexOf("{:else if lastVerdict}"))).not.toContain("role=");
  });

  it("returns focus to the message box when the identity prompt goes away", () => {
    expect(source).toContain("bind:this={messageEl}");
    const fn = source.slice(source.indexOf("async function focusMessage()"));
    const body = fn.slice(0, fn.indexOf("\n  }"));
    expect(body).toContain("await tick();");
    expect(body).toContain("messageEl?.focus(");
    const prompt = source.slice(source.indexOf("<IdentityPrompt"), source.indexOf("{:else if commitError}"));
    expect(prompt.match(/void focusMessage\(\)/g)).toHaveLength(2);
    expect(prompt.indexOf("onSaved")).toBeLessThan(prompt.indexOf("void focusMessage()"));
    expect(prompt.indexOf("onDismiss")).toBeLessThan(prompt.lastIndexOf("void focusMessage()"));
  });

  // Rendered in harness/repo-churn.html: the default commit takes the staged
  // files only ("Commit (1)"); an empty message cannot commit; Cmd/Ctrl+Enter
  // commits the staged files without amending; Shift adds the unstaged files
  // as a quick commit; Include unstaged names "Commit all" and counts both;
  // a conflict disables Commit and the chord sends nothing.
  it("passes the amend choice through the staged-only commit", () => {
    // The fixture never amends.
    expect(source).toContain("repoStore.commit(message, isAmending)");
  });
  it("leaves an IME composition's Enter to the composition", () => {
    expect(source).toContain("isImeComposition");
  });
  it("previews staged edits via the shared codeintel preview store", () => {
    expect(source).toContain("previewStore.refresh");
    expect(source).toContain("What this commit breaks");
    expect(source).toContain("parse=");
    expect(source).toContain("bodies not compared=");
    expect(source).toContain("walk incomplete:");
    expect(source).toContain("tooltipWalkIncomplete");
    expect(source).toContain('data-testid="commit-preview-breaks"');
  });

  it("depends on the staged-path key, not the statuses array identity", () => {
    expect(source).toMatch(/import\s*\{[^}]*\buntrack\b[^}]*\}\s*from\s*"svelte"/);
    const start = source.indexOf("let previewPathsKey");
    expect(start).toBeGreaterThan(-1);
    const effect = source.slice(start, source.indexOf("A3: blast radius"));
    expect(effect).toContain("void previewPathsKey");
    expect(effect).toContain("untrack(() => {");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(
      effect.indexOf("previewStore.refresh"),
    );
  });

  it("composes staged blast radius with layered impact (no min_rung)", () => {
    expect(source).toContain("getImpactLayeredMany");
    expect(source).toContain("cancelCodeintelQuery");
    expect(source).toContain("newCodeintelCancelToken");
    expect(source).toContain("BlastRadiusPanel");
    expect(source).not.toContain("minRung");
  });

  it("caps staged blast fan-out and reports omitted seeds", () => {
    expect(source).toContain("capFanout");
    expect(source).toContain("omittedLayeredImpact");
  });

  it("summarizes each previewed file instead of printing the report struct", () => {
    // The rail already derived a verdict per file from markerForHonesty; this
    // panel re-rendered the raw fields beside it, so the two surfaces could
    // describe the same preview differently. One owner, both callers.
    expect(source).toContain("markerForHonesty(file)");
    expect(source).toContain("fileGlance(file)");
    // The telemetry is kept, but behind a disclosure rather than above the
    // finding it was meant to support.
    const summaryRegion = source.slice(0, source.indexOf("Preview details"));
    expect(summaryRegion).not.toContain("bodies not compared=");
    expect(source).toContain("bodies not compared=");
    expect(source).toContain("<details");
  });

  it("keeps each commit toggle's label on one line", () => {
    // "Include unstaged" broke across two lines inside its own label at
    // sidebar width and took the commit button's row height with it. Each
    // control refuses to wrap; the ROW wraps when it genuinely cannot fit.
    const footer = source.slice(source.indexOf("<!-- The two toggles"));
    expect(footer.length).toBeGreaterThan(0);
    expect(footer).toContain("flex-wrap");
    expect((footer.match(/whitespace-nowrap/g) ?? []).length).toBeGreaterThanOrEqual(3);
  });

  it("merges Ask agent into one full-width commit split button", () => {
    // The composer used to draw a separate "Ask agent" pill beside Commit,
    // which wrapped the footer onto a second row at sidebar width. One split
    // control now holds Commit and an icon-only sparkle that opens the same
    // dialog; the words are the sparkle's accessible name, not a second label.
    const { body } = render(CommitComposer);
    expect(body).not.toMatch(/>\s*Ask agent\s*</);
    expect(body).not.toContain(">Ask Agent<");
    // The old trigger's visible label, in either casing, is gone from the footer.
    expect(body).not.toContain("Ask Agent to Commit");
    expect(body).not.toContain("Ask agent to commit");

    const splitAt = body.indexOf("data-commit-split");
    const menuAt = body.indexOf("data-agent-commit-menu");
    const sparkAt = body.indexOf('aria-label="Ask agent"');
    expect(splitAt).toBeGreaterThan(-1);
    expect(menuAt).toBeGreaterThan(splitAt);
    expect(sparkAt).toBeGreaterThan(menuAt);

    // The group used to be an inline-flex shrink wrap, only as wide as
    // "Commit (0)" plus the sparkle. It now uses the message box's w-full.
    const groupTag = body.slice(body.lastIndexOf("<div", splitAt), body.indexOf(">", splitAt) + 1);
    expect(groupTag).toContain("w-full");
    expect(groupTag).toContain('role="group"');
    expect(groupTag).not.toContain("inline-flex");
    expect(body.match(/<textarea\b[^>]*>/)?.[0] ?? "").toContain("w-full");

    const split = body.slice(splitAt, sparkAt);
    expect(split).toContain("data-commit-split-divider");
    const commitButton = split.match(/<button\b[^>]*>[\s\S]*?Commit \(0\)[\s\S]*?<\/button>/)?.[0] ?? "";
    expect(commitButton).toContain("flex-1");
    expect(commitButton).toContain("justify-center");
    expect(commitButton).not.toContain("shrink-0");
    expect(commitButton).toContain("disabled");
    // A clean tree says why Commit is off, in the agent menu's own words.
    expect(commitButton).toContain("No uncommitted changes to commit.");
    // The sparkle keeps a fixed width beside the stretching Commit.
    expect(split).toContain("shrink-0");

    const sparkleButton = body.slice(
      body.lastIndexOf("<button", sparkAt),
      body.indexOf("</button>", sparkAt) + "</button>".length,
    );
    expect(sparkleButton).toContain('aria-haspopup="dialog"');
    expect(sparkleButton).not.toContain("justify-center");
    expect(sparkleButton).toContain("disabled");
    expect(sparkleButton).toContain("<svg");
    expect(sparkleButton).not.toMatch(/>\s*Ask agent\s*</i);

    const footer = source.slice(source.indexOf("<!-- The two toggles"));
    expect(footer).toContain("void handleCommit()");
    expect(footer).toMatch(/<AgentCommitMenu segment\b/);
    expect(footer).not.toContain("compact={true}");

    const menuSource = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "AgentCommitMenu.svelte"),
      "utf8",
    );
    const segmentStart = menuSource.indexOf("{#if segment}");
    const segmentEnd = menuSource.indexOf("{:else if compact}");
    expect(segmentStart).toBeGreaterThan(-1);
    expect(segmentEnd).toBeGreaterThan(segmentStart);
    const segment = menuSource.slice(segmentStart, segmentEnd);
    expect(segment).toContain("onclick={toggle}");
    expect(segment).toContain('aria-haspopup="dialog"');
    expect(segment).toContain('aria-label="Ask agent"');
    expect(segment).not.toMatch(/>\s*Ask agent\s*</i);
  });

  it("writes a commit message without waiting for a loopback model", () => {
    // The patch is classified in the app. A missing model server is not a
    // reason to leave the composer blank, and Apple Intelligence is only the
    // subject when no loopback model is selected.
    const button = source.slice(source.indexOf("onclick={generateMessage}"));
    expect(button.startsWith("onclick={generateMessage}")).toBe(true);
    expect(button.slice(0, 400)).not.toContain("!aiReady");
    expect(button).toContain("appleReady(apple)");
    expect(button).toContain("No model is running");
  });

  it("keeps the impact harness's stand-in commit footer in step with this one", () => {
    // harness/impact.html measures that footer's geometry against a copy.
    const host = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../../../harness/ImpactHost.svelte"),
      "utf8",
    );
    const standIn = host.slice(host.indexOf("data-impact-commit-footer"));
    expect(standIn).toContain("flex-wrap");
    expect(standIn).toContain("whitespace-nowrap");
    for (const label of ["Amend", "Include unstaged"]) {
      expect(standIn, `stand-in footer is missing ${label}`).toContain(label);
    }
  });


  /**
   * `$repoStore.currentPath` read in an effect tracks the whole store, and the
   * store publishes a fresh object on every draft keystroke — so each key
   * typed cancelled the staged blast-radius query and issued it again. The
   * effects read a derived repository path, which compares by value.
   */
  it("re-runs the preview and blast-radius effects on repo or staged-key change, not per keystroke", () => {
    expect(source).toMatch(/import \{ currentRepoPath, repoStore \} from "\.\.\/stores\/repoStore";/);
    const region = source.slice(
      source.indexOf("// The effects below key on the shared repository path"),
      source.indexOf("$effect(() => () => blastGuard?.cancel());"),
    );
    const effects = region.split("$effect(() => {").slice(1);
    expect(effects).toHaveLength(2);
    for (const effect of effects) {
      expect(effect).toContain("const repo = $currentRepoPath;");
      expect(effect).toContain("void previewPathsKey;");
      expect(effect).not.toContain("$repoStore");
    }
    expect(effects[1]).toContain("getImpactLayeredMany(repo");
  });
});
