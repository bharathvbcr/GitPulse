/**
 * Payloads for the events the backend emits about a repository.
 *
 * Events are a second wire surface, separate from command returns, and this
 * one was consumed as an anonymous `{ path?: string }` in App.svelte — which
 * also had it optional, though Rust always sends it.
 */

/** Emitted when the filesystem watcher sees the repository change. */
export interface RepoChangedPayload {
  path: string;
}
