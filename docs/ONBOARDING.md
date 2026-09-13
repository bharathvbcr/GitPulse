# First-run walkthrough and permissions

The walkthrough opens the first time this version of GitPulse runs in a webview
profile. It also introduces the tour to existing profiles that have never saved
walkthrough progress. Six steps cover repositories and trust, the registered
Work/Code/History/Insights views, Tasks and optional tools, platform access, and
getting started. The title-bar **Walkthrough** button is available with or
without an open repository.

**Start tour**, **Continue**, **Skip step**, and **Back** save the current step. **Later**, the close button, and
Escape defer the tour without recording completion; reopen it to resume.
**Finish** records completion, and replay then starts at the beginning.

The welcome and finish cards are modal. The four middle steps are live guides:
the app remains clickable, keyboard focus can leave the guide, and a highlight
follows the existing Open menu, view tabs, or Tasks control. **Focus highlighted
control** moves keyboard focus to that control. The guide observes layout and
target changes only while it is active and stays below menus and native trust
or Settings dialogs. If a control is hidden or unavailable, the guide remains
on screen with direct actions and a skip option. Small windows scroll the card.

Choosing a repository keeps the guide open. Canceling or failing the picker
does not claim success; the open repository state controls its acknowledgment.
Work/Code/History/Insights buttons navigate the actual repository using the
existing view registry. Without a repository they are disabled and the guide
offers the picker. **Open Tasks** opens the real task board without creating a
task. **Open Settings** keeps the current tour step; close Settings to continue.
The acknowledgment means Settings was opened, not that a permission was granted.
Optional tool setup defers the tour before opening its existing wizard.
The final repository action records tour completion, not a successful
repository open or a permission grant. Skipping an interaction never blocks finishing.

Progress lives in `gitpulse.product-tour.v1` in the webview's local storage,
separately from CLI installation configuration and repository data. A crash or
reload resumes an active tour. Invalid data starts over. Read/write failures are
visible; a failed save does not close the tour or report saved completion.
**Close without saving** provides an explicit session-only exit. Clearing webview
data resets the tour. No account, remote service, or installed CLI is required.

## Platform access

GitPulse uses the existing native folder picker and repository trust flow.
macOS bundle purpose strings explain access to repositories opened in Desktop,
Documents, and Downloads. The OS decides whether to prompt and whether access is
allowed. After denial, review GitPulse in System Settings → Privacy & Security →
Files and Folders and retry opening the repository. Windows and Linux guidance
points to account/folder permissions and platform protection or confinement.
An unknown host is explicitly identified without claiming verified access.

Launch at login remains optional in Settings, through the existing autostart
plugin. Remote Git credentials, provider credentials, and tool installation
remain their own setup flows. The tour neither grants OS access nor broadens
Tauri capabilities, and repository trust cannot bypass OS denial. It requests
no camera, microphone, Accessibility, or Full Disk Access.

The purpose keys follow Apple's [Desktop folder documentation](https://developer.apple.com/documentation/bundleresources/information-property-list/nsdesktopfolderusagedescription),
[Documents folder documentation](https://developer.apple.com/documentation/bundleresources/information-property-list/nsdocumentsfolderusagedescription),
and [Downloads folder documentation](https://developer.apple.com/documentation/bundleresources/information-property-list/nsdownloadsfolderusagedescription).

## Verification

- `npm test -- src/lib/tools/productTour.test.ts src/lib/tools/productTourTarget.test.ts scripts/onboarding-contract.test.ts`
  checks persistence, bounded navigation, corrupt state, unavailable storage,
  failed completion/retry, viewport placement, native purpose strings, and App integration.
- `npm run test:browser -- --harness onboarding` mounts the production component
  and exercises live targets, actual Open-menu interactions, picker cancellation,
  failure and retry, view/Tasks/Settings state, moving and missing targets,
  replay/relaunch, modal focus trapping, live focus handoff, restoration, Escape,
  platform copy, and storage failure recovery.
- `npm run test:webkit -- --harness onboarding` repeats those checks in native
  WebKit. Both are registered in the repository's all-harness runners.
- `/harness/onboarding.html` is the manual preview; `?check=1` runs its assertions.

Browser fixtures verify callback delivery, not the native folder picker or real
OS permission decisions. Fresh-profile installed-app testing of allow/deny/retry
on macOS and actual Windows/Linux permissions requires those platform sessions.
