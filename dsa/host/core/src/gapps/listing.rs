//! LiteGapps' builds on its file host, SourceForge, and downloading one.
//!
//! LiteGapps keeps each build at `litegapps/<arch>/<api>/core/<yyyy-mm-dd>/<file>.zip`, and the
//! host lists a folder's files, newest first, with each file's size and MD5 digest, in an RSS
//! feed (`<project>/rss?path=<folder>`). The feed is read for the guest's architecture and API
//! level, the newest build named as LiteGapps names its official builds is taken, and it is
//! downloaded from the folder's `/download` address, which the host sends on to a mirror.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use super::md5::{hex, Md5};
use super::{GappsPackage, Target, VARIANT};
use crate::update::store::Fetch;

/// LiteGapps' project on SourceForge.
pub const PROJECT: &str = "https://sourceforge.net/projects/litegapps";
/// The largest build downloaded; LiteGapps' Core builds are 120 to 260 MiB.
pub const MAX_BUILD_BYTES: u64 = 1 << 30;
/// The largest feed read.
const MAX_LISTING_BYTES: usize = 4 << 20;

/// One build in the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// The folder it is in, `yyyy-mm-dd`.
    pub folder: String,
    /// The file name, `LiteGapps-core-<arch>-<android>-<yyyymmdd>-official.zip`.
    pub file: String,
    /// The Android version it names ("16.0").
    pub android_version: String,
    /// Its date, yyyymmdd.
    pub version: String,
    pub size: u64,
    /// Its MD5 digest, lower-case hexadecimal.
    pub md5: String,
}

/// The feed of the folder holding the builds for `target`.
pub fn listing_url(project: &str, target: &Target) -> String {
    format!(
        "{project}/rss?path=/litegapps/{}/{}/{VARIANT}",
        target.arch, target.sdk
    )
}

/// Where `build` is downloaded from.
pub fn download_url(project: &str, target: &Target, build: &Build) -> String {
    format!(
        "{project}/files/litegapps/{}/{}/{VARIANT}/{}/{}/download",
        target.arch, target.sdk, build.folder, build.file
    )
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let to = text[from..].find(end)? + from;
    Some(&text[from..to])
}

/// The parts of an official build's file name for `target`: its Android version and date.
fn parse_file(file: &str, target: &Target) -> Option<(String, String)> {
    let rest = file
        .strip_prefix(&format!("LiteGapps-{VARIANT}-{}-", target.arch))?
        .strip_suffix("-official.zip")?;
    let (android, date) = rest.rsplit_once('-')?;
    let android_ok = !android.is_empty()
        && android.len() <= 8
        && android.chars().all(|c| c.is_ascii_digit() || c == '.');
    let date_ok = date.len() == 8 && date.chars().all(|c| c.is_ascii_digit());
    (android_ok && date_ok).then(|| (android.to_string(), date.to_string()))
}

/// The builds for `target` in the feed `rss`, newest first.
pub fn builds(rss: &str, target: &Target) -> Vec<Build> {
    let folder_prefix = format!("/litegapps/{}/{}/{VARIANT}/", target.arch, target.sdk);
    let mut found: Vec<Build> = rss
        .split("<item>")
        .skip(1)
        .filter_map(|item| {
            let title = between(item, "<title><![CDATA[", "]]></title>")?;
            let (folder, file) = title.strip_prefix(&folder_prefix)?.split_once('/')?;
            let folder_ok = folder.len() == 10
                && folder.chars().enumerate().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        c == '-'
                    } else {
                        c.is_ascii_digit()
                    }
                });
            if !folder_ok {
                return None;
            }
            let (android_version, version) = parse_file(file, target)?;
            let content = between(item, "<media:content", "</media:content>")?;
            let size = between(content, "filesize=\"", "\"")?.parse().ok()?;
            let md5 = between(content, "<media:hash algo=\"md5\">", "</media:hash>")?
                .trim()
                .to_ascii_lowercase();
            let md5_ok = md5.len() == 32 && md5.chars().all(|c| c.is_ascii_hexdigit());
            (md5_ok && size > 0).then(|| Build {
                folder: folder.to_string(),
                file: file.to_string(),
                android_version,
                version,
                size,
                md5,
            })
        })
        .collect();
    found.sort_by(|a, b| (&b.version, &b.folder).cmp(&(&a.version, &a.folder)));
    found
}

