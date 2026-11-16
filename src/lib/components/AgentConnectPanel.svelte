<script lang="ts">
  /**
   * Connect Claude Code and Codex to GitPulse's MCP server — previewed first.
   *
   * Same shape as `AgentIntegrationPanel`: read a plan, show every action with
   * its path or command, and write only after the user confirms the plan shown.
   * What Apply does is link two binaries into `~/.local/bin` and register the
   * server through each agent's own CLI; nothing else, and nothing the plan did
   * not list. "Copy setup prompt" is the no-click alternative: the same facts
   * as text for the user's own agent.
   */
  import { AlertTriangle, Check, Clipboard, RefreshCw } from "@lucide/svelte";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { askConfirm } from "../stores/modalStore";
  import { copyText } from "../desktop/clipboard";
  import Skeleton from "./Skeleton.svelte";
  import {
    applyAgentConnect,
    buildSetupPrompt,
    canConnect,
    connectConfirmText,
    connectSummary,
    getAgentConnectPlan,
    type ConnectPlan,
    type ConnectResult,
  } from "../tools/agentConnect";

  let { active = true }: { active?: boolean } = $props();

  let plan = $state<ConnectPlan | null>(null);
  let results = $state<ConnectResult[]>([]);
  let loading = $state(false);
  let applying = $state(false);
  let error = $state<string | null>(null);
  let copyNote = $state<string | null>(null);
  let loaded = $state(false);

  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  async function refresh() {
    if (loading) return;
    loading = true;
    try {
      plan = await getAgentConnectPlan();
      error = null;
    } catch (cause) {
      plan = null;
      error = failure("check", "the agent connection", cause);
    } finally {
      loading = false;
      loaded = true;
    }
  }

  async function apply() {
    if (!plan || applying || !canConnect(plan)) return;
    // The app's own dialog: the native confirm answers null under WKWebView.
    const ok = await askConfirm({
      title: "Connect your agent?",
      message: connectConfirmText(plan),
      confirmLabel: "Connect",
    });
    if (!ok) return;
    applying = true;
    try {
      const outcome = await applyAgentConnect();
      results = outcome.results;
      plan = outcome.plan;
      error = null;
    } catch (cause) {
      error = failure("connect", "your agent", cause);
    } finally {
      applying = false;
    }
  }

  async function copyPrompt() {
    if (!plan) return;
    const windows = typeof navigator !== "undefined" && /Win/i.test(navigator.platform);
    copyNote = (await copyText(buildSetupPrompt(plan, windows)))
      ? "Copied. Paste it into Claude Code or Codex."
      : "Couldn't copy. Try again.";
  }

  function resultFor(id: string): ConnectResult | undefined {
    return results.find((r) => r.id === id);
  }

  $effect(() => {
    if (active && !loaded) void refresh();
  });
</script>

<div class="space-y-2" data-testid="agent-connect">
  <div class="flex items-center justify-between gap-2">
    <h4 class="text-textPrimary text-meta font-medium">Connect your agent</h4>
    <button type="button" class="gp-btn" data-size="sm" disabled={loading} onclick={() => void refresh()}>
      <RefreshCw size={10} class={loading ? "animate-spin" : undefined} />
      Rescan
    </button>
  </div>

  <p class="text-textMuted text-micro leading-snug">
    Lets Claude Code and Codex start GitPulse's MCP server. Links the bundled binaries into
    <span class="font-mono">~/.local/bin</span> and registers the server with each agent's own
    command. Previewed first; nothing is written until you confirm.
  </p>

  {#if error}
    <div class="gp-notice" data-tone="danger" role="alert">{error}</div>
  {/if}

  {#if loading && !plan}
    <Skeleton variant="text" count={3} />
  {:else if plan}
    <p class="text-textMuted text-micro leading-snug" data-testid="connect-summary">{connectSummary(plan)}</p>
    {#if plan.agents_found.length === 0 && plan.available}
      <p class="text-textMuted text-micro leading-snug" data-testid="connect-no-agents">
        No Claude Code or Codex command was found on this machine. Copy the setup prompt and give it to your agent instead.
      </p>
    {/if}
    {#if plan.actions.length > 0}
      <ul class="space-y-1.5">
        {#each plan.actions as action (action.id)}
          {@const result = resultFor(action.id)}
          <li class="rounded-xl bg-background/60 p-2 text-micro leading-snug" data-testid="connect-action">
            <span class="flex items-center gap-1.5">
              {#if action.state === "blocked" || (result && !result.ok)}
                <AlertTriangle size={11} class="text-warn shrink-0" />
              {:else if action.state === "done"}
                <Check size={11} class="text-ok shrink-0" />
              {/if}
              <span class="text-textPrimary text-meta font-medium">{action.title}</span>
            </span>
            <span class="text-textMuted block font-mono break-all opacity-80">{action.target}</span>
            <span class="text-textMuted block">{action.detail}</span>
            {#if result}
              <span class={result.ok ? "text-ok block" : "text-warn block"} data-testid="connect-result">{result.message}</span>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  <div class="flex flex-wrap gap-1.5">
    <button
      type="button"
      class="gp-btn"
      data-size="sm"
      disabled={applying || !plan || !canConnect(plan)}
      onclick={() => void apply()}
    >
      {applying ? "Connecting…" : "Connect"}
    </button>
    <button
      type="button"
      class="gp-btn inline-flex items-center gap-1"
      data-size="sm"
      disabled={!plan}
      onclick={() => void copyPrompt()}
    >
      <Clipboard size={10} />
      Copy setup prompt
    </button>
  </div>
  {#if copyNote}
    <p class="text-textMuted text-micro" role="status">{copyNote}</p>
  {/if}
</div>
