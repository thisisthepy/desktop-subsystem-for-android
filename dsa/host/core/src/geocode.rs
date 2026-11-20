//! Address lookups (geocoding) for the apps of the user's Android, answered by the host.
//!
//! An app calls Android's `Geocoder`. Without Google Play the agent is the guest's geocode
//! provider, and sends each lookup to the host as a `GeocodeRequest`: a place's name to
//! positions (forward), or a position to addresses (reverse). The session checks it
//! ([`GeocodeQuery::from_control`]) and hands it to one watcher in the user's session
//! ([`serve`]), which looks it up through a [`Geocoder`] and reports what it found. The answer
//! goes back to the guest as a `GeocodeResult`, which the agent turns into Android
//! `Address`es, or into the error Android's geocoder reports. The guest holds no geocoding
//! engine or data. With Google Play, Google's own geocoder answers and the agent is not asked.
//!
//! Every host answers from the administrative boundaries it keeps ([`BoundaryGeocoder`], over
//! [`crate::boundaries`]): a position becomes its country, province, city, district and
//! neighbourhood. No server is asked, so no key is needed and the position stays on the
//! computer. Windows' own geocoder needs a paid Bing Maps key and goes with Bing Maps for
//! Enterprise, and a public geocoding server is not meant to be every installation's default.
//! A forward lookup (an address to positions) finds nothing: the boundaries hold no streets
//! or house numbers to place an address on.
//!
//! Nothing a guest or a geocoder does ends the session or the watcher: a request that cannot
//! be answered is answered with an error.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dsa_protocol::control;
use dsa_protocol::host_api::types;
use dsa_transport::Connector;

use crate::boundaries::Boundaries;
use crate::host_api::HostApiClient;

/// The most addresses one lookup returns, whatever the app asks for.
pub const MAX_RESULTS: u32 = 10;

/// The longest place name the host looks up, in characters.
pub const MAX_QUERY_CHARS: usize = 512;

/// The longest part of an address the host passes on, in characters.
pub const MAX_PART_CHARS: usize = 256;

/// The most lines an address keeps.
pub const MAX_ADDRESS_LINES: usize = 8;

/// How long the boundaries stay in memory after the last lookup. They take some megabytes,
/// and lookups come in bursts, while an app shows where the user is.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(600);

/// A box on the map, in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub lower_left_latitude: f64,
    pub lower_left_longitude: f64,
    pub upper_right_latitude: f64,
    pub upper_right_longitude: f64,
}

/// What a lookup asks.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// A place's name or address to positions, preferring those inside `bounds`.
    Forward {
        query: String,
        bounds: Option<Bounds>,
    },
    /// A position to addresses.
    Reverse { latitude: f64, longitude: f64 },
}

/// One checked lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct GeocodeQuery {
    /// The number the answer is reported under.
    pub query_id: u32,
    pub lookup: Lookup,
    /// From 1 to [`MAX_RESULTS`].
    pub max_results: u32,
    /// A BCP 47 tag, or empty for the host's own language.
    pub language: String,
}

/// Why a lookup has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeocodeErrorKind {
    /// The host cannot answer now: its address data is missing or damaged.
    Unavailable,
    /// The user turned geocoding off on the host, or nobody on the host can answer.
    Refused,
    /// Too many lookups in a short time.
    Busy,
    /// The lookup cannot be answered as it is.
    Invalid,
}

/// A lookup that has no answer, and why, in words for the guest's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeocodeError {
    pub kind: GeocodeErrorKind,
    pub message: String,
}

impl GeocodeError {
    pub fn new(kind: GeocodeErrorKind, message: impl Into<String>) -> GeocodeError {
        GeocodeError {
            kind,
            message: message.into(),
        }
    }
}

/// What a lookup found: the addresses, best first (none when the place is not known), or why
/// there is no answer.
pub type GeocodeOutcome = Result<Vec<Address>, GeocodeError>;

/// One address a lookup found. Empty strings are parts the host does not know.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Address {
    pub address_lines: Vec<String>,
    pub feature_name: String,
    pub premises: String,
    pub sub_thoroughfare: String,
    pub thoroughfare: String,
    pub sub_locality: String,
    pub locality: String,
    pub sub_admin_area: String,
    pub admin_area: String,
    pub postal_code: String,
    /// ISO 3166-1 alpha-2, upper case.
    pub country_code: String,
    pub country_name: String,
    /// Latitude and longitude, in degrees.
    pub position: Option<(f64, f64)>,
}

