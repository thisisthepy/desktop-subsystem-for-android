//! Address lookups through the host: the checks on a guest's lookup, the boundary file and
//! the addresses it gives, the geocoder over it, and the wire forms of what it finds.
//!
//! tests/data/address-boundaries-fixture.bin is written by dsa/host/core/geodata/
//! test_build_geodata.py, so these tests also hold the reader to the format the build script
//! writes: two dongs (Seoul's Myeong-dong and Suwon's Jowon 2-dong), the province around them,
//! Tokyo's prefecture with a hole in its middle, and the cities of Seoul, Tokyo and Hachioji.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dsa_host_core::boundaries::{
    self, language_choices, Boundaries, BoundaryData, Name, PlaceData, RegionData,
};
use dsa_host_core::geocode::{
    self, Address, BoundaryGeocoder, Bounds, GeocodeErrorKind, GeocodeQuery, Geocoder, Lookup,
};
use dsa_protocol::control;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("address-boundaries-fixture.bin")
}

fn fixture() -> Boundaries {
    Boundaries::read(&fixture_path()).unwrap()
}

/// A folder of its own for one test, empty.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsa-geocode-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn reverse(latitude: f64, longitude: f64, language: &str) -> GeocodeQuery {
    GeocodeQuery::from_control(&control::GeocodeRequest {
        request_id: 2,
        reverse: true,
        latitude_degrees: latitude,
        longitude_degrees: longitude,
        max_results: 1,
        language: language.into(),
        ..Default::default()
    })
    .unwrap()
}

