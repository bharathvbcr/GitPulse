import { invoke } from "@tauri-apps/api/core";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { interfaceStore } from "../stores/interfaceStore";
import { toastStore } from "../stores/toastStore";
import { IpcError } from "../ipc/invoke";
import { DEFAULT_EDITOR_CHOICE, EDITOR_CHOICES, isEditorChoice } from "./editorChoice";
import { openInPreferredEditor, reportEditorFailure } from "./openInEditor";
import { openInEditor } from "./openInShell";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValue(undefined);
  toastStore.clear();
  interfaceStore.reset();
});
afterEach(() => {
  toastStore.clear();
  interfaceStore.reset();
});

describe("openInEditor (the invoke wrapper)", () => {
  it("sends the root and the relative path as two arguments, with the position and the closed choice", async () => {
    await openInEditor("/repos/gitpulse", "src/main.rs", { line: 12, column: 3, editor: "zed" });
    expect(invoke).toHaveBeenCalledWith("cmd_open_in_editor", {
      repo: "/repos/gitpulse",
      relative: "src/main.rs",
      line: 12,
      column: 3,
      editor: "zed",
    });
  });

  it("sends no position when none is known, so the backend opens the plain path", async () => {
    await openInEditor("/repos/gitpulse", "src/main.rs");
    const args = vi.mocked(invoke).mock.calls[0]?.[1] as Record<string, unknown>;
    expect(args.line).toBeUndefined();
    expect(args.column).toBeUndefined();
    expect(args.editor).toBeUndefined();
  });

  it("propagates the backend's refusal instead of resolving", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("Refusing to open a path outside the repository: ../x"));
    await expect(openInEditor("/repos/gitpulse", "../x")).rejects.toThrow("outside the repository");
  });
});

describe("the editor choice", () => {
  it("is exactly the four names Rust accepts, and nothing else parses", () => {
    expect(EDITOR_CHOICES.map((choice) => choice.value)).toEqual(["auto", "vscode", "antigravity", "cursor", "zed"]);
    expect(isEditorChoice(DEFAULT_EDITOR_CHOICE)).toBe(true);
    for (const other of ["code", "/usr/bin/vim", "VSCODE", "", null, undefined, 1, ["zed"]]) {
      expect(isEditorChoice(other), String(other)).toBe(false);
    }
  });
});

describe("openInPreferredEditor", () => {
  it("sends the editor the user chose in Settings", async () => {
    interfaceStore.setPreferredEditor("cursor");
    await openInPreferredEditor("/repos/gitpulse", "src/main.rs", { line: 7 });
    expect(invoke).toHaveBeenCalledWith("cmd_open_in_editor", {
      repo: "/repos/gitpulse",
      relative: "src/main.rs",
      line: 7,
      column: undefined,
      editor: "cursor",
    });
  });

  it("defaults to auto", async () => {
    await openInPreferredEditor("/repos/gitpulse", "src/main.rs");
    expect(vi.mocked(invoke).mock.calls[0]?.[1]).toMatchObject({ editor: "auto" });
  });

  it("refuses to store a name that is not one of the four", () => {
    interfaceStore.setPreferredEditor("vim" as never);
    expect(get(interfaceStore).preferredEditor).toBe("auto");
  });
});

describe("reportEditorFailure", () => {
  it("offers a way to the setting when the editor is not installed", () => {
    const opened = vi.fn();
    const target = new EventTarget();
    target.addEventListener("gitpulse:settings", opened);
    vi.stubGlobal("window", target);
    reportEditorFailure(new IpcError("editor_not_found", "Zed is not installed: choose another editor in Settings."));
    const [toast] = get(toastStore);
    expect(toast?.kind).toBe("error");
    expect(toast?.message).toContain("Zed is not installed");
    expect(toast?.action?.label).toBe("Choose editor");
    void toast?.action?.onClick();
    expect(opened).toHaveBeenCalledTimes(1);
    vi.unstubAllGlobals();
  });

  it("shows any other failure as it is, with nothing to click", () => {
    reportEditorFailure(new IpcError("unclassified", "VS Code could not open src/a.rs (exit status 3): bad option"));
    const [toast] = get(toastStore);
    expect(toast?.message).toContain("exit status 3");
    expect(toast?.action).toBeUndefined();
  });
});
