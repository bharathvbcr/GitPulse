<script lang="ts">
  import { slide } from "svelte/transition";
  import { Activity, Archive, ArrowDown, ArrowUp, ArrowUpRight, Check, ChevronDown, ChevronRight, CircleAlert, Command, Copy, FileDiff, FolderOpen, GitBranch, Globe, HeartPulse, History, Layers, LayoutGrid, Moon, Power, RefreshCw, Settings2, Sun, Terminal } from "@lucide/svelte";
  import type { MenuState } from "./menuState";
  import { formatFetchAge, statusDetailRows, statusInsights, statusKeyAction, statusShortcuts } from "./menuState";
  let { snapshot, error = null, pending = false, material = "opaque", onaction }: {
    snapshot: MenuState | null; error?: string | null; pending?: boolean;
    material?: "opaque" | "preview" | "native";
    onaction: (id: string) => void;
  } = $props();
  let expanded = $state(false);
  let choosing = $state(false);
  let copied = $state<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  let clock = $state(Date.now());
  $effect(() => {
    const timer = setInterval(() => { clock = Date.now(); }, 30_000);
    return () => clearInterval(timer);
  });
  const card = $derived(snapshot?.status);
  const hasRepo = $derived(!!snapshot?.activePath);
  const fetchCaption = $derived(
    !card || card.ahead === null || card.behind === null
      ? null
      : card.fetchedAt == null
        ? "never fetched"
        : `fetched ${formatFetchAge(card.fetchedAt, clock)}`,
  );
  const enabled = (id: string) => !pending && !!snapshot?.enabled.includes(id);
  const dark = $derived(!!snapshot?.checked.includes("theme-dark"));
  const motion = $derived(card?.reduceMotion ? 0 : 180);
  const shortcuts = $derived(snapshot ? statusShortcuts(snapshot) : []);
  const go = $derived(shortcuts.filter((item) => item.group === "go"));
  const tools = $derived(shortcuts.filter((item) => item.group === "tool"));
  const insights = $derived(card ? statusInsights(card) : []);
  const details = $derived(snapshot ? statusDetailRows(snapshot.trayDetails) : []);
  const metrics = $derived([
    { label: "Changed", value: card?.changed, icon: FileDiff, action: "section:work:overview" },
    { label: "Staged", value: card?.staged, icon: Layers, action: "section:work:overview" },
    { label: "Conflicts", value: card?.conflicts, icon: CircleAlert, action: "section:work:resolve" },
  ]);
  const shortcutIcon = $derived<Record<string, typeof History>>({
    "section:history:graph": History, "section:insights:pulse": HeartPulse, fleet: LayoutGrid,
    "terminal-dock": Terminal, "copy-branch": Copy, "reveal-repo": FolderOpen, "open-remote": Globe,
    "toggle-theme": dark ? Sun : Moon,
  });
  function act(id: string) {
    if (id.startsWith("copy-")) {
      copied = id;
      clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => { copied = null; }, 1400);
    }
    onaction(id);
  }
  function handleKey(event: KeyboardEvent) {
    const next = statusKeyAction(event, snapshot, { choosing, expanded });
    if (!next) return;
    event.preventDefault();
    if (next.collapse === "chooser") choosing = false;
    else if (next.collapse === "details") expanded = false;
    else if (next.dismiss) act("dismiss");
    else if (next.id && !pending) act(next.id);
  }
</script>

<svelte:window onkeydown={handleKey} />

