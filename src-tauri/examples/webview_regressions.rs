//! Native-engine WebView regression runner using GitPulse's vendored
//! `framework/tao` and `framework/wry`.
//!
//! Opens an ephemeral 1400x1000 native WebView window (WebKitGTK 4.1 on Linux
//! under X11/Xvfb or headless Wayland/Weston; WebView2 on Windows; WKWebView
//! on macOS) and loads one allowlisted local GitPulse harness URL. The Node
//! runner (`scripts/browser-regressions.mjs`) owns the assertion verdict and
//! kills this process as soon as the page POSTs its result.

use std::path::PathBuf;
use std::process;
use std::sync::mpsc;
use std::time::Duration;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::{WebContext, WebViewBuilder};

/// Every supported GitPulse browser harness path. Must stay aligned with
/// `BROWSER_HARNESSES` in `scripts/browser-regressions.mjs` (enforced by
/// `scripts/browser-regressions.test.ts`).
const ALLOWED_HARNESS_PATHS: &[&str] = &[
    "/harness/diagnostics.html",
    "/harness/devmap.html",
    "/harness/pull-requests.html",
    "/harness/search.html",
    "/harness/conflicts.html",
    "/harness/uncommitted.html",
    "/harness/diff-history.html",
    "/harness/reflog.html",
    "/harness/coverage.html",
    "/harness/health.html",
    "/harness/blame.html",
    "/harness/branches.html",
    "/harness/hygiene.html",
    "/harness/palette.html",
    "/harness/status.html",
    "/harness/tasks.html",
    "/harness/task-materials.html",
    "/harness/task-runs.html",
    "/harness/onboarding.html",
    "/harness/firebase.html",
    "/harness/delivery.html",
    "/harness/terminal.html",
    "/harness/impact.html",
    "/harness/secrets.html",
    "/harness/markdown.html",
    "/harness/html-preview.html",
    "/harness/svg-preview.html",
    "/harness/repo-tabs.html",
    "/harness/scroll-cue.html",
    "/harness/agents.html",
    "/harness/agent-capacity.html",
    "/harness/repo-churn.html",
    "/harness/diff.html",
    "/harness/file-tree.html",
    "/harness/file-viewer.html",
    "/harness/github-panel.html",
    "/harness/fleet.html",
    "/harness/tab-switch.html",
    "/harness/send-to-agent.html",
    "/harness/frame-rate.html",
    "/harness/mutation-routing.html",
    "/harness/browser-session.html",
    "/harness/primitives.html",
    "/harness/settings.html",
    "/harness/appearance.html",
    "/harness/hidden-pane.html",
    "/harness/overview.html",
];

fn validate_harness_url(raw: &str) -> Result<(), String> {
    let rest = raw
        .strip_prefix("http://127.0.0.1:")
        .ok_or_else(|| "Expected an http://127.0.0.1:<port>/harness/<name>.html URL".to_string())?;
    let (port_str, after_port) = rest
        .split_once('/')
        .ok_or_else(|| "URL is missing a path".to_string())?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("Invalid loopback port: {port_str:?}"))?;
    if port == 0 {
        return Err("Loopback port must be non-zero".to_string());
    }
    let full_path = format!("/{after_port}");
    let path_only = full_path
        .split(['?', '#'])
        .next()
        .unwrap_or(full_path.as_str());
    if !ALLOWED_HARNESS_PATHS.contains(&path_only) {
        return Err(format!("Unsupported GitPulse harness path: {path_only:?}"));
    }
    Ok(())
}

fn validate_deadline(raw: Option<&str>) -> Result<f64, String> {
    let secs = match raw {
        Some(value) => value
            .parse::<f64>()
            .map_err(|_| format!("Invalid deadline: {value:?}"))?,
        None => 65.0,
    };
    if !secs.is_finite() || !(1.0..=600.0).contains(&secs) {
        return Err("Expected a deadline between 1 and 600 seconds".to_string());
    }
    Ok(secs)
}

fn engine_label() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "WebKitGTK"
    }
    #[cfg(target_os = "windows")]
    {
        "WebView2"
    }
    #[cfg(target_os = "macos")]
    {
        "WKWebView"
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        "WebView"
    }
}

