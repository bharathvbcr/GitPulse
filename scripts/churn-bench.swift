import AppKit
import ObjectiveC
import WebKit

// A WKWebView for `scripts/churn-bench.mjs`: the same setup as
// `webkit-regressions.swift` (ephemeral profile, no hidden-page throttling, no
// occlusion detection, a transparent never-activated window), for the one page
// that runs longer than that runner's 600 second cap. It is a separate file on
// purpose: that runner's allow-list and deadline are pinned to the CI harnesses
// by `browser-regressions.test.ts`, and this page must never run under `--all`.
// Arguments: the bench URL and a deadline in seconds (at most 4 hours).
guard CommandLine.arguments.count == 3,
      let url = URL(string: CommandLine.arguments[1]),
      url.scheme == "http", url.host == "127.0.0.1", url.path == "/harness/churn-bench.html",
      let deadline = Double(CommandLine.arguments[2]), deadline.isFinite, deadline >= 1, deadline <= 14_400 else {
    fputs("Usage: churn-bench.swift <http://127.0.0.1:port/harness/churn-bench.html?...> <deadline seconds>\n", stderr)
    exit(2)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let config = WKWebViewConfiguration()
if #available(macOS 14.0, *) { config.preferences.inactiveSchedulingPolicy = .none }
config.websiteDataStore = .nonPersistent()
typealias SetEnabled = @convention(c) (AnyObject, Selector, Bool) -> Void
for name in ["_setHiddenPageDOMTimerThrottlingEnabled:", "_setPageVisibilityBasedProcessSuppressionEnabled:"] {
    let selector = NSSelectorFromString(name)
    if let method = class_getInstanceMethod(WKPreferences.self, selector) {
        unsafeBitCast(method_getImplementation(method), to: SetEnabled.self)(config.preferences, selector, false)
    } else {
        fputs("WebKit lacks \(name); a locked or sleeping display will throttle the page\n", stderr)
    }
}
let frame = NSRect(x: 0, y: 0, width: 1400, height: 1000)
let webview = WKWebView(frame: frame, configuration: config)
let window = NSWindow(contentRect: frame, styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "GitPulse context churn bench"
window.contentView = webview
let occlusionSetter = NSSelectorFromString("_setWindowOcclusionDetectionEnabled:")
if let method = class_getInstanceMethod(WKWebView.self, occlusionSetter) {
    unsafeBitCast(method_getImplementation(method), to: SetEnabled.self)(webview, occlusionSetter, false)
} else {
    fputs("WebKit lacks _setWindowOcclusionDetectionEnabled:; an occluded window will make the page hidden\n", stderr)
}
window.alphaValue = 0
window.ignoresMouseEvents = true
window.collectionBehavior = [.transient, .ignoresCycle]
window.orderFrontRegardless()
webview.load(URLRequest(url: url))
Timer.scheduledTimer(withTimeInterval: deadline, repeats: false) { _ in
    fputs("The bench deadline expired\n", stderr)
    exit(1)
}
app.run()
