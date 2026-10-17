//! dsa-release: the project's side of updates. It makes the signing key, describes image
//! files for the release list, writes the program list for a setup file, and signs both
//! lists.
//!
//! ```text
//! dsa-release keygen <secret.key>              writes a new key, prints its public half
//! dsa-release public-key <secret.key>          prints the public half of a key
//! dsa-release entry <version> <major> <build> <base-url> <file>...
//!                                              prints one image entry for the list
//! dsa-release image-entry <base-url> <image-dir>
//!                                              the same, for a folder made by make-image.py:
//!                                              its image.json and the files it names
//! dsa-release program <version> <setup-url> <setup-file>
//!                                              prints the program list for a setup file
//! dsa-release sign <secret.key> <list.json>    writes <list.json>.sig
//! dsa-release verify <public-key> <list.json>  checks <list.json>.sig and the list
//! dsa-release verify-program <public-key> <program.json>
//!                                              the same, for the program list
//! ```
//!
//! The secret key is 32 bytes in hexadecimal. Keep it out of the repository; CI reads it
//! from a secret. The public key goes into the service's configuration.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use dsa_host_core::update::manifest::{self, FileEntry, ImageEntry};
use dsa_host_core::update::program::{self, ProgramManifest, SetupFile, Version};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: dsa-release keygen <secret.key> | public-key <secret.key> | \
                     entry <version> <major> <build> <base-url> <file>... | \
                     image-entry <base-url> <image-dir> | \
                     program <version> <setup-url> <setup-file> | \
                     sign <secret.key> <list.json> | verify <public-key> <list.json> | \
                     verify-program <public-key> <program.json>";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_key(path: &str) -> Result<SigningKey, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
    let text = text.trim();
    let mut seed = [0u8; 32];
    if text.len() != 64 {
        return Err(format!("{path} does not hold a 32-byte key in hexadecimal"));
    }
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("{path} does not hold a 32-byte key in hexadecimal"))?;
    }
    Ok(SigningKey::from_bytes(&seed))
}

fn keygen(path: &str) -> Result<String, String> {
    if Path::new(path).exists() {
        return Err(format!("{path} exists; a signing key is never overwritten"));
    }
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| format!("no random bytes: {e}"))?;
    std::fs::write(path, format!("{}\n", hex(&seed)))
        .map_err(|e| format!("writing {path}: {e}"))?;
    Ok(hex(SigningKey::from_bytes(&seed)
        .verifying_key()
        .as_bytes()))
}

fn entry(args: &[String]) -> Result<String, String> {
    let [version, major, build, base, files @ ..] = args else {
        return Err(USAGE.into());
    };
    if files.is_empty() {
        return Err("name at least one image file".into());
    }
    let image = ImageEntry {
        version: version.clone(),
        android_major: major
            .parse()
            .map_err(|_| format!("{major:?} is not a major version"))?,
        build: build
            .parse()
            .map_err(|_| format!("{build:?} is not a build number"))?,
        files: describe(base, files.iter().map(Path::new))?,
    };
    Ok(serde_json::to_string_pretty(&image).expect("an entry serialises"))
}

/// An entry for an image folder: the version and build come from its `image.json`, and
/// the files are `image.json` and every file it names. Anything else in the folder, such
/// as the signed build inputs, is not part of the image.
fn image_entry(base: &str, dir: &str) -> Result<String, String> {
    let dir = Path::new(dir);
    let manifest_path = dir.join("image.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("reading {}: {e}", manifest_path.display()))?;
    let manifest: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not JSON: {e}", manifest_path.display()))?;
    let field = |name: &str| {
        manifest
            .get(name)
            .ok_or_else(|| format!("{} has no {name}", manifest_path.display()))
    };
    let version = field("version")?
        .as_str()
        .ok_or("the version in image.json is not text")?
        .to_string();
    let android_major = field("android_major")?
        .as_u64()
        .and_then(|m| u32::try_from(m).ok())
        .ok_or("android_major in image.json is not a number")?;
    let build = field("build")?
        .as_u64()
        .ok_or("build in image.json is not a number")?;
    let mut names: Vec<String> = field("files")?
        .as_object()
        .ok_or("files in image.json is not an object")?
        .keys()
        .cloned()
        .collect();
    names.push("image.json".into());
    names.sort();
    let paths: Vec<_> = names.iter().map(|n| dir.join(n)).collect();
    let image = ImageEntry {
        version,
        android_major,
        build,
        files: describe(base, paths.iter().map(|p| p.as_path()))?,
    };
    Ok(serde_json::to_string_pretty(&image).expect("an entry serialises"))
}

/// The list's description of each file: its name, where it will be served, size and hash.
fn describe<'a>(
    base: &str,
    files: impl Iterator<Item = &'a Path>,
) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    for path in files {
        let file = path.display().to_string();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{file} has no file name"))?;
        let (size, sha256) = measure(path)?;
        entries.push(FileEntry {
            url: format!("{}/{name}", base.trim_end_matches('/')),
            name,
            size,
            sha256,
        });
    }
    Ok(entries)
}

