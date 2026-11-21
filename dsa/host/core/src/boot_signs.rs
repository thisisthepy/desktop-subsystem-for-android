//! What a boot's console and Android's log show about a guest that is late, and what to tell
//! the user about it.
//!
//! The counting and the sentences are the same on every host. What differs is where the
//! renderer runs and how a user starts over, which the host passes in as [`BootWords`].

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::guest_log::{LogFilter, LogWatch, DEFAULT_BUFFERS, WARN};
use crate::session::{Session, SessionState};

/// What a boot has shown that explains a guest that is late, counted line by line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootSigns {
    /// Lines where SELinux refused someone the control daemon's service.
    pub channel_denials: u64,
    /// The SELinux domain last refused it, such as `priv_app`.
    pub denied_domain: Option<String>,
    /// Lines where the hwcomposer could not reach the host's renderer.
    pub renderer_unreachable: u64,
}

impl BootSigns {
    /// Whether the agent was refused the channel for running as `priv_app`, which is what the
    /// package manager makes of a disk from other signing keys. Another domain refused says
    /// nothing about the disk.
    pub fn agent_refused_for_keys(&self) -> bool {
        self.channel_denials > 0 && self.denied_domain.as_deref() == Some("priv_app")
    }

    /// Adds what the signs of `other` count, as for a boot read from two sources.
    pub fn merge(&mut self, other: &BootSigns) {
        self.channel_denials += other.channel_denials;
        self.renderer_unreachable += other.renderer_unreachable;
        if other.denied_domain.is_some() {
            self.denied_domain = other.denied_domain.clone();
        }
    }

    /// What a console kept as text shows, for a host whose console is a file the hypervisor
    /// writes rather than a stream the host reads.
    pub fn from_console(text: &str) -> BootSigns {
        let mut signs = BootSigns::default();
        for line in text.lines() {
            signs.read_line(line);
        }
        signs
    }

    /// Reads one line of the boot's console or of Android's log.
    pub fn read_line(&mut self, line: &str) {
        if line.contains("denied") && line.contains("IChannel/default") {
            self.channel_denials += 1;
            if let Some((_, rest)) = line.split_once("scontext=u:r:") {
                self.denied_domain = rest.split([':', ' ']).next().map(str::to_string);
            }
        } else if line.contains("cannot reach the host renderer") {
            self.renderer_unreachable += 1;
        }
    }
}

/// The parts of the explanations that depend on the host.
#[derive(Debug, Clone, Copy)]
pub struct BootWords<'a> {
    /// A sentence saying where the renderer runs and where its log is, for a guest that cannot
    /// reach it.
    pub renderer_note: &'a str,
    /// How the user starts over, as a phrase that ends before the disk's path, such as
    /// ``run `dsa stop` and move``.
    pub start_over: &'a str,
}

/// What to tell the user about a guest that has not finished booting.
pub fn explain_unfinished_boot(signs: &BootSigns, log: &Path, words: &BootWords<'_>) -> String {
    if signs.renderer_unreachable > 0 {
        format!(
            "Android could not reach its renderer {} times, so it cannot draw and keeps \
             restarting. {} The whole boot is in {}.",
            signs.renderer_unreachable,
            words.renderer_note,
            log.display()
        )
    } else {
        format!(
            "The whole boot is in {}. A first boot on a new disk can take several minutes.",
            log.display()
        )
    }
}

/// What to tell the user about a guest that has booted without its agent.
pub fn explain_unreachable_agent(
    signs: &BootSigns,
    log: &Path,
    data_disk: Option<&Path>,
    words: &BootWords<'_>,
) -> String {
    let disk = data_disk
        .map(|disk| disk.display().to_string())
        .unwrap_or_else(|| "the Android data disk".to_string());
    let remedy = format!(
        "To start over, {} {disk} somewhere else: Android makes a new disk at its next start, \
         without your apps and their data, which stay in the file you moved.",
        words.start_over
    );
    if signs.channel_denials > 0 {
        let domain = signs
            .denied_domain
            .as_deref()
            .map(|domain| format!(" (it runs as {domain}, not platform_app)"))
            .unwrap_or_default();
        format!(
            "SELinux refused the agent the control channel {} times{domain}. That usually means \
             the data disk was made under an Android image signed with other keys, whose \
             certificates the package manager keeps using. {remedy} The whole boot is in {}.",
            signs.channel_denials,
            log.display()
        )
    } else {
        format!(
            "The agent may have crashed; the whole boot is in {}. If the data disk was made \
             under an Android image signed with other keys, that also stops the agent. {remedy}",
            log.display()
        )
    }
}

