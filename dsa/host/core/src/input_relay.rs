//! The connection that carries a window's pointer and key input to Android, and what it does
//! when the service cannot be reached.
//!
//! Input is sent event by event. The connection opens on the first event and stays open. When
//! sending an event fails the connection is opened again at once and the event is sent once
//! more. When opening fails, no new attempt is made for a while, so a stopped service does not
//! cost a connection attempt per mouse move, and the same failure is reported once, not per
//! event.

use std::time::{Duration, Instant};

use crate::host_api::InputStream;
use crate::input::InputEvent;

/// A connection for input events, opened when needed.
pub struct InputRelay {
    stream: Option<InputStream>,
    retry_at: Option<Instant>,
    last_error: String,
    retry: Duration,
}

impl InputRelay {
    /// A relay that waits `retry` after a failed attempt to open its connection.
    pub fn new(retry: Duration) -> InputRelay {
        InputRelay {
            stream: None,
            retry_at: None,
            last_error: String::new(),
            retry,
        }
    }

    /// Sends `event`, opening the connection with `connect` when there is none and `now` is
    /// not before the end of the wait after a failed attempt. It returns the reason when an
    /// attempt failed for a reason not returned before, so the caller logs it once.
    pub fn send(
        &mut self,
        event: &InputEvent,
        now: Instant,
        mut connect: impl FnMut() -> Result<InputStream, String>,
    ) -> Option<String> {
        for _ in 0..2 {
            if self.stream.is_none() {
                if self.retry_at.is_some_and(|at| now < at) {
                    return None;
                }
                match connect() {
                    Ok(connected) => {
                        self.stream = Some(connected);
                        self.retry_at = None;
                        self.last_error.clear();
                    }
                    Err(error) => {
                        self.retry_at = Some(now + self.retry);
                        if error == self.last_error {
                            return None;
                        }
                        self.last_error.clone_from(&error);
                        return Some(error);
                    }
                }
            }
            let connected = self.stream.as_mut()?;
            match connected.send_input(event) {
                Ok(()) => return None,
                Err(_) => self.stream = None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> InputEvent {
        InputEvent::Ime(crate::ime::ImeEvent::Commit {
            task_id: 1,
            text: "a".into(),
        })
    }

    #[test]
    fn fr7_a_failed_connection_is_reported_once_and_not_tried_again_until_the_wait_ends() {
        let mut relay = InputRelay::new(Duration::from_millis(500));
        let start = Instant::now();
        let attempts = std::cell::Cell::new(0);
        let mut connect = || {
            attempts.set(attempts.get() + 1);
            Err::<InputStream, String>("no service".into())
        };
        assert_eq!(
            relay.send(&event(), start, &mut connect),
            Some("no service".into())
        );
        assert_eq!(relay.send(&event(), start, &mut connect), None);
        assert_eq!(attempts.get(), 1);
        // After the wait it is tried again, and the same reason is not reported again.
        let later = start + Duration::from_millis(600);
        assert_eq!(relay.send(&event(), later, &mut connect), None);
        assert_eq!(attempts.get(), 2);
        // Another reason is.
        let mut other = || Err::<InputStream, String>("refused".into());
        let much_later = later + Duration::from_millis(600);
        assert_eq!(
            relay.send(&event(), much_later, &mut other),
            Some("refused".into())
        );
    }
}
