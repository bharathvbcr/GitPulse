<script lang="ts">
  /**
   * The task sheet's due date: one trigger, one popover, one grammar.
   *
   * This replaces `<input type="datetime-local">`, which had three faults the
   * sheet could not paint around. It drew a system box with a system calendar
   * inside a soft-bubble panel; once set it could not be cleared, so tasks
   * accumulated deadlines nobody meant to set; and it had no idea what
   * "Friday" means, although `due:friday` in quick add always has.
   *
   * What is kept is the part the platform does better: the clock is a real
   * `<input type="time">`, for the same reason `gp-select` keeps a native
   * `<select>` (`app.css`) — locale, keyboard handling and the mobile widget
   * come free, and only the painting is ours. Only the month grid and the
   * word box are hand-drawn.
   *
   * Every date it produces comes from `taskDue`, which delegates the grammar
   * to `parseQuickAddDue`: a shortcut chip, a typed phrase and `due:friday`
   * typed on the board all land on the same second.
   *
   * Portaled and positioned the way the other nine anchored popovers in this
   * app are (`shouldDismissOverlay` + `clampMenuPosition`), because
   * `.sheet-body` is `overflow:auto` and would clip a popover positioned
   * inside it. That dance has no shared owner yet; this is the tenth copy and
   * should be the last before it gets one.
   */
  import { onMount } from "svelte";
  import { CalendarDays, ChevronDown, ChevronLeft, ChevronRight, Clock, WandSparkles } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { clampMenuPosition } from "../branches/menuPosition";
  import {
    composeDue,
    DUE_SHORTCUTS,
    dueLabel,
    duePartsOf,
    monthGrid,
    monthLabel,
    readDuePhrase,
    relativeDue,
    shiftView,
    viewFor,
  } from "../workbench/taskDue";
  import { parseQuickAddDue } from "../workbench/taskQuickAdd";

  let {
    value = null,
    disabled = false,
    /** Injected so a test can pin the calendar; production reads the clock. */
    now = () => Date.now(),
    onChange,
  }: {
    value?: number | null;
    disabled?: boolean;
    now?: () => number;
    onChange: (next: number | null) => void;
  } = $props();

  const WEEKDAY_INITIALS = ["S", "M", "T", "W", "T", "F", "S"];

  let open = $state(false);
  let triggerEl: HTMLButtonElement | undefined = $state();
  let popupEl: HTMLDivElement | undefined = $state();
  let pos = $state({ left: 0, top: 0 });
  let phrase = $state("");
  /**
   * The month on screen.
   *
   * `null` means "follow the value" — the state only becomes a number once the
   * reader pages away from it, so opening the picker on a task due in December
   * shows December rather than whatever month was last browsed.
   */
  let browsed = $state<{ year: number; month: number } | null>(null);

  const parts = $derived(duePartsOf(value));
  const view = $derived(browsed ?? viewFor(value, now()));
  const cells = $derived(monthGrid(view, parts?.day ?? null, now()));
  const label = $derived(dueLabel(value));
  const relative = $derived(relativeDue(value, Math.floor(now() / 1000)));
  const time = $derived(parts?.time ?? "");
  const reading = $derived(readDuePhrase(phrase, value, now()));
  const readingLabel = $derived(reading === null ? "" : dueLabel(reading));

  function close(options?: { restoreFocus?: boolean }) {
    const opener = triggerEl;
    open = false;
    phrase = "";
    browsed = null;
    if (options?.restoreFocus && opener?.isConnected) window.setTimeout(() => opener.focus(), 0);
  }

  function fit(estimate?: { width: number; height: number }) {
    if (!triggerEl) return;
    const rect = triggerEl.getBoundingClientRect();
    const width = popupEl?.offsetWidth || estimate?.width || 286;
    const height = popupEl?.offsetHeight || estimate?.height || 360;
    pos = clampMenuPosition(rect.left, rect.bottom + 6, width, height, window.innerWidth, window.innerHeight);
  }

  function toggleOpen() {
    if (open) { close(); return; }
    fit({ width: 286, height: 360 });
    open = true;
  }

  /** Write a new deadline, keeping the clock the reader already chose. */
  function pickDay(cell: { year: number; month: number; day: number }) {
    const next = composeDue({ year: cell.year, month: cell.month, day: cell.day }, time);
    if (next === null) return;
    browsed = { year: cell.year, month: cell.month };
    onChange(next);
  }

  /** Write a new clock, keeping the day. With no day yet, today takes it. */
  function pickTime(next: string) {
    const day = parts?.day ?? duePartsOf(Math.floor(now() / 1000))?.day;
    if (!day) return;
    const composed = composeDue(day, next);
    if (composed !== null) onChange(composed);
  }

  function pickWord(word: string) {
    const next = parseQuickAddDue(word, now());
    if (next === null) return;
    browsed = null;
    onChange(next);
  }

  function commitPhrase() {
    if (reading === null) return;
    browsed = null;
    onChange(reading);
    phrase = "";
  }

  function clear() {
    onChange(null);
    close({ restoreFocus: true });
  }

  $effect(() => { if (open && popupEl) fit(); });
  $effect(() => { if (disabled && open) close(); });

  onMount(() => {
    const onPointer = (event: PointerEvent) => {
      if (!open) return;
      if (shouldDismissOverlay(event.target, "[data-task-due-picker], [data-task-due-popup]")) close();
    };
    const onKey = (event: KeyboardEvent) => {
      if (!open) return;
      if (event.key === "Escape") {
        // Capture phase, and stopped here: one Escape closes the popover, not
        // the popover and the task sheet behind it.
        event.preventDefault();
        event.stopPropagation();
        close({ restoreFocus: true });
      }
    };
    const onScroll = (event: Event) => {
      if (!open) return;
      if (event.target instanceof Node && popupEl?.contains(event.target)) return;
      close();
    };
    const onResize = () => close();
    window.addEventListener("pointerdown", onPointer, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
    };
  });
