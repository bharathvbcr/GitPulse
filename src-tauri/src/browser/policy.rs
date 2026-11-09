//! Which URLs the browser pane may load: loopback dev servers only, until an
//! any-URL mode is approved (board task M3).

use tauri::Url;

/// Why a URL was refused, or `Ok` with the parsed URL.
pub fn check_loopback(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("{raw:?} is not a URL: {e}"))?;
    if url.as_str() == "about:blank" {
        return Ok(url);
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "{raw:?} uses the {scheme:?} scheme; only http and https to a loopback host are allowed",
            scheme = url.scheme()
        ));
    }
    // `host_str` is the parser's serialized host: IPv4 in dotted form whatever
    // spelling was given (`0x7f.1` becomes `127.0.0.1`), IPv6 in brackets.
    let loopback = match url.host_str() {
        Some(host) if host.eq_ignore_ascii_case("localhost") => true,
        Some(host) => match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            Some(v6) => v6
                .parse::<std::net::Ipv6Addr>()
                .is_ok_and(|ip| ip.is_loopback()),
            None => host
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|ip| ip.is_loopback()),
        },
        None => false,
    };
    if !loopback {
        return Err(format!(
            "{raw:?} is not a loopback address; only localhost, 127.0.0.0/8 and ::1 are allowed"
        ));
    }
    Ok(url)
}

/// The navigation handler's verdict: may the pane (any frame) go to `url`?
pub fn allows_navigation(url: &str) -> bool {
    check_loopback(url).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_dev_servers_are_allowed() {
        for url in [
            "http://localhost:5173/",
            "http://LOCALHOST:3000/app?x=1#y",
            "https://localhost/",
            "http://127.0.0.1:8080/",
            "http://127.1.2.3/",
            "http://0x7f.1/",
            "http://[::1]:4000/",
            "about:blank",
        ] {
            assert!(check_loopback(url).is_ok(), "{url} should be allowed");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for url in [
            "https://example.com/",
            "http://localhost.example.com/",
            "http://evil.localhost/",
            "http://0.0.0.0:5173/",
            "http://10.0.0.1/",
            "http://[::ffff:7f00:1]/",
            "file:///etc/passwd",
            "ipc://localhost/cmd_list_branches",
            "tauri://localhost/",
            "javascript:alert(1)",
            "data:text/html,hi",
            "about:srcdoc",
            "localhost:5173",
            "",
            "not a url",
        ] {
            assert!(check_loopback(url).is_err(), "{url:?} should be refused");
        }
    }

    #[test]
    fn userinfo_cannot_disguise_a_remote_host() {
        // The host is what follows the `@`.
        assert!(check_loopback("http://localhost@example.com/").is_err());
        assert!(check_loopback("http://example.com@127.0.0.1/").is_ok());
    }
}
