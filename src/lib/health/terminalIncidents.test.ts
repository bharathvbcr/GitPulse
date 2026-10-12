import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalIncidentsSection from "../components/health/TerminalIncidentsSection.svelte";
import {
  MAX_INCIDENTS_SHOWN,
  describeSender,
  mergeIncident,
} from "./terminalIncidents";
import type { TerminalIncident } from "../terminal/incident";

const incident = (id: string, at_ms: number, over: Partial<TerminalIncident> = {}): TerminalIncident => ({
  id,
  signal: "Terminated: 15",
  at_ms,
  session_ids: ["t1", "t2", "t3"],
  sender: null,
  sender_note: "no launchd job reported a sender",
  ...over,
});

describe("terminal incidents", () => {
  it("merges newest first, one row per id, bounded", () => {
    let list: TerminalIncident[] = [];
    list = mergeIncident(list, incident("a", 100));
    list = mergeIncident(list, incident("b", 300));
    list = mergeIncident(list, incident("a", 100, { sender_note: "again" }));
    expect(list.map((i) => i.id)).toEqual(["b", "a"]);
    for (let n = 0; n < MAX_INCIDENTS_SHOWN + 10; n++) list = mergeIncident(list, incident(`x${n}`, 1000 + n));
    expect(list).toHaveLength(MAX_INCIDENTS_SHOWN);
    expect(list[0].id).toBe(`x${MAX_INCIDENTS_SHOWN + 9}`);
  });

  it("names the sender, or says why there is none", () => {
    expect(describeSender(incident("a", 1, { sender: { pid: 89829, name: "pkill" } }))).toBe("pkill[89829]");
    expect(describeSender(incident("a", 1))).toBe("unknown (no launchd job reported a sender)");
  });

  it("is a catalogued Health section and uses the command and event the backend defines", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const read = (p: string) => readFileSync(join(here, p), "utf8");
    expect(read("sections.ts")).toContain('id: "terminal-incidents"');
    expect(read("terminalIncidents.ts")).toContain('"cmd_terminal_incidents"');
    expect(read("terminalIncidents.ts")).toContain('"terminal-incident"');
    expect(read("../components/HealthPanel.svelte")).toContain("<TerminalIncidentsSection />");
    const rust = read("../../../src-tauri/src/terminal/incident.rs");
    for (const field of ["id", "signal", "at_ms", "session_ids", "sender", "sender_note"]) {
      expect(rust, field).toMatch(new RegExp(`pub ${field}:`));
    }
  });

  it("renders its section shell", () => {
    const { body } = render(TerminalIncidentsSection);
    expect(body).toContain('data-health-section="terminal-incidents"');
  });
});