fn on_earth(latitude: f64, longitude: f64) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}

/// `text` without control characters, at most `limit` characters long.
fn clean(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A language tag that is only letters, digits and hyphens, or nothing.
fn clean_language(tag: &str) -> String {
    let tag = tag.trim().replace('_', "-");
    if tag.len() <= 35 && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        tag
    } else {
        String::new()
    }
}

impl GeocodeQuery {
    #[allow(clippy::too_many_arguments)]
    fn checked(
        query_id: u32,
        reverse: bool,
        query: &str,
        latitude: f64,
        longitude: f64,
        bounds: Option<Bounds>,
        max_results: u32,
        language: &str,
    ) -> Result<GeocodeQuery, GeocodeError> {
        let lookup = if reverse {
            if !on_earth(latitude, longitude) {
                return Err(GeocodeError::new(
                    GeocodeErrorKind::Invalid,
                    format!("({latitude}, {longitude}) is not a position on Earth"),
                ));
            }
            Lookup::Reverse {
                latitude,
                longitude,
            }
        } else {
            let query = clean(query, MAX_QUERY_CHARS);
            if query.is_empty() {
                return Err(GeocodeError::new(
                    GeocodeErrorKind::Invalid,
                    "the place to look up has no name",
                ));
            }
            let bounds = bounds.filter(|b| {
                on_earth(b.lower_left_latitude, b.lower_left_longitude)
                    && on_earth(b.upper_right_latitude, b.upper_right_longitude)
            });
            Lookup::Forward { query, bounds }
        };
        Ok(GeocodeQuery {
            query_id,
            lookup,
            max_results: max_results.clamp(1, MAX_RESULTS),
            language: clean_language(language),
        })
    }

    /// The lookup an app asked for, checked, under the guest's number for it.
    pub fn from_control(request: &control::GeocodeRequest) -> Result<GeocodeQuery, GeocodeError> {
        let bounds = request.has_bounds.then_some(Bounds {
            lower_left_latitude: request.lower_left_latitude,
            lower_left_longitude: request.lower_left_longitude,
            upper_right_latitude: request.upper_right_latitude,
            upper_right_longitude: request.upper_right_longitude,
        });
        GeocodeQuery::checked(
            request.request_id,
            request.reverse,
            &request.query,
            request.latitude_degrees,
            request.longitude_degrees,
            bounds,
            request.max_results,
            &request.language,
        )
    }

    /// The lookup on the local API.
    pub fn to_host_api(&self) -> types::GeocodeQuery {
        let mut wire = types::GeocodeQuery {
            query_id: self.query_id,
            max_results: self.max_results,
            language: self.language.clone(),
            ..Default::default()
        };
        match &self.lookup {
            Lookup::Forward { query, bounds } => {
                wire.query = query.clone();
                if let Some(b) = bounds {
                    wire.has_bounds = true;
                    wire.lower_left_latitude = b.lower_left_latitude;
                    wire.lower_left_longitude = b.lower_left_longitude;
                    wire.upper_right_latitude = b.upper_right_latitude;
                    wire.upper_right_longitude = b.upper_right_longitude;
                }
            }
            Lookup::Reverse {
                latitude,
                longitude,
            } => {
                wire.reverse = true;
                wire.latitude_degrees = *latitude;
                wire.longitude_degrees = *longitude;
            }
        }
        wire
    }

    /// The lookup from the local API, checked again: the client trusts the service no more
    /// than the service trusts the guest.
    pub fn from_host_api(query: &types::GeocodeQuery) -> Result<GeocodeQuery, String> {
        let bounds = query.has_bounds.then_some(Bounds {
            lower_left_latitude: query.lower_left_latitude,
            lower_left_longitude: query.lower_left_longitude,
            upper_right_latitude: query.upper_right_latitude,
            upper_right_longitude: query.upper_right_longitude,
        });
        GeocodeQuery::checked(
            query.query_id,
            query.reverse,
            &query.query,
            query.latitude_degrees,
            query.longitude_degrees,
            bounds,
            query.max_results,
            &query.language,
        )
        .map_err(|e| e.message)
    }
}

