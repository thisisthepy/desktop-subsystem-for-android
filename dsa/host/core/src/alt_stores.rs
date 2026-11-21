//! Alternative app stores a user may install: ONE store, Aurora Store and F-Droid.
//!
//! DSA ships no app store. On the user's request the command line (and the settings apps,
//! through it) downloads the store's APK from the project's own official address, which always
//! serves its newest release, checks that the APK is the store's package and is signed by the
//! certificate recorded here, and installs it like any other app. The package installer in
//! Android then checks the signature itself. A store is removed like any other app.
//!
//! The rules live here once, so every host and every settings app follows the same list,
//! sources and checks.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::apk_info::ApkInfo;
use crate::tasks::AppInfo;
use crate::update::store::Fetch;

/// The User-Agent ONE store's download address asks for: it serves its APK to a phone's
/// browser only.
pub const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 14; Pixel 8) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";

/// The address Aurora's official site lists its files at.
pub const AURORA_LISTING: &str = "https://auroraoss.com/api/files";
/// Where the paths in Aurora's listing are served from.
pub const AURORA_SITE: &str = "https://auroraoss.com";
/// The folder of Aurora's plain releases in its listing. The `huawei` and `preload` folders
/// below it hold variants made for other uses.
pub const AURORA_RELEASES: &str = "/downloads/AuroraStore/Release/";

/// One of the stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreId {
    OneStore,
    Aurora,
    FDroid,
}

/// Every store, in the order the settings apps show them.
pub const ALL: [StoreId; 3] = [StoreId::OneStore, StoreId::Aurora, StoreId::FDroid];

/// How a store's newest APK is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The address serves the newest APK itself (after redirects), asked for with
    /// `user_agent` when it is set.
    Direct {
        url: &'static str,
        user_agent: Option<&'static str>,
    },
    /// Aurora's listing names the releases; the newest plain one is downloaded.
    AuroraListing,
}

/// What DSA knows of a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Store {
    pub id: StoreId,
    /// The store's name, a trademark of its owner, written plainly.
    pub name: &'static str,
    /// Its Android package.
    pub package: &'static str,
    /// The SHA-256 of the certificate its APK is signed with, lower-case hex.
    pub certificate_sha256: &'static str,
    /// The project's own page.
    pub homepage: &'static str,
    pub source: Source,
}

impl StoreId {
    /// The name the command line takes: `onestore`, `aurora` or `fdroid`.
    pub fn code(self) -> &'static str {
        match self {
            StoreId::OneStore => "onestore",
            StoreId::Aurora => "aurora",
            StoreId::FDroid => "fdroid",
        }
    }

    /// The store the command line names, ignoring case.
    pub fn parse(name: &str) -> Result<StoreId, String> {
        let lower = name.to_ascii_lowercase();
        ALL.into_iter()
            .find(|id| id.code() == lower)
            .ok_or_else(|| {
                format!(
                    "{name:?} is not a store DSA installs; it installs onestore, aurora or fdroid"
                )
            })
    }

    /// What DSA knows of the store.
    pub fn store(self) -> Store {
        match self {
            StoreId::OneStore => Store {
                id: self,
                name: "ONE store",
                package: "com.skt.skaf.A000Z00040",
                // "SKT Signed System Level3", SK Telecom's signing certificate.
                certificate_sha256:
                    "77e88a40cb954b2b409bd54199106918b599c116cd51d31a2c7eb7743f56e33a",
                homepage: "https://m.onestore.co.kr/",
                source: Source::Direct {
                    url: "https://m.onestore.co.kr/ko-kr/binary/download.omp?type=V01",
                    user_agent: Some(MOBILE_USER_AGENT),
                },
            },
            StoreId::Aurora => Store {
                id: self,
                name: "Aurora Store",
                package: "com.aurora.store",
                // The fingerprint Aurora's official site publishes for its releases.
                certificate_sha256:
                    "4c626157ad02bda3401a7263555f68a79663fc3e13a4d4369a12570941aa280f",
                homepage: "https://auroraoss.com/",
                source: Source::AuroraListing,
            },
            StoreId::FDroid => Store {
                id: self,
                name: "F-Droid",
                package: "org.fdroid.fdroid",
                // The fingerprint F-Droid publishes for its client's official releases.
                certificate_sha256:
                    "43238d512c1e5eb2d6569f4a3afbf5523418b82e0a3ed1552770abb9a9c9ccab",
                homepage: "https://f-droid.org/",
                source: Source::Direct {
                    url: "https://f-droid.org/F-Droid.apk",
                    user_agent: None,
                },
            },
        }
    }
}

