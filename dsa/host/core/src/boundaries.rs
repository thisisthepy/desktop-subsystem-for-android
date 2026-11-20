//! Positions to addresses from administrative boundaries kept on the host.
//!
//! The host answers Android's reverse geocoding (a position to an address) from one file,
//! `address-boundaries.bin`, which `dsa/host/core/geodata/build-geodata.py` makes from pinned
//! open data and the packages install beside the host programs. Nothing is asked of a server,
//! so no key is needed and the position stays on the computer. The file holds:
//!
//! - Korea's administrative dongs (읍면동), each with its province or metropolitan city, its
//!   city, its district and its own name: Statistics Korea's boundaries, as admdongkor keeps
//!   them.
//! - The world's states and provinces with their countries (Natural Earth admin-1), with
//!   their names in Natural Earth's languages.
//! - The world's cities and towns as points (Natural Earth populated places). Where the
//!   boundaries name no city, the nearest one in the same country is the address's city.
//!
//! The answer is at most a neighbourhood: no street, no house number. [`Boundaries::read`]
//! checks every count and index against the file, so a damaged file is refused rather than
//! read past its end. A lookup tests the cells of a grid around the position and then only
//! the regions whose box holds it, and takes milliseconds.
//!
//! [`BoundaryData`] writes the same format, for tests and for anyone who wants a smaller file
//! of their own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::geocode::Address;

/// The file's name, in a `geodata` folder beside the host programs.
pub const FILE_NAME: &str = "address-boundaries.bin";

/// The first bytes of the file: its name and format version.
pub const MAGIC: &[u8; 8] = b"DSAGEO01";

/// Positions are stored in units of 1e-5 degrees, about 1.1 m.
pub const SCALE: f64 = 100_000.0;

/// How far from a position the nearest city may be to be its city, in kilometres.
pub const CITY_RADIUS_KM: f64 = 50.0;

/// How far from the coast a position outside every boundary may be and still be given its
/// nearest city, in kilometres: simplified coasts leave some coastal points outside.
pub const COAST_RADIUS_KM: f64 = 15.0;

/// The most of anything the reader accepts, so a damaged count cannot ask for gigabytes.
const MOST_ITEMS: usize = 20_000_000;

/// The parts of a region's address, in the file's order.
pub const FIELDS: usize = 5;

/// The address parts a region names, by index into [`Region::fields`].
pub const ADMIN_AREA: usize = 0;
pub const SUB_ADMIN_AREA: usize = 1;
pub const LOCALITY: usize = 2;
pub const SUB_LOCALITY: usize = 3;
pub const THOROUGHFARE: usize = 4;

/// One region of a layer.
#[derive(Debug, Clone)]
struct Region {
    country: Option<u32>,
    fields: [Option<u32>; FIELDS],
    /// Ranges of `Boundaries::points`, one per ring.
    rings: Vec<(usize, usize)>,
    /// South, north, west, east, in file units.
    bounds: (i32, i32, i32, i32),
}

/// The regions of one level of detail, and the grid that finds them.
#[derive(Debug, Clone)]
struct Layer {
    regions: Vec<Region>,
    cell: i32,
    grid: HashMap<(i32, i32), Vec<u32>>,
    /// Regions too large for the grid, tested at every lookup.
    wide: Vec<u32>,
}

/// The most grid cells one region is entered in.
const WIDE_CELLS: i64 = 4096;

#[derive(Debug, Clone)]
struct Place {
    country: Option<u32>,
    name: u32,
    lat: i32,
    lon: i32,
}

/// The boundaries of one file, read and indexed.
#[derive(Debug, Clone)]
pub struct Boundaries {
    made_from: String,
    languages: Vec<String>,
    strings: Vec<String>,
    /// Each name: (language index, string index), the language-less one first.
    names: Vec<Vec<(u32, u32)>>,
    countries: Vec<([u8; 2], u32)>,
    points: Vec<(i32, i32)>,
    layers: Vec<Layer>,
    places: Vec<Place>,
    /// Places by the whole degree cell they are in.
    place_grid: HashMap<(i32, i32), Vec<u32>>,
}

