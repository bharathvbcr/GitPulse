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
}

export interface ConfirmPromptOptions extends PromptBaseOptions {
  mode: "confirm";
}

export type PromptOptions = TextPromptOptions | ConfirmPromptOptions;

interface PendingPrompt {
  options: PromptOptions;
  resolve: (value: string | boolean | null) => void;
}

/** The currently shown prompt, if any. Rendered once by PromptModal.svelte. */
export const promptState = writable<PendingPrompt | null>(null);

/**
 * window.prompt / window.confirm return undefined / null under WKWebView,
 * silently no-oping branch create/rename/delete. These helpers promise the
 * answer from an in-app dialog instead.
 */
function takePending(): PendingPrompt | null {
  const current = get(promptState);
  if (current) promptState.set(null);
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

/** Completes the open prompt with the user's answer (a string, boolean, or null). */
export function completePrompt(value: string | boolean | null): void {
  takePending()?.resolve(value);
}

/** Cancels the open prompt (Escape key or backdrop click). */
export function cancelPrompt(): void {
  completePrompt(null);
}