/// Reads Android's log while it boots (`Starting` or `Stalled`), and only then, for what
/// explains a boot that stalls: SELinux refusing the agent the channel, or the hwcomposer
/// not reaching the renderer. Once Android is up, or stopped, the log is no longer read.
pub fn watch_boot_signs(session: Weak<Session>, signs: Arc<Mutex<BootSigns>>) {
    let Some(events) = session.upgrade().map(|s| s.events()) else {
        return;
    };
    let mut watch: Option<LogWatch> = None;
    loop {
        let Some(running) = session.upgrade() else {
            return;
        };
        let booting = matches!(
            running.state(),
            SessionState::Starting | SessionState::Stalled
        );
        if booting && watch.is_none() {
            watch = Some(running.watch_log(LogFilter {
                buffers: DEFAULT_BUFFERS,
                tags: vec![("*".into(), WARN)],
            }));
        } else if !booting {
            watch = None;
        }
        drop(running);
        match &watch {
            Some(log) => {
                if let Ok(entry) = log.entries.recv_timeout(Duration::from_millis(500)) {
                    let mut signs = signs.lock().unwrap_or_else(|e| e.into_inner());
                    for entry in std::iter::once(entry).chain(log.entries.try_iter()) {
                        signs.read_line(&format!("{}: {}", entry.tag, entry.message));
                    }
                }
            }
            None => match events.recv_timeout(Duration::from_secs(1)) {
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORDS: BootWords<'static> = BootWords {
        renderer_note: "The renderer runs in the window manager.",
        start_over: "run `dsa stop` and move",
    };

    fn refusal(domain: &str) -> String {
        format!(
            "servicemanager E SELinux: avc:  denied  {{ find }} for pid=812 uid=10099 \
             name=io.github.thisisthepy.dsa.channel.IChannel/default \
             scontext=u:r:{domain}:s0:c512,c768 tcontext=u:object_r:dsa_channel_service:s0"
        )
    }

    #[test]
    fn fr14_only_a_refusal_as_priv_app_points_at_the_disks_signing_keys() {
        let mut signs = BootSigns::default();
        assert!(!signs.agent_refused_for_keys());
        signs.read_line(&refusal("untrusted_app"));
        assert!(!signs.agent_refused_for_keys());
        signs.read_line(&refusal("priv_app"));
        assert!(signs.agent_refused_for_keys());
    }

    #[test]
    fn fr2_a_refused_agent_is_counted_with_its_domain_and_gets_a_way_out_that_keeps_its_data() {
        let mut signs = BootSigns::default();
        for _ in 0..3 {
            signs.read_line(&refusal("priv_app"));
        }
        assert_eq!(signs.channel_denials, 3);
        assert_eq!(signs.denied_domain.as_deref(), Some("priv_app"));
        let said = explain_unreachable_agent(
            &signs,
            Path::new("android.log"),
            Some(Path::new("android.vhdx")),
            &WORDS,
        );
        assert!(said.contains("3 times"), "{said}");
        assert!(said.contains("priv_app"), "{said}");
        assert!(said.contains("signed with other keys"), "{said}");
        assert!(said.contains("move android.vhdx"), "{said}");
        assert!(said.contains("which stay in the file you moved"), "{said}");
    }

    #[test]
    fn fr2_an_unnamed_data_disk_is_called_the_android_data_disk() {
        let said = explain_unreachable_agent(
            &BootSigns::default(),
            Path::new("android.log"),
            None,
            &WORDS,
        );
        assert!(
            said.contains("move the Android data disk somewhere else"),
            "{said}"
        );
        assert!(said.contains("The agent may have crashed"), "{said}");
    }

    #[test]
    fn fr2_an_unreachable_renderer_is_named_as_the_cause_of_an_unfinished_boot() {
        let mut signs = BootSigns::default();
        signs.read_line("HWC W cannot reach the host renderer on vsock port 5567: No such device");
        let said = explain_unfinished_boot(&signs, Path::new("android.log"), &WORDS);
        assert!(said.contains("1 times"), "{said}");
        assert!(
            said.contains("The renderer runs in the window manager."),
            "{said}"
        );

        let said = explain_unfinished_boot(&BootSigns::default(), Path::new("android.log"), &WORDS);
        assert!(said.contains("android.log"), "{said}");
        assert!(!said.contains("window manager"), "{said}");
    }

    #[test]
    fn fr21_signs_from_the_guest_log_add_to_the_consoles() {
        let mut console = BootSigns::default();
        let mut log = BootSigns::default();
        log.read_line(&refusal("priv_app"));
        console.merge(&log);
        assert!(console.agent_refused_for_keys());
    }

    #[test]
    fn fr21_a_console_kept_as_text_is_read_line_by_line() {
        let text = format!(
            "kernel up\r\n{}\nhwcomposer: cannot reach the host renderer\nhwcomposer: cannot reach the host renderer\n",
            refusal("priv_app")
        );
        let signs = BootSigns::from_console(&text);
        assert_eq!(signs.channel_denials, 1);
        assert_eq!(signs.renderer_unreachable, 2);
        assert!(signs.agent_refused_for_keys());
    }
}
