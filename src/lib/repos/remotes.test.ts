import { describe, expect, it } from "vitest";
import {
  carriesEmbeddedCredential,
  describeRemotes,
  effectivePushUrl,
  hasSplitUrls,
  isDestructiveRemoteChange,
  redactRemoteUrl,
  remoteChangeConsequence,
  remoteHost,
  validateRemoteName,
  validateRemoteUrl,
  type RemoteChange,
  type RemoteInfo,
} from "./remotes";

function remote(extra: Partial<RemoteInfo> = {}): RemoteInfo {
  return {
    name: "origin",
    fetch_url: "https://github.com/owner/repo.git",
    push_url: null,
    tracking_branches: 3,
    is_default: true,
    ...extra,
  };
}

describe("redactRemoteUrl", () => {
  it("removes an embedded token while keeping the account visible", () => {
    // The failure this prevents: a personal access token rendered on screen,
    // captured in a screenshot, and pasted into a bug report.
    expect(redactRemoteUrl("https://alice:ghp_secrettoken@github.com/o/r.git")).toBe(
      "https://alice@github.com/o/r.git",
    );
  });

  it("keeps a plain userinfo with no password intact", () => {
    expect(redactRemoteUrl("ssh://git@github.com/o/r.git")).toBe("ssh://git@github.com/o/r.git");
  });

  it("drops userinfo entirely when it is only a password", () => {
    expect(redactRemoteUrl("https://:token@github.com/o/r.git")).toBe("https://github.com/o/r.git");
  });

  it("leaves URLs without userinfo untouched", () => {
    for (const url of [
      "https://github.com/o/r.git",
      "git@github.com:o/r.git",
      "/Volumes/Disk/repo.git",
      "../sibling-repo",
    ]) {
      expect(redactRemoteUrl(url)).toBe(url);
    }
  });
});

describe("carriesEmbeddedCredential", () => {
  it("detects a password in the URL", () => {
    expect(carriesEmbeddedCredential("https://alice:token@github.com/o/r.git")).toBe(true);
  });

  it("does not flag a bare username, which is normal for ssh", () => {
    expect(carriesEmbeddedCredential("ssh://git@github.com/o/r.git")).toBe(false);
    expect(carriesEmbeddedCredential("git@github.com:o/r.git")).toBe(false);
  });

  it("handles an absent URL", () => {
    expect(carriesEmbeddedCredential(null)).toBe(false);
  });
});

describe("push URL handling", () => {
  it("falls back to the fetch URL when no push URL is set", () => {
    expect(effectivePushUrl(remote())).toBe("https://github.com/owner/repo.git");
    expect(hasSplitUrls(remote())).toBe(false);
  });

  it("surfaces a push URL that differs from the fetch URL", () => {
    // Either a deliberate fork workflow or work going to the wrong repo;
    // collapsing the two fields makes both invisible.
    const split = remote({ push_url: "ssh://git@github.com/me/fork.git" });
    expect(hasSplitUrls(split)).toBe(true);
    expect(effectivePushUrl(split)).toBe("ssh://git@github.com/me/fork.git");
  });

  it("does not call identical URLs a split", () => {
    const same = remote({ push_url: "https://github.com/owner/repo.git" });
    expect(hasSplitUrls(same)).toBe(false);
  });
});

describe("remoteHost", () => {
  it("extracts the host from every shape git accepts", () => {
    expect(remoteHost("https://github.com/o/r.git")).toBe("github.com");
    expect(remoteHost("https://alice:tok@gitlab.com/o/r.git")).toBe("gitlab.com");
    expect(remoteHost("ssh://git@ssh.dev.azure.com/v3/o/p/r")).toBe("ssh.dev.azure.com");
    expect(remoteHost("git@github.com:o/r.git")).toBe("github.com");
  });

  it("returns nothing for a local path rather than inventing a host", () => {
    expect(remoteHost("/Volumes/Disk/repo.git")).toBeNull();
    expect(remoteHost("../sibling")).toBeNull();
    expect(remoteHost(null)).toBeNull();
  });
});

