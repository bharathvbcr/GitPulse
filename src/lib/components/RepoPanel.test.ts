import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import RepoPanel from "./RepoPanel.svelte";
import { parseRemoteList } from "../repos/remotes";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "RepoPanel.svelte"), "utf8");

describe("RepoPanel remotes", () => {
  it("renders the remotes heading", () => {
    const { body } = render(RepoPanel);
    expect(body).toContain("Remotes");
  });

  it("invokes add, set-url, rename, prune and remove through the store", () => {
    expect(source).toContain('kind: "add"');
    expect(source).toContain('kind: "seturl"');
    expect(source).toContain('kind: "rename"');
    expect(source).toContain('kind: "prune"');
    expect(source).toContain('kind: "remove"');
    expect(source).toContain("repoStore.remoteChange");
    expect(source).toContain("isDestructiveRemoteChange");
    expect(source).toContain("remoteChangeConsequence");
    expect(source).toContain("submitRename");
  });

  it("says when the remote list was capped instead of looking complete", () => {
    expect(source).toContain("remotesTruncated");
    expect(source).toContain("More exist in .git/config and are not listed.");
  });

  it("prefills Set URL from the backend's display URL, never a token", () => {
    // The backend owns redaction and re-attaches stored credentials when the
    // display form is submitted unchanged, so the prefill is the display URL
    // and the panel carries no second redactor of its own.
    expect(source).toContain('editUrl = remote.fetch_url ?? "";');
    expect(source).not.toContain("redactRemoteUrl");
    expect(source).not.toContain("carriesEmbeddedCredential");
    expect(source).toContain("{#if remote.has_credentials}");
    // The only value the list handler can prefill from is the parsed payload.
    const parsed = parseRemoteList({
      remotes: [
        { name: "origin", fetch_url: "https://user@host/o/r.git", push_url: null, has_credentials: true, tracking_branches: 0, is_default: true },
      ],
      truncated: false,
    });
    expect(parsed.remotes[0].fetch_url).not.toMatch(/:[^@/]+@/);
  });

  it("offers an explicit way to remove saved credentials from Set URL", () => {
    expect(source).toContain("drop_credentials: editDropCredentials");
    expect(source).toContain("bind:checked={editDropCredentials}");
    expect(source).toMatch(/\{#if remote\.has_credentials\}\s*<label/);
  });

  it("arms destructive remote changes before executing them", () => {
    expect(source).toContain("Confirm prune");
    expect(source).toContain("Confirm remove");
    expect(source).toContain("Confirm URL");
  });
});

describe("RepoPanel section headers on a failed read", () => {
  // The headers sit above the body's own error line; a failed read leaves the
  // list empty and `*Loaded` true, so without an error arm the header would
  // state "No remotes configured" / "has no submodules" as fact.
  const header = (name: string) => {
    const at = source.indexOf(`{${name}Error ? "Could not read ${name}"`);
    expect(at, `${name} header has no error arm`).toBeGreaterThan(-1);
    return source.slice(source.lastIndexOf("<span", at), source.indexOf("</span>", at));
  };

  it("names the failure instead of describing an empty list", () => {
    expect(header("remotes")).toContain('remotesLoaded ? describeRemotes(remotes) : "Loading…"');
    expect(header("submodules")).toContain('submodulesLoaded ? describeSubmodules(submodules) : "Loading…"');
  });

  it("checks the error before the loaded flag and uses the body's error colour", () => {
    for (const name of ["remotes", "submodules"]) {
      const h = header(name);
      expect(h.indexOf(`${name}Error ?`)).toBeLessThan(h.indexOf(`${name}Loaded ?`));
      expect(h).toContain("text-danger");
    }
  });

  it("keeps the header reload, which reloads both lists, as the retry", () => {
    expect(source).toContain("onclick={() => void load()}");
    const load = source.slice(source.indexOf("await Promise.all(["));
    expect(load.slice(0, load.indexOf("]);"))).toContain("repoStore.listSubmodules()");
    expect(load.slice(0, load.indexOf("]);"))).toContain("repoStore.listRemotes()");
  });
});

describe("RepoPanel submodules", () => {
  it("exposes sync and deinit, not only initialize", () => {
    expect(source).toContain('kind: "sync"');
    expect(source).toContain('kind: "deinit"');
    expect(source).toContain("activateSubmodule");
    expect(source).toContain("canDeinit");
    expect(source).toContain("canSync");
  });

  it("says when the submodule list was capped instead of looking complete", () => {
    expect(source).toContain("submodulesTruncated");
    expect(source).toContain("More exist and are not listed.");
  });

  it("never force-deinits; an unknown dirty tree is not discarded", () => {
    expect(source).toContain("force: false");
    expect(source).not.toMatch(/kind:\s*"deinit"[\s\S]{0,80}force:\s*true/);
    expect(source).toContain("isDestructiveSubmoduleChange");
    expect(source).toContain("Confirm deinit");
  });

  it("disarms a pending destructive confirm when the repository changes", () => {
    // `armed` names a row, not a repository, so a second click after a tab
    // switch would otherwise confirm an action armed against the old tab.
    const reset = source.slice(source.indexOf("$effect(() => {\n    const path = repoPath;"), source.indexOf("function resetForRepository"));
    expect(reset).toContain("untrack(() => resetForRepository(path))");
    const body = source.slice(source.indexOf("function resetForRepository"), source.indexOf("stashMessage = \"\";"));
    expect(body).toContain("armed = null;");
    // Arming and confirming are both clicks on the active repository; neither awaits a prompt.
    const flows = source.slice(source.indexOf("function activateStash"), source.indexOf("function submitAddRemote"));
    expect(flows).not.toContain("askConfirm");
  });
});
