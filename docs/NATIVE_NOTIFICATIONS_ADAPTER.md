# Native activity notifications

Manvi schema seven now owns notification settings and delivery records alongside
the durable activity inbox introduced in schema six. GitPulse implements the
macOS adapter, preference controls and recovered review view. Source compilation,
store/native boundary tests and browser interaction are verified below. Actual
OS banner display and installed-app activation remain unverified.

Schema eight adds structured permission/question sources. Their previews stay
generic, and native eligibility shares the decision validity check: an expired
request or changed run, task or repository cannot produce a current banner.
Opening or acknowledging its notice never grants permission. The inspector saves
human decisions separately from host delivery and provider confirmation. Live
managed callback producers remain to be connected.

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
passed. The Clippy log is `/tmp/gitpulse-native-bindings-clippy.log`; the feature
comparison is `/tmp/gitpulse-native-bindings-feature-diff.json`. That declaration-only
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
- Authorization is requested only by the explicit enable/save interaction. OS
  calls have bounded callback waits. Windows/Linux report unsupported native
  delivery; no additional platform bindings were added.

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
`/tmp/gitpulse-native-notifications-browser-evidence.json`.

The full verification checkpoint is recorded in
[the implementation plan](AGENTIC_WORKSPACES_PLAN.md). Remaining work includes
installed bundle identity and permission/Focus tests, physical banner/action
delivery, click-after-quit behavior, callback persistence before OS acknowledgment,
OS Snooze/withdrawal behavior, burst/overload qualification and measured native
idle CPU/memory. Agent approval/review, CI and reminder event producers still
depend on those workflows being implemented.

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