fn print_version() -> ! {
    match wry::webview_version() {
        Ok(version) => {
            println!("{} {} (tao/wry)", engine_label(), version);
            process::exit(0);
        }
        Err(error) => {
            eprintln!("Native {} runtime is unavailable: {error}", engine_label());
            process::exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--version" || a == "-V") {
        print_version();
    }
    if !(2..=3).contains(&args.len()) {
        eprintln!("Usage: webview_regressions <http://127.0.0.1:<port>/harness/<name>.html> [deadline_seconds]");
        eprintln!("       webview_regressions --version");
        process::exit(2);
    }

    let url = &args[1];
    if let Err(error) = validate_harness_url(url) {
        eprintln!("Expected a supported local GitPulse harness URL: {error}");
        process::exit(2);
    }
    let deadline_secs = match validate_deadline(args.get(2).map(String::as_str)) {
        Ok(secs) => secs,
        Err(error) => {
            eprintln!("{error}");
            process::exit(2);
        }
    };

    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            eprintln!(
                "WebKitGTK runner requires an X11 (DISPLAY via xvfb-run) or Wayland (WAYLAND_DISPLAY via headless weston/cage) display"
            );
            process::exit(3);
        }
    }

    let owned_profile: Option<tempfile::TempDir>;
    let profile_dir: PathBuf = match std::env::var_os("GITPULSE_WEBVIEW_PROFILE") {
        Some(dir) if !dir.is_empty() => {
            owned_profile = None;
            let path = PathBuf::from(dir);
            if let Err(error) = std::fs::create_dir_all(&path) {
                eprintln!(
                    "Failed to create webview profile directory {}: {error}",
                    path.display()
                );
                process::exit(2);
            }
            path
        }
        _ => {
            let dir = tempfile::Builder::new()
                .prefix("gitpulse-webview-")
                .tempdir()
                .unwrap_or_else(|error| {
                    eprintln!("Failed to create temporary webview profile: {error}");
                    process::exit(2);
                });
            let path = dir.path().to_path_buf();
            owned_profile = Some(dir);
            path
        }
    };

    let take_focus = match std::env::var("GITPULSE_NATIVE_TAKE_FOCUS")
        .or_else(|_| std::env::var("GITPULSE_WEBKIT_TAKE_FOCUS"))
        .as_deref()
    {
        Ok("1") => true,
        Ok("0") => false,
        _ => !cfg!(target_os = "macos"),
    };

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("GitPulse native webview regression test")
        .with_inner_size(LogicalSize::new(1400.0, 1000.0))
        .with_focused(take_focus)
        .build(&event_loop)
        .unwrap_or_else(|error| {
            eprintln!("Failed to create native window: {error}");
            process::exit(3);
        });

    let mut web_context = WebContext::new(Some(profile_dir));
    #[allow(unused_mut)]
    let mut builder = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_url(url)
        .with_focused(take_focus)
        .with_incognito(true);

    #[cfg(target_os = "windows")]
    {
        use wry::WebViewBuilderExtWindows;
        builder = builder.with_additional_browser_args(
            "--enable-unsafe-swiftshader \
             --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
             --disable-background-timer-throttling \
             --disable-renderer-backgrounding \
             --disable-backgrounding-occluded-windows \
             --autoplay-policy=no-user-gesture-required",
        );
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = builder.build(&window);

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().unwrap_or_else(|| {
            eprintln!("GTK window has no default vbox");
            process::exit(3);
        });
        builder.build_gtk(vbox)
    };

    let _webview = webview.unwrap_or_else(|error| {
        eprintln!("Failed to initialize {} webview: {error}", engine_label());
        process::exit(3);
    });

    let (done_tx, done_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if done_rx
            .recv_timeout(Duration::from_secs_f64(deadline_secs))
            .is_err()
        {
            eprintln!("Native webview regression deadline expired");
            process::exit(1);
        }
    });

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _keep_context = &web_context;
        let _keep_webview = &_webview;
        let _keep_profile = &owned_profile;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            let _ = done_tx.send(());
            *control_flow = ControlFlow::Exit;
        }
    });
}