impl Address {
    /// The address with every part cleaned and cut to size, and a position that is not on
    /// Earth left out.
    pub fn checked(self) -> Address {
        let part = |text: String| clean(&text, MAX_PART_CHARS);
        Address {
            address_lines: self
                .address_lines
                .into_iter()
                .map(part)
                .filter(|line| !line.is_empty())
                .take(MAX_ADDRESS_LINES)
                .collect(),
            feature_name: part(self.feature_name),
            premises: part(self.premises),
            sub_thoroughfare: part(self.sub_thoroughfare),
            thoroughfare: part(self.thoroughfare),
            sub_locality: part(self.sub_locality),
            locality: part(self.locality),
            sub_admin_area: part(self.sub_admin_area),
            admin_area: part(self.admin_area),
            postal_code: part(self.postal_code),
            country_code: {
                let code = part(self.country_code).to_ascii_uppercase();
                if code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic()) {
                    code
                } else {
                    String::new()
                }
            },
            country_name: part(self.country_name),
            position: self.position.filter(|(lat, lon)| on_earth(*lat, *lon)),
        }
    }

    pub fn to_control(&self) -> control::GeocodeAddress {
        let (has_position, (latitude, longitude)) = match self.position {
            Some(position) => (true, position),
            None => (false, (0.0, 0.0)),
        };
        control::GeocodeAddress {
            address_lines: self.address_lines.clone(),
            feature_name: self.feature_name.clone(),
            premises: self.premises.clone(),
            sub_thoroughfare: self.sub_thoroughfare.clone(),
            thoroughfare: self.thoroughfare.clone(),
            sub_locality: self.sub_locality.clone(),
            locality: self.locality.clone(),
            sub_admin_area: self.sub_admin_area.clone(),
            admin_area: self.admin_area.clone(),
            postal_code: self.postal_code.clone(),
            country_code: self.country_code.clone(),
            country_name: self.country_name.clone(),
            has_position,
            latitude_degrees: latitude,
            longitude_degrees: longitude,
        }
    }

    pub fn to_host_api(&self) -> types::GeocodeAddress {
        let wire = self.to_control();
        types::GeocodeAddress {
            address_lines: wire.address_lines,
            feature_name: wire.feature_name,
            premises: wire.premises,
            sub_thoroughfare: wire.sub_thoroughfare,
            thoroughfare: wire.thoroughfare,
            sub_locality: wire.sub_locality,
            locality: wire.locality,
            sub_admin_area: wire.sub_admin_area,
            admin_area: wire.admin_area,
            postal_code: wire.postal_code,
            country_code: wire.country_code,
            country_name: wire.country_name,
            has_position: wire.has_position,
            latitude_degrees: wire.latitude_degrees,
            longitude_degrees: wire.longitude_degrees,
        }
    }

    pub fn from_host_api(wire: types::GeocodeAddress) -> Address {
        Address {
            address_lines: wire.address_lines,
            feature_name: wire.feature_name,
            premises: wire.premises,
            sub_thoroughfare: wire.sub_thoroughfare,
            thoroughfare: wire.thoroughfare,
            sub_locality: wire.sub_locality,
            locality: wire.locality,
            sub_admin_area: wire.sub_admin_area,
            admin_area: wire.admin_area,
            postal_code: wire.postal_code,
            country_code: wire.country_code,
            country_name: wire.country_name,
            position: wire
                .has_position
                .then_some((wire.latitude_degrees, wire.longitude_degrees)),
        }
        .checked()
    }
}

impl GeocodeErrorKind {
    fn to_control(self) -> control::GeocodeStatus {
        match self {
            GeocodeErrorKind::Unavailable => control::GeocodeStatus::Unavailable,
            GeocodeErrorKind::Refused => control::GeocodeStatus::Refused,
            GeocodeErrorKind::Busy => control::GeocodeStatus::Busy,
            GeocodeErrorKind::Invalid => control::GeocodeStatus::Invalid,
        }
    }

    fn to_host_api(self) -> types::GeocodeStatus {
        match self {
            GeocodeErrorKind::Unavailable => types::GeocodeStatus::Unavailable,
            GeocodeErrorKind::Refused => types::GeocodeStatus::Refused,
            GeocodeErrorKind::Busy => types::GeocodeStatus::Busy,
            GeocodeErrorKind::Invalid => types::GeocodeStatus::Invalid,
        }
    }
}

