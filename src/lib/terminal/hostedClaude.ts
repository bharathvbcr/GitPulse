/**
 * Backward-compatibility wrapper for hostedClaude plus Claude-specific fork
 * source resolution for lane forking.
 * Generalized storage/lifecycle implementation lives in hostedAgent.ts.
 */
import type { PermissionMode } from "./agentDefaults";
import { isCaseInsensitiveFs, sameRepo, type PathIdentityOptions } from "../repos/paths";
import { isSessionId } from "./tabs";
import type { HostedAgent } from "./hostedAgent";

export {
  carriesSession,
  withSessionId,
  conversationForStart,
  type ConversationSpawnPlan,
  type HostedClaude,
  type HostedAgent,
  MAX_HOSTED_CLAUDE,
  MAX_HOSTED_AGENT,
  type HostedStorage,
  readHostedClaude,
  readHostedAgent,
  createHostedClaudeStore,
  createHostedAgentStore,
  hostedClaude,
  hostedAgent,
  type ResumeOutcome,
  type Resumer,
  type ExitFacts,
  endedFromOutside,
  settleConversation,
  AGY_CONTINUE_SENTINEL,
  type TranscriptCheckAnswer,
  resolveTranscriptAnswer,
  unindexedExitOffer,
  createResumers,
  resumers,
  type AgentProvider,
} from "./hostedAgent";

export interface ClaudeForkSource {
  sessionId: string | null;
  mode: PermissionMode | null;
}

export interface HostedClaudeLookup {
  list(): HostedAgent[];
  get(sessionId: string): HostedAgent | undefined;
}

function effectiveCheckoutPath(record: HostedAgent): string {
  const sub = record.startDir?.trim();
  if (!sub || sub === ".") return record.repoPath;
  if (sub.startsWith("/") || /^[A-Za-z]:[\\/]/.test(sub)) return sub;
  return `${record.repoPath}/${sub}`;
}

/**
 * Resolves the source Claude session id and recorded permission mode when
 * forking a lane. Prefers the backend-discovered session id when valid, matches
 * hosted tabs launched either directly in `checkoutPath` or via `startDir`,
 * and only carries a mode recorded on that exact Claude session so another
 * tab's authority (such as `bypass`) is never inherited implicitly.
 */
export function resolveClaudeForkSource(
  store: HostedClaudeLookup,
  checkoutPath: string,
  backendSessionId: string | null | undefined,
  options: PathIdentityOptions = { caseInsensitive: isCaseInsensitiveFs() },
): ClaudeForkSource {
  const hostedMatches = store
    .list()
    .filter(
      (record) =>
        record.provider === "claude" && sameRepo(effectiveCheckoutPath(record), checkoutPath, options),
    )
    .sort((a, b) => b.startedAt - a.startedAt);
  const trimmedBackendId = typeof backendSessionId === "string" ? backendSessionId.trim() : "";
  const validBackendId = trimmedBackendId && isSessionId(trimmedBackendId) ? trimmedBackendId.toLowerCase() : null;
  const sessionId = validBackendId ?? hostedMatches[0]?.sessionId ?? null;
  const owner = sessionId ? store.get(sessionId) : undefined;
  const mode = owner && owner.provider === "claude" ? owner.mode : null;
  return { sessionId, mode };
}
