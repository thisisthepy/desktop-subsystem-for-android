//! Web links that apps in the guest open, which the host opens in the user's default browser.
//!
//! Only `http` and `https` links cross. Anything else is refused at each step (the agent, the
//! session and the window manager each check again), because the host hands the link to the
//! shell, and a shell given a file path or another scheme would run a program on the host.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dsa_transport::Connector;

use crate::follower::wait;
use crate::host_api::HostApiClient;

/// The longest link that crosses, in bytes.
///
/// The host's shell gives the link to the browser on its command line, and a Windows command
/// line holds at most 32767 characters. A link this long leaves room for the browser's own
/// path and options; no real page needs a longer one.
pub const MAX_URL_BYTES: usize = 32_000;

/// The arguments that open DSA's settings on the Apps page with `package`'s row in view.
pub fn app_page_arguments(package: &str) -> [String; 4] {
    [
        "--page".into(),
        "apps".into(),
        "--app".into(),
        package.into(),
    ]
}

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

/// Starts the settings app `program` on its Apps page with `package`'s row in view
/// ([`app_page_arguments`]). The program is the host's own; it is not waited for.
pub fn open_app_page_with(program: &std::path::Path, package: &str) -> Result<(), String> {
    std::process::Command::new(program)
        .args(app_page_arguments(package))
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start {}: {e}", program.display()))
}

/// Opens a link the user's Android opened, in the user's default browser. Each host says how:
/// the shell, `xdg-open` or `open`.
pub trait LinkOpener: Send {
    fn open(&mut self, url: &str) -> Result<(), String>;
}

/// How long the watch waits for a link before it looks at `stop` again.
const POLL: Duration = Duration::from_millis(200);

/// How long the watch waits before it reconnects to the service.
const RETRY: Duration = Duration::from_secs(2);

/// How long a connection to the service may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Checks one link from Android and hands it to `opener`. A link that fails the check is
/// dropped and logged.
pub fn open_link(url: &str, opener: &mut dyn LinkOpener, log: &dyn Fn(&str)) {
    match web_url(url) {
        Ok(url) => {
            if let Err(why) = opener.open(url) {
                log(&format!("links: {why}"));
            }
        }
        Err(why) => log(&format!("links: dropped a link from Android: {why}")),
    }
}

/// Watches the links of the user's Android through `connector` and opens each with `opener`,
/// until `stop` is set. A lost connection is made again; a link that fails the check is
/// dropped and logged. Not being able to watch yet is logged once until a watch succeeds, so
/// an Android that is down does not fill the log.
pub fn follow(
    connector: &mut dyn Connector,
    build: &str,
    opener: &mut dyn LinkOpener,
    stop: &AtomicBool,
    log: &dyn Fn(&str),
) {
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(connector, build, CONNECT_TIMEOUT)
            .and_then(|client| client.watch_links());
        let mut stream = match stream {
            Ok(stream) => {
                warned = false;
                stream
            }
            Err(why) => {
                if !warned {
                    log(&format!("links: cannot watch Android's links yet: {why}"));
                    warned = true;
                }
                wait(stop, RETRY);
                continue;
            }
        };
        while !stop.load(Ordering::Relaxed) {
            match stream.next(POLL) {
                Ok(None) => {}
                Ok(Some(url)) => open_link(&url, opener, log),
                Err(why) => {
                    log(&format!("links: the watch of Android's links ended: {why}"));
                    wait(stop, RETRY);
                    break;
                }
            }
        }
    }
}

/// The command that opens `url` with `program` (the host's own opener, which takes the link as
/// its only argument), with no input and no output. The link is checked first ([`web_url`]),
/// and it starts with `http`, so no opener reads it as an option.
pub fn open_command(program: &str, url: &str) -> Result<std::process::Command, String> {
    use std::process::{Command, Stdio};
    let url = web_url(url)?;
    let mut command = Command::new(program);
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr9_the_open_command_carries_the_checked_link_as_its_one_argument() {
        let command = open_command("opener", " https://example.com/a ").unwrap();
        assert_eq!(command.get_program(), "opener");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["https://example.com/a"]);
        assert!(open_command("opener", "file:///etc/passwd").is_err());
    }

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

    struct Recorder(Vec<String>, bool);

    impl LinkOpener for Recorder {
        fn open(&mut self, url: &str) -> Result<(), String> {
            self.0.push(url.to_string());
            if self.1 {
                Err("no browser".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn fr9_a_link_is_checked_before_the_opener_sees_it() {
        let mut opener = Recorder(Vec::new(), false);
        let logged = std::cell::RefCell::new(Vec::new());
        let log = |line: &str| logged.borrow_mut().push(line.to_string());
        open_link(" https://example.com/a ", &mut opener, &log);
        open_link("file:///etc/passwd", &mut opener, &log);
        assert_eq!(opener.0, ["https://example.com/a"]);
        let logged = logged.into_inner();
        assert_eq!(logged.len(), 1);
        assert!(logged[0].starts_with("links: dropped a link from Android"));
    }

    #[test]
    fn fr9_an_opener_that_fails_is_logged_and_the_watch_goes_on() {
        let mut opener = Recorder(Vec::new(), true);
        let logged = std::cell::RefCell::new(Vec::new());
        let log = |line: &str| logged.borrow_mut().push(line.to_string());
        open_link("https://example.com/", &mut opener, &log);
        assert_eq!(logged.into_inner(), ["links: no browser"]);
    }

    #[test]
    fn fr9_a_settings_app_that_cannot_start_is_reported_with_its_path() {
        let missing = std::path::Path::new("/dsa-test/no-such-settings-app");
        let why = open_app_page_with(missing, "com.example.chat").unwrap_err();
        assert!(why.contains("no-such-settings-app"), "{why}");
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