/// The answer to the guest's request `request_id`, at most `max_results` addresses long.
pub fn result_to_control(request_id: u32, outcome: &GeocodeOutcome) -> control::GeocodeResult {
    match outcome {
        Ok(addresses) => control::GeocodeResult {
            request_id,
            status: control::GeocodeStatus::Ok as i32,
            addresses: addresses
                .iter()
                .take(MAX_RESULTS as usize)
                .map(Address::to_control)
                .collect(),
            message: String::new(),
        },
        Err(error) => control::GeocodeResult {
            request_id,
            status: error.kind.to_control() as i32,
            addresses: Vec::new(),
            message: clean(&error.message, 512),
        },
    }
}

/// The report of query `query_id` on the local API.
pub fn report_to_host_api(query_id: u32, outcome: &GeocodeOutcome) -> types::ReportGeocode {
    match outcome {
        Ok(addresses) => types::ReportGeocode {
            query_id,
            status: types::GeocodeStatus::Ok as i32,
            addresses: addresses
                .iter()
                .take(MAX_RESULTS as usize)
                .map(Address::to_host_api)
                .collect(),
            message: String::new(),
        },
        Err(error) => types::ReportGeocode {
            query_id,
            status: error.kind.to_host_api() as i32,
            addresses: Vec::new(),
            message: clean(&error.message, 512),
        },
    }
}

/// What a client reported, its addresses checked.
pub fn outcome_from_host_api(report: types::ReportGeocode) -> Result<GeocodeOutcome, String> {
    let kind = match types::GeocodeStatus::try_from(report.status) {
        Ok(types::GeocodeStatus::Ok) => {
            return Ok(Ok(report
                .addresses
                .into_iter()
                .take(MAX_RESULTS as usize)
                .map(Address::from_host_api)
                .collect()))
        }
        Ok(types::GeocodeStatus::Unavailable) => GeocodeErrorKind::Unavailable,
        Ok(types::GeocodeStatus::Refused) => GeocodeErrorKind::Refused,
        Ok(types::GeocodeStatus::Busy) => GeocodeErrorKind::Busy,
        Ok(types::GeocodeStatus::Invalid) => GeocodeErrorKind::Invalid,
        _ => return Err(format!("a lookup that ended as {}", report.status)),
    };
    Ok(Err(GeocodeError::new(kind, report.message)))
}

/// What answers the lookups on the host.
pub trait Geocoder {
    /// Looks `query` up.
    fn geocode(&mut self, query: &GeocodeQuery) -> GeocodeOutcome;

    /// Called while no lookup comes, so the geocoder can let go of what it holds.
    fn idle(&mut self) {}
}

/// The host's geocoder: positions to addresses from the boundary file at its path, which it
/// reads at the first lookup and lets go of after [`IDLE_UNLOAD`] without one.
pub struct BoundaryGeocoder {
    path: PathBuf,
    loaded: Option<(Boundaries, Instant)>,
    unload_after: Duration,
}

impl BoundaryGeocoder {
    pub fn new(path: impl Into<PathBuf>) -> BoundaryGeocoder {
        BoundaryGeocoder {
            path: path.into(),
            loaded: None,
            unload_after: IDLE_UNLOAD,
        }
    }

    /// The geocoder of the host program at `exe`, with the file installed beside it
    /// ([`crate::boundaries::installed_file`]).
    pub fn installed(exe: &Path) -> BoundaryGeocoder {
        BoundaryGeocoder::new(crate::boundaries::installed_file(exe))
    }

    /// The geocoder of the running program.
    pub fn for_this_program() -> BoundaryGeocoder {
        match std::env::current_exe() {
            Ok(exe) => BoundaryGeocoder::installed(&exe),
            Err(_) => BoundaryGeocoder::new(crate::boundaries::FILE_NAME),
        }
    }

