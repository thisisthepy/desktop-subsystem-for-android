//! The root certificates the host adds to Android's system certificates.
//!
//! Internet filters and company proxies that inspect HTTPS install a root certificate on the
//! computer; apps in Android reach the internet there only if Android trusts it too. The host
//! sends the roots a user or an administrator added (`HostCertificates`), and the daemon adds
//! each to Android's system certificate folders as a file of its own, beside Android's.
//!
//! Android reads its system certificates from `/apex/com.android.conscrypt/cacerts` (and, on a
//! build that asks for it, `/system/etc/security/cacerts`). Both are read only, so at boot,
//! before the zygote starts, init makes a folder of the same certificates in a tmpfs for each
//! ([`setup`], `dsa-channeld --cacerts-setup`) and binds it over the original
//! (`dsa-cacerts.rc`). Every app's mount namespace is made after that and inherits the binds,
//! and since the folders are one tmpfs, a file the daemon adds or takes away later shows in
//! every app at once ([`Installer`]).
//!
//! Android looks a certificate up by the file name `<hash>.<n>`: the hash of its subject
//! ([`subject_hash_old`], OpenSSL's `X509_NAME_hash_old`) and the lowest number not taken by
//! another certificate of the same subject. The daemon names the host's the same way, after
//! Android's own, and keeps the names it added in a list outside the bound folders
//! ([`MANIFEST`]), so it takes away only what it added and never one of Android's.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The tmpfs folder init makes for the certificate folders (`dsa-cacerts.rc`).
pub const ROOT: &str = "/dev/dsa_cacerts";

/// The list of the files the daemon added, one `<folder>/<name>` a line, in [`ROOT`].
pub const MANIFEST: &str = "host-added";

/// Each folder under [`ROOT`] and the folder of Android's it stands in for.
pub const FOLDERS: [(&str, &str); 2] = [
    ("conscrypt", "/apex/com.android.conscrypt/cacerts"),
    ("system", "/system/etc/security/cacerts"),
];

/// The most certificates the daemon adds at once; the host's store holds far fewer.
pub const MAX_CERTIFICATES: usize = 512;

/// The largest certificate the daemon takes, in bytes.
pub const MAX_CERTIFICATE_LEN: usize = 64 * 1024;

/// What one [`Installer::install`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The files written, as `<folder>/<name>`.
    pub added: Vec<String>,
    /// The files that were already there as wanted.
    pub kept: Vec<String>,
    /// The files taken away.
    pub removed: Vec<String>,
    /// Certificates left out because Android already has the same one.
    pub already_trusted: usize,
    /// What went wrong, one line each; the rest was still done.
    pub problems: Vec<String>,
}

/// Adds the host's certificates to the folders under `root`, and takes away those it added
/// that the host no longer names.
pub struct Installer {
    root: PathBuf,
    folders: Vec<String>,
}

impl Installer {
    /// The installer of the guest: [`ROOT`] and its [`FOLDERS`].
    pub fn of_system() -> Installer {
        Installer::new(
            ROOT,
            FOLDERS.iter().map(|(name, _)| name.to_string()).collect(),
        )
    }

    /// An installer over the folders named `folders` under `root`.
    pub fn new(root: impl Into<PathBuf>, folders: Vec<String>) -> Installer {
        Installer {
            root: root.into(),
            folders,
        }
    }

