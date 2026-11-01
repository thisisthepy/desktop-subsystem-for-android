//! Web links that apps in the guest open, which the host opens in the user's default browser.
//!
//! Only `http` and `https` links cross. Anything else is refused at each step (the agent, the
//! session and the window manager each check again), because the host hands the link to the
//! shell, and a shell given a file path or another scheme would run a program on the host.

use std::time::{Duration, Instant};

/// The longest link that crosses, in bytes.
///
/// The host's shell gives the link to the browser on its command line, and a Windows command
/// line holds at most 32767 characters. A link this long leaves room for the browser's own
/// path and options; no real page needs a longer one.
pub const MAX_URL_BYTES: usize = 32_000;

/// Checks that `url` is a web link the host may open, and returns it without the spaces
/// around it. The scheme is `http` or `https` (in any case), a host follows `//`, and the link
/// has no whitespace or control character, which a correctly encoded link never has.
pub fn web_url(url: &str) -> Result<&str, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("the link is empty".into());
    }
    if url.len() > MAX_URL_BYTES {
        return Err(format!(
            "the link is {} bytes long, longer than the {MAX_URL_BYTES} the host opens",
            url.len()
        ));
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("the link contains a space or a control character".into());
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err("the link has no scheme".into());
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(format!(
            "only http and https links open on the host, not {:?}",
            scheme.chars().take(32).collect::<String>()
        ));
    }
    let Some(rest) = rest.strip_prefix("//") else {
        return Err("the link has no host after its scheme".into());
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() || host.starts_with(':') || host.contains('\\') {
        return Err("the link names no host".into());
    }
    Ok(url)
}

/// How many links an Android may open on the host in [`LINK_WINDOW`]. A person taps a few
/// links a second at most; an app that opens more is not following a person, and the extra
/// links are dropped so it cannot flood the host with browser tabs.
pub const LINKS_PER_WINDOW: usize = 5;

/// The time [`LINKS_PER_WINDOW`] counts over.
pub const LINK_WINDOW: Duration = Duration::from_secs(10);

/// Lets through at most [`LINKS_PER_WINDOW`] links in any [`LINK_WINDOW`].
#[derive(Debug, Default)]
pub struct LinkGate {
    recent: Vec<Instant>,
}

impl LinkGate {
    /// Whether a link that arrives `now` may open.
    pub fn allow(&mut self, now: Instant) -> bool {
        self.recent
            .retain(|at| now.saturating_duration_since(*at) < LINK_WINDOW);
        if self.recent.len() >= LINKS_PER_WINDOW {
            return false;
        }
        self.recent.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr9_only_http_and_https_links_are_accepted() {
        assert_eq!(
            web_url(" https://example.com/a?b=c#d ").unwrap(),
            "https://example.com/a?b=c#d"
        );
        assert!(web_url("HTTP://example.com").is_ok());
        assert!(web_url("http://user@example.com:8080/").is_ok());
        for bad in [
            "",
            "file:///C:/Windows/System32/calc.exe",
            "C:\\Windows\\System32\\calc.exe",
            "calc.exe",
            "javascript:alert(1)",
            "ms-settings:",
            "mailto:a@example.com",
            "intent://example.com#Intent;end",
            "https:example.com",
            "https://",
            "http:///etc/passwd",
            "https://exa mple.com",
            "https://example.com/a\nb",
            "http://\\\\server\\share",
        ] {
            assert!(web_url(bad).is_err(), "{bad:?} was accepted");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        assert!(web_url(&long).is_err());
    }

    #[test]
    fn fr9_a_flood_of_links_is_cut_off() {
        let mut gate = LinkGate::default();
        let start = Instant::now();
        for _ in 0..LINKS_PER_WINDOW {
            assert!(gate.allow(start));
        }
        assert!(!gate.allow(start + Duration::from_secs(1)));
        assert!(gate.allow(start + LINK_WINDOW));
    }
}
