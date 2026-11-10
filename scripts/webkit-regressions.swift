import AppKit
import ObjectiveC
import WebKit

// Use the system WKWebView, with an ephemeral profile and the same fixture
// page as Chrome. The Node runner owns the assertion verdict and deadline.
// The optional second argument is the deadline in seconds; the Node runner
// passes each page its own (`harnessDeadlineSeconds`). It stays bounded so a
// malformed value can never mean "wait forever".
guard [2, 3].contains(CommandLine.arguments.count),
      let url = URL(string: CommandLine.arguments[1]),
      url.scheme == "http", url.host == "127.0.0.1",
      ["/harness/diagnostics.html", "/harness/pull-requests.html", "/harness/search.html", "/harness/conflicts.html", "/harness/uncommitted.html", "/harness/diff-history.html", "/harness/reflog.html", "/harness/coverage.html", "/harness/health.html", "/harness/blame.html", "/harness/branches.html", "/harness/hygiene.html", "/harness/palette.html", "/harness/status.html", "/harness/tasks.html", "/harness/task-materials.html", "/harness/task-runs.html", "/harness/onboarding.html", "/harness/firebase.html", "/harness/delivery.html", "/harness/terminal.html", "/harness/impact.html", "/harness/secrets.html", "/harness/markdown.html", "/harness/repo-tabs.html", "/harness/scroll-cue.html", "/harness/agents.html", "/harness/agent-capacity.html", "/harness/repo-churn.html", "/harness/diff.html", "/harness/file-tree.html", "/harness/file-viewer.html", "/harness/github-panel.html", "/harness/fleet.html", "/harness/tab-switch.html", "/harness/send-to-agent.html", "/harness/frame-rate.html", "/harness/mutation-routing.html", "/harness/browser-session.html", "/harness/primitives.html", "/harness/settings.html"].contains(url.path) else {

    fputs("Expected a supported local GitPulse harness URL\n", stderr)
    exit(2)
}
let requestedDeadline: Double? = CommandLine.arguments.count == 3 ? Double(CommandLine.arguments[2]) : 65
guard let deadline = requestedDeadline, deadline.isFinite, deadline >= 1, deadline <= 600 else {
    fputs("Expected a deadline between 1 and 600 seconds\n", stderr)
    exit(2)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let config = WKWebViewConfiguration()
// The app under test may lose focus while the agent reports progress. Keep
// fixture timers running; otherwise a hidden webview can suspend mid-assertion.
// This affects only the ephemeral test view, not GitPulse's runtime settings.
if #available(macOS 14.0, *) { config.preferences.inactiveSchedulingPolicy = .none }
config.websiteDataStore = .nonPersistent()
// A session whose screen is locked or whose display is off (an unattended
// run) makes WebKit treat every page as unseen, and it then throttles on two
// separate switches: WebCore aligns hidden-page timers to a one-second tick,
// and the WebContent process is put to sleep between wake-ups. Both start
// about 30 s into a run, which is where `tasks` stopped near check 263 (each
// `settle()` cost a second) and where `scroll-cue` halved its frame rate.
// Measured on a locked, display-off session: switching only one of them off
// left `tasks` timing out at its 150 s deadline; switching both off ran its
// 486 checks in about 100 s. They are WebKit SPI, so a build without one is
// reported rather than silently throttled.
for name in ["_setHiddenPageDOMTimerThrottlingEnabled:", "_setPageVisibilityBasedProcessSuppressionEnabled:"] {
    let selector = NSSelectorFromString(name)
    if let method = class_getInstanceMethod(WKPreferences.self, selector) {
        typealias SetEnabled = @convention(c) (AnyObject, Selector, Bool) -> Void
        unsafeBitCast(method_getImplementation(method), to: SetEnabled.self)(config.preferences, selector, false)
    } else {
        fputs("WebKit lacks \(name); a locked or sleeping display will throttle the page\n", stderr)
    }
}
let frame = NSRect(x: 0, y: 0, width: 1400, height: 1000)
let webview = WKWebView(frame: frame, configuration: config)
let window = NSWindow(contentRect: frame, styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "GitPulse WebKit regression test"
window.contentView = webview
// The window stays in the on-screen window list but is fully transparent,
// ignores the mouse and is never activated, so a run neither shows a window
// nor takes focus from whatever the person is typing into. It must not be
// ordered out or moved off every screen: a webview without an on-screen
// window is reported hidden and loses its display-driven animation frames,
// which none of the switches here cover. Measured against the activated,
// visible window on this host (3 paired runs): 59-60 fps and a 28 ms mean
// gap on a 25 ms timer either way, `document.hidden` false in both.
// `document.hasFocus()` was never something the runner could count on
// (activation is cooperative and another app can refuse it or take it back),
// so a fixture whose waits depend on the product's focus-gated polls says it
// is in front itself (see harness/foreground.ts).
// WebKit reports `document.hidden` whenever the window is occluded, and a
// locked screen, a sleeping display or another app's window all occlude it.
// A hidden page gets no animation frames and heavily throttled timers, so
// every fixture that waits on a transition or a poll timed out for reasons
// that had nothing to do with the product (the locked session that ran the
// whole suite unattended failed 11 of 27 pages, none of them in Chrome).
// Turn the occlusion detection off for this test view only; it is a WebKit
// SPI, so a build without it is reported rather than silently run hidden.
let occlusionSetter = NSSelectorFromString("_setWindowOcclusionDetectionEnabled:")
if let method = class_getInstanceMethod(WKWebView.self, occlusionSetter) {
    typealias SetEnabled = @convention(c) (AnyObject, Selector, Bool) -> Void
    unsafeBitCast(method_getImplementation(method), to: SetEnabled.self)(webview, occlusionSetter, false)
} else {
    fputs("WebKit lacks _setWindowOcclusionDetectionEnabled:; an occluded window will make pages hidden\n", stderr)
}
window.alphaValue = 0
window.ignoresMouseEvents = true
window.collectionBehavior = [.transient, .ignoresCycle]
window.orderFrontRegardless()
webview.load(URLRequest(url: url))
Timer.scheduledTimer(withTimeInterval: deadline, repeats: false) { _ in
    fputs("WebKit regression deadline expired\n", stderr)
    exit(1)
}
app.run()
