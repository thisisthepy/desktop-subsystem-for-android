//! Address lookups for the apps in the user's Android.
//!
//! The window manager answers them from the administrative boundaries installed beside it
//! (`geodata\address-boundaries.bin` under the install folder, read by
//! [`dsa_host_core::geocode::BoundaryGeocoder`]). Windows' own geocoder
//! (`Windows.Services.Maps.MapLocationFinder`) needs a Bing Maps key and goes with Bing Maps
//! for Enterprise, which Microsoft is retiring. Nothing is sent over the network, so the
//! position stays on the PC.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use dsa_host_core::geocode;
use dsa_transport::Connector;

use crate::compositor::log::{note, CompositorLogger};

/// What the window manager calls itself to the service.
const BUILD: &str = "dsa-wm geocode";

/// Answers the address lookups of the user's Android until `stop`.
pub fn serve(
    mut connector: Box<dyn Connector>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let log = |line: &str| note(logger.as_deref(), line);
    geocode::serve_boundaries(&mut *connector, BUILD, &stop, &log);
}