    /// Makes the folders hold Android's certificates and `certificates` (DER-encoded X.509),
    /// and nothing else of the host's. An empty list takes away every one added before.
    pub fn install(&self, certificates: &[Vec<u8>]) -> Report {
        let mut report = Report::default();
        let previous = self.read_manifest();
        let mut wanted: Vec<(String, Vec<u8>)> = Vec::new();
        let mut seen = BTreeSet::new();
        for der in certificates {
            if wanted.len() >= MAX_CERTIFICATES {
                report.problems.push(format!(
                    "the host sent more than {MAX_CERTIFICATES} certificates; the rest are left out"
                ));
                break;
            }
            if der.len() > MAX_CERTIFICATE_LEN {
                report
                    .problems
                    .push(format!("a certificate of {} bytes is too large", der.len()));
                continue;
            }
            match subject_hash_old(der) {
                Ok(hash) => {
                    if seen.insert(der.clone()) {
                        wanted.push((hash, der.clone()));
                    }
                }
                Err(why) => report
                    .problems
                    .push(format!("a certificate is not usable: {why}")),
            }
        }

        let mut added_now = BTreeSet::new();
        for folder in &self.folders {
            let dir = self.root.join(folder);
            let mine: BTreeSet<String> = previous
                .iter()
                .filter_map(|entry| entry.strip_prefix(&format!("{folder}/")))
                .map(str::to_string)
                .collect();
            let names = match fs::read_dir(&dir) {
                Ok(entries) => entries
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect::<BTreeSet<String>>(),
                Err(why) => {
                    report
                        .problems
                        .push(format!("{} cannot be read: {why}", dir.display()));
                    continue;
                }
            };
            let android: BTreeSet<&String> = names.iter().filter(|n| !mine.contains(*n)).collect();
            // Android's certificates of each subject hash, read once.
            let mut android_ders: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
            let mut taken: BTreeSet<String> = android.iter().map(|n| n.to_string()).collect();
            let mut assigned: Vec<(String, Vec<u8>)> = Vec::new();
            for (hash, der) in &wanted {
                let ders = android_ders.entry(hash.clone()).or_insert_with(|| {
                    android
                        .iter()
                        .filter(|n| n.split_once('.').is_some_and(|(h, _)| h == hash))
                        .filter_map(|n| fs::read(dir.join(n.as_str())).ok())
                        .filter_map(|bytes| certificate_of(&bytes))
                        .collect()
                });
                if ders.iter().any(|d| d == der) {
                    if folder == &self.folders[0] {
                        report.already_trusted += 1;
                    }
                    continue;
                }
                let name = (0u32..)
                    .map(|n| format!("{hash}.{n}"))
                    .find(|name| !taken.contains(name))
                    .expect("a free number");
                taken.insert(name.clone());
                assigned.push((name, der.clone()));
            }
            for (name, der) in &assigned {
                let entry = format!("{folder}/{name}");
                added_now.insert(entry.clone());
                let path = dir.join(name);
                let text = pem(der);
                if mine.contains(name) && fs::read(&path).is_ok_and(|b| b == text.as_bytes()) {
                    report.kept.push(entry);
                    continue;
                }
                // The list names a file before it is written, so a file is never left that the
                // list does not name.
                let mut listed: BTreeSet<String> = previous.iter().cloned().collect();
                listed.extend(added_now.iter().cloned());
                if let Err(why) = self.write_manifest(&listed) {
                    report.problems.push(why);
                    continue;
                }
                match write_atomically(&path, text.as_bytes()) {
                    Ok(()) => report.added.push(entry),
                    Err(why) => report.problems.push(why),
                }
            }
            let keep: BTreeSet<&String> = assigned.iter().map(|(n, _)| n).collect();
            for name in &mine {
                if keep.contains(name) {
                    continue;
                }
                match fs::remove_file(dir.join(name)) {
                    Ok(()) => report.removed.push(format!("{folder}/{name}")),
                    Err(why) if why.kind() == std::io::ErrorKind::NotFound => {}
                    Err(why) => {
                        // Still listed, so the next install tries again.
                        added_now.insert(format!("{folder}/{name}"));
                        report
                            .problems
                            .push(format!("{folder}/{name} cannot be taken away: {why}"));
                    }
                }
            }
        }
        // A folder that could not be read keeps what the list said of it.
        for entry in &previous {
            let folder = entry.split('/').next().unwrap_or_default();
            if !self.root.join(folder).is_dir() {
                added_now.insert(entry.clone());
            }
        }
        if let Err(why) = self.write_manifest(&added_now) {
            report.problems.push(why);
        }
        report
    }

