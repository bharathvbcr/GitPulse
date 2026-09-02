import { describe, expect, it } from "vitest";
import {
  isDestructiveRemoteChange,
  parseRemoteList,
  remoteChangeConsequence,
  validateRemoteName,
  validateRemoteUrl,
  type RemoteChange,
  type RemoteInfo,
} from "./remotes";

function remote(name: string, extra: Partial<RemoteInfo> = {}): RemoteInfo {
  return {
    name,
    fetch_url: `https://example.test/${name}.git`,
    push_url: null,
    tracking_branches: 0,
    is_default: name === "origin",
    ...extra,
  };
}

const KINDS: RemoteChange[] = [
  { kind: "add", name: "origin", url: "https://example.test/a.git" },
  { kind: "remove", name: "origin" },
  { kind: "rename", name: "origin", new_name: "upstream" },
  { kind: "seturl", name: "origin", url: "https://example.test/b.git", push: false },
  { kind: "seturl", name: "origin", url: "https://example.test/b.git", push: true },
  { kind: "prune", name: "origin" },
];

describe("remotes stress", () => {
  it("describes every kind without a blank consequence, including rename", () => {
    for (const change of KINDS) {
      const text = remoteChangeConsequence(change);
      expect(text.length, change.kind).toBeGreaterThan(10);
      expect(text).toContain(change.name);
    }
  });

  it("refuses a barrage of names git or the shell would misread", () => {
    const hostile = [
      "",
      " ",
      "-x",
      "--exec=sh",
      "a..b",
      "a b",
      "ends/",
      ".hidden",
      "x.lock",
      "a~1",
      "a^b",
      "a:b",
      "a?b",
      "a*b",
      "a[b",
      "a\\b",
      "trailing.",
      "has\0nul",
    ];
    for (const name of hostile) {
      expect(validateRemoteName(name), JSON.stringify(name)).toBeTruthy();
    }
  });

  it("refuses a barrage of URLs that would become flags or helpers", () => {
    const hostile = [
      "",
      "   ",
      "--upload-pack=touch /tmp/pwn",
      "-oProxyCommand=sh",
      "ext::sh -c 'id'",
      "transport::whatever",
      "https://example.test/\u0000evil",
      "https://example.test/a\u001bb",
      "https://example.test/a\u007fb",
    ];
    for (const url of hostile) {
      expect(validateRemoteUrl(url), JSON.stringify(url)).toBeTruthy();
    }
  });

  it("parses a 200-remote listing without dropping the truncated flag", () => {
    const remotes = Array.from({ length: 200 }, (_, i) => remote(`r${String(i).padStart(3, "0")}`));
    const parsed = parseRemoteList({ remotes, truncated: true });
    expect(parsed.failed).toBe(false);
    expect(parsed.truncated).toBe(true);
    expect(parsed.remotes).toHaveLength(200);
    expect(parsed.remotes[0].name).toBe("r000");
    expect(parsed.remotes[199].name).toBe("r199");
  });

  it("does not treat a 200-long bare array as a complete listing", () => {
    const remotes = Array.from({ length: 200 }, (_, i) => remote(`r${i}`));
    const parsed = parseRemoteList(remotes);
    expect(parsed.failed).toBe(true);
    expect(parsed.remotes).toEqual([]);
  });

  it("rename is the non-destructive name change; seturl still is not", () => {
    expect(isDestructiveRemoteChange({ kind: "rename", name: "a", new_name: "b" })).toBe(false);
    expect(isDestructiveRemoteChange({ kind: "seturl", name: "a", url: "u", push: false })).toBe(
      true,
    );
  });
});