/// A version such as `4.8.4` as numbers, so 4.10 sorts after 4.9; None when a part is not a
/// number.
fn version_of(text: &str) -> Option<Vec<u64>> {
    text.split('.').map(|part| part.parse().ok()).collect()
}

/// The path of the newest plain release in Aurora's listing (JSON, folders with `contents`
/// and files with `path`): a file directly in [`AURORA_RELEASES`] named
/// `AuroraStore-<version>.apk`, never one of the variant folders below it.
pub fn newest_aurora_release(listing: &[u8]) -> Result<String, String> {
    let json: serde_json::Value = serde_json::from_slice(listing)
        .map_err(|e| format!("Aurora's list of releases is not readable: {e}"))?;
    let mut paths = Vec::new();
    collect_paths(&json, &mut paths);
    paths
        .into_iter()
        .filter_map(|path| {
            let name = path.strip_prefix(AURORA_RELEASES)?;
            let version = name.strip_prefix("AuroraStore-")?.strip_suffix(".apk")?;
            if name.contains('/') {
                return None;
            }
            Some((version_of(version)?, path.clone()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
        .ok_or_else(|| "Aurora's list of releases names no Aurora Store release".to_string())
}

fn collect_paths(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(path)) = map.get("path") {
                out.push(path.clone());
            }
            for child in map.values() {
                collect_paths(child, out);
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| collect_paths(item, out)),
        _ => {}
    }
}

/// Lower-case hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether `apk` is `store`'s package, signed by the certificate recorded for it. The reason
/// says what differs, so a user can tell a changed source from a broken download.
pub fn check(store: &Store, apk: &ApkInfo) -> Result<(), String> {
    if apk.package != store.package {
        return Err(format!(
            "the file downloaded for {} is the package {}, not {}, so it was not installed",
            store.name, apk.package, store.package
        ));
    }
    let signer = apk.signer.as_ref().ok_or_else(|| {
        format!(
            "the file downloaded for {} is not signed, so it was not installed",
            store.name
        )
    })?;
    let got = hex(&signer.sha256());
    if got != store.certificate_sha256 {
        return Err(format!(
            "the file downloaded for {} is signed by a certificate with SHA-256 {got}, not by \
             {}'s ({}), so it was not installed",
            store.name, store.name, store.certificate_sha256
        ));
    }
    Ok(())
}

/// The address of `store`'s newest APK, and the User-Agent to ask with.
pub fn apk_url(store: &Store, fetch: &dyn Fetch) -> Result<(String, Option<&'static str>), String> {
    match store.source {
        Source::Direct { url, user_agent } => Ok((url.to_string(), user_agent)),
        Source::AuroraListing => {
            let mut listing = Vec::new();
            fetch
                .fetch(AURORA_LISTING, &mut listing)
                .map_err(|e| format!("Aurora's list of releases did not download: {e}"))?;
            let path = newest_aurora_release(&listing)?;
            Ok((format!("{AURORA_SITE}{path}"), None))
        }
    }
}

/// Downloads `store`'s newest APK into `dir` and checks it. The file is removed when it is not
/// the store's, so nothing unchecked is left behind.
pub fn download(store: &Store, fetch: &dyn Fetch, dir: &Path) -> Result<PathBuf, String> {
    let (url, user_agent) = apk_url(store, fetch)?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.apk", store.id.code()));
    let result = (|| {
        let mut file = fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        fetch
            .fetch_as(&url, user_agent, &mut file)
            .map_err(|e| format!("{} did not download: {e}", store.name))?;
        file.flush()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        drop(file);
        let info = ApkInfo::read(&path)
            .map_err(|e| format!("the file downloaded for {} is not an APK: {e}", store.name))?;
        check(store, &info)
    })();
    match result {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = fs::remove_file(&path);
            Err(e)
        }
    }
}

/// Where one store stands for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    NotInstalled,
    /// Installed, with the version Android reports (empty when it does not say).
    Installed {
        version: String,
    },
}