fn forward(query: &str) -> GeocodeQuery {
    GeocodeQuery::from_control(&control::GeocodeRequest {
        request_id: 1,
        query: query.into(),
        max_results: 5,
        language: "ko_KR".into(),
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn fr36_a_korean_position_reads_as_its_city_district_and_dong() {
    let boundaries = fixture();
    assert_eq!(boundaries.made_from(), "fixture");
    let address = boundaries.address(37.5663, 126.9779, "ko-KR").unwrap();
    assert_eq!(
        address,
        Address {
            address_lines: vec!["대한민국 서울특별시 중구 명동".into()],
            feature_name: "명동".into(),
            thoroughfare: "명동".into(),
            sub_locality: "중구".into(),
            locality: "서울특별시".into(),
            admin_area: "서울특별시".into(),
            country_code: "KR".into(),
            country_name: "대한민국".into(),
            position: Some((37.5663, 126.9779)),
            ..Default::default()
        }
    );

    // A city with districts in a province.
    let suwon = boundaries.address(37.3036, 127.0102, "ko").unwrap();
    assert_eq!(
        (
            suwon.admin_area.as_str(),
            suwon.locality.as_str(),
            suwon.sub_locality.as_str(),
            suwon.thoroughfare.as_str()
        ),
        ("경기도", "수원시", "장안구", "조원2동")
    );
    assert_eq!(
        suwon.address_lines,
        vec!["대한민국 경기도 수원시 장안구 조원2동"]
    );

    // In another language the country follows it, the Korean names stay, and the line runs
    // from the dong up.
    let english = boundaries.address(37.5663, 126.9779, "en-US").unwrap();
    assert_eq!(english.country_name, "South Korea");
    assert_eq!(english.sub_locality, "중구");
    assert_eq!(
        english.address_lines,
        vec!["명동, 중구, 서울특별시, South Korea"]
    );
}

#[test]
fn fr36_outside_the_detailed_boundaries_the_province_and_the_nearest_city_answer() {
    let boundaries = fixture();
    // In Seoul's province, between the dongs: the nearest city in the same country.
    let seoul = boundaries.address(37.45, 126.80, "ko").unwrap();
    assert_eq!(seoul.admin_area, "서울특별시");
    assert_eq!(seoul.locality, "서울");
    assert_eq!(seoul.thoroughfare, "");
    assert_eq!(seoul.country_code, "KR");

    // Tokyo, in Japanese, and the line from the country down.
    let tokyo = boundaries.address(35.9, 139.9, "ja-JP").unwrap();
    assert_eq!(tokyo.admin_area, "東京都");
    assert_eq!(tokyo.locality, "東京");
    assert_eq!(tokyo.country_name, "日本");
    assert_eq!(tokyo.address_lines, vec!["日本 東京都 東京"]);
    assert_eq!(tokyo.feature_name, "東京");

    // A language the file does not have falls back to English, then to the place's own name.
    let german = boundaries.address(35.9, 139.9, "de").unwrap();
    assert_eq!(german.admin_area, "Tokyo");
    // The prefecture and the city have the same name, which the line gives once.
    assert_eq!(german.address_lines, vec!["Tokyo, Japan"]);
}

#[test]
fn fr36_a_hole_in_a_region_is_not_part_of_it_and_the_sea_has_no_address() {
    let boundaries = fixture();
    // The hole in Tokyo's prefecture: in no region, but within reach of Tokyo the city, as a
    // coastal point cut off by a simplified coast is.
    let hole = boundaries.address(35.68, 139.76, "ja").unwrap();
    assert_eq!(hole.admin_area, "");
    assert_eq!(hole.locality, "東京");
    assert_eq!(hole.country_code, "JP");
    // The open sea.
    assert_eq!(boundaries.address(0.0, 0.0, "en"), None);
    assert_eq!(boundaries.address(f64::NAN, 0.0, "en"), None);
    assert_eq!(boundaries.address(91.0, 0.0, "en"), None);
}

#[test]
fn fr36_languages_are_matched_by_their_primary_tag_and_traditional_chinese_apart() {
    assert_eq!(language_choices("ko-KR"), vec!["ko"]);
    assert_eq!(language_choices("zh_TW"), vec!["zht", "zh"]);
    assert_eq!(language_choices("zh-Hant-HK"), vec!["zht", "zh"]);
    assert_eq!(language_choices("zh-CN"), vec!["zh"]);
    assert!(language_choices("").is_empty());
}

#[test]
fn fr36_what_the_writer_writes_the_reader_reads() {
    let data = BoundaryData {
        made_from: "test".into(),
        countries: vec![("FR".into(), Name::plain("France").with("ko", "프랑스"))],
        layers: vec![vec![RegionData {
            country: Some("FR".into()),
            fields: [
                Some(Name::plain("Île-de-France").with("ko", "일드프랑스")),
                None,
                None,
                None,
                None,
            ],
            rings: vec![vec![(48.0, 1.5), (48.0, 3.5), (49.2, 3.5), (49.2, 1.5)]],
        }]],
        places: vec![PlaceData {
            country: Some("FR".into()),
            name: Name::plain("Paris").with("ko", "파리"),
            latitude: 48.8566,
            longitude: 2.3522,
        }],
    };
    let boundaries = Boundaries::from_bytes(&data.to_bytes()).unwrap();
    let paris = boundaries.address(48.86, 2.35, "ko").unwrap();
    assert_eq!(paris.admin_area, "일드프랑스");
    assert_eq!(paris.locality, "파리");
    assert_eq!(paris.country_name, "프랑스");
    assert_eq!(paris.address_lines, vec!["프랑스 일드프랑스 파리"]);
}

#[test]
fn fr36_a_damaged_boundary_file_is_refused_not_read_past_its_end() {
    let good = std::fs::read(fixture_path()).unwrap();
    for cut in [0, 4, 8, 9, 40, good.len() / 2, good.len() - 1] {
        assert!(
            Boundaries::from_bytes(&good[..cut]).is_err(),
            "{cut} bytes were read as a whole file"
        );
    }
    let mut longer = good.clone();
    longer.push(0);
    assert!(Boundaries::from_bytes(&longer).is_err());
    // A count far beyond the file.
    let mut huge = boundaries::MAGIC.to_vec();
    huge.extend_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
    assert!(Boundaries::from_bytes(&huge).is_err());
    // Every single flipped byte is either read or refused, never a panic.
    for i in 8..good.len() {
        let mut bad = good.clone();
        bad[i] ^= 0xA5;
        let _ = Boundaries::from_bytes(&bad);
    }
}

#[test]
fn fr36_the_geocoder_answers_a_reverse_lookup_and_finds_nothing_forward() {
    let mut geocoder = BoundaryGeocoder::new(fixture_path());
    // A forward lookup does not even read the file.
    assert_eq!(geocoder.geocode(&forward("서울시청")), Ok(Vec::new()));
    assert!(!geocoder.is_loaded());

    let found = geocoder
        .geocode(&reverse(37.5663, 126.9779, "ko-KR"))
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].sub_locality, "중구");
    assert!(geocoder.is_loaded());
    // The sea: a lookup that ran and found nothing.
    assert_eq!(geocoder.geocode(&reverse(0.0, 0.0, "en")), Ok(Vec::new()));
}

#[test]
fn fr36_the_boundaries_are_let_go_of_while_no_lookup_comes() {
    let mut geocoder = BoundaryGeocoder::new(fixture_path()).unloading_after(Duration::ZERO);
    geocoder.geocode(&reverse(37.5663, 126.9779, "ko")).unwrap();
    assert!(geocoder.is_loaded());
    geocoder.idle();
    assert!(!geocoder.is_loaded());
    // And read again at the next lookup.
    assert_eq!(
        geocoder.geocode(&reverse(37.5663, 126.9779, "ko")).unwrap()[0].thoroughfare,
        "명동"
    );

    let mut kept = BoundaryGeocoder::new(fixture_path());
    kept.geocode(&reverse(37.5663, 126.9779, "ko")).unwrap();
    kept.idle();
    assert!(
        kept.is_loaded(),
        "{:?} has not passed",
        geocode::IDLE_UNLOAD
    );
}

#[test]
fn fr36_a_missing_boundary_file_makes_lookups_unavailable() {
    let dir = scratch("missing");
    let mut geocoder = BoundaryGeocoder::new(dir.join(boundaries::FILE_NAME));
    let error = geocoder
        .geocode(&reverse(37.5663, 126.9779, "ko"))
        .unwrap_err();
    assert_eq!(error.kind, GeocodeErrorKind::Unavailable);
    assert!(error.message.contains("address data"), "{}", error.message);

    std::fs::write(dir.join(boundaries::FILE_NAME), b"not boundaries").unwrap();
    let error = geocoder
        .geocode(&reverse(37.5663, 126.9779, "ko"))
        .unwrap_err();
    assert_eq!(error.kind, GeocodeErrorKind::Unavailable);
}

#[test]
fn fr36_the_installed_file_is_found_beside_the_host_programs_on_every_host() {
    let file = boundaries::FILE_NAME;
    // Windows: <install folder>\dsa-wm.exe and <install folder>\geodata\.
    let windows = scratch("installed-windows");
    std::fs::create_dir_all(windows.join("geodata")).unwrap();
    std::fs::write(windows.join("geodata").join(file), b"x").unwrap();
    assert_eq!(
        boundaries::installed_file(&windows.join("dsa-wm.exe")),
        windows.join("geodata").join(file)
    );
    // Linux: /usr/lib/dsa/bin/dsa-devices and /usr/lib/dsa/geodata/.
    let linux = scratch("installed-linux");
    std::fs::create_dir_all(linux.join("bin")).unwrap();
    std::fs::create_dir_all(linux.join("geodata")).unwrap();
    std::fs::write(linux.join("geodata").join(file), b"x").unwrap();
    assert!(boundaries::installed_file(&linux.join("bin").join("dsa-devices")).is_file());
    // macOS: DSA.app/Contents/MacOS/dsa-devices and DSA.app/Contents/Resources/geodata/.
    let mac = scratch("installed-macos");
    std::fs::create_dir_all(mac.join("MacOS")).unwrap();
    std::fs::create_dir_all(mac.join("Resources").join("geodata")).unwrap();
    std::fs::write(mac.join("Resources").join("geodata").join(file), b"x").unwrap();
    assert!(boundaries::installed_file(&mac.join("MacOS").join("dsa-devices")).is_file());
    // macOS, the device lender in its helper bundle:
    // DSA.app/Contents/Helpers/DSA Devices.app/Contents/MacOS/dsa-devices.
    let helper = mac.join("Helpers/DSA Devices.app/Contents/MacOS");
    std::fs::create_dir_all(&helper).unwrap();
    assert!(boundaries::installed_file(&helper.join("dsa-devices")).is_file());
}

/// A file about the size of the shipped one's Korean layer: a grid of small dongs over a
/// province, and a city in each cell.
fn many_regions() -> BoundaryData {
    let mut dongs = Vec::new();
    let mut places = Vec::new();
    for row in 0..60 {
        for column in 0..60 {
            let (lat, lon) = (36.0 + row as f64 * 0.02, 127.0 + column as f64 * 0.02);
            // A ring of 80 points, as a real dong's is after simplification.
            let ring: Vec<(f64, f64)> = (0..80)
                .map(|i| {
                    let angle = i as f64 / 80.0 * std::f64::consts::TAU;
                    (lat + 0.009 * angle.sin(), lon + 0.009 * angle.cos())
                })
                .collect();
            dongs.push(RegionData {
                country: Some("KR".into()),
                fields: [
                    Some(Name::plain("도")),
                    None,
                    Some(Name::plain("시")),
                    None,
                    Some(Name::plain(&format!("{row}-{column}동"))),
                ],
                rings: vec![ring],
            });
            places.push(PlaceData {
                country: Some("KR".into()),
                name: Name::plain(&format!("{row}-{column}")),
                latitude: lat,
                longitude: lon,
            });
        }
    }
    let province = RegionData {
        country: Some("KR".into()),
        fields: [Some(Name::plain("도")), None, None, None, None],
        rings: vec![vec![
            (35.0, 126.0),
            (35.0, 129.0),
            (38.0, 129.0),
            (38.0, 126.0),
        ]],
    };
    BoundaryData {
        made_from: "many".into(),
        countries: vec![("KR".into(), Name::plain("대한민국"))],
        layers: vec![dongs, vec![province]],
        places,
    }
}

#[test]
fn fr36_a_lookup_takes_milliseconds() {
    let boundaries = Boundaries::from_bytes(&many_regions().to_bytes()).unwrap();
    let lookups = 2_000;
    let started = Instant::now();
    let mut found = 0;
    for i in 0..lookups {
        let lat = 36.0 + (i % 60) as f64 * 0.02 + 0.001;
        let lon = 127.0 + ((i / 60) % 60) as f64 * 0.02 + 0.001;
        let address = boundaries.address(lat, lon, "ko").unwrap();
        assert!(address.thoroughfare.ends_with('동'), "{address:?}");
        found += 1;
    }
    let each = started.elapsed() / lookups;
    assert_eq!(found, lookups);
    // Generous for a debug build on a busy runner; a release build takes microseconds.
    assert!(each < Duration::from_millis(5), "a lookup took {each:?}");
}

#[test]
fn fr36_errors_reach_the_guest_as_their_status() {
    let busy: geocode::GeocodeOutcome = Err(geocode::GeocodeError::new(
        GeocodeErrorKind::Busy,
        "slow down",
    ));
    let result = geocode::result_to_control(4, &busy);
    assert_eq!(result.status, control::GeocodeStatus::Busy as i32);
    assert_eq!(result.message, "slow down");
    let unavailable: geocode::GeocodeOutcome = Err(geocode::GeocodeError::new(
        GeocodeErrorKind::Unavailable,
        "no data",
    ));
    assert_eq!(
        geocode::result_to_control(5, &unavailable).status,
        control::GeocodeStatus::Unavailable as i32
    );
}

#[test]
fn fr36_a_lookup_that_cannot_be_answered_is_refused_before_it_reaches_the_host() {
    let empty = GeocodeQuery::from_control(&control::GeocodeRequest {
        query: "   ".into(),
        ..Default::default()
    });
    assert_eq!(empty.unwrap_err().kind, GeocodeErrorKind::Invalid);
    let off_earth = GeocodeQuery::from_control(&control::GeocodeRequest {
        reverse: true,
        latitude_degrees: 91.0,
        ..Default::default()
    });
    assert_eq!(off_earth.unwrap_err().kind, GeocodeErrorKind::Invalid);

    // The count is kept in bounds, the language tag made plain, and bounds kept.
    let query = GeocodeQuery::from_control(&control::GeocodeRequest {
        query: "cafe".into(),
        max_results: 500,
        language: "en-US;drop".into(),
        has_bounds: true,
        lower_left_latitude: 37.0,
        lower_left_longitude: 126.0,
        upper_right_latitude: 38.0,
        upper_right_longitude: 127.0,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(query.max_results, geocode::MAX_RESULTS);
    assert_eq!(query.language, "");
    assert_eq!(
        query.lookup,
        Lookup::Forward {
            query: "cafe".into(),
            bounds: Some(Bounds {
                lower_left_latitude: 37.0,
                lower_left_longitude: 126.0,
                upper_right_latitude: 38.0,
                upper_right_longitude: 127.0,
            })
        }
    );
}

#[test]
fn fr36_a_lookup_and_its_addresses_survive_the_local_api() {
    let query = forward("Seoul");
    assert_eq!(
        GeocodeQuery::from_host_api(&query.to_host_api()).unwrap(),
        query
    );
    let address = fixture().address(37.5663, 126.9779, "ko").unwrap();
    let outcome: geocode::GeocodeOutcome = Ok(vec![address.clone()]);
    let report = geocode::report_to_host_api(3, &outcome);
    assert_eq!(geocode::outcome_from_host_api(report), Ok(outcome));
    assert_eq!(
        Address::from_host_api(address.to_host_api()),
        address,
        "the same address comes back"
    );
}