    /// The files the list says the daemon added.
    pub fn read_manifest(&self) -> BTreeSet<String> {
        fs::read_to_string(self.root.join(MANIFEST))
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| plain_entry(line))
            .map(str::to_string)
            .collect()
    }

    fn write_manifest(&self, entries: &BTreeSet<String>) -> Result<(), String> {
        let mut text = String::new();
        for entry in entries {
            text.push_str(entry);
            text.push('\n');
        }
        write_atomically(&self.root.join(MANIFEST), text.as_bytes())
    }
}

/// Whether a line of the list is `<folder>/<name>` with plain names, so a list that was
/// tampered with never leads outside the folders.
fn plain_entry(line: &str) -> bool {
    let plain = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    };
    matches!(line.split_once('/'), Some((folder, name)) if plain(folder) && plain(name))
}

/// Writes `data` to `path` through a file beside it, so a reader never sees half a file, and
/// leaves it readable by every app (0644).
pub fn write_atomically(path: &Path, data: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let partial = path.with_file_name(format!(".{name}.partial"));
    let written = (|| {
        let mut file = fs::File::create(&partial)?;
        file.write_all(data)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&partial, fs::Permissions::from_mode(0o644))?;
        }
        fs::rename(&partial, path)
    })();
    written.map_err(|why| {
        let _ = fs::remove_file(&partial);
        format!("{} cannot be written: {why}", path.display())
    })
}

