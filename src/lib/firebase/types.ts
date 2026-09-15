/**
 * Wire types for Firebase App Hosting payloads returned by the Tauri commands.
 * These are the serde field names as serialized by src-tauri; do not rename
 * fields.
 *
 * They live here rather than in the panel because `wire-type-locality-contract`
 * forbids snake_case payload shapes inside `.svelte` files, where `check:types`
 * cannot reach them.
 */

/**
 * App Hosting's rollout lifecycle.
 *
 * Internally tagged, so every variant is an object with a `kind` — including
 * `unrecognised`, which carries the state string this build does not know.
 * That variant exists so an unknown state renders as unknown: treating
 * everything that is not a known failure as a success would give the first
 * state Firebase invents a green badge.
 */
export type RolloutState =
  | {
      kind:
        | "unspecified"
        | "queued"
        | "pending_build"
        | "progressing"
        | "paused"
        | "succeeded"
        | "failed"
        | "cancelled"
        | "skipped";
    }
  | { kind: "unrecognised"; raw: string };

/** The commit a rollout's build came from — the join key to local history. */
export interface RolloutCommit {
  /** Full SHA-1, the same key `git cat-file` speaks. */
  hash: string;
  branch: string | null;
  message: string | null;
  author: string | null;
  commit_time: string | null;
  /**
   * Whether this SHA resolves in the opened checkout.
   *
   * False is a real answer, not a failure: a force-push, a fork or a shallow
   * clone all legitimately deploy commits this working copy does not hold.
   */
  present_locally: boolean;
}

export interface RolloutInfo {
  id: string;
  state: RolloutState;
  create_time: string | null;
  update_time: string | null;
  error: string | null;
  commit: RolloutCommit | null;
}

export interface BackendInfo {
  id: string;
  location: string | null;
  uri: string | null;
}

/** One `.firebaserc` alias and the project id it names. */
export interface FirebaseProjectAlias {
  alias: string;
  project_id: string;
}

/** Whether the `firebase` CLI answered, and why not when it did not. */
export interface FirebaseCliProbe {
  present: boolean;
  program: string;
  version: string | null;
  reason: string | null;
}

/**
 * Whether one subcommand exists in the installed CLI.
 *
 * `checked` is separate from `available` for the usual reason: an uninstalled
 * CLI cannot be asked what it supports, and "we could not look" must not render
 * as "your CLI does not have it" — the first sends a reader to install
 * Firebase, the second to enable an experiment they do not need.
 */
export interface FirebaseCapability {
  available: boolean;
  checked: boolean;
  reason: string | null;
}

/** Wire shape of `cmd_firebase_status` — computed from files, never the network. */
export interface FirebaseStatus {
  configured: boolean;
  projects: FirebaseProjectAlias[];
  /**
   * The `default` alias when `.firebaserc` names one.
   *
   * Named so the UI can mark it — never auto-selected. `default` is very often
   * production, and a rollout aimed at it by default is one the user never
   * chose.
   */
  default_alias: string | null;
  /**
   * True when more aliases exist than the picker cap allowed through.
   *
   * `.firebaserc` is repository content, so the row count is not ours to
   * assume. A capped list that renders like a complete one is exactly the
   * substitution every other field here exists to prevent.
   */
  projects_truncated: boolean;
  has_apphosting_config: boolean;
  cli: FirebaseCliProbe;
  /**
   * Whether the installed CLI exposes `apphosting:rollouts:list`.
   *
   * Asked, never assumed. Upstream registers that subcommand only behind the
   * `internaltesting` experiment, which is off by default — so on a stock
   * install it does not exist, and an unregistered subcommand exits non-zero
   * having printed nothing, which reaches a reader as a parse error rather
   * than as the missing feature it is.
   */
  rollout_listing: FirebaseCapability;
  firebaserc_error: string | null;
  firebasejson_error: string | null;
}

/**
 * Fields every Firebase listing carries so a partial answer cannot render as a
 * complete one.
 *
 * `truncated` and `walk_incomplete` are not synonyms: `truncated` is our own
 * display cap, `walk_incomplete` is the producer stopping early — for App
 * Hosting, regions it could not reach. A UI that collapses them cannot tell
 * "we showed 50 of 90" from "a whole region is missing".
 */
interface FirebaseListing {
  available: boolean;
  /**
   * Whether the listing actually ran.
   *
   * Separate from the rows for the reason `UpdateCheck.checked` is separate: a
   * missing CLI, an unauthenticated user or a disabled API must never render
   * as a backend that has never deployed.
   */
  checked: boolean;
  cli_present: boolean;
  project_id: string;
  truncated: boolean;
  unreachable: string[];
  walk_incomplete: string | null;
  error: string | null;
}

/** Wire shape of `cmd_firebase_rollouts`. */
export interface FirebaseRolloutsReport extends FirebaseListing {
  backend_id: string;
  rollouts: RolloutInfo[];
}

/** Wire shape of `cmd_firebase_backends`. */
export interface FirebaseBackendsReport extends FirebaseListing {
  backends: BackendInfo[];
}

/**
 * Wire shape of `cmd_firebase_create_rollout` — what the attempt actually did.
 *
 * `created` is the CLI's exit status and nothing else. Creating a rollout is
 * not idempotent: upstream allocates the next rollout id per call, so a run
 * that succeeded and is reported as failed costs a second deployment when the
 * user retries. `unconfirmed` carries the case where the CLI exited zero
 * without confirming it — the rollout started, and saying so is more useful
 * than picking one of the two clean answers we do not have.
 */
export interface RolloutCreateOutcome {
  project_id: string;
  backend_id: string;
  git_commit: string;
  created: boolean;
  unconfirmed: string | null;
}
