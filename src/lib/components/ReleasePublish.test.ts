import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ReleasePublish from "./ReleasePublish.svelte";
import { asRunOutcome, type WorkflowRunInfo } from "../github/types";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "ReleasePublish.svelte"), "utf8");
const panel = readFileSync(join(here, "GitHubPanel.svelte"), "utf8");

const release = {
  tag_name: "v1.2.3",
  name: "v1.2.3",
  is_draft: false,
  is_prerelease: false,
  is_latest: true,
  published_at: "2026-01-02T00:00:00Z",
  created_at: "2026-01-02T00:00:00Z",
  url: "https://example.invalid/r",
};

describe("ReleasePublish", () => {
  it("renders Push release tag… with the latest release named", () => {
    const { body } = render(ReleasePublish, { props: { releases: [release], runs: [], onPublished: () => {} } });
    expect(body).toContain("Push release tag…");
    expect(body).toContain("v1.2.3");
  });

  it("says so when no release exists", () => {
    const { body } = render(ReleasePublish, { props: { releases: [], runs: [], onPublished: () => {} } });
    expect(body).toContain("No release has been published yet.");
  });

  it("keeps the form closed until asked", () => {
    const { body } = render(ReleasePublish, { props: { releases: [], runs: [], onPublished: () => {} } });
    expect(body).not.toContain("I confirm the working tree is ready.");
  });

  it("shows the most recent workflow runs the pane already read", () => {
    const run: WorkflowRunInfo = { outcome: asRunOutcome("success"), id: 1, name: "Release", title: "Release v1.2.3", status: "completed", conclusion: "success", url: "u", head_branch: "main", created_at: "", started_at: "", updated_at: "", head_sha: "", event: "push" };
    const { body } = render(ReleasePublish, { props: { releases: [release], runs: [run], onPublished: () => {} } });
    expect(body).toContain("Recent workflow activity");
    expect(body).toContain("success");
  });
});

describe("ReleasePublish after a repository switch", () => {
  it("still says what happened to a push that ran in the repository it was asked for", () => {
    // The old panel always reported the outcome; dropping it when the reader
    // switched tabs mid-push would leave a pushed tag with no word about it.
    const body = source.slice(source.indexOf("async function publishRelease"));
    const switched = body.slice(body.indexOf("if ($repoStore.currentPath !== repo) {"), body.indexOf("if (!outcome.ok)"));
    expect(switched).toContain("toastStore.success(");
    expect(switched).toContain("toastStore.error(");
    expect(switched).toContain("return;");
  });

  it("lets the next release's message follow its tag again", () => {
    const body = source.slice(source.indexOf("async function publishRelease"));
    const success = body.slice(body.indexOf("releaseConfirmed = false;"), body.indexOf("onPublished("));
    expect(success).toContain("messageEdited = false;");
  });
});

describe("ReleasePublish contracts", () => {
  it("cannot push without the confirmation and a tag", () => {
    expect(source).toContain('const canPush = $derived(releaseConfirmed && releaseTag.trim() !== "" && !publishing);');
    expect(source).toContain("if (!canPush) return;");
    expect(source).toContain("disabled={!canPush}");
  });

  it("publishes through the gated store action and shows the verdict", () => {
    expect(source).toContain("repoStore.publishRelease(tag,");
    expect(source).toContain("verdictLabel(outcome.policy)");
    expect(source).toContain("the release workflow can now build the app");
  });

  it("states what the gate will still require", () => {
    expect(source).toContain("GitPulse will still require a clean, fully synchronized default branch and will refuse duplicate remote tags.");
  });

  it("suggests the next tag from the tag list without overwriting one the person typed", () => {
    expect(source).toContain("releaseTagSuggestion(repoTags.map((tag) => tag.name))");
    expect(source).toContain("if (!tagEdited) {");
  });

  it("is mounted in the Releases block and re-reads GitHub through the pane's notice path", () => {
    expect(panel).toContain("<ReleasePublish");
    expect(panel).toContain("onPublished={(message) => void reloadThenNotice(message)}");
  });
});