/// Reads the file's values, refusing anything past its end.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let byte = *self
            .data
            .get(self.at)
            .ok_or("the boundary file ends too soon")?;
        self.at += 1;
        Ok(byte)
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("a number in the boundary file is too long".into())
    }

    fn count(&mut self) -> Result<usize, String> {
        let count = self.varint()?;
        if count > MOST_ITEMS as u64 {
            return Err(format!("the boundary file counts {count} of something"));
        }
        Ok(count as usize)
    }

    fn index(&mut self, below: usize, what: &str) -> Result<u32, String> {
        let value = self.varint()?;
        if value >= below as u64 {
            return Err(format!("the boundary file names {what} {value} of {below}"));
        }
        Ok(value as u32)
    }

    /// An index plus one, where 0 means none.
    fn optional(&mut self, below: usize, what: &str) -> Result<Option<u32>, String> {
        let value = self.varint()?;
        if value == 0 {
            return Ok(None);
        }
        if value > below as u64 {
            return Err(format!(
                "the boundary file names {what} {} of {below}",
                value - 1
            ));
        }
        Ok(Some(value as u32 - 1))
    }

    fn zigzag(&mut self) -> Result<i64, String> {
        let value = self.varint()?;
        Ok(((value >> 1) as i64) ^ -((value & 1) as i64))
    }

    fn coordinate(&mut self, value: i64) -> Result<i32, String> {
        i32::try_from(value)
            .ok()
            .filter(|v| v.unsigned_abs() <= 180 * SCALE as u32)
            .ok_or_else(|| format!("the boundary file has a position {value} off the Earth"))
    }

    fn text(&mut self) -> Result<String, String> {
        let length = self.count()?;
        let end = self
            .at
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .ok_or("the boundary file ends inside a text")?;
        let text = std::str::from_utf8(&self.data[self.at..end])
            .map_err(|_| "the boundary file has a text that is not UTF-8")?
            .to_string();
        self.at = end;
        Ok(text)
    }
}

/// The grid cell of a position, for cells `cell` units wide.
fn cell_of(lat: i32, lon: i32, cell: i32) -> (i32, i32) {
    (lat.div_euclid(cell), lon.div_euclid(cell))
}

/// Whether (`lat`, `lon`) is inside an odd number of `rings`.
fn inside(points: &[(i32, i32)], rings: &[(usize, usize)], lat: i32, lon: i32) -> bool {
    let (y, x) = (f64::from(lat), f64::from(lon));
    let mut odd = false;
    for &(start, end) in rings {
        let ring = &points[start..end];
        let mut j = ring.len() - 1;
        for i in 0..ring.len() {
            let (yi, xi) = (f64::from(ring[i].0), f64::from(ring[i].1));
            let (yj, xj) = (f64::from(ring[j].0), f64::from(ring[j].1));
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                odd = !odd;
            }
            j = i;
        }
    }
    odd
}

/// The distance between two positions in file units, in kilometres, near enough for picking
/// the nearest city.
fn distance_km(a: (i32, i32), b: (i32, i32)) -> f64 {
    let to_radians = |units: i32| f64::from(units) / SCALE * std::f64::consts::PI / 180.0;
    let mid = (to_radians(a.0) + to_radians(b.0)) / 2.0;
    let dy = to_radians(a.0) - to_radians(b.0);
    let mut dx = (to_radians(a.1) - to_radians(b.1)).abs();
    if dx > std::f64::consts::PI {
        dx = 2.0 * std::f64::consts::PI - dx;
    }
    let dx = dx * mid.cos();
    (dx * dx + dy * dy).sqrt() * 6371.0
}

/// Which of the file's languages answer for a BCP 47 tag, best first. Traditional Chinese is
/// "zht" in the file.
pub fn language_choices(tag: &str) -> Vec<String> {
    let tag = tag.trim().replace('_', "-").to_ascii_lowercase();
    let mut parts = tag.split('-');
    let primary = parts.next().unwrap_or("").to_string();
    let rest: Vec<&str> = parts.collect();
    let mut choices = Vec::new();
    if primary == "zh"
        && rest
            .iter()
            .any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"))
    {
        choices.push("zht".to_string());
    }
    if !primary.is_empty() {
        choices.push(primary);
    }
    choices
}

