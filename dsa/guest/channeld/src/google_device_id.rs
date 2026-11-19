//! The Google Services Framework Android ID, as the daemon gets it from Google Play services.
//!
//! Google's page for uncertified devices asks for this ID before it lets the user sign in. The
//! agent asks Google's gservices provider for it first, but current Google Play services refuse
//! every caller that is not on their own list, the agent included, so the answer is often empty.
//! The services also write the ID, in decimal, into their own preferences file after the
//! device's first check-in with Google. That file belongs to the services' app user, and the
//! platform's policy forbids every domain but a few of its own to read an app's data, the daemon's
//! included. So the daemon does not read it: it asks a helper init runs as root in a domain of its
//! own ([`crate::google_id_reader`]), through the property [`PROPERTY`]. The daemon sets the
//! property to [`REQUEST`]; init starts the helper; the helper sets it to the ID in decimal, or to
//! [`NONE`] while the services have not made one.
//!
//! The ID is kept and shown in decimal: that is the form Google's page takes. Other tools print
//! it in hexadecimal, which that page does not accept.
//!
//! The agent's empty answer is filled in from the property on its way to the host. Asking never
//! waits: when there is no ID yet the answer stays empty, a request goes out, and the host's next
//! look finds the ID.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The property the request and the answer go through.
pub const PROPERTY: &str = "vendor.dsa.google_device_id";
/// What the daemon sets the property to when it wants the ID read.
pub const REQUEST: &str = "request";
/// What the helper sets the property to when there is no ID to read.
pub const NONE: &str = "none";

/// How long the daemon waits before it asks the helper again, so a host that keeps asking while
/// the services have no ID yet does not start a process each time.
pub const ASK_AGAIN: Duration = Duration::from_secs(30);

/// Whether `value` is an Android ID: a decimal number.
pub fn is_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

/// The daemon's side of the exchange: reads the answer, and asks for one when there is none.
pub struct DeviceIdSource {
    get: Box<dyn Fn() -> String + Send + Sync>,
    set: Box<dyn Fn(&str) + Send + Sync>,
    asked: Mutex<Option<Instant>>,
}

impl DeviceIdSource {
    /// A source over the property: `get` reads it, `set` sets it.
    pub fn new(
        get: impl Fn() -> String + Send + Sync + 'static,
        set: impl Fn(&str) + Send + Sync + 'static,
    ) -> DeviceIdSource {
        DeviceIdSource {
            get: Box::new(get),
            set: Box::new(set),
            asked: Mutex::new(None),
        }
    }

    /// The ID the helper has found, if it has. When there is none, asks the helper to look
    /// (at most once every [`ASK_AGAIN`]) and says so with `None`.
    pub fn read(&self) -> Option<String> {
        let value = (self.get)();
        let value = value.trim();
        if is_id(value) {
            return Some(value.to_string());
        }
        let mut asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
        if asked.is_none_or(|at| at.elapsed() >= ASK_AGAIN) {
            *asked = Some(Instant::now());
            (self.set)(REQUEST);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn source(value: &str) -> (DeviceIdSource, Arc<Mutex<Vec<String>>>) {
        let property = Arc::new(Mutex::new(value.to_string()));
        let sets = Arc::new(Mutex::new(Vec::new()));
        let (read, written) = (Arc::clone(&property), Arc::clone(&property));
        let log = Arc::clone(&sets);
        let source = DeviceIdSource::new(
            move || read.lock().unwrap().clone(),
            move |v| {
                *written.lock().unwrap() = v.to_string();
                log.lock().unwrap().push(v.to_string());
            },
        );
        (source, sets)
    }

    #[test]
    fn fr32_the_id_the_helper_left_in_the_property_is_the_answer() {
        let (source, sets) = source("4562097385933913176");
        assert_eq!(source.read().as_deref(), Some("4562097385933913176"));
        assert!(sets.lock().unwrap().is_empty(), "nothing is asked for");
    }

    #[test]
    fn fr32_without_an_id_the_helper_is_asked_once_and_the_answer_is_empty() {
        let (source, sets) = source("");
        assert_eq!(source.read(), None);
        assert_eq!(source.read(), None);
        assert_eq!(*sets.lock().unwrap(), vec![REQUEST.to_string()]);
    }

    #[test]
    fn fr32_a_none_from_the_helper_is_not_an_id_and_is_asked_again_later() {
        let (source, sets) = source(NONE);
        assert_eq!(source.read(), None);
        *source.asked.lock().unwrap() = Some(Instant::now() - ASK_AGAIN);
        assert_eq!(source.read(), None);
        assert_eq!(sets.lock().unwrap().len(), 2);
    }

    #[test]
    fn fr32_only_a_decimal_number_is_an_id() {
        assert!(is_id("987"));
        assert!(!is_id(""));
        assert!(!is_id(REQUEST));
        assert!(!is_id("3f2a9c0d11e4b7a8"));
    }
}
