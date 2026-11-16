import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalIncidentsSection from "../components/health/TerminalIncidentsSection.svelte";
import {
  MAX_INCIDENTS_SHOWN,
  describeSender,
  mergeIncident,
} from "./terminalIncidents";
import { setSessionLedgerViewForTest, type SessionLedgerOffer } from "../terminal/sessionLedger";
import type { TerminalIncident } from "../terminal/incident";

afterEach(() => {
  setSessionLedgerViewForTest({ offers: [], reopenable_count: 0, unreadable_count: 0 });
});

const incident = (id: string, at_ms: number, over: Partial<TerminalIncident> = {}): TerminalIncident => ({
  id,
  signal: "Terminated: 15",
  at_ms,
  session_ids: ["t1", "t2", "t3"],
  sender: null,
  sender_note: "no launchd job reported a sender",
  ...over,
});

const rustEmits = (rust: string) => rust.includes('emit("terminal-incident"');

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
    // One owner for the event name: the Health view subscribes through the
    // shared constant, which names the event the backend emits.
    expect(read("terminalIncidents.ts")).toMatch(/listen<unknown>\(INCIDENT_EVENT,/);
    expect(read("../terminal/incident.ts")).toContain('INCIDENT_EVENT = "terminal-incident"');
    expect(rustEmits(read("../../../src-tauri/src/terminal/incident.rs"))).toBe(true);
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

  it("renders Reopen N agents ledger offers, not restorable shells, unreadable corrupt rows, and per-row errors in Health (S2-03 & S2-05)", () => {
    const base = (over: Partial<SessionLedgerOffer>): SessionLedgerOffer => ({
      row_id: "row-1",
      terminal_id: "term-1",
      launcher: "claude",
      native_id: "6f0a6d1e-1111-4222-8333-444455556661",
      repo_path: "/work/repo",
      start_dir: null,
      cwd: "/work/repo",
      mode: "edit",
      started_at: 1000,
      ended_at: 2000,
      end_state: "app_quit",
      signal: null,
      resumable: true,
      reason: null,
      last_error: null,
      ...over,
    });

    setSessionLedgerViewForTest({
      offers: [
        base({ row_id: "row-claude-1", end_state: "app_quit", mode: "bypass" }),
        base({ row_id: "row-claude-2", end_state: "lost", native_id: "6f0a6d1e-1111-4222-8333-444455556662" }),
        base({
          row_id: "row-shell",
          launcher: "shell",
          native_id: null,
          mode: null,
          resumable: false,
          reason: "not restorable",
        }),
        base({
          row_id: "row-gone",
          cwd: "/missing/cwd",
          resumable: false,
          reason: "Recorded working directory /missing/cwd no longer exists.",
          last_error: "Recorded working directory /missing/cwd no longer exists.",
        }),
        base({
          row_id: "corrupt-line-1",
          launcher: "unreadable",
          native_id: null,
          repo_path: "",
          cwd: "",
          mode: null,
          end_state: "unreadable",
          resumable: false,
          reason: "unreadable",
        }),
      ],
      reopenable_count: 2,
      unreadable_count: 1,
    });

    const { body } = render(TerminalIncidentsSection);
    expect(body).toContain('data-testid="health-session-ledger"');
    expect(body).toContain("Reopen 2 agents");
    expect(body).toContain('data-testid="health-ledger-not-restorable"');
    expect(body).toContain("not restorable");
    expect(body).toContain('data-testid="health-ledger-unreadable"');
    expect(body).toContain("unreadable");
    expect(body).toContain('data-testid="health-ledger-row-error"');
    expect(body).toContain("Recorded working directory /missing/cwd no longer exists.");
  });
});