/// Whether addresses in a language are written from the country down, without commas.
fn largest_first(choices: &[String]) -> bool {
    choices
        .first()
        .is_some_and(|c| matches!(c.as_str(), "ko" | "ja" | "zh" | "zht"))
}

impl Boundaries {
    /// Reads and indexes the file at `path`.
    pub fn read(path: &Path) -> Result<Boundaries, String> {
        let data = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        Boundaries::from_bytes(&data)
    }

    /// Reads and indexes a file's bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Boundaries, String> {
        if !data.starts_with(MAGIC) {
            return Err("this is not a DSA boundary file, or one of another format".into());
        }
        let mut r = Reader {
            data,
            at: MAGIC.len(),
        };
        let made_from = r.text()?;
        let languages = (0..r.count()?)
            .map(|_| r.text())
            .collect::<Result<Vec<_>, _>>()?;
        let strings = (0..r.count()?)
            .map(|_| r.text())
            .collect::<Result<Vec<_>, _>>()?;
        let mut names = Vec::new();
        for _ in 0..r.count()? {
            let mut entries = Vec::new();
            for _ in 0..r.count()? {
                let language = r.index(languages.len(), "language")?;
                let string = r.index(strings.len(), "text")?;
                entries.push((language, string));
            }
            if entries.is_empty() {
                return Err("the boundary file has a name with no text".into());
            }
            names.push(entries);
        }
        let mut countries = Vec::new();
        for _ in 0..r.count()? {
            let code = [r.byte()?, r.byte()?];
            if !code.iter().all(u8::is_ascii_uppercase) {
                return Err("the boundary file has a country code that is not two letters".into());
            }
            countries.push((code, r.index(names.len(), "name")?));
        }
        let mut points = Vec::new();
        let mut layers = Vec::new();
        for _ in 0..r.count()? {
            let mut regions = Vec::new();
            for _ in 0..r.count()? {
                let country = r.optional(countries.len(), "country")?;
                let mut fields = [None; FIELDS];
                for field in &mut fields {
                    *field = r.optional(names.len(), "name")?;
                }
                let mut rings = Vec::new();
                let mut bounds = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
                for _ in 0..r.count()? {
                    let count = r.count()?;
                    if count < 3 {
                        return Err(
                            "the boundary file has a ring of fewer than three points".into()
                        );
                    }
                    if points.len() + count > MOST_ITEMS {
                        return Err("the boundary file has too many points".into());
                    }
                    let start = points.len();
                    let (mut lat, mut lon) = (0i64, 0i64);
                    for _ in 0..count {
                        lat += r.zigzag()?;
                        lon += r.zigzag()?;
                        let point = (r.coordinate(lat)?, r.coordinate(lon)?);
                        bounds.0 = bounds.0.min(point.0);
                        bounds.1 = bounds.1.max(point.0);
                        bounds.2 = bounds.2.min(point.1);
                        bounds.3 = bounds.3.max(point.1);
                        points.push(point);
                    }
                    rings.push((start, points.len()));
                }
                if !rings.is_empty() {
                    regions.push(Region {
                        country,
                        fields,
                        rings,
                        bounds,
                    });
                }
            }
            layers.push(Layer::indexed(regions));
        }
        let mut places = Vec::new();
        for _ in 0..r.count()? {
            let country = r.optional(countries.len(), "country")?;
            let name = r.index(names.len(), "name")?;
            let lat = r.zigzag()?;
            let lon = r.zigzag()?;
            places.push(Place {
                country,
                name,
                lat: r.coordinate(lat)?,
                lon: r.coordinate(lon)?,
            });
        }
        if r.at != data.len() {
            return Err(format!(
                "the boundary file has {} bytes after its places",
                data.len() - r.at
            ));
        }
        let mut place_grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, place) in places.iter().enumerate() {
            place_grid
                .entry(cell_of(place.lat, place.lon, SCALE as i32))
                .or_default()
                .push(i as u32);
        }
        Ok(Boundaries {
            made_from,
            languages,
            strings,
            names,
            countries,
            points,
            layers,
            places,
            place_grid,
        })
    }

    /// What the file was made from, for the log.
    pub fn made_from(&self) -> &str {
        &self.made_from
    }

    /// The name `name` in the first of `choices` it has, else in English, else its own.
    fn name(&self, name: u32, choices: &[String]) -> String {
        let entries = &self.names[name as usize];
        let in_language = |wanted: &str| {
            entries
                .iter()
                .find(|(language, _)| self.languages[*language as usize] == wanted)
                .map(|(_, string)| self.strings[*string as usize].clone())
        };
        choices
            .iter()
            .map(String::as_str)
            .chain(["en"])
            .find_map(in_language)
            .unwrap_or_else(|| self.strings[entries[0].1 as usize].clone())
    }

    /// The most detailed region that holds the position, if any.
    fn region_at(&self, lat: i32, lon: i32) -> Option<&Region> {
        self.layers.iter().find_map(|layer| {
            let cell = layer.grid.get(&cell_of(lat, lon, layer.cell));
            cell.into_iter()
                .flatten()
                .chain(&layer.wide)
                .find_map(|&i| {
                    let region = &layer.regions[i as usize];
                    let (south, north, west, east) = region.bounds;
                    ((south..=north).contains(&lat)
                        && (west..=east).contains(&lon)
                        && inside(&self.points, &region.rings, lat, lon))
                    .then_some(region)
                })
        })
    }

    /// The nearest place to the position within `radius_km`, in `country` when one is given.
    fn place_near(
        &self,
        lat: i32,
        lon: i32,
        country: Option<u32>,
        radius_km: f64,
    ) -> Option<&Place> {
        let (row, column) = cell_of(lat, lon, SCALE as i32);
        let mut best: Option<(f64, &Place)> = None;
        for dr in -1..=1 {
            for dc in -1..=1 {
                // Whole degrees wrap at the antimeridian.
                let mut c = column + dc;
                if c < -180 {
                    c += 360;
                } else if c >= 180 {
                    c -= 360;
                }
                for &i in self.place_grid.get(&(row + dr, c)).into_iter().flatten() {
                    let place = &self.places[i as usize];
                    if country.is_some() && place.country != country {
                        continue;
                    }
                    let distance = distance_km((lat, lon), (place.lat, place.lon));
                    if distance <= radius_km && best.is_none_or(|(d, _)| distance < d) {
                        best = Some((distance, place));
                    }
                }
            }
        }
        best.map(|(_, place)| place)
    }

    /// The address of a position in `language` (a BCP 47 tag; empty for each place's own
    /// name), or None when the position is in no region the file knows and near no city.
    pub fn address(&self, latitude: f64, longitude: f64, language: &str) -> Option<Address> {
        if !(latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude))
        {
            return None;
        }
        let lat = (latitude * SCALE).round() as i32;
        let lon = (longitude * SCALE).round() as i32;
        let choices = language_choices(language);
        let mut parts: [String; FIELDS] = Default::default();
        let country;
        match self.region_at(lat, lon) {
            Some(region) => {
                for (part, field) in parts.iter_mut().zip(region.fields) {
                    if let Some(name) = field {
                        *part = self.name(name, &choices);
                    }
                }
                country = region.country;
                if parts[LOCALITY].is_empty() {
                    if let Some(place) = self.place_near(lat, lon, country, CITY_RADIUS_KM) {
                        parts[LOCALITY] = self.name(place.name, &choices);
                    }
                }
            }
            None => {
                let place = self.place_near(lat, lon, None, COAST_RADIUS_KM)?;
                parts[LOCALITY] = self.name(place.name, &choices);
                country = place.country;
            }
        }
        let (country_code, country_name) = match country {
            Some(i) => {
                let (code, name) = self.countries[i as usize];
                (
                    String::from_utf8_lossy(&code).into_owned(),
                    self.name(name, &choices),
                )
            }
            None => (String::new(), String::new()),
        };

        // From the country down, each part once (a metropolitan city is its own province).
        let mut down: Vec<String> = Vec::new();
        for part in [
            &country_name,
            &parts[ADMIN_AREA],
            &parts[SUB_ADMIN_AREA],
            &parts[LOCALITY],
            &parts[SUB_LOCALITY],
            &parts[THOROUGHFARE],
        ] {
            if !part.is_empty() && down.last() != Some(part) {
                down.push(part.clone());
            }
        }
        let line = if largest_first(&choices) {
            down.join(" ")
        } else {
            down.iter().rev().cloned().collect::<Vec<_>>().join(", ")
        };
        let feature_name = [
            THOROUGHFARE,
            SUB_LOCALITY,
            LOCALITY,
            SUB_ADMIN_AREA,
            ADMIN_AREA,
        ]
        .into_iter()
        .map(|i| parts[i].clone())
        .find(|part| !part.is_empty())
        .unwrap_or_else(|| country_name.clone());
        let [admin_area, sub_admin_area, locality, sub_locality, thoroughfare] = parts;
        Some(
            Address {
                address_lines: vec![line],
                feature_name,
                thoroughfare,
                sub_locality,
                locality,
                sub_admin_area,
                admin_area,
                country_code,
                country_name,
                position: Some((latitude, longitude)),
                ..Default::default()
            }
            .checked(),
        )
    }
}

