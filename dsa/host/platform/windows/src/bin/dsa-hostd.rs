//! dsa-hostd: runs the Android VM and its control channel.
//!
//! ```text
//! dsa-hostd <config.json>
//! ```
//!
//! It starts the VM, waits for the guest's channel daemon to connect and pass the protocol
//! handshake, and prints every state change. Commands on standard input: `start`, `stop`,
//! `status`, `quit`. End of input also quits; quitting stops the VM first.
//!
//! Creating a VM needs administrator rights, so run it from an elevated prompt. In the
//! installed product the DSA service plays this part.

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-hostd runs on Windows only");
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match windows_host::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dsa-hostd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod windows_host {
    use std::io::BufRead;
    use std::thread;
    use std::time::Duration;

    use dsa_host_core::handshake::ControlHandshake;
    use dsa_host_core::session::{Session, SessionState};
    use dsa_host_windows::account::current_user_sid;
    use dsa_host_windows::android_vm::AndroidVmLauncher;
    use dsa_host_windows::guid::Guid;
    use dsa_host_windows::host_config::HostConfig;
    use dsa_host_windows::hvsocket::HvSocketListener;
    use dsa_host_windows::vm_listener::VmBoundListener;
    use dsa_transport::Listener;

    pub fn run() -> Result<(), String> {
        let path = std::env::args()
            .nth(1)
            .ok_or("usage: dsa-hostd <config.json>")?;
        let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
        let config = HostConfig::parse(&text).map_err(|e| e.to_string())?;

        let fallback_sid = if config.user_sid.is_none() {
            Some(current_user_sid()?)
        } else {
            None
        };
        let launcher =
            AndroidVmLauncher::new(&config.vm_name, config.vm_config(fallback_sid.as_deref()));
        let service = Guid::vsock_service(config.control_port);
        let listener = VmBoundListener::new(
            launcher.vm_id_slot(),
            Box::new(move |vm_id| {
                HvSocketListener::bind(vm_id, service).map(|l| Box::new(l) as Box<dyn Listener>)
            }),
        );
        let session_config = config.session_config();
        let build = format!("dsa-hostd {}", env!("CARGO_PKG_VERSION"));
        let handshake = ControlHandshake::new(&build, session_config.connect_timeout);
        let session = Session::new(launcher, Box::new(listener), handshake, session_config);

        let events = session.events();
        thread::spawn(move || {
            for event in events {
                match event.reason {
                    Some(reason) => println!("state: {} ({reason})", event.state),
                    None => println!("state: {}", event.state),
                }
            }
        });

        println!(
            "{build}: starting the Android VM {:?}; commands: start, stop, status, quit",
            config.vm_name
        );
        session.start();
        for line in std::io::stdin().lock().lines() {
            match line.map_err(|e| e.to_string())?.trim() {
                "start" => session.start(),
                "stop" => session.stop(),
                "status" => match session.last_reason() {
                    Some(reason) => println!("status: {} (last: {reason})", session.state()),
                    None => println!("status: {}", session.state()),
                },
                "quit" => break,
                "" => {}
                other => println!("unknown command {other:?}; commands: start, stop, status, quit"),
            }
        }

        session.stop();
        let grace = Duration::from_secs(config.stop_timeout_secs + 15);
        if !session.wait_for(SessionState::Stopped, grace) {
            return Err(format!(
                "the Android VM did not stop: {}",
                session
                    .last_reason()
                    .map(|r| r.to_string())
                    .unwrap_or_default()
            ));
        }
        Ok(())
    }
}