/// Copies the certificates of each of Android's folders into its folder under `root`, keeping
/// their names. It runs once at boot, before init binds the folders over Android's. A folder
/// is made under another name and given its own only once every file is in it: init's bind of
/// a folder that is not there fails and leaves Android's in place, so a copy that went wrong
/// never hides Android's certificates behind an empty or partial folder. Returns what went
/// wrong, one line each.
pub fn setup(root: &Path, folders: &[(&str, &str)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (folder, android) in folders {
        let to = root.join(folder);
        if to.is_dir() {
            continue;
        }
        let partial = root.join(format!(".{folder}.partial"));
        let _ = fs::remove_dir_all(&partial);
        match copy_folder(Path::new(android), &partial) {
            Ok(()) => {
                if let Err(why) = fs::rename(&partial, &to) {
                    problems.push(format!("{} cannot be made: {why}", to.display()));
                }
            }
            Err(why) => {
                let _ = fs::remove_dir_all(&partial);
                problems.push(format!(
                    "{android} is left as it is, without the host's certificates: {why}"
                ));
            }
        }
    }
    problems
}

/// Copies every file of `from` into the new folder `to`, readable by every app.
fn copy_folder(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir(to).map_err(|why| format!("{} cannot be made: {why}", to.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(to, fs::Permissions::from_mode(0o755))
            .map_err(|why| format!("{} cannot be opened to apps: {why}", to.display()))?;
    }
    let entries =
        fs::read_dir(from).map_err(|why| format!("{} cannot be read: {why}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|why| format!("{} cannot be read: {why}", from.display()))?;
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let data = fs::read(entry.path())
            .map_err(|why| format!("{} cannot be read: {why}", entry.path().display()))?;
        write_atomically(&to.join(entry.file_name()), &data)?;
    }
    Ok(())
}

/// The DER of the certificate in a file of Android's folder: PEM (with or without the text
/// Android puts after it) or DER.
pub fn certificate_of(bytes: &[u8]) -> Option<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    if bytes.first() == Some(&0x30) {
        return Some(bytes.to_vec());
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let start = text.find(BEGIN)? + BEGIN.len();
    let end = start + text[start..].find(END)?;
    base64_decode(&text[start..end])
}

/// A certificate as PEM, which Android's certificate readers take.
pub fn pem(der: &[u8]) -> String {
    let body = base64_encode(der);
    let mut text = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in body.as_bytes().chunks(64) {
        text.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        text.push('\n');
    }
    text.push_str("-----END CERTIFICATE-----\n");
    text
}

/// The name Android gives a certificate's file before the number: the first four bytes of
/// the MD5 of its subject's DER, as a little-endian number in eight hexadecimal digits
/// (OpenSSL's `X509_NAME_hash_old`, which Android's `TrustedCertificateStore` uses).
pub fn subject_hash_old(der: &[u8]) -> Result<String, String> {
    let digest = md5(subject_der(der)?);
    let value = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
    Ok(format!("{value:08x}"))
}

/// One DER element: its tag, the whole element and its contents.
struct Element<'a> {
    tag: u8,
    whole: &'a [u8],
    contents: &'a [u8],
}

/// Reads the DER element at the start of `data`, and returns it and what follows.
fn element(data: &[u8]) -> Result<(Element<'_>, &[u8]), String> {
    let tag = *data.first().ok_or("the certificate ends early")?;
    if tag & 0x1f == 0x1f {
        return Err("the certificate has a tag of more than one byte".into());
    }
    let first = *data.get(1).ok_or("the certificate ends early")?;
    let (len, header) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 {
            return Err("the certificate has a length DER does not allow".into());
        }
        let bytes = data.get(2..2 + count).ok_or("the certificate ends early")?;
        let len = bytes.iter().fold(0usize, |len, b| (len << 8) | *b as usize);
        (len, 2 + count)
    };
    let end = header
        .checked_add(len)
        .filter(|end| *end <= data.len())
        .ok_or("the certificate ends early")?;
    Ok((
        Element {
            tag,
            whole: &data[..end],
            contents: &data[header..end],
        },
        &data[end..],
    ))
}

/// The DER of a certificate's subject name, tag and length included.
pub fn subject_der(der: &[u8]) -> Result<&[u8], String> {
    const SEQUENCE: u8 = 0x30;
    let (certificate, _) = element(der)?;
    if certificate.tag != SEQUENCE {
        return Err("it is not an X.509 certificate".into());
    }
    let (tbs, _) = element(certificate.contents)?;
    if tbs.tag != SEQUENCE {
        return Err("it is not an X.509 certificate".into());
    }
    let mut rest = tbs.contents;
    let (first, after) = element(rest)?;
    // The version, [0], is there for every certificate but version 1.
    if first.tag == 0xa0 {
        rest = after;
    }
    let (serial, rest) = element(rest)?;
    if serial.tag != 0x02 {
        return Err("the certificate has no serial number".into());
    }
    let (_signature, rest) = element(rest)?;
    let (_issuer, rest) = element(rest)?;
    let (_validity, rest) = element(rest)?;
    let (subject, _) = element(rest)?;
    if subject.tag != SEQUENCE {
        return Err("the certificate has no subject".into());
    }
    Ok(subject.whole)
}

/// MD5 (RFC 1321). Only for the file names Android expects, never for anything that has to
/// resist an attacker.
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    for block in message.chunks(64) {
        let m: Vec<u32> = block
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | ((*b as u32) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut bits, mut count) = (0u32, 0);
    for c in text.bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            break;
        }
        let value = BASE64.iter().position(|b| *b == c)? as u32;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn fr28_md5_matches_the_rfc_1321_vectors() {
        let cases = [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (input, digest) in cases {
            assert_eq!(hex(&md5(input.as_bytes())), digest, "MD5 of {input:?}");
        }
    }

    #[test]
    fn fr28_pem_round_trips_through_base64() {
        for len in 0..10 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 1) as u8).collect();
            let mut file = pem(&data).into_bytes();
            file.extend_from_slice(b"Certificate:\n    Data: ...\n");
            assert_eq!(certificate_of(&file), Some(data.clone()), "{len} bytes");
        }
    }
}
