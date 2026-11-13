import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>());
vi.mock("../ipc/invoke", () => ({ invoke: invokeMock }));

import { anchorDownload, saveExport, type SaveRequest } from "./saveFile";

const theme: SaveRequest = { kind: "theme", fileName: "Dusk.json", data: '{"schema":"gitpulse-theme/1"}' };

function recorder() {
  const downloads: { fileName: string; blob: Blob }[] = [];
  return { downloads, download: (fileName: string, blob: Blob) => downloads.push({ fileName, blob }) };
}

describe("saveExport in the app", () => {
  it("sends the kind, the suggested name and the text to cmd_save_export", async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce("Dusk.json");
    const outcome = await saveExport(theme, { native: true });
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("cmd_save_export", {
      kind: "theme",
      fileName: "Dusk.json",
      data: '{"schema":"gitpulse-theme/1"}',
    });
    expect(outcome).toEqual({ status: "saved", fileName: "Dusk.json" });
  });

  it("reports the name the person saved under, not the suggestion", async () => {
    const outcome = await saveExport(theme, { native: true, saveNative: async () => "Renamed.json" });
    expect(outcome).toEqual({ status: "saved", fileName: "Renamed.json" });
  });

  it("a cancelled panel is cancelled: no download, no error", async () => {
    const { downloads, download } = recorder();
    const outcome = await saveExport(theme, { native: true, saveNative: async () => null, download });
    expect(outcome).toEqual({ status: "cancelled" });
    expect(downloads).toEqual([]);
  });

  it("a failed write rejects with the backend's sentence and never falls back to a download", async () => {
    const { downloads, download } = recorder();
    const failure = new Error("The folder chosen for Dusk.json no longer exists.");
    await expect(
      saveExport(theme, { native: true, saveNative: async () => { throw failure; }, download }),
    ).rejects.toBe(failure);
    expect(downloads).toEqual([]);
  });

  it("an answer that is neither a name nor null is a failure, not a save", async () => {
    for (const answer of [true, "", undefined, 42]) {
      await expect(saveExport(theme, { native: true, saveNative: async () => answer })).rejects.toThrow(
        "The save finished without saying which file it wrote.",
      );
    }
  });
});

describe("saveExport without a native shell", () => {
  it("asks the browser for a download of the text with the kind's media type", async () => {
    const { downloads, download } = recorder();
    const saveNative = vi.fn(async () => "never.json");
    const outcome = await saveExport(theme, { native: false, saveNative, download });
    expect(outcome).toEqual({ status: "requested", fileName: "Dusk.json" });
    expect(saveNative).not.toHaveBeenCalled();
    expect(downloads).toHaveLength(1);
    expect(downloads[0].fileName).toBe("Dusk.json");
    expect(downloads[0].blob.type).toBe("application/json");
    expect(await downloads[0].blob.text()).toBe(theme.data);
  });

  it("gives each kind its own media type", async () => {
    const { downloads, download } = recorder();
    await saveExport({ kind: "pulse_card", fileName: "r-pulse-card.svg", data: "<svg/>" }, { native: false, download });
    await saveExport({ kind: "terminal_output", fileName: "terminal-output.txt", data: "$ ls" }, { native: false, download });
    expect(downloads.map((d) => d.blob.type)).toEqual(["image/svg+xml", "text/plain"]);
  });

  it("without a Tauri runtime the default is the browser path", async () => {
    invokeMock.mockReset();
    const { downloads, download } = recorder();
    expect("__TAURI_INTERNALS__" in globalThis).toBe(false);
    const outcome = await saveExport(theme, { download });
    expect(outcome.status).toBe("requested");
    expect(invokeMock).not.toHaveBeenCalled();
    expect(downloads).toHaveLength(1);
  });
});

describe("anchorDownload", () => {
  it("clicks a download anchor, removes it, and revokes the URL only later", () => {
    vi.useFakeTimers();
    const created = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:test");
    const revoked = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    const clicks: { href: string; download: string }[] = [];
    const removed: string[] = [];
    const link = {
      href: "",
      download: "",
      click() { clicks.push({ href: this.href, download: this.download }); },
      remove() { removed.push(this.download); },
    };
    const doc = {
      createElement: () => link,
      body: { append: () => {} },
    } as unknown as Document;
    try {
      anchorDownload("card.svg", new Blob(["<svg/>"]), doc);
      expect(clicks).toEqual([{ href: "blob:test", download: "card.svg" }]);
      expect(removed).toEqual(["card.svg"]);
      expect(revoked).not.toHaveBeenCalled();
      vi.advanceTimersByTime(1000);
      expect(revoked).toHaveBeenCalledWith("blob:test");
    } finally {
      created.mockRestore();
      revoked.mockRestore();
      vi.useRealTimers();
    }
  });
});

describe("the exports route through saveExport", () => {
  const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");
  const sources = {
    theme: read("../components/ThemeFileSettings.svelte"),
    pulse: read("../components/pulse/PulseExportModal.svelte"),
    terminal: read("../components/TerminalSession.svelte"),
  };

  it("no export builds its own download anchor", () => {
    for (const source of Object.values(sources)) {
      expect(source).not.toMatch(/\.download\s*=/);
      expect(source).not.toContain("createObjectURL");
      expect(source).not.toContain("cmd_terminal_export");
    }
  });

  it("each export names its kind", () => {
    expect(sources.theme).toContain('saveExport({ kind: "theme"');
    expect(sources.pulse).toMatch(/saveExport\(\{\s*kind: "pulse_card"/);
    expect(sources.terminal).toContain('saveExport({ kind: "terminal_output"');
  });

  it("a cancelled save says nothing, and a failure is described", () => {
    for (const source of [sources.theme, sources.pulse]) {
      expect(source).toContain('outcome.status === "saved"');
      expect(source).toContain('outcome.status === "requested"');
      expect(source).not.toContain('outcome.status === "cancelled"');
      expect(source).toMatch(/describeFailure\(\{ verb: "save", object: "the (theme|card)" \}, error\)/);
    }
  });
});