impl Layer {
    /// The layer with a grid of cells about as large as its typical region, so a lookup tests
    /// few regions.
    fn indexed(regions: Vec<Region>) -> Layer {
        let mut sizes: Vec<i32> = regions
            .iter()
            .map(|r| (r.bounds.1 - r.bounds.0).max(r.bounds.3 - r.bounds.2))
            .collect();
        sizes.sort_unstable();
        let typical = sizes.get(sizes.len() / 2).copied().unwrap_or(SCALE as i32);
        // Between 0.01 and 2 degrees.
        let cell = typical.clamp(1_000, 200_000);
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        let mut wide = Vec::new();
        for (i, region) in regions.iter().enumerate() {
            let (south, north, west, east) = region.bounds;
            let (r0, c0) = cell_of(south, west, cell);
            let (r1, c1) = cell_of(north, east, cell);
            // A region far larger than its layer's cells is tested at every lookup rather
            // than entered in thousands of cells.
            if i64::from(r1 - r0 + 1) * i64::from(c1 - c0 + 1) > WIDE_CELLS {
                wide.push(i as u32);
                continue;
            }
            for row in r0..=r1 {
                for column in c0..=c1 {
                    grid.entry((row, column)).or_default().push(i as u32);
                }
            }
        }
        Layer {
            regions,
            cell,
            grid,
            wide,
        }
    }
}

