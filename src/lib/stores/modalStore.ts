import { get, writable } from "svelte/store";

export interface PromptBaseOptions {
  title: string;
  /** Body text. Newlines render as line breaks (branch-name lists etc.). */
  message?: string;
  confirmLabel?: string;
  cancelLabel?: string;
}

export interface TextPromptOptions extends PromptBaseOptions {
  mode?: "text";
  placeholder?: string;
  initialValue?: string;
  /**
   * Judges the answer when the person submits. A message keeps the dialog open
   * and shows it under the field; null accepts. Not consulted on cancel.
   */
  validate?: (value: string) => string | null;
  /** Shown under the field when the dialog opens, e.g. a refusal from the host for the name just typed. */
  error?: string;
}

export interface ConfirmPromptOptions extends PromptBaseOptions {
  mode: "confirm";
  /** Danger confirm (delete). The confirm control uses the danger button. */
  destructive?: boolean;
  /**
   * A third answer beside confirm and cancel, shown as a danger button when
   * set (see {@link askChoice}). Plain confirms never show it.
   */
  alternateLabel?: string;
}

export type PromptOptions = TextPromptOptions | ConfirmPromptOptions;

interface PendingPrompt {
  options: PromptOptions;
  resolve: (value: string | boolean | null) => void;
}

/** The currently shown prompt, if any. Rendered once by PromptModal.svelte. */
export const promptState = writable<PendingPrompt | null>(null);

/** Whether a prompt is open right now. */
export function isPromptOpen(): boolean {
  return get(promptState) !== null;
}

/** The open prompt's `validate` refusal, shown under its field; null when none. */
export const promptError = writable<string | null>(null);

/**
 * window.prompt / window.confirm return undefined / null under WKWebView,
 * silently no-oping branch create/rename/delete. These helpers promise the
 * answer from an in-app dialog instead.
 */
function takePending(): PendingPrompt | null {
  const current = get(promptState);
  if (current) promptState.set(null);
  promptError.set(null);
  return current;
}

function begin(options: PromptOptions): Promise<string | boolean | null> {
  // One prompt at a time: a newer request retires the previous one as
  // cancelled so its awaiter never hangs.
  takePending()?.resolve(null);
  return new Promise((resolve) => {
    promptState.set({ options, resolve });
  });
}

/** Resolves with the trimmed-free raw string, or null on cancel/Escape/backdrop. */
export function askText(options: Omit<TextPromptOptions, "mode">): Promise<string | null> {
  return begin({ ...options, mode: "text" }).then((value) =>
    typeof value === "string" ? value : null
  );
}

/** Resolves true only on explicit confirm; cancel/Escape/backdrop resolve false. */
export function askConfirm(options: Omit<ConfirmPromptOptions, "mode">): Promise<boolean> {
  return begin({ ...options, mode: "confirm" }).then((value) => value === true);
}

/** What an alternate press completes a prompt with. */
export const ALTERNATE_ANSWER = "alternate";

/**
 * A confirm with a third way out: `"confirm"`, `"alternate"`, or `null` on
 * cancel/Escape/backdrop. For a question whose two answers both proceed, such
 * as quitting with terminals that can be left running or stopped.
 */
export function askChoice(
  options: Omit<ConfirmPromptOptions, "mode" | "alternateLabel"> & { alternateLabel: string },
): Promise<"confirm" | "alternate" | null> {
  return begin({ ...options, mode: "confirm" }).then((value) =>
    value === true ? "confirm" : value === ALTERNATE_ANSWER ? "alternate" : null
  );
}

/**
 * Completes the open prompt with the user's answer (a string, boolean, or
 * null). A text answer its `validate` refuses does not complete it: the
 * refusal is published on [`promptError`] and the prompt stays open.
 */
export function completePrompt(value: string | boolean | null): void {
  const current = get(promptState);
  if (current && typeof value === "string" && current.options.mode === "text") {
    // A validator that throws is a refusal too, shown in the prompt: letting it
    // escape would leave the prompt open with the tab guard armed and nothing
    // on screen saying why.
    let problem: string | null;
    try {
      problem = current.options.validate?.(value) ?? null;
    } catch (err: unknown) {
      problem = `This answer could not be checked: ${err instanceof Error ? err.message : String(err)}`;
    }
    if (problem !== null) {
      promptError.set(problem);
      return;
    }
  }
  takePending()?.resolve(value);
}

/** Cancels the open prompt (Escape key or backdrop click). */
export function cancelPrompt(): void {
  completePrompt(null);
}
