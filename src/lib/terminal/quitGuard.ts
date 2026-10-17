/**
 * What quitting would end that is worth asking about.
 *
 * The registry knows the tabs this page holds, and a tab running an agent
 * launcher. It does not know a session the host keeps that no tab shows (the
 * registry was reloaded, or a lowered limit hid it), nor that `claude` was
 * typed into a plain shell tab. Both die with the app as surely as a launched
 * agent, so both are counted: the host's own session list is asked, and each
 * shell tab's foreground program is looked at.
 *
 * A question that cannot be answered is not "nothing is running", but it must
 * not wedge a quit either: the quit goes ahead on what is known, and the
 * failure is reported to the caller.
 */

import { invoke as tauriInvoke } from "../ipc/invoke";
import { parseListing } from "./detachedSessions";
import { parseTerminalContext } from "./sessionContext";
import { runningAgents, type TerminalSessionRecord } from "./sessionRegistry";
import { LAUNCHERS } from "./tabs";

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

/** One thing a quit would end. */
export interface AtRisk {
  label: string;
  repoPath: string;
}

const MAX_SHELL_TABS_PROBED = 32;

/** The program name a foreground process reports, reduced to a launcher kind. */
function agentKindOf(process: string | null): string | null {
  const stem = (process ?? "").trim().split(/[\\/]/).pop()?.replace(/\.exe$/i, "").toLowerCase() ?? "";
  return LAUNCHERS.find((launcher) => launcher.kind !== "shell" && launcher.kind === stem)?.kind ?? null;
}

export async function quitRisks(
  records: readonly TerminalSessionRecord[],
  deps: { invoke?: Invoke; onError?: (what: string, error: unknown) => void } = {},
): Promise<AtRisk[]> {
  const invoke = deps.invoke ?? (tauriInvoke as Invoke);
  const report = deps.onError ?? (() => {});
  const risks: AtRisk[] = runningAgents(records).map((record) => ({ label: record.label, repoPath: record.repoPath }));
  const known = new Set(records.map((record) => record.sessionId).filter((id): id is string => Boolean(id)));
  const counted = new Set(runningAgents(records).map((record) => record.sessionId));

  try {
    const raw = await invoke<unknown>("cmd_terminal_sessions");
    if (!Array.isArray(raw)) throw new Error("The terminal host returned an invalid session list.");
    for (const listing of raw.map(parseListing)) {
      if (!listing || known.has(listing.id)) continue;
      risks.push({ label: listing.launcher ?? "Terminal", repoPath: listing.repo });
    }
  } catch (error) {
    report("the host's session list", error);
  }

  const shells = records.filter((record) => record.sessionId && !counted.has(record.sessionId));
  await Promise.all(
    shells.slice(0, MAX_SHELL_TABS_PROBED).map(async (record) => {
      try {
        const context = parseTerminalContext(await invoke<unknown>("cmd_terminal_context", { sessionId: record.sessionId }));
        // A malformed answer is a check that did not run, and is reported as
        // one rather than passing silently as "no agent here".
        if (!context) throw new Error("The terminal host returned an unreadable context.");
        const agent = agentKindOf(context.process);
        if (agent && context.busy === true) risks.push({ label: `${record.label} (${agent})`, repoPath: record.repoPath });
      } catch (error) {
        report(`the foreground of ${record.label}`, error);
      }
    }),
  );
  if (shells.length > MAX_SHELL_TABS_PROBED) report("shell tabs", new Error(`only ${MAX_SHELL_TABS_PROBED} of ${shells.length} were checked`));
  return risks;
}