    /// Lets go of the boundaries `time` after the last lookup instead.
    pub fn unloading_after(mut self, time: Duration) -> BoundaryGeocoder {
        self.unload_after = time;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the boundaries are in memory now.
    pub fn is_loaded(&self) -> bool {
        self.loaded.is_some()
    }

    fn boundaries(&mut self) -> Result<&Boundaries, GeocodeError> {
        if self.loaded.is_none() {
            let boundaries = Boundaries::read(&self.path).map_err(|why| {
                GeocodeError::new(
                    GeocodeErrorKind::Unavailable,
                    format!("the host's address data cannot be read: {why}"),
                )
            })?;
            self.loaded = Some((boundaries, Instant::now()));
        }
        let (boundaries, used) = self.loaded.as_mut().expect("read above");
        *used = Instant::now();
        Ok(boundaries)
    }
}

impl Geocoder for BoundaryGeocoder {
    fn geocode(&mut self, query: &GeocodeQuery) -> GeocodeOutcome {
        match query.lookup {
            // The boundaries hold no streets or house numbers to place an address on.
            Lookup::Forward { .. } => Ok(Vec::new()),
            Lookup::Reverse {
                latitude,
                longitude,
            } => {
                let language = query.language.clone();
                let boundaries = self.boundaries()?;
                Ok(boundaries
                    .address(latitude, longitude, &language)
                    .into_iter()
                    .collect())
            }
        }
    }

    fn idle(&mut self) {
        let unload_after = self.unload_after;
        if self
            .loaded
            .as_ref()
            .is_some_and(|(_, used)| used.elapsed() >= unload_after)
        {
            self.loaded = None;
        }
    }
}

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);
/// How long the watch waits for a query before it looks at `stop` again.
const POLL: Duration = Duration::from_millis(200);
/// How long the watcher waits before it connects again.
const RETRY: Duration = Duration::from_secs(2);

/// What the device lenders of the Linux and macOS hosts call their address follower to the
/// service.
const DEVICES_BUILD: &str = "dsa-devices geocode";

/// The address follower of a device lender: [`serve_boundaries`] under the name the lenders
/// give it, for the thread the lender starts it on.
pub fn serve_follower(
    mut connector: Box<dyn Connector>,
    stop: std::sync::Arc<AtomicBool>,
    log: crate::follower::Log,
) {
    serve_boundaries(&mut *connector, DEVICES_BUILD, &stop, &|line| log(line));
}

/// Answers the lookups of the caller's Android from the administrative boundaries installed
/// beside this program ([`BoundaryGeocoder::for_this_program`]) until `stop` is set, saying
/// where they are read from first. Nothing is sent over the network, so the position stays on
/// the computer.
pub fn serve_boundaries(
    connector: &mut dyn Connector,
    build: &str,
    stop: &AtomicBool,
    log: &dyn Fn(&str),
) {
    let mut geocoder = BoundaryGeocoder::for_this_program();
    log(&format!(
        "geocoding: answering from {}",
        geocoder.path().display()
    ));
    serve(connector, build, &mut geocoder, stop, log);
}

/// Answers the lookups of the caller's Android with `geocoder` until `stop` is set, one at a
/// time, connecting to the service again whenever the connection ends.
pub fn serve(
    connector: &mut dyn Connector,
    build: &str,
    geocoder: &mut dyn Geocoder,
    stop: &AtomicBool,
    log: &dyn Fn(&str),
) {
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(connector, build, ANSWER)
            .and_then(|client| client.watch_geocoding());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(why) => {
                if !warned {
                    log(&format!(
                        "geocoding: cannot answer Android's lookups yet: {why}"
                    ));
                    warned = true;
                }
                wait(stop, RETRY);
                continue;
            }
        };
        warned = false;
        while !stop.load(Ordering::Relaxed) {
            let query = match stream.next(POLL) {
                Ok(None) => {
                    geocoder.idle();
                    continue;
                }
                Ok(Some(query)) => query,
                Err(why) => {
                    log(&format!("geocoding: the service connection ended: {why}"));
                    break;
                }
            };
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| geocoder.geocode(&query)))
                    .unwrap_or_else(|_| {
                        Err(GeocodeError::new(
                            GeocodeErrorKind::Unavailable,
                            "the geocoding code failed",
                        ))
                    });
            if let Err(error) = &outcome {
                log(&format!("geocoding: a lookup failed: {}", error.message));
            }
            let reported = HostApiClient::connect(connector, build, ANSWER)
                .and_then(|mut client| client.report_geocode(query.query_id, &outcome));
            if let Err(why) = reported {
                log(&format!(
                    "geocoding: the answer did not reach Android: {why}"
                ));
            }
        }
        wait(stop, RETRY);
    }
}

/// Sleeps `time`, or less once `stop` is set.
fn wait(stop: &AtomicBool, time: Duration) {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < time && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(step);
        waited += step;
    }
}
