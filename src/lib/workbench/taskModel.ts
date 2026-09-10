/**
 * Effective local-model selection for task enhancements.
 *
 * The sheet never owns a second model picker: GitPulse's "Local model servers"
 * card pins `preferred`, discovery fills `ai.selected`, and host methods carry
 * that pair so the sidecar can spawn with matching MANVI_* env.
 */

export interface ModelSelection {
  base_url: string;
  model: string;
}

export interface SelectionState {
  preferred: ModelSelection | null;
  ai: { selected: ModelSelection | null } | null;
}

export interface EnhancementFailureAdvice {
  guidance: string;
  action: "retry" | "change_model" | "restart" | "wait" | null;
}

const LOOPBACK = /^(https?:\/\/)?(127\.0\.0\.1|localhost)(:\d+)?(\/.*)?$/i;

/** preferred wins; otherwise the auto-chosen scan result. */
export function effectiveSelection(state: SelectionState): ModelSelection | null {
  return normalizeSelection(state.preferred) ?? normalizeSelection(state.ai?.selected ?? null);
}

/** Refuse empty, oversized, control-laden, or non-loopback selections. */
export function normalizeSelection(selection: ModelSelection | null | undefined): ModelSelection | null {
  if (!selection) return null;
  const base_url = typeof selection.base_url === "string" ? selection.base_url.trim() : "";
  const model = typeof selection.model === "string" ? selection.model.trim() : "";
  if (!base_url || !model) return null;
  if (base_url.length > 512 || model.length > 128) return null;
  if (/[\u0000-\u001f\u007f]/.test(base_url + model)) return null;
  if (!LOOPBACK.test(base_url)) return null;
  return { base_url, model };
}

/** Wire shape for enhancements.generate|configuration|wake|worker. */
export function selectionWire(selection: ModelSelection | null | undefined): { model?: ModelSelection } {
  const normalized = normalizeSelection(selection);
  return normalized ? { model: normalized } : {};
}

/** "qwen3.8:27b-mlx on 127.0.0.1:11434" */
export function describeSelection(selection: ModelSelection): string {
  const normalized = normalizeSelection(selection);
  if (!normalized) return "";
  let host = normalized.base_url.replace(/^https?:\/\//i, "");
  host = host.replace(/\/v1\/?$/i, "").replace(/\/$/, "");
  return `${normalized.model} on ${host}`;
}

/**
 * Map decoder / adapter / transport failure text to one-line guidance.
 * Unknown text is returned as-is with no suggested action.
 */
export function explainEnhancementFailure(text: string): EnhancementFailureAdvice {
  const raw = (text ?? "").trim();
  const lower = raw.toLowerCase();
  if (!raw) return { guidance: "Enhancement failed.", action: "retry" };
  if (lower.includes("not served") || lower.includes("errnotserved") || lower.includes("capability is unavailable")) {
    return {
      guidance: "That model is not served on the selected local server. Pick another model.",
      action: "change_model",
    };
  }
  if (lower.includes("stopmaxtokens") || lower.includes("max tokens") || lower.includes("token limit") || /\bstop[_\s-]?reason\b.*\blength\b/.test(lower)) {
    return {
      guidance: "The model hit its output limit before finishing. Shorten the notes or try again.",
      action: "retry",
    };
  }
  if (lower.includes("cancelled") || lower.includes("canceled")) {
    return {
      guidance: "Cancelled: the model selection changed.",
      action: "restart",
    };
  }
  if (lower.includes("expired")) {
    return {
      guidance: "Suggestion request expired; start again.",
      action: "restart",
    };
  }
  if (lower.includes("busy")) {
    return {
      guidance: "Manvi is busy with another suggestion. Wait, then retry.",
      action: "wait",
    };
  }
  if (lower.includes("timeout") || lower.includes("unavailable") || lower.includes("still starting")) {
    return {
      guidance: "Manvi is still starting; retry.",
      action: "retry",
    };
  }
  return { guidance: raw, action: null };
}