describe("validation", () => {
  it("rejects names git or the shell would misread", () => {
    for (const name of ["", "  ", "-x", "a..b", "a b", "ends/", ".hidden", "x.lock", "a~1", "a^"]) {
      expect(validateRemoteName(name), `name ${JSON.stringify(name)}`).toBeTruthy();
    }
  });

  it("accepts ordinary names, including ones with dots", () => {
    for (const name of ["origin", "upstream", "my.fork", "team-a"]) {
      expect(validateRemoteName(name)).toBeNull();
    }
  });

  it("rejects URLs that would execute a helper program", () => {
    // `ext::sh -c '…'` runs an arbitrary command as a git transport.
    expect(validateRemoteUrl("ext::sh -c 'id'")).toBeTruthy();
    expect(validateRemoteUrl("transport::whatever")).toBeTruthy();
  });

  it("rejects a URL that would be read as a flag", () => {
    expect(validateRemoteUrl("--upload-pack=touch /tmp/pwn")).toBeTruthy();
  });

  it("rejects an empty URL", () => {
    expect(validateRemoteUrl("   ")).toBeTruthy();
  });

  it("accepts URLs containing spaces and hyphens", () => {
    expect(validateRemoteUrl("/Volumes/My Disk/repo.git")).toBeNull();
    expect(validateRemoteUrl("https://git.example.com/my-org/my-repo.git")).toBeNull();
  });

  it("rejects a URL carrying a control character", () => {
    expect(validateRemoteUrl("https://example.test/\u0000evil")).toBeTruthy();
    expect(validateRemoteUrl("https://example.test/a\u001bb")).toBeTruthy();
    expect(validateRemoteUrl("https://example.test/a\u007fb")).toBeTruthy();
  });

  it("accepts the transports git actually uses", () => {
    for (const url of [
      "https://github.com/o/r.git",
      "ssh://git@github.com/o/r.git",
      "git://github.com/o/r.git",
      "file:///srv/repo.git",
      "git@github.com:o/r.git",
      "/Volumes/My Disk/repo.git",
      "../sibling-repo",
    ]) {
      expect(validateRemoteUrl(url), url).toBeNull();
    }
  });
});

describe("change consequences", () => {
  const changes: RemoteChange[] = [
    { kind: "add", name: "upstream", url: "https://example.test/u.git" },
    { kind: "remove", name: "upstream" },
    { kind: "rename", name: "origin", new_name: "upstream" },
    { kind: "seturl", name: "origin", url: "https://example.test/n.git", push: false },
    { kind: "seturl", name: "origin", url: "https://example.test/n.git", push: true },
    { kind: "prune", name: "origin" },
  ];

  it("describes every change without leaving one blank", () => {
    for (const change of changes) {
      expect(remoteChangeConsequence(change), change.kind).toBeTruthy();
    }
  });

  it("treats repointing a URL as destructive", () => {
    // It silently redirects where every future push lands — the most
    // consequential change here and the least obviously dangerous.
    expect(isDestructiveRemoteChange({ kind: "seturl", name: "o", url: "u", push: true })).toBe(true);
    expect(isDestructiveRemoteChange({ kind: "remove", name: "o" })).toBe(true);
    expect(isDestructiveRemoteChange({ kind: "prune", name: "o" })).toBe(true);
    expect(isDestructiveRemoteChange({ kind: "add", name: "o", url: "u" })).toBe(false);
  });

  it("reassures that removing a remote keeps local work", () => {
    expect(remoteChangeConsequence({ kind: "remove", name: "origin" })).toContain(
      "local branches and commits are untouched",
    );
  });

  it("distinguishes repointing pushes from repointing fetches", () => {
    const push = remoteChangeConsequence({ kind: "seturl", name: "o", url: "u", push: true });
    const fetch = remoteChangeConsequence({ kind: "seturl", name: "o", url: "u", push: false });
    expect(push).toContain("push");
    expect(fetch).toContain("fetch");
    expect(push).not.toBe(fetch);
  });
});

describe("describeRemotes", () => {
  it("explains a local-only repository instead of showing an empty list", () => {
    // This is why a first-time user's push fails, and nothing else says so.
    expect(describeRemotes([])).toContain("only on this machine");
  });

  it("names the single remote and its host", () => {
    expect(describeRemotes([remote()])).toBe("1 remote — origin at github.com");
  });

  it("omits a host it could not determine", () => {
    expect(describeRemotes([remote({ fetch_url: "../sibling" })])).toBe("1 remote — origin");
  });

  it("counts several remotes", () => {
    expect(describeRemotes([remote(), remote({ name: "upstream" })])).toBe("2 remotes configured");
  });
});
