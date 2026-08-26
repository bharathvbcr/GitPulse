/**
 * Shared dismiss gate for modals with a long-running operation.
 *
 * Backdrop clicks, Escape, and the Cancel button all funnel through the same
 * `onClose` callback; once the operation starts, that callback is disabled
 * (matching the disabled Cancel button) so a stray click or key cannot tear
 * down a modal whose work is still in flight — cloning a repo, mid-rebase.
 */
export function guardedDismiss(busy: boolean, onClose?: () => void): void {
  if (busy) return;
  onClose?.();
}
