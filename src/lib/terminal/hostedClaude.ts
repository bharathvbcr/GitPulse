/**
 * Backward-compatibility wrapper for hostedClaude.
 * Generalized implementation lives in hostedAgent.ts.
 */
export {
  carriesSession,
  withSessionId,
  conversationForStart,
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