/// The size of the file at `path` and its SHA-256 in hexadecimal.
fn measure(path: &Path) -> Result<(u64, String), String> {
    let file = path.display().to_string();
    let mut reader = std::fs::File::open(path).map_err(|e| format!("opening {file}: {e}"))?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("reading {file}: {e}"))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        size += n as u64;
    }
    Ok((size, hex(&hash.finalize())))
}

/// The program list offering `version`, installed by the setup file at `path`, which
/// will be served at `url`.
fn program_list(version: &str, url: &str, path: &str) -> Result<String, String> {
    Version::parse(version)?;
    let (size, sha256) = measure(Path::new(path))?;
    let list = ProgramManifest {
        format: program::FORMAT,
        version: version.to_string(),
        setup: SetupFile {
            url: url.to_string(),
            size,
            sha256,
        },
    };
    Ok(serde_json::to_string_pretty(&list).expect("a program list serialises"))
}

/// Reads `<list>` and `<list>.sig`.
fn read_signed(list: &str) -> Result<(Vec<u8>, String), String> {
    let bytes = std::fs::read(list).map_err(|e| format!("reading {list}: {e}"))?;
    let sig_path = format!("{list}.sig");
    let signature =
        std::fs::read_to_string(&sig_path).map_err(|e| format!("reading {sig_path}: {e}"))?;
    Ok((bytes, signature))
}

fn run(args: &[String]) -> Result<String, String> {
    match args {
        [command, key] if command == "keygen" => keygen(key),
        [command, key] if command == "public-key" => {
            Ok(hex(read_key(key)?.verifying_key().as_bytes()))
        }
        [command, rest @ ..] if command == "entry" => entry(rest),
        [command, base, dir] if command == "image-entry" => image_entry(base, dir),
        [command, key, list] if command == "sign" => {
            let bytes = std::fs::read(list).map_err(|e| format!("reading {list}: {e}"))?;
            let signature = hex(&read_key(key)?.sign(&bytes).to_bytes());
            let out = format!("{list}.sig");
            std::fs::write(&out, format!("{signature}\n"))
                .map_err(|e| format!("writing {out}: {e}"))?;
            Ok(format!("wrote {out}"))
        }
        [command, version, url, setup] if command == "program" => program_list(version, url, setup),
        [command, public, list] if command == "verify-program" => {
            let (bytes, signature) = read_signed(list)?;
            let key = manifest::parse_key(public).map_err(|e| e.to_string())?;
            let list = program::verify(&bytes, &signature, &[key]).map_err(|e| e.to_string())?;
            Ok(format!(
                "signed and valid: DSA {}, setup {} ({} bytes)",
                list.version, list.setup.url, list.setup.size
            ))
        }
        [command, public, list] if command == "verify" => {
            let (bytes, signature) = read_signed(list)?;
            let key = manifest::parse_key(public).map_err(|e| e.to_string())?;
            let list = manifest::verify(&bytes, &signature, &[key]).map_err(|e| e.to_string())?;
            Ok(format!(
                "signed and valid: {} images, supported Android {:?}",
                list.images.len(),
                list.supported_majors
            ))
        }
        _ => Err(USAGE.into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("dsa-release: {error}");
            ExitCode::FAILURE
        }
    }
}