/// Reads the feed for `target` and returns its newest build.
pub fn newest(fetch: &dyn Fetch, project: &str, target: &Target) -> Result<Build, String> {
    let url = listing_url(project, target);
    let mut feed = Vec::new();
    fetch
        .fetch(&url, &mut feed)
        .map_err(|e| format!("LiteGapps' list of builds could not be read: {e}"))?;
    if feed.len() > MAX_LISTING_BYTES {
        return Err("LiteGapps' list of builds is larger than any list it publishes".into());
    }
    let feed = String::from_utf8_lossy(&feed);
    let found = builds(&feed, target);
    let build = found.into_iter().next().ok_or_else(|| {
        format!(
            "LiteGapps lists no {VARIANT} build for {} Android at API level {}",
            target.arch, target.sdk
        )
    })?;
    if build.size > MAX_BUILD_BYTES {
        return Err(format!(
            "{} is {} MiB, more than the {} MiB taken",
            build.file,
            build.size >> 20,
            MAX_BUILD_BYTES >> 20
        ));
    }
    Ok(build)
}

/// What a download is written through: the file, its digest, its length and the news of it.
struct Counted<'a> {
    file: BufWriter<File>,
    md5: Md5,
    written: u64,
    limit: u64,
    progress: &'a mut dyn FnMut(u64),
}

impl Write for Counted<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.written + data.len() as u64 > self.limit {
            return Err(std::io::Error::other(
                "the download is larger than its listing says",
            ));
        }
        self.file.write_all(data)?;
        self.md5.update(data);
        self.written += data.len() as u64;
        (self.progress)(self.written);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Downloads `build` to `path`, telling `progress` how many bytes have arrived, and checks
