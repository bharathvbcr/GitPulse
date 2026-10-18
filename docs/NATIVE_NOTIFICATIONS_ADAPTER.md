# Native activity notifications

> **Evidence names.** Log and evidence file names quoted in this document (`*.log`,
> `*.json`) name local run artifacts that were never committed and are not
> retained; the figures quoted beside them are the record. They are not paths a
> reader can open. `scripts/documented-counts-contract.test.ts` rejects any new
> `/tmp/...` evidence path.

Manvi schema seven now owns notification settings and delivery records alongside
the durable activity inbox introduced in schema six. GitPulse implements the
macOS adapter, preference controls and recovered review view. Source compilation,
store/native boundary tests and browser interaction are verified below. Actual
OS banner display and installed-app activation remain unverified.

Schema eight adds structured permission/question sources. Their previews stay
generic, and native eligibility shares the decision validity check: an expired
request or changed run, task or repository cannot produce a current banner.
Opening or acknowledging its notice never grants permission. The inspector saves
human decisions separately from host delivery and provider confirmation. Every
managed callback type now has a producer; see
[Event producers](#event-producers).

## Approved macOS bindings

The user approved adding the four bindings. GitPulse now declares these versions
already present in Cargo.lock as direct, macOS-only dependencies: `objc2` 0.6.4,
`block2` 0.6.2, `objc2-foundation` 0.3.2 and `objc2-user-notifications` 0.3.2.
All four declarations disable default features. Selected APIs cover the
notification center, content, request, response, settings, sound, action/category
and supporting Foundation collections. `UNNotificationTrigger` is required by
the request constructor even when immediate delivery uses a null trigger.

The resolved macOS feature comparison adds the UserNotifications APIs and
Foundation's `NSCalendar` feature. Existing transitive dependencies retain their
previous feature selections; disabling defaults on these direct declarations
does not turn off defaults enabled elsewhere. CoreLocation is not introduced.
Cargo.lock adds the four direct edges plus the existing `bitflags` and `block2`
edges used by UserNotifications, with no package additions, version changes,
source changes or checksum changes.

Verified on the macOS host: the resolved-feature and lockfile comparison,
`cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` and
`cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked --offline -- -D warnings`
passed. The Clippy log is `gitpulse-native-bindings-clippy.log`; the feature
comparison is `gitpulse-native-bindings-feature-diff.json`. That declaration-only
checkpoint did not repeat the full test run; the adapter verification below does.
No installed
notification delivery or Windows/Linux runtime validation was performed.

These bindings expose Apple's UNUserNotificationCenter APIs in the app process,
including authorization, notification submission and delegate activation. They
avoid an additional helper process, server or JavaScript notification dependency.
The current crate API exposes the required methods, but installed-app delivery
and restart behavior still require physical validation.
[Rust binding documentation](https://docs.rs/objc2-user-notifications/latest/objc2_user_notifications/struct.UNUserNotificationCenter.html).

Tauri's notification plugin supports desktop banners, but its documented Actions
API is mobile-only. Adding that plugin alone would leave the required desktop
review activation incomplete. Use the platform adapter behind one coordinator;
choose and qualify Windows and Linux bindings separately, reporting unsupported
actions instead of presenting inactive controls.
[Tauri notification documentation](https://v2.tauri.app/plugin/notification/#actions).

## Implemented integration

- The profile starts disabled. Enabling records the current event watermark;
  historical activity does not become a banner flood. Eligibility is checked
  again in the same transaction as a unique claim: unread and not dismissed,
  snooze expired, current task/target, created within one hour, outside local
  quiet hours and outside muted workspaces, repositories and tasks. Shared
  repository membership applies workspace mutes without duplicating notices.
  An expression index bounds reads by the recent activity window.
  Saved mutes may retain known deleted identities; deletion cannot block other
  preference saves. Clear saved scope mutes changes the draft until Save, and
  retains sound, quiet hours and the other preferences.
- One worker reconciles enabled profiles every five seconds in batches of at
  most three. Disabled profiles have no periodic wake. Hidden/minimized delivery
  requires the saved background setting. This worker starts no model, repository
  watcher, coding agent or helper process. Preference forms load when expanded;
  the inbox has a bounded scroll area that leaves the board usable.
- The store commits `uncertain` before the OS call. A consumed claim cannot be
  replayed as submission authority. Definite callbacks record `submitted` or
  `failed`; timeout, crash or failed receipt persistence retain uncertainty and
  never trigger automatic resubmission. Submitted means OS acceptance, not
  proof that Focus/system policy displayed a banner.
- The macOS delegate lives for the app lifetime and supports foreground banners
  and an Open task action. Only generic activity summaries enter banner content.
  Notification identities include a persisted profile identity and source event.
  The native request boundary refuses renderer-authored claim, finish and
  activation writes; they belong to the coordinator.
- Native activation callbacks use a bounded queue of 32. The worker validates
  the profile and delivery identity and saves the activation before waking the
  UI. Pending saved activations survive reload/restart. The review dialog loads
  current task data, explains changed/deleted/unavailable targets, uses the
  existing focus trap and acknowledges closing independently of task acceptance.
  Overflow or storage failure is reported. A process crash before a queued
  callback is persisted can still lose that activation; the activity inbox
  remains the recovery surface. This is not an exactly-once OS callback claim.
- An activity banner carries **Snooze 1 hour** beside Open task. It is a
  background action: it does not raise GitPulse. The coordinator checks the
  banner identity against the saved delivery, as Open does, then writes the
  same `attention.update` snooze the inbox's own button sends. The duration is
  `SNOOZE_SECONDS` in `workbench/notifications.rs`.
  `scripts/notification-snooze-contract.test.ts` holds it equal to the
  renderer's `attentionWrite`. The claim stays consumed. When the snooze ends,
  the notice is back in the inbox but **gets no second banner**: eligibility
  excludes any notice with a delivery record and any notice older than an
  hour. That is the one-banner invariant holding, not Snooze failing. A
  re-banner would need an upstream dc-store change to eligibility. Session
  banners have no Snooze, because nothing durable stands behind them.
- Delivered banners are **withdrawn** when their notice is resolved somewhere
  else. Each pass asks Notification Center for the activity banners it still
  shows, at most 64. It keeps only identities whose saved delivery is this
  profile's, then removes those whose notice is read, dismissed, snoozed or no
  longer `current`. The `current` check uses the store's `target_status`,
  the same one eligibility uses. Tests cover an answered decision and an
  edited or deleted task. A decision that expires or that the provider
  resolves should be covered too, because `fresh_notice` and
  `decisions.resolve` change what `target_status` reads, but that is inferred
  from the SQL and not tested. The pass runs before the background/visibility
  gate, because it posts nothing. A failed pass is reported but does not hold
  back delivery. On an enabled profile, every five-second pass now makes one
  `getDeliveredNotifications` call (5 s bounded) and up to two store reads per
  delivered banner (64 banners, 128 reads at most). The idle CPU/memory
  measurement still owed must include this work. `attention.update` and `decisions.decide` wake the coordinator so
  in-app actions withdraw at once. Go-host writes are picked up by the
  five-second reconcile. A disabled profile has no timer, so it withdraws
  only on a wake.
- Swiping a banner away does not mark its notice read. No category sets
  `CustomDismissAction`, so macOS never reports the dismissal. The inbox row
  stays unread on purpose: a swipe clears the screen and is not an answer.
- Authorization is requested only by the explicit enable/save interaction. OS
  calls have bounded callback waits. Windows/Linux report unsupported native
  delivery; no additional platform bindings were added.

## Forwarding to Graft (phone)

Graft.app, the Mac side of the user's phone pairing, listens on a local Unix
socket (Graft `PROTOCOL.md` §10.13, "Local submission"). GitPulse offers it the
banners it has already shown, so they also reach the paired phone. The code is
`workbench/notifications/graft.rs`; it uses `std::os::unix::net` and adds no
dependency.

- **What is forwarded.** The same heading and body as the banner, after the OS
  accepted it: an activity banner once its saved delivery reads `submitted`,
  and a session banner once the OS accepts it (a session has no durable record).
  Nothing is forwarded for `failed` or `uncertain`, so the phone never shows a
  banner the Mac did not. Withdrawal sends `remove` for each banner the
  withdrawal pass takes down; session banners have no withdrawal, and Graft
  drops held items after 24 hours (its contract, not something GitPulse
  checked). The banner's sound setting becomes the
  request's `silent` flag.
- **Key.** The banner's own identity (`gitpulse.<profile>.event-N`, or
  `gitpulse.session.<session>`), which is already inside Graft's key grammar.
  The same banner always yields the same key and a `remove` finds what its
  `post` made. An identity outside the grammar is not forwarded; Graft rejects
  a bad key and never truncates it, so it is not guessed at.
- **Caps.** The heading, body and application name are cut to Graft's byte
  caps (256, 2048 and 128) on a code-point boundary. A body that JSON escaping
  inflates past the 8192-byte request limit is shortened, not dropped.
- **Socket.** `GRAFT_NOTIFY_SOCKET`, else
  `~/Library/Application Support/Graft/notify.sock`. One request per connection;
  connect, write and read share a hard one-second budget.
- **Off the delivery path.** The coordinator and the session notifier only
  enqueue to a bounded queue (16) that drops when full; one supervised thread
  does the socket work. A hung or absent Graft can therefore cost at most a
  dropped forward. It changes no delivery record, never moves the
  claim/uncertain/submitted state machine, never delays a banner or the
  five-second reconcile, and nothing is retried. A missing socket (Graft not
  running) is a quiet no-op.
- **Refusals.** `disabled`, `source-not-allowed`, `rate-limited`, `bad-request`
  and `too-large`, and a timeout or I/O error, are logged once per code
  (`graft-notify`), with the code and never a title, text or key.
- **The gate is Graft's.** GitPulse has no switch for this and adds none: the
  activity preferences live in the Manvi store (a schema change there cascades
  to DevCouncil, Manvi and Jarvis), and the session settings in `tools.json`
  would cover only half the banners. Graft's own master switch (off by default)
  and per-program allow list are the user's control; GitPulse is offered to the
  user in Graft's settings the first time it is refused `source-not-allowed`.
  The phone only ever receives banners that GitPulse's own settings (enabled,
  quiet hours, mutes, background) let through.

Verified (Rust tests against a fake Unix-socket server in a temp directory, via
`GRAFT_NOTIFY_SOCKET`): request shape, caps and the oversized-field case; the
key's stability and charset; every refusal code and a garbled answer; a missing
socket and a stale socket file; a server that never answers and one that drips
bytes (both bounded by the one-second budget and not retried); a full queue
dropping while the caller is not delayed; the delivery record being unchanged
with a listening Graft and with none. **Unverified:** a real Graft.app (the
Graft side was not running), delivery to the phone, and the installed GitPulse
bundle posting through the socket.

## Verification and remaining qualification

Verified: eight canonical store tests cover watermark/disabled behavior,
eligibility changes, quiet-hour boundaries, scoped mutes, migration rollback,
atomic claim/receipt failure, activation identity and preference changes after
muted task/workspace deletion. The last regression failed before the fix with
`invalid_input: mute scope does not exist`. A real-binary Go restart
test preserves an uncertain claim and refuses another delivery. Two native Rust
tests exercise identity refusal and a lost OS callback without a second send.
Twenty frontend tests cover strict metadata, authorization intent and recovery
after lost settings/acknowledgment replies.

Nineteen browser checks use a disposable real Manvi profile and explicitly report OS
delivery as unavailable. They verify sound/quiet-hour persistence, workspace,
repository and task mutes, current and stale activation recovery, unchanged
task/proposal state on closing, clearing saved mutes and rendered layout. Activation records are
seeded through the real store; no OS submission is implied. Evidence:
`gitpulse-native-notifications-browser-evidence.json`.

Snooze and withdrawal (2026-10-07) add three native tests against the real
store. `the_banner_snooze_is_the_inbox_snooze_and_never_opens_or_rebanners`
checks that a banner's Snooze sets the inbox row's `snoozed_until` to
now + `SNOOZE_SECONDS`. It also checks that Snooze records no activation,
leaves nothing pending and makes the banner stale, and that a foreign-profile
or unclaimed identity is refused.
`withdrawal_takes_down_exactly_the_banners_resolved_elsewhere` uses read,
dismissed, snoozed, task-edited, task-deleted, current, foreign-profile and
session banners. It removes exactly the first five, never calls the OS when
nothing is stale, and stops treating an ended snooze as stale.
`every_managed_callback_kind_produces_a_bannerable_notice` is described below; it also answers the permission request with `decisions.decide` and checks that the delivered banner then reads as stale.
Removing the `target_status` arm or the snooze arm from `resolved` fails the
withdrawal tests. Changing the renderer's snooze to 1800 seconds fails the
contract test. The macOS calls themselves (`getDeliveredNotifications`,
`removeDeliveredNotificationsWithIdentifiers`, the Snooze action) compile and
pass Clippy. **Nobody has watched them run in an installed bundle.**

The full verification checkpoint is recorded in
[the implementation plan](AGENTIC_WORKSPACES_PLAN.md). Remaining work is
installed-bundle qualification and the other platforms. Installed-bundle
qualification covers bundle identity and permission/Focus tests, physical
banner/action delivery (now including Snooze and withdrawal), click-after-quit
behavior, callback persistence before OS acknowledgment, burst/overload
qualification and measured native idle CPU/memory. For the other platforms,
see [Windows and Linux](#windows-and-linux). It also includes the producers
listed as blocked below.

## Event producers

A notice exists only when a store mutation records it. `attention::record_event`
in dc-store is the one producer, and `work_attention.source_sequence` must
reference a `work_events` row. A timer, a renderer poll or a GitHub fetch
cannot raise a banner until dc-store records an event for it. That is an
upstream change: DevCouncil first, then a re-vendor.

### Managed callbacks: every type connected

| Provider callback | Adapter (Manvi `codingagent/`) | Store write | Notice |
| --- | --- | --- | --- |
| Codex `item/commandExecution/requestApproval` | `codex.go`, kind `permission` | `decisions.create` (`serve/managed.go`) | `decision_permission` |
| Codex `item/fileChange/requestApproval` | `codex.go`, kind `permission` | `decisions.create` | `decision_permission` |
| Codex `item/tool/requestUserInput` | `codex.go`, kind `question` | `decisions.create` | `decision_question` |
| Claude `can_use_tool` | `claude.go`, kind `permission` | `decisions.create` | `decision_permission` |
| Claude `request_user_dialog`, `elicitation`, other subtypes; Codex other methods | Refused: no response is granted and the run ends | `runs.finish` | `run_exit_failed` / `run_failed` |
| Provider resolves or cancels a callback | `decisions.resolve` | none; the notice's target changes | withdrawn as not `current` |
| Run ends: exited, failed, unresolved | `runs.finish` | `runs.finish` | `run_exited`, `run_exit_failed`, `run_failed`, `run_unresolved` |

The first four rows are verified. `decisions.create` accepts exactly
`permission` and `question`, and
`every_managed_callback_kind_produces_a_bannerable_notice` drives both through
a live run and finds each in `notifications.pending.list` with its own title.
The refused row is traced in code and adapter-tested, but it has not been
driven through `serve`. That remains in [QUALIFICATION.md](QUALIFICATION.md)
under Agent supervision.

### Other producers

| Producer | State | What unblocks it |
| --- | --- | --- |
| Background recovery | **Connected**: `runs.reconcile` → `run_unresolved`, `enhancements.recover` → `enhancement_interrupted` | — |
| Agent approval, managed lane | **Connected**: the table above | — |
| Agent approval, terminal lane | **Blocked, and not by ft-723b56f2** (done). Session banners only (`src-tauri/src/alerts/`). A hook report (Notification, permission prompt) arrives with no provider thread, turn or protocol request id and no channel to answer it, while a `decision_permission` notice is a pending `decisions.create` that the inspector answers and the store delivers back to the provider. A notice raised from a hook report would be one nobody could act on. Wiring it needs a terminal-lane response path (the PTY, not the hook) designed first | A terminal-lane approval design; no card names it. Tracked on ft-971055ea |
| Agent output review | **Connected by design, through the run's exit notice.** A review is a host `change_review` / `merge_unreviewed` decision created and decided in one step on the person's action (`workbench/review.rs`), and dc-store deliberately raises no notice for it (`attention.rs:50-53`: the person is already looking at the range, and the run's own `run_exited` / `run_exit_failed` / `run_unresolved` notice points at the ended attempt). `change_review_is_created_only_for_an_ended_run_with_a_thirty_day_cap` pins that a review adds no notice | — (changing it means a new notice kind in dc-store and in every consumer's closed kind set) |
| GitHub changes | Blocked: no sync workflow writes to the store | ft-75540dc2 (two-way GitHub issue sync with a durable outbox) |
| Workspace results | Blocked: no cross-repository execution queue exists | ft-75540dc2 (cross-repository execution queue) |
| CI | Blocked: GitPulse reads workflow runs on demand (`load_workflow_runs_report`). It records no store event, and a notice must name a task, which a CI run does not | Needs a dc-store event and a task binding. Owned by ft-971055ea (store-event producers: CI results and reminders) |
| Reminders | Blocked: tasks carry `due_at`, but nothing records an event when it passes. The store has no clock-driven producer | Needs a dc-store scheduled event. Owned by ft-971055ea (store-event producers) |

## Windows and Linux

Today both platforms return `unsupported` and the settings panel names the
platform. Nothing has been built for them. The binding candidates below are
already in `Cargo.lock` as transitive dependencies, which is how the four
macOS crates were approved. Declaring one directly adds no package, but it
still needs approval, and enabling new features changes what is compiled.

- **Windows:** `windows` 0.62.2, which `tao` already pulls in. Toast
  delivery, actions and activation need its `UI_Notifications` and
  `Data_Xml_Dom` features. Those features are not enabled today, so the
  declaration adds compiled code but no package. Activation after quit also
  needs an AppUserModelID and a COM activator registered by the installer.
  The agent-hook bridge would need a named pipe (`Win32_System_Pipes`) in
  place of the Unix socket, with a peer-identity check equivalent to
  `getpeereid`.
- **Linux:** `zbus` 5.19.0, which `tauri-plugin-opener` and
  `tauri-plugin-single-instance` already pull in. It can call
  `org.freedesktop.Notifications.Notify` with actions and listen for
  `ActionInvoked` / `NotificationClosed`. Action support varies by notification
  server, so `GetCapabilities` decides whether Open/Snooze are offered. That
  follows the rule above: report unsupported actions, never show dead ones.
  `CloseNotification` gives withdrawal. The hook bridge's Unix socket is
  compiled there (`cfg!(unix)`), but nobody has run it on Linux, and a report
  has no banner to raise until delivery exists.

Until a binding is approved and qualified on real hosts, the Windows hook
switch stays disabled and explains why (`bridge_supported = cfg!(unix)`).

## Qualification contract

1. Manvi owns durable delivery records, settings and eligibility. Extend the
   existing inbox identity, with one notification per profile/source event,
   delivery receipts and restart reconciliation. Preserve submitted, failed,
   uncertain, read, dismissed and resolved as distinct facts. A lost submission
   reply must not produce an automatic second banner.
2. A single lazy native coordinator consumes bounded batches, uses private
   previews by default, applies profile/workspace/repository/task settings,
   snooze, quiet hours and burst grouping. No per-card worker, timer or model.
   Background delivery runs only with the user's background-mode setting.
3. Keep the notification delegate alive for the app lifetime. Resolve activation
   through durable profile/notice/task/run/review identities, including on launch.
   Re-read the current target before opening it. Deleted or changed requests open
   an explanatory history view; opaque IDs never become executable commands.
4. Request OS notification access only from the user's enable-notifications
   action. Permission grants, bypass and task acceptance remain application
   actions. Notification buttons offer Open, Review or Snooze only when supported.
   This integration adds no coding-agent authority or external communication.
5. Qualify a built app with its actual bundle identity: permission allowed/denied,
   Focus suppression, clicks while open and after quit, stale target, duplicate
   submission, failed receipt persistence, bursts, shutdown and recovery. Measure
   idle CPU/memory for the entire process tree. Unit tests cannot replace these
   installed-platform checks.

The dependency approval covers the four macOS bindings above. Notification
preferences and OS authorization remain opt-in; no coding-agent permission,
task acceptance, deployment or external communication is granted by a banner.
