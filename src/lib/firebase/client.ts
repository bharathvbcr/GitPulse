/**
 * Typed IPC seam for the Firebase App Hosting commands.
 *
 * Command names are spelled out literally at each call rather than selected
 * into a variable: `check:ipc` verifies every invoked command against the Rust
 * registry statically, and a computed name is a hole in that check. The
 * injected seam is named `invokeFn` for the same reason — that is one of the
 * two callee names the checker recognises.
 */
import { invoke } from "@tauri-apps/api/core";
import type { Guarded } from "../stores/harnessStore";
import type {
  FirebaseBackendsReport,
  FirebaseRolloutsReport,
  FirebaseStatus,
} from "./types";

export type InvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * What the working tree says about this repository's Firebase setup.
 *
 * Free: files plus a bounded CLI probe, no network and no credential. Safe to
 * call on panel mount, unlike everything below it.
 */
export function getFirebaseStatus(
  repoPath: string,
  invokeFn: InvokeFn = invoke,
): Promise<FirebaseStatus> {
  return invokeFn<FirebaseStatus>("cmd_firebase_status", { repoPath });
}

/**
 * Lists App Hosting backends.
 *
 * Returns `Guarded` because this read can mutate: the Firebase CLI enables the
 * App Hosting API on the project when it is off, which is a change to the
 * user's Google Cloud project. It therefore runs only on an explicit action,
 * never on mount.
 */
export function listFirebaseBackends(
  repoPath: string,
  projectId: string,
  invokeFn: InvokeFn = invoke,
): Promise<Guarded<FirebaseBackendsReport>> {
  return invokeFn<Guarded<FirebaseBackendsReport>>("cmd_firebase_backends", {
    repoPath,
    projectId,
  });
}

/** Lists rollouts for one backend, joined to local commits. Gated as above. */
export function listFirebaseRollouts(
  repoPath: string,
  projectId: string,
  backendId: string,
  location: string | null,
  invokeFn: InvokeFn = invoke,
): Promise<Guarded<FirebaseRolloutsReport>> {
  return invokeFn<Guarded<FirebaseRolloutsReport>>("cmd_firebase_rollouts", {
    repoPath,
    projectId,
    backendId,
    location,
  });
}