/// A name and its translations: the language-less text first, then (language, text) pairs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Name {
    pub text: String,
    pub translations: Vec<(String, String)>,
}

impl Name {
    pub fn plain(text: &str) -> Name {
        Name {
            text: text.into(),
            translations: Vec::new(),
        }
    }

    pub fn with(mut self, language: &str, text: &str) -> Name {
        self.translations.push((language.into(), text.into()));
        self
    }
}

/// A region to write: its country (ISO 3166-1 alpha-2), its address parts by [`FIELDS`]
/// order, and its rings of (latitude, longitude) in degrees.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RegionData {
    pub country: Option<String>,
    pub fields: [Option<Name>; FIELDS],
    pub rings: Vec<Vec<(f64, f64)>>,
}

/// A city or town to write.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlaceData {
    pub country: Option<String>,
    pub name: Name,
    pub latitude: f64,
    pub longitude: f64,
}

/// The contents of a boundary file, to write one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoundaryData {
    pub made_from: String,
    /// Each country's code and name.
    pub countries: Vec<(String, Name)>,
    /// The layers, the most detailed first.
    pub layers: Vec<Vec<RegionData>>,
    pub places: Vec<PlaceData>,
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_zigzag(out: &mut Vec<u8>, value: i64) {
    put_varint(out, ((value << 1) ^ (value >> 63)) as u64);
}

fn put_text(out: &mut Vec<u8>, text: &str) {
    put_varint(out, text.len() as u64);
    out.extend_from_slice(text.as_bytes());
}

/// Collects the strings and names of a file being written.
#[derive(Default)]
struct Tables {
    languages: Vec<String>,
    strings: Vec<String>,
    names: Vec<Vec<(u64, u64)>>,
}

