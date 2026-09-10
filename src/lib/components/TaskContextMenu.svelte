<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { portal } from "../dom/portal";
  import { clampMenuPosition } from "../branches/menuPosition";
  import { cycleFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { STATUSES, STATUS_LABELS, type TaskStatus } from "../workbench/client";
  import { PRIORITY_LABELS } from "../workbench/boardDrag";
  import { Sparkles, Trash2, Copy, ChevronRight, ArrowLeft, CheckSquare, SquarePen } from "@lucide/svelte";
  type Action = "open" | "enhance" | "duplicate" | "copy-title" | "copy-brief" | "select" | "delete" | { status: TaskStatus } | { priority: number };
  let { x, y, count, selected, onAction, onClose }: { x:number; y:number; count:number; selected:boolean; onAction:(action:Action)=>void; onClose:(restore?:boolean)=>void } = $props();
  let menu: HTMLDivElement;
  let page = $state("root");
  let left = $state(untrack(() => x)), top = $state(untrack(() => y));
  $effect(() => {
    page;
    void tick().then(() => {
      if (!menu?.isConnected) return;
      const pos = clampMenuPosition(x,y,menu.offsetWidth,menu.offsetHeight,window.innerWidth-8,window.innerHeight-8);
      left = Math.max(8,pos.left); top = Math.max(8,pos.top);
      menu.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
    });
  });
  onMount(() => {
    const outside = (e: Event) => { if (e.target instanceof Node && !menu.contains(e.target)) onClose(false); };
    const close = () => onClose(false);
    document.addEventListener("pointerdown",outside,true); document.addEventListener("contextmenu",outside,true);
    document.addEventListener("scroll",outside,true); window.addEventListener("resize",close);
    return () => { document.removeEventListener("pointerdown",outside,true); document.removeEventListener("contextmenu",outside,true); document.removeEventListener("scroll",outside,true); window.removeEventListener("resize",close); };
  });
  function key(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Escape" || e.key === "Tab") { e.preventDefault(); onClose(); }
    else if(e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); cycleFocus(menu,e.key === "ArrowDown"); }
    else if(e.key === "ArrowLeft" && page !== "root") { e.preventDefault(); page = "root"; }
    else if(e.key === "Home" || e.key === "End") { e.preventDefault(); const buttons = menu.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'); buttons[e.key === "Home" ? 0 : buttons.length-1]?.focus(); }
  }
</script>
<div bind:this={menu} use:portal class="task-context gp-menu gp-pop" style="left:{left}px;top:{top}px;z-index:{LAYERS.MENU}" role="menu" aria-label="Task actions" tabindex="-1" onkeydown={key}>
  {#if page === "root"}
    <div class="caption">{count > 1 ? `${count} selected tasks` : "Task actions"}</div>
    <button role="menuitem" class="gp-menu-item" disabled={count !== 1} onclick={() => onAction("open")}><SquarePen size={13} />Open task</button>
    <button role="menuitem" class="gp-menu-item" disabled={count !== 1} onclick={() => onAction("enhance")}><Sparkles size={13} />Quick Enhance</button>
    <button role="menuitem" class="gp-menu-item" onclick={() => { page = "status"; }}>Move to…<ChevronRight size={13} /></button>
    <button role="menuitem" class="gp-menu-item" onclick={() => { page = "priority"; }}>Set priority…<ChevronRight size={13} /></button>
    <div class="gp-menu-sep"></div>
    <button role="menuitem" class="gp-menu-item" disabled={count !== 1} onclick={() => onAction("duplicate")}><Copy size={13} />Duplicate task…</button>
    <button role="menuitem" class="gp-menu-item" onclick={() => onAction("copy-title")}>Copy {count > 1 ? "task titles" : "task title"}</button>
    <button role="menuitem" class="gp-menu-item" disabled={count !== 1} onclick={() => onAction("copy-brief")}>Copy saved brief</button>
    <button role="menuitem" class="gp-menu-item" onclick={() => onAction("select")}><CheckSquare size={13} />{selected ? "Clear selection" : "Select task"}</button>
    <div class="gp-menu-sep"></div>
    <button role="menuitem" class="gp-menu-item danger" onclick={() => onAction("delete")}><Trash2 size={13} />{count > 1 ? `Delete ${count} tasks…` : "Delete task…"}</button>
  {:else}
    <button role="menuitem" class="gp-menu-item" onclick={() => { page = "root"; }}><ArrowLeft size={13} />Task actions</button>
    <div class="gp-menu-sep"></div>
    {#if page === "status"}{#each STATUSES as status}<button role="menuitem" class="gp-menu-item" onclick={() => onAction({status})}>{STATUS_LABELS[status]}</button>{/each}
    {:else}{#each PRIORITY_LABELS as label, priority}<button role="menuitem" class="gp-menu-item" onclick={() => onAction({priority})}>{label}</button>{/each}{/if}
  {/if}
</div>
<style>
  .task-context{position:fixed;width:230px;max-width:calc(100vw - 16px);max-height:calc(100dvh - 16px);overflow:auto;font-size:12px;color:rgb(var(--c-text))}.caption{padding:7px 10px;font-size:10px;color:rgb(var(--c-text-muted))}.gp-menu-item{width:100%;display:flex;gap:9px;align-items:center;text-align:left}.gp-menu-item:focus-visible{outline:none;background:rgb(var(--c-accent)/.18)}button:disabled{opacity:.4}.danger{color:#dc6565}
</style>