<div class="status-shell" class:reduce-motion={card?.reduceMotion} data-testid="status-popover" data-material={material} data-tone={card?.tone ?? "neutral"}>
  <section class="panel" aria-label="GitPulse status">
    <header>
      <div class="mark" aria-hidden="true"><Activity size={21} strokeWidth={1.8} /></div>
      <div class="identity">
        <button class="repository" onclick={() => choosing = !choosing} disabled={(snapshot?.repositories.length ?? 0) < 2}
          aria-expanded={choosing} aria-label="Choose repository" title={snapshot?.activePath ?? "GitPulse"}>
          <span>{card?.repository ?? "GitPulse"}</span>
          {#if snapshot && snapshot.repositories.length > 1}
            <ChevronDown size={13} class={choosing ? "expanded" : ""} />
          {/if}
        </button>
        <div class="branch" title={card?.branch}><GitBranch size={12} /><span>{card?.branch || "Menu bar companion"}</span></div>
      </div>
      <button class="icon-button refresh" aria-label="Refresh repository" title="Refresh · R"
        disabled={!enabled("refresh")} onclick={() => act("refresh")}>
        <RefreshCw size={15} class={card?.tone === "busy" || pending ? "rotating" : ""} />
      </button>
    </header>

    {#if choosing && snapshot}
      <nav class="repositories" aria-label="Open repositories" transition:slide={{ duration: motion }}>
        {#each snapshot.repositories as repo (repo.path)}
          <button class:current={repo.active} disabled={pending} title={repo.path}
            onclick={() => { choosing = false; act(`activate-repo:${repo.path}`); }}>
            <span>
              <strong>{repo.label}</strong>
              <em>{repo.path}</em>
              {#if repo.busy || repo.changed != null || repo.conflicts}
                <span class="repo-badges" aria-hidden="true">
                  {#if repo.busy}<span class="badge busy">…</span>{/if}
                  {#if repo.changed}<span class="badge">{repo.changed}</span>{/if}
                  {#if repo.conflicts}<span class="badge warn">⚠{repo.conflicts}</span>{/if}
                </span>
              {/if}
            </span>
            {#if repo.active}<Check size={14} />{/if}
          </button>
        {/each}
      </nav>
    {/if}

    <div class="content">
      {#if !snapshot}
        <div class="empty"><div class="empty-mark"><Activity size={27} /></div><h1>{error ? "Status unavailable" : "Connecting…"}</h1>
          <p>{error || "Getting your workspace status."}</p>
          {#if error}<button class="primary" onclick={() => act("retry")}>Try again</button>{/if}
        </div>
      {:else if !hasRepo}
        <div class="empty"><div class="empty-mark"><GitBranch size={29} strokeWidth={1.5} /></div>
          <h1>Your work, at a glance</h1><p>Open a repository to see what needs attention.</p>
          <button class="primary" disabled={pending} onclick={() => act("open")}>Open repository <ArrowUpRight size={15} /></button>
          {#if enabled("clone")}
            <button class="secondary" disabled={pending} onclick={() => act("clone")}>Clone repository</button>
          {/if}
        </div>
      {:else if card}
        <div class="status-heading" aria-live="polite">
          <span class="status-dot" class:pulse={card.tone === "busy"}></span>
          <h1>{card.headline}</h1>
        </div>
        <div class="metrics" aria-label="Working tree counts">
          {#each metrics as metric}
            <button class="metric" class:attention={metric.label === "Conflicts" && !!metric.value}
              class:zero={metric.value === 0} class:unknown={metric.value == null}
              disabled={!enabled(metric.action) || !metric.value}
              aria-label={`${metric.value ?? "Unknown"} ${metric.label.toLowerCase()}`} onclick={() => act(metric.action)}>
              <span class="metric-label"><metric.icon size={12} />{metric.label}</span>
              <strong>{metric.value ?? "—"}</strong>
            </button>
          {/each}
        </div>
        <div class="sync" title="Compared with the upstream state from the last fetch">
          {#if card.ahead !== null && card.behind !== null}
            <div class="sync-counts">
              <span aria-label={`${card.ahead} ahead`}><ArrowUp size={12} />{card.ahead}</span>
              <span aria-label={`${card.behind} behind`}><ArrowDown size={12} />{card.behind}</span>
            </div>
            <span class="upstream">{card.upstream}</span><span class="caption">{fetchCaption}</span>
          {:else}
            <GitBranch size={13} /><span class="upstream">{card.branch ? "No upstream data" : "Branch unavailable"}</span>
          {/if}
        </div>
        <button class="primary" disabled={!enabled(snapshot.traySummary.id)}
          onclick={() => act(snapshot.traySummary.id)}>
          {card.primaryLabel}<ArrowUpRight size={15} />
        </button>
        <button class="details-toggle" aria-expanded={expanded} aria-controls="status-details" onclick={() => expanded = !expanded}>
          <span><ChevronRight size={13} class={expanded ? "expanded" : ""} />Details</span>
          <span class="live" class:degraded={card.watchStatus === "degraded"}>
            <span></span>{card.watchStatus === "watching" ? "Live" : card.watchStatus === "degraded" ? "Not live" : "Connecting"}
          </span>
        </button>
        {#if expanded}
          <!-- Justified: the bounded details region needs focus for keyboard scrolling. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div class="details" id="status-details" tabindex="0" role="region" aria-label="Repository details"
            transition:slide={{ duration: motion }}>
            <div class="detail-actions">
              <button class="detail-action" aria-label={`${card.stashes ?? "Unknown"} stashes`}
                disabled={!enabled("section:work:overview") || !card.stashes} onclick={() => act("section:work:overview")}>
                <Archive size={13} /><span>Stashes</span><strong>{card.stashes ?? "—"}</strong>
              </button>
              <button class="detail-action" aria-label="Command palette" disabled={!enabled("palette")} onclick={() => act("palette")}>
                <Command size={13} /><span>Command palette</span>
              </button>
            </div>
            {#if insights.length}
              <div class="insights" aria-label="Workspace insights">
                {#each insights as insight}
                  <button class="chip" class:warning={insight.tone === "warning"} class:busy={insight.tone === "busy"}
                    disabled={!enabled(insight.id)} onclick={() => act(insight.id)}>{insight.text}</button>
                {/each}
              </div>
            {/if}
            <div class="detail-rows">
              {#each details as row}
                <div>{#if row.label}<span>{row.label}</span>{/if}{row.value}</div>
              {/each}
            </div>
            {#if go.length}
              <div class="shortcut-block">
                <span class="group-label">Go</span>
                <div class="shortcuts" aria-label="Go">
                  {#each go as item}
                    {@const Icon = shortcutIcon[item.id]}
                    <button class="shortcut" disabled={!enabled(item.id)} title={item.label} onclick={() => act(item.id)}>
                      {#if Icon}<Icon size={13} />{/if}<span>{item.label}</span>
                    </button>
                  {/each}
                </div>
              </div>
            {/if}
            {#if tools.length}
              <div class="shortcut-block">
                <span class="group-label">Tools</span>
                <div class="shortcuts" aria-label="Tools">
                  {#each tools as item}
                    {@const Icon = shortcutIcon[item.id]}
                    <button class="shortcut" class:copied={copied === item.id} disabled={!enabled(item.id)}
                      title={item.label} onclick={() => act(item.id)}>
                      {#if copied === item.id}<Check size={13} />{:else if Icon}<Icon size={13} />{/if}
                      <span>{copied === item.id ? "Copied" : item.label}</span>
                    </button>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
        {/if}
      {/if}
      {#if error && snapshot}<div class="error" role="alert"><CircleAlert size={14} /><span>{error}</span></div>{/if}
    </div>
    <footer>
      <button class="open-app" onclick={() => act("show")}>Open GitPulse <ArrowUpRight size={12} /></button>
      <div>
        <button class="icon-button" aria-label="Settings" title="Settings" onclick={() => act("settings")}><Settings2 size={14} /></button>
        <button class="icon-button" aria-label="Quit GitPulse" title="Quit GitPulse" onclick={() => act("quit")}><Power size={14} /></button>
      </div>
    </footer>
  </section>
</div>

<style>
  .status-shell {
    --base:250 251 252;
    --card-base:255 255 255;
    --solid-muted:#69727f;
    --glass-muted:#424c5b;
    --glass-blue:#174cb0;
    --glass-green:#185b39;
    --glass-amber:#75450b;
    --sheen:rgb(255 255 255 / .18);
    --bg:#fafbfc;
    --surface:#fff;
    --text:#20242c;
    --muted:#69727f;
    --line:#e5e8ed;
    --soft:#f0f2f5;
    --blue:#2563eb;
    --action:#2563eb;
    --action-hover:#1e55ce;
    --green:#258354;
    --amber:#ac6719; padding:8px; color:var(--text); font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; font-size:12px; -webkit-font-smoothing:antialiased;
  }
  :global(:where(.dark)) .status-shell {
    --base:32 35 41;
    --card-base:41 45 52;
    --solid-muted:#a1a9b6;
    --glass-muted:#c2c9d5;
    --glass-blue:#b6ceff;
    --glass-green:#95ddbb;
    --glass-amber:#ffd099;
    --sheen:rgb(255 255 255 / .025);
    --bg:#202329;
    --surface:#292d34;
    --text:#f0f2f5;
    --muted:#a1a9b6;
    --line:#383d46;
    --soft:#2b3038;
    --blue:#558aff;
    --action:#326ade;
    --action-hover:#285fcf;
    --green:#76c59d;
    --amber:#f1bb70;
  }
  @media(prefers-color-scheme:dark) { :global(:where(html:not(.light))) .status-shell {
    --base:32 35 41;
    --card-base:41 45 52;
    --solid-muted:#a1a9b6;
    --glass-muted:#c2c9d5;
    --glass-blue:#b6ceff;
    --glass-green:#95ddbb;
    --glass-amber:#ffd099;
    --sheen:rgb(255 255 255 / .025);
    --bg:#202329;
    --surface:#292d34;
    --text:#f0f2f5;
    --muted:#a1a9b6;
    --line:#383d46;
    --soft:#2b3038;
    --blue:#558aff;
    --action:#326ade;
    --action-hover:#285fcf;
    --green:#76c59d;
    --amber:#f1bb70;
  } }
  .panel { background:var(--bg); border:1px solid var(--line); border-radius:18px; overflow:hidden; box-shadow:0 6px 20px #00000012,0 1px 3px #00000008; }
  /* The native window server supplies desktop blur. Only the browser fixture
     uses a CSS filter, sampling its simulated background instead. */
  .status-shell[data-material="native"] { padding:0; --bg:rgb(var(--base) / .8); --surface:rgb(var(--card-base) / .64); --muted:var(--glass-muted); --blue:var(--glass-blue); --green:var(--glass-green); --amber:var(--glass-amber); }
  .status-shell[data-material="native"] .panel { box-shadow:none; background-image:linear-gradient(155deg,var(--sheen),transparent 46%); }
  @supports ((backdrop-filter:blur(1px)) or (-webkit-backdrop-filter:blur(1px))) {
    .status-shell[data-material="preview"] { --bg:rgb(var(--base) / .8); --surface:rgb(var(--card-base) / .64); --muted:var(--glass-muted); --blue:var(--glass-blue); --green:var(--glass-green); --amber:var(--glass-amber); }
    .status-shell[data-material="preview"] .panel {
      -webkit-backdrop-filter:blur(34px) saturate(135%);
      backdrop-filter:blur(34px) saturate(135%);
      background-image:linear-gradient(155deg,var(--sheen),transparent 46%);
    }
  }
  @media (prefers-reduced-transparency: reduce), (prefers-contrast: more), (forced-colors: active) {
    .status-shell[data-material] { --bg:rgb(var(--base)); --surface:rgb(var(--card-base)); --muted:var(--solid-muted); }
    .status-shell[data-material] .panel { -webkit-backdrop-filter:none; backdrop-filter:none; background-image:none; box-shadow:none; }
  }
  button { font:inherit; cursor:pointer; border:0; color:inherit; background:none; padding:0; }
  button:disabled { cursor:default; }
  button:focus-visible,.details:focus-visible { outline:2px solid var(--blue); outline-offset:3px; }
  header { display:flex; align-items:center; gap:10px; padding:20px 20px 17px; }
  .mark { width:34px; height:34px; flex-shrink:0; display:grid; place-items:center; border:1px solid var(--line); border-radius:10px; background:var(--surface); }
  .identity { min-width:0; flex:1; }
  .repository { display:flex; align-items:center; gap:6px; max-width:100%; font-weight:650; font-size:15px; letter-spacing:-.2px; text-align:left; }
  .repository span,.branch span,.upstream { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .repository:not(:disabled):hover { color:var(--blue); }
  .branch { display:flex; align-items:center; gap:4px; margin-top:5px; color:var(--muted); font-size:11px; }
  .icon-button { width:28px; height:28px; display:inline-grid; place-items:center; border-radius:7px; color:var(--muted); }
  .icon-button:hover:not(:disabled) { background:var(--soft); color:var(--text); }
  .icon-button:disabled { opacity:.4; }
  .content { padding:0 20px; }
  .status-heading { display:flex; align-items:center; gap:7px; margin:0 0 16px; }
  h1 { font-size:12px; font-weight:500; margin:0; letter-spacing:-.1px; }
  .status-dot { width:6px; height:6px; border-radius:50%; background:var(--muted); flex-shrink:0; }
  [data-tone="clean"] .status-dot { background:var(--green); }
  [data-tone="warning"] .status-dot { background:var(--amber); }
  [data-tone="changed"] .status-dot,[data-tone="busy"] .status-dot { background:var(--blue); }
  .metrics { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:8px; }
  .metric { text-align:left; padding:12px 10px; border:1px solid var(--line); background:var(--surface); border-radius:10px; min-width:0; transition:background .14s,border-color .14s; }
  .metric:hover:not(:disabled) { border-color:var(--blue); background:var(--soft); }
  .metric-label { display:flex; align-items:center; gap:4px; font-size:10px; color:var(--muted); }
  .metric strong { display:block; margin-top:8px; font-size:27px; line-height:1; font-weight:550; letter-spacing:-1px; font-variant-numeric:tabular-nums; }
  .metric.attention { color:var(--amber); border-color:color-mix(in srgb,var(--amber) 40%,var(--line)); background:color-mix(in srgb,var(--amber) 7%,var(--surface)); }
  .sync { display:flex; align-items:center; gap:8px; min-width:0; color:var(--muted); margin:14px 0 18px; font-size:10px; }
  .sync-counts { display:flex; gap:9px; color:var(--text); font-variant-numeric:tabular-nums; }
  .sync-counts span { display:flex; align-items:center; gap:2px; }
  .upstream { flex:1; }
  .caption { font-size:9px; }
  .primary { display:flex; align-items:center; justify-content:center; gap:8px; width:100%; min-height:34px; color:#fff; background:var(--action); border-radius:8px; font-size:12px; font-weight:550; box-shadow:0 1px 2px #00000012; }
  .primary:hover:not(:disabled) { background:var(--action-hover); }
  .primary:disabled { opacity:.45; }
  .details-toggle { display:flex; align-items:center; justify-content:space-between; width:100%; min-height:43px; font-size:10px; color:var(--muted); }
  .details-toggle>span { display:flex; align-items:center; gap:4px; }
  .details-toggle:hover { color:var(--text); }
  .details-toggle :global(.expanded) { transform:rotate(90deg); }
  .live { font-size:9px; }
  .live>span { width:4px; height:4px; background:var(--muted); border-radius:50%; }
  .live.degraded { color:var(--amber); }
  .live.degraded>span { background:var(--amber); }
  .details { max-height:190px; overflow:auto; padding:0 0 14px; margin-bottom:2px; font-size:10px; line-height:1.5; color:var(--muted); overflow-wrap:anywhere; }
  .detail-rows>div { display:flex; gap:8px; padding:7px 0; border-top:1px solid var(--line); }
  .detail-rows span { flex:0 0 62px; font-weight:550; }
  .detail-actions { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:6px; padding-bottom:12px; }
  .detail-action { display:flex; align-items:center; gap:6px; padding:9px 8px; border:1px solid var(--line); border-radius:7px; background:var(--surface); }
  .detail-action strong { margin-left:auto; color:var(--text); font-variant-numeric:tabular-nums; }
  .detail-action:disabled { opacity:.55; }
  .detail-action:hover:not(:disabled) { color:var(--text); background:var(--soft); }
  .shortcut-block { padding:10px 0 0; border-top:1px solid var(--line); }
  .group-label { display:block; margin-bottom:6px; font-size:10px; font-weight:550; }
  .shortcuts { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:4px; padding-bottom:10px; }
  .shortcut { display:flex; align-items:center; gap:6px; padding:8px; border-radius:6px; text-align:left; font-size:10px; }
  .shortcut:hover:not(:disabled) { color:var(--text); background:var(--soft); }
  .shortcut:disabled { opacity:.45; }
  .shortcut.copied { color:var(--green); }
  .insights { display:flex; flex-wrap:wrap; gap:6px; padding-bottom:12px; }
  .chip { padding:6px 8px; border:1px solid var(--line); border-radius:6px; font-size:10px; text-align:left; }
  .chip.warning { color:var(--amber); }
  .chip.busy { color:var(--blue); }
  .chip:hover:not(:disabled) { background:var(--soft); }
  .secondary { display:flex; align-items:center; justify-content:center; width:100%; min-height:32px; margin-top:8px; color:var(--muted); }
  .secondary:hover:not(:disabled) { color:var(--blue); }
  .repository :global(.expanded) { transform:rotate(180deg); }
  footer>div { display:flex; }
  .metric.unknown strong { color:var(--muted); }
  footer { display:flex; justify-content:space-between; align-items:center; border-top:1px solid var(--line); padding:8px 14px 8px 20px; }
  .open-app { display:flex; align-items:center; gap:4px; color:var(--muted); font-size:10px; }
  .open-app:hover { color:var(--text); }
  .repositories { max-height:180px; overflow:auto; border:1px solid var(--line); background:var(--surface); margin:0 20px 15px; padding:4px; border-radius:9px; }
  .repositories button { display:flex; align-items:center; justify-content:space-between; gap:10px; width:100%; text-align:left; padding:8px; border-radius:5px; }
  .repositories span { min-width:0; display:flex; flex-direction:column; gap:3px; }
  .repositories strong,.repositories em { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .repositories strong { font-weight:550; }
  .repositories em { font-size:10px; font-style:normal; color:var(--muted); }
  .repositories button:hover,.repositories .current { background:var(--soft); }
  .repo-badges { display:flex; flex-direction:row; gap:4px; margin-top:2px; }
  .repo-badges .badge { font-size:10px; font-variant-numeric:tabular-nums; color:var(--muted); background:var(--soft); border-radius:4px; padding:1px 5px; }
  .repo-badges .badge.warn { color:var(--amber); }
  .repo-badges .badge.busy { letter-spacing:.5px; }
  .empty { text-align:center; padding:6px 0 22px; }
  .empty-mark { width:56px; height:56px; border-radius:16px; display:grid; place-items:center; margin:0 auto 16px; background:var(--soft); color:var(--muted); }
  .empty h1 { font-size:16px; font-weight:600; }
  .empty p { color:var(--muted); line-height:1.5; margin:9px auto 20px; max-width:230px; }
  .error { display:flex; gap:7px; color:var(--amber); font-size:11px; line-height:1.4; margin:0 0 14px; }
  .error>span { min-width:0; overflow-wrap:anywhere; }
  .error :global(svg) { flex-shrink:0; margin-top:1px; }
  :global(.rotating) { animation:spin 1.5s linear infinite; }
  .pulse { animation:pulse 1.5s ease-in-out infinite; }
  .reduce-motion :global(*) { animation:none!important; transition:none!important; }
  @keyframes spin { to { transform:rotate(360deg); } }
  @keyframes pulse { 50% { opacity:.35; } }
  @media(prefers-reduced-motion:reduce) { * { animation:none!important; transition:none!important; } :global(.rotating) { animation:none!important; } }
</style>