impl Tables {
    fn index(list: &mut Vec<String>, text: &str) -> u64 {
        match list.iter().position(|t| t == text) {
            Some(i) => i as u64,
            None => {
                list.push(text.to_string());
                (list.len() - 1) as u64
            }
        }
    }

    fn name(&mut self, name: &Name) -> u64 {
        let mut entries = vec![(
            Tables::index(&mut self.languages, ""),
            Tables::index(&mut self.strings, &name.text),
        )];
        for (language, text) in &name.translations {
            entries.push((
                Tables::index(&mut self.languages, language),
                Tables::index(&mut self.strings, text),
            ));
        }
        self.names.push(entries);
        (self.names.len() - 1) as u64
    }
}

impl BoundaryData {
    /// The file's bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut tables = Tables::default();
        let countries: Vec<(&String, u64)> = self
            .countries
            .iter()
            .map(|(code, name)| (code, tables.name(name)))
            .collect();
        let country = |code: &Option<String>| -> u64 {
            code.as_ref()
                .and_then(|code| countries.iter().position(|(c, _)| *c == code))
                .map_or(0, |i| i as u64 + 1)
        };
        let to_units = |degrees: f64| (degrees * SCALE).round() as i64;
        let mut body = Vec::new();
        put_varint(&mut body, self.layers.len() as u64);
        for regions in &self.layers {
            put_varint(&mut body, regions.len() as u64);
            for region in regions {
                put_varint(&mut body, country(&region.country));
                for field in &region.fields {
                    let value = field.as_ref().map_or(0, |name| tables.name(name) + 1);
                    put_varint(&mut body, value);
                }
                put_varint(&mut body, region.rings.len() as u64);
                for ring in &region.rings {
                    put_varint(&mut body, ring.len() as u64);
                    let mut last = (0i64, 0i64);
                    for &(lat, lon) in ring {
                        let point = (to_units(lat), to_units(lon));
                        put_zigzag(&mut body, point.0 - last.0);
                        put_zigzag(&mut body, point.1 - last.1);
                        last = point;
                    }
                }
            }
        }
        put_varint(&mut body, self.places.len() as u64);
        for place in &self.places {
            put_varint(&mut body, country(&place.country));
            put_varint(&mut body, tables.name(&place.name));
            put_zigzag(&mut body, to_units(place.latitude));
            put_zigzag(&mut body, to_units(place.longitude));
        }

        let mut out = MAGIC.to_vec();
        put_text(&mut out, &self.made_from);
        put_varint(&mut out, tables.languages.len() as u64);
        for language in &tables.languages {
            put_text(&mut out, language);
        }
        put_varint(&mut out, tables.strings.len() as u64);
        for text in &tables.strings {
            put_text(&mut out, text);
        }
        put_varint(&mut out, tables.names.len() as u64);
        for entries in &tables.names {
            put_varint(&mut out, entries.len() as u64);
            for &(language, string) in entries {
                put_varint(&mut out, language);
                put_varint(&mut out, string);
            }
        }
        put_varint(&mut out, countries.len() as u64);
        for (code, name) in &countries {
            let mut bytes = code.to_ascii_uppercase().into_bytes();
            bytes.resize(2, b'X');
            out.extend_from_slice(&bytes[..2]);
            put_varint(&mut out, *name);
        }
        out.extend_from_slice(&body);
        out
    }
}

/// Where the boundary file is for a host program at `exe`: in a `geodata` folder beside it
/// (Windows), beside its folder (Linux, /usr/lib/dsa/geodata), or in the bundle's Resources
/// (macOS), also from a helper bundle in the app's `Contents/Helpers`, where the macOS device
/// lender runs. The first that exists, else the first place looked.
pub fn installed_file(exe: &Path) -> PathBuf {
    let folder = exe.parent().unwrap_or(Path::new("."));
    let candidates = [
        folder.join("geodata").join(FILE_NAME),
        folder.join("..").join("geodata").join(FILE_NAME),
        folder
            .join("..")
            .join("Resources")
            .join("geodata")
            .join(FILE_NAME),
        folder
            .join("../../../..")
            .join("Resources")
            .join("geodata")
            .join(FILE_NAME),
    ];
    candidates
        .iter()
        .find(|path| path.is_file())
        .unwrap_or(&candidates[0])
        .clone()
}
