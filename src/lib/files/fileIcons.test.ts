import { describe, expect, it } from "vitest";
import { getFileIconMeta } from "./fileIcons";

describe("fileIcons", () => {
  it("resolves correct metadata for standard source files", () => {
    expect(getFileIconMeta("src/App.svelte").badgeLabel).toBe("SVELTE");
    expect(getFileIconMeta("src/main.rs").badgeLabel).toBe("RS");
    expect(getFileIconMeta("index.ts").badgeLabel).toBe("TS");
    expect(getFileIconMeta("Component.tsx").badgeLabel).toBe("TSX");
    expect(getFileIconMeta("server.js").badgeLabel).toBe("JS");
    expect(getFileIconMeta("package.json").badgeLabel).toBe("CONF");
    expect(getFileIconMeta("Cargo.lock").badgeLabel).toBe("LOCK");
    expect(getFileIconMeta("README.md").badgeLabel).toBe("MD");
    expect(getFileIconMeta("app.py").badgeLabel).toBe("PY");
    expect(getFileIconMeta("main.go").badgeLabel).toBe("GO");
    expect(getFileIconMeta("logo.png").badgeLabel).toBe("IMG");
    expect(getFileIconMeta("logo.png").isImage).toBe(true);
    expect(getFileIconMeta("icon.svg").isImage).toBe(true);
  });

  it("handles fallback and empty inputs safely", () => {
    expect(getFileIconMeta("").badgeLabel).toBe("TXT");
    expect(getFileIconMeta("file.unknown").badgeLabel).toBe("UNKN");
  });
});
