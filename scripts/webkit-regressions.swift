import AppKit
import WebKit

// Use the system WKWebView, with an ephemeral profile and the same fixture
// page as Chrome. The Node runner owns the assertion verdict and deadline.
guard CommandLine.arguments.count == 2,
      let url = URL(string: CommandLine.arguments[1]),
      url.scheme == "http", url.host == "127.0.0.1",
      ["/harness/diagnostics.html", "/harness/conflicts.html", "/harness/uncommitted.html", "/harness/coverage.html", "/harness/branches.html"].contains(url.path) else {
    fputs("Expected a supported local GitPulse harness URL\n", stderr)
    exit(2)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let config = WKWebViewConfiguration()
config.websiteDataStore = .nonPersistent()
let frame = NSRect(x: 0, y: 0, width: 1400, height: 1000)
let webview = WKWebView(frame: frame, configuration: config)
let window = NSWindow(contentRect: frame, styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "GitPulse WebKit regression test"
window.contentView = webview
// A visible key window keeps WebKit's background timer throttling from
// stretching the fixture's bounded waits beyond the runner deadline.
window.makeKeyAndOrderFront(nil)
if #available(macOS 14.0, *) { app.activate() }
webview.load(URLRequest(url: url))
Timer.scheduledTimer(withTimeInterval: 65, repeats: false) { _ in
    fputs("WebKit regression deadline expired\n", stderr)
    exit(1)
}
app.run()