</script>

<div class="due" data-task-due-picker>
  <button
    bind:this={triggerEl}
    type="button"
    class="due-trigger"
    class:set={value !== null}
    aria-haspopup="dialog"
    aria-expanded={open}
    {disabled}
    data-testid="task-due-trigger"
    onclick={toggleOpen}
  >
    <CalendarDays size={13} class="shrink-0 text-textMuted" aria-hidden="true" />
    <span class="due-label">{label}</span>
    {#if relative.text}
      <span class="gp-pill due-relative" class:overdue={relative.state === "overdue"} data-testid="task-due-relative">{relative.text}</span>
    {/if}
    <ChevronDown size={11} class="shrink-0 text-textMuted" aria-hidden="true" />
  </button>
</div>

{#if open}
  <div
    bind:this={popupEl}
    use:portal={"body"}
    data-task-due-popup
    data-testid="task-due-popup"
    class="gp-menu due-popup"
    style="left: {pos.left}px; top: {pos.top}px; z-index: {LAYERS.MENU}"
    role="dialog"
    aria-label="Pick a due date and time"
  >
    <div class="phrase-row">
      <WandSparkles size={12} class="shrink-0 text-textMuted" aria-hidden="true" />
      <input
        class="phrase"
        type="text"
        bind:value={phrase}
        maxlength="32"
        aria-label="Type a date in words"
        placeholder="friday 5pm"
        data-testid="task-due-phrase"
        onkeydown={(event) => {
          if (event.key !== "Enter") return;
          event.preventDefault();
          commitPhrase();
        }}
      />
      {#if readingLabel}
        <button type="button" class="reading" data-testid="task-due-reading" onclick={commitPhrase}>{readingLabel}</button>
      {/if}
    </div>

    <div class="cal-head">
      <button type="button" class="gp-icon-btn" aria-label="Previous month" onclick={() => { browsed = shiftView(view, -1); }}>
        <ChevronLeft size={13} aria-hidden="true" />
      </button>
      <span class="cal-title" data-testid="task-due-month">{monthLabel(view)}</span>
      <button type="button" class="gp-icon-btn" aria-label="Next month" onclick={() => { browsed = shiftView(view, 1); }}>
        <ChevronRight size={13} aria-hidden="true" />
      </button>
    </div>

    <div class="cal-grid">
      {#each WEEKDAY_INITIALS as initial, index (index)}<span class="dow" aria-hidden="true">{initial}</span>{/each}
      {#each cells as cell (cell.key)}
        <button
          type="button"
          class="day"
          data-outside={!cell.inMonth}
          data-today={cell.isToday}
          data-selected={cell.isSelected}
          aria-pressed={cell.isSelected}
          onclick={() => pickDay(cell)}
        >{cell.label}</button>
      {/each}
    </div>

    <div class="cal-time">
      <Clock size={13} class="shrink-0 text-textMuted" aria-hidden="true" />
      <input
        class="time"
        type="time"
        value={time}
        aria-label="Time"
        data-testid="task-due-time"
        oninput={(event) => pickTime(event.currentTarget.value)}
      />
      <button type="button" class="gp-btn" disabled={value === null} data-testid="task-due-clear" onclick={clear}>Clear</button>
    </div>

    <div class="cal-quick">
      {#each DUE_SHORTCUTS as shortcut (shortcut.id)}
        <button type="button" class="mini" onclick={() => pickWord(shortcut.word)}>{shortcut.label}</button>
      {/each}
    </div>
  </div>
{/if}

<style>
  .due{position:relative;min-width:0}
  .due-trigger{display:flex;align-items:center;gap:7px;width:100%;text-align:left;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:rgb(var(--c-text-muted));font:inherit;font-size:12px;cursor:pointer;min-width:0}
  .due-trigger:hover:not(:disabled){border-color:rgb(var(--c-accent) / 0.5)}
  .due-trigger:disabled{opacity:.5;cursor:not-allowed}
  /* Muted until there is a deadline, so "No due date" reads as the absence it
     is rather than as a value someone chose. */
  .due-trigger.set{color:rgb(var(--c-text))}
  .due-label{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .due-relative{flex-shrink:0}
  .due-relative.overdue{color:rgb(var(--c-status-rose, 244 63 94));border-color:rgb(225 29 72 / 0.5);background:rgb(225 29 72 / 0.14)}
  :global(.dark) .due-relative.overdue{color:rgb(253 164 175)}

  .due-popup{position:fixed;width:min(286px,92vw);padding:10px;font-size:12px;color:rgb(var(--c-text))}
  .phrase-row{display:flex;align-items:center;gap:6px;border:1px solid rgb(var(--c-border));border-radius:8px;background:rgb(var(--c-bg) / 0.6);padding:5px 8px;margin-bottom:9px}
  .phrase{flex:1;min-width:0;border:0;background:none;color:inherit;font:inherit;font-size:12px;outline:none}
  .phrase::placeholder{color:rgb(var(--c-text-muted) / 0.7)}
  /* The reading is a button, not a label: seeing what was understood and
     accepting it are the same gesture. */
  .reading{flex-shrink:0;border:0;background:none;padding:0;font:inherit;font-size:10px;color:rgb(var(--c-accent));cursor:pointer;white-space:nowrap}
  .reading:hover{text-decoration:underline}

  .cal-head{display:flex;align-items:center;justify-content:space-between;gap:6px;margin-bottom:8px}
  .cal-title{font-size:12px;font-weight:600}
  .cal-grid{display:grid;grid-template-columns:repeat(7,minmax(0,1fr));gap:2px}
  .dow{text-align:center;font-size:9px;color:rgb(var(--c-text-muted));padding-bottom:3px;letter-spacing:.04em}
  .day{border:1px solid transparent;background:none;border-radius:8px;padding:6px 0;font:inherit;font-size:11px;color:rgb(var(--c-text));cursor:pointer}
  /* Slash-alpha, not a flat token: an alpha-less fill is a slab that paints
     over the window material rather than reading as a recess in it. */
  .day:hover{background:rgb(var(--c-surface-hover) / 0.7)}
  .day[data-outside="true"]{color:rgb(var(--c-text-muted) / 0.55)}
  /* Outline is "you are here", fill is "this is chosen". Drawing today filled
     as well made the old grid ambiguous on the day a task was due. */
  .day[data-today="true"]{border-color:rgb(var(--c-border));font-weight:650}
  .day[data-selected="true"]{background:rgb(var(--c-accent));color:rgb(var(--c-bg));font-weight:700;border-color:rgb(var(--c-accent))}

  .cal-time{display:flex;align-items:center;gap:6px;border-top:1px solid rgb(var(--c-border) / 0.45);margin-top:8px;padding-top:8px}
  .time{flex:1;min-width:0;padding:5px 8px;border:1px solid rgb(var(--c-border));border-radius:8px;background:rgb(var(--c-bg) / 0.6);color:inherit;font:inherit;font-size:12px}
  .cal-quick{display:flex;flex-wrap:wrap;gap:4px;margin-top:8px}
  .mini{border-radius:999px;border:1px solid rgb(var(--c-border) / 0.7);background:rgb(var(--c-surface-hover) / 0.55);padding:3px 9px;font:inherit;font-size:10px;color:rgb(var(--c-text-muted));cursor:pointer}
  .mini:hover{color:rgb(var(--c-text));border-color:rgb(var(--c-accent) / 0.45)}
</style>