/// its length and MD5 digest against the listing. Returns its description; on any failure the
/// file is removed.
pub fn download(
    fetch: &dyn Fetch,
    project: &str,
    target: &Target,
    build: &Build,
    path: &Path,
    progress: &mut dyn FnMut(u64),
) -> Result<GappsPackage, String> {
    let url = download_url(project, target, build);
    let result = download_to(fetch, &url, target, build, path, progress);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn download_to(
    fetch: &dyn Fetch,
    url: &str,
    target: &Target,
    build: &Build,
    path: &Path,
    progress: &mut dyn FnMut(u64),
) -> Result<GappsPackage, String> {
    let file =
        File::create(path).map_err(|e| format!("{} cannot be written: {e}", path.display()))?;
    let mut out = Counted {
        file: BufWriter::new(file),
        md5: Md5::new(),
        written: 0,
        limit: build.size,
        progress,
    };
    fetch
        .fetch(url, &mut out)
        .map_err(|e| format!("{} could not be downloaded: {e}", build.file))?;
    out.flush()
        .map_err(|e| format!("{} cannot be written: {e}", path.display()))?;
    if out.written != build.size {
        return Err(format!(
            "{} arrived with {} bytes, not the {} its listing says; try again",
            build.file, out.written, build.size
        ));
    }
    let md5 = hex(&out.md5.finish());
    if md5 != build.md5 {
        return Err(format!(
            "{} did not arrive intact (its MD5 digest is {md5}, the listing says {}); \
             try again",
            build.file, build.md5
        ));
    }
    Ok(GappsPackage {
        variant: VARIANT.to_string(),
        version: build.version.clone(),
        android_version: build.android_version.clone(),
        sdk: target.sdk,
        arch: target.arch.to_string(),
        file: build.file.clone(),
        source: url.to_string(),
        md5,
    })
}

#[cfg(any(test, feature = "conformance"))]
pub mod fixture {
    //! A feed as SourceForge writes one.

    /// One `<item>` of the feed.
    pub fn item(path: &str, size: u64, md5: &str) -> String {
        format!(
            "<item>\n<title><![CDATA[{path}]]></title>\n\
             <link>https://sourceforge.net/projects/litegapps/files{path}/download</link>\n\
             <pubDate>Thu, 01 Oct 2026 11:13:36 UT</pubDate>\n\
             <media:content xmlns:media=\"http://video.search.yahoo.com/mrss/\" \
             type=\"application/zip; charset=binary\" \
             url=\"https://sourceforge.net/projects/litegapps/files{path}/download\" \
             filesize=\"{size}\"><media:hash algo=\"md5\">{md5}</media:hash></media:content>\n\
             </item>\n"
        )
    }

    /// A feed of `items`.
    pub fn feed(items: &[String]) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<rss version=\"2.0\"><channel>\
             <title>LiteGapps</title>\n{}</channel></rss>\n",
            items.concat()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const X86_36: Target = Target {
        arch: "x86_64",
        sdk: 36,
    };

    /// Serves fixed bodies by URL.
    pub(crate) struct Served(pub Mutex<HashMap<String, Vec<u8>>>);

    impl Fetch for Served {
        fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
            let body = self
                .0
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .ok_or_else(|| format!("404 for {url}"))?;
            out.write_all(&body).map_err(|e| e.to_string())?;
            Ok(body.len() as u64)
        }
    }

    fn sample_feed() -> String {
        fixture::feed(&[
            fixture::item(
                "/litegapps/x86_64/36/core/2026-09-28/LiteGapps-core-x86_64-16.0-20260928-official.zip",
                122_162_521,
                "0ba555455f04e4a667ebac137fbe4c02",
            ),
            fixture::item(
                "/litegapps/x86_64/36/core/2026-10-01/LiteGapps-core-x86_64-16.0-20261001-official.zip",
                122_196_440,
                "EA9945A6D63849FA9E51782EDFE613A4",
            ),
            // Not an official build, another architecture, and a folder of another shape.
            fixture::item(
                "/litegapps/x86_64/36/core/2026-10-02/LiteGapps-core-x86_64-16.0-20261002-test.zip",
                1,
                "0ba555455f04e4a667ebac137fbe4c02",
            ),
            fixture::item(
                "/litegapps/x86_64/36/core/2026-10-03/LiteGapps-core-arm64-16.0-20261003-official.zip",
                1,
                "0ba555455f04e4a667ebac137fbe4c02",
            ),
            fixture::item(
                "/litegapps/x86_64/36/core/latest/LiteGapps-core-x86_64-16.0-20261004-official.zip",
                1,
                "0ba555455f04e4a667ebac137fbe4c02",
            ),
        ])
    }

    #[test]
    fn fr32_the_newest_official_build_for_the_guest_is_chosen() {
        let found = builds(&sample_feed(), &X86_36);
        assert_eq!(found.len(), 2);
        let newest = &found[0];
        assert_eq!(newest.folder, "2026-10-01");
        assert_eq!(
            newest.file,
            "LiteGapps-core-x86_64-16.0-20261001-official.zip"
        );
        assert_eq!(newest.android_version, "16.0");
        assert_eq!(newest.version, "20261001");
        assert_eq!(newest.size, 122_196_440);
        assert_eq!(newest.md5, "ea9945a6d63849fa9e51782edfe613a4");
        assert_eq!(
            listing_url(PROJECT, &X86_36),
            "https://sourceforge.net/projects/litegapps/rss?path=/litegapps/x86_64/36/core"
        );
        assert_eq!(
            download_url(PROJECT, &X86_36, newest),
            "https://sourceforge.net/projects/litegapps/files/litegapps/x86_64/36/core/\
             2026-10-01/LiteGapps-core-x86_64-16.0-20261001-official.zip/download"
        );
        let arm = Target {
            arch: "arm64",
            sdk: 36,
        };
        assert!(builds(&sample_feed(), &arm).is_empty(), "another folder");
    }

    #[test]
    fn fr32_a_download_is_checked_against_its_listing() {
        let data = b"the build".to_vec();
        let mut md5 = Md5::new();
        md5.update(&data);
        let build = Build {
            folder: "2026-10-01".into(),
            file: "LiteGapps-core-x86_64-16.0-20261001-official.zip".into(),
            android_version: "16.0".into(),
            version: "20261001".into(),
            size: data.len() as u64,
            md5: hex(&md5.finish()),
        };
        let url = download_url("https://example.com/p", &X86_36, &build);
        let served = Served(Mutex::new(HashMap::from([(url, data.clone())])));
        let dir = std::env::temp_dir().join(format!("dsa-gapps-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("build.zip");
        let mut seen = 0;
        let package = download(
            &served,
            "https://example.com/p",
            &X86_36,
            &build,
            &path,
            &mut |done| seen = done,
        )
        .unwrap();
        assert_eq!(seen, data.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(package.version, "20261001");
        assert_eq!(package.arch, "x86_64");
        assert_eq!(package.sdk, 36);
        let wrong = Build {
            md5: "00000000000000000000000000000000".into(),
            ..build.clone()
        };
        let why = download(
            &served,
            "https://example.com/p",
            &X86_36,
            &wrong,
            &path,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(why.contains("MD5"), "{why}");
        assert!(!path.exists(), "a download that does not match is removed");
        let short = Build {
            size: data.len() as u64 + 1,
            ..build
        };
        assert!(download(
            &served,
            "https://example.com/p",
            &X86_36,
            &short,
            &path,
            &mut |_| {}
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
