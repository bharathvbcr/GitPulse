/**
 * The Agents plane as the header reads it: one projection shared by the view
 * bar's Agents pill and every repository tab's attention mark.
 *
 * It is the plane the Agents surface projects (`projectAgentPlane`) over the
 * same stores, minus task attempts (`tasks: null`: the header does not watch
 * the board). A projection per tab, or per pill, would redo the same tree
 * walk, and `sessionActivity` publishes as often as once a second per session,
 * so the store re-emits only when what the plane reads has changed.
 */

import { readable, type Readable } from "svelte/store";
import { isCaseInsensitiveFs } from "../repos/paths";
import { sessionActivity, type SessionActivity } from "../terminal/sessionActivity";
import { terminalSessions } from "../terminal/sessionRegistry";
import { agentDirectories } from "./cwd";
import { planeTerminals } from "./repoAttention";
import { projectAgentPlane, type AgentPlane, type PlaneProbe } from "./plane";
import { agentPlaneStore } from "./store";

export interface HeaderPlane {
  plane: AgentPlane;
  /** What the last sweep read, for telling "no agents" from "never read". */
  probes: readonly PlaneProbe[];
}

const paths = () => ({ caseInsensitive: isCaseInsensitiveFs() });

const EMPTY: HeaderPlane = {
  plane: projectAgentPlane({ probes: [], terminals: [], tasks: null, paths: { caseInsensitive: false } }),
  probes: [],
};

export const headerAgentPlane: Readable<HeaderPlane> = readable(EMPTY, (set) => {
  let sessions: Parameters<typeof planeTerminals>[0] = [];
  let activity: ReadonlyMap<string, SessionActivity> = new Map();
  let directories: ReadonlyMap<string, string> = new Map();
  let probes: readonly PlaneProbe[] = [];
  let signature = "";
  let probesSeen: readonly PlaneProbe[] | null = null;
  let started = false;

  const refresh = () => {
    if (!started) return;
    const terminals = planeTerminals(sessions, activity, directories);
    // Probes compare by identity (a sweep replaces the array); terminals by
    // value, because an unrelated output tick republishes the same facts.
    const next = `${JSON.stringify(terminals)}|${probes.length}`;
    if (next === signature && probesSeen === probes) return;
    signature = next;
    probesSeen = probes;
    set({ plane: projectAgentPlane({ probes, terminals, tasks: null, paths: paths() }), probes });
  };

  const stops = [
    terminalSessions.subscribe((value) => { sessions = value; refresh(); }),
    sessionActivity.subscribe((value) => { activity = value; refresh(); }),
    agentDirectories.subscribe((value) => { directories = value; refresh(); }),
    agentPlaneStore.subscribe((value) => { probes = value.probes; refresh(); }),
  ];
  started = true;
  refresh();
  return () => {
    for (const stop of stops) stop();
  };
});
