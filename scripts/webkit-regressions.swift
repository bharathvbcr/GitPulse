import AppKit
import WebKit

// Use the system WKWebView, with an ephemeral profile and the same fixture
// page as Chrome. The Node runner owns the assertion verdict and deadline.
// The optional second argument is the deadline in seconds; the Node runner
// passes each page its own (`harnessDeadlineSeconds`). It stays bounded so a
// malformed value can never mean "wait forever".
guard [2, 3].contains(CommandLine.arguments.count),
      let url = URL(string: CommandLine.arguments[1]),
      url.scheme == "http", url.host == "127.0.0.1",
      ["/harness/diagnostics.html", "/harness/pull-requests.html", "/harness/search.html", "/harness/conflicts.html", "/harness/uncommitted.html", "/harness/coverage.html", "/harness/health.html", "/harness/blame.html", "/harness/branches.html", "/harness/hygiene.html", "/harness/palette.html", "/harness/status.html", "/harness/tasks.html", "/harness/task-materials.html", "/harness/task-runs.html", "/harness/onboarding.html", "/harness/firebase.html", "/harness/delivery.html", "/harness/terminal.html", "/harness/impact.html", "/harness/secrets.html", "/harness/markdown.html", "/harness/repo-tabs.html", "/harness/agents.html", "/harness/agent-capacity.html"].contains(url.path) else {

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
let frame = NSRect(x: 0, y: 0, width: 1400, height: 1000)
let webview = WKWebView(frame: frame, configuration: config)
let window = NSWindow(contentRect: frame, styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "GitPulse WebKit regression test"
window.contentView = webview
// A visible window keeps WebKit's background timer throttling from
// stretching the fixture's bounded waits beyond the runner deadline. Key
// status is only requested: activation is cooperative, so another frontmost
// app (or a concurrent run) can refuse it or take it back mid-run, leaving
// `document.hasFocus()` false. A fixture whose waits depend on the product's
// focus-gated polls has to say it is in front itself (see tasksChecks.js).
window.makeKeyAndOrderFront(nil)
if #available(macOS 14.0, *) { app.activate() }
webview.load(URLRequest(url: url))
Timer.scheduledTimer(withTimeInterval: deadline, repeats: false) { _ in
    fputs("WebKit regression deadline expired\n", stderr)
    exit(1)
}
app.run()