/// Each store's state, from the apps installed in the user's Android, in [`ALL`]'s order.
pub fn states(installed: &[AppInfo]) -> Vec<(Store, State)> {
    ALL.into_iter()
        .map(|id| {
            let store = id.store();
            let state = installed
                .iter()
                .find(|app| app.package_name == store.package)
                .map_or(State::NotInstalled, |app| State::Installed {
                    version: app.version_name.clone(),
                });
            (store, state)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk_info::signing::{Scheme, Signer};
    use std::cell::RefCell;

    fn apk(package: &str, certificate: &[u8]) -> ApkInfo {
        ApkInfo {
            package: package.into(),
            label: None,
            version_code: 1,
            version_name: None,
            min_sdk: 21,
            target_sdk: 36,
            permissions: Vec::new(),
            icon: None,
            signer: Some(Signer {
                scheme: Scheme::V2,
                certificate: certificate.to_vec(),
                past_signers: Vec::new(),
            }),
            abis: Vec::new(),
            size: 1,
        }
    }

    #[test]
    fn fr43_each_store_has_its_official_source_package_and_certificate() {
        let one = StoreId::parse("onestore").unwrap().store();
        assert_eq!(one.package, "com.skt.skaf.A000Z00040");
        assert_eq!(
            one.source,
            Source::Direct {
                url: "https://m.onestore.co.kr/ko-kr/binary/download.omp?type=V01",
                user_agent: Some(MOBILE_USER_AGENT),
            }
        );
        assert!(MOBILE_USER_AGENT.contains("Mobile"));
        let aurora = StoreId::parse("Aurora").unwrap().store();
        assert_eq!(aurora.package, "com.aurora.store");
        assert_eq!(aurora.source, Source::AuroraListing);
        let fdroid = StoreId::parse("fdroid").unwrap().store();
        assert_eq!(fdroid.package, "org.fdroid.fdroid");
        assert_eq!(
            fdroid.source,
            Source::Direct {
                url: "https://f-droid.org/F-Droid.apk",
                user_agent: None
            }
        );
        for id in ALL {
            let store = id.store();
            assert_eq!(store.certificate_sha256.len(), 64, "{}", store.name);
            assert!(store
                .certificate_sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert_eq!(StoreId::parse(id.code()).unwrap(), id);
        }
        assert!(StoreId::parse("playstore").is_err());
    }

    #[test]
    fn fr43_the_newest_plain_aurora_release_is_chosen() {
        let listing = br#"{"name":"downloads","contents":[{"name":"AuroraStore","path":"/downloads/AuroraStore","contents":[
            {"name":"Release","path":"/downloads/AuroraStore/Release","isDirectory":true,"contents":[
              {"name":"AuroraStore-4.8.4.apk","path":"/downloads/AuroraStore/Release/AuroraStore-4.8.4.apk"},
              {"name":"AuroraStore-4.10.0.apk","path":"/downloads/AuroraStore/Release/AuroraStore-4.10.0.apk"},
              {"name":"AuroraStore-4.9.1.apk","path":"/downloads/AuroraStore/Release/AuroraStore-4.9.1.apk"},
              {"name":"huawei","path":"/downloads/AuroraStore/Release/huawei","contents":[
                {"name":"AuroraStore-hw-9.0.0.apk","path":"/downloads/AuroraStore/Release/huawei/AuroraStore-hw-9.0.0.apk"},
                {"name":"AuroraStore-9.0.0.apk","path":"/downloads/AuroraStore/Release/huawei/AuroraStore-9.0.0.apk"}]},
              {"name":"preload","path":"/downloads/AuroraStore/Release/preload","contents":[
                {"name":"AuroraStore-preload-9.0.0.apk","path":"/downloads/AuroraStore/Release/preload/AuroraStore-preload-9.0.0.apk"}]}]},
            {"name":"Nightly","path":"/downloads/AuroraStore/Nightly/AuroraStore-99.0.apk"}]}]}"#;
        assert_eq!(
            newest_aurora_release(listing).unwrap(),
            "/downloads/AuroraStore/Release/AuroraStore-4.10.0.apk"
        );
        assert!(newest_aurora_release(br#"{"contents":[]}"#).is_err());
        assert!(newest_aurora_release(b"not json").is_err());

        struct Served(RefCell<Vec<String>>);
        impl Fetch for Served {
            fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
                self.0.borrow_mut().push(url.to_string());
                let body = br#"[{"path":"/downloads/AuroraStore/Release/AuroraStore-4.8.4.apk"}]"#;
                out.write_all(body).map_err(|e| e.to_string())?;
                Ok(body.len() as u64)
            }
        }
        let served = Served(RefCell::new(Vec::new()));
        let (url, agent) = apk_url(&StoreId::Aurora.store(), &served).unwrap();
        assert_eq!(
            url,
            "https://auroraoss.com/downloads/AuroraStore/Release/AuroraStore-4.8.4.apk"
        );
        assert_eq!(agent, None);
        assert_eq!(served.0.borrow().as_slice(), [AURORA_LISTING]);
    }

    #[test]
    fn fr43_an_apk_from_another_signer_is_refused() {
        let store = StoreId::FDroid.store();
        let error = check(&store, &apk("org.fdroid.fdroid", b"someone else")).unwrap_err();
        assert!(error.contains("not installed"), "{error}");
        assert!(error.contains(store.certificate_sha256), "{error}");
        let mut unsigned = apk("org.fdroid.fdroid", b"x");
        unsigned.signer = None;
        assert!(check(&store, &unsigned).is_err());
    }

    #[test]
    fn fr43_an_apk_of_another_package_is_refused() {
        let store = StoreId::Aurora.store();
        let error = check(&store, &apk("com.example.fake", b"x")).unwrap_err();
        assert!(error.contains("com.example.fake"), "{error}");
    }

    #[test]
    fn fr43_a_refused_download_leaves_no_file() {
        struct Junk;
        impl Fetch for Junk {
            fn fetch(&self, _url: &str, out: &mut dyn Write) -> Result<u64, String> {
                out.write_all(b"not an apk").map_err(|e| e.to_string())?;
                Ok(10)
            }
        }
        let dir = std::env::temp_dir().join(format!("dsa-fr43-{}", std::process::id()));
        let error = download(&StoreId::FDroid.store(), &Junk, &dir).unwrap_err();
        assert!(error.contains("not an APK"), "{error}");
        assert!(!dir.join("fdroid.apk").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr43_the_installed_state_comes_from_the_installed_apps() {
        let app = AppInfo {
            package_name: "org.fdroid.fdroid".into(),
            version_name: "1.23.1".into(),
            ..AppInfo::default()
        };
        let states = states(&[app]);
        assert_eq!(
            states.iter().map(|(s, _)| s.id).collect::<Vec<_>>(),
            ALL.to_vec()
        );
        assert_eq!(states[0].1, State::NotInstalled);
        assert_eq!(states[1].1, State::NotInstalled);
        assert_eq!(
            states[2].1,
            State::Installed {
                version: "1.23.1".into()
            }
        );
    }
}
