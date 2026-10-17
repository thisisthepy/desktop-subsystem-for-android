//! dsa-release: the project's side of image updates. It makes the signing key, describes
//! image files for the release list, and signs the list.
//!
//! ```text
//! dsa-release keygen <secret.key>              writes a new key, prints its public half
//! dsa-release public-key <secret.key>          prints the public half of a key
//! dsa-release entry <version> <major> <build> <base-url> <file>...
//!                                              prints one image entry for the list
//! dsa-release sign <secret.key> <list.json>    writes <list.json>.sig
//! dsa-release verify <public-key> <list.json>  checks <list.json>.sig and the list
//! ```
//!
//! The secret key is 32 bytes in hexadecimal. Keep it out of the repository; CI reads it
//! from a secret. The public key goes into the service's configuration.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use dsa_host_core::update::manifest::{self, FileEntry, ImageEntry};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: dsa-release keygen <secret.key> | public-key <secret.key> | \
                     entry <version> <major> <build> <base-url> <file>... | \
                     sign <secret.key> <list.json> | verify <public-key> <list.json>";

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
    let mut entries = Vec::new();
    for file in files {
        let path = Path::new(file);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{file} has no file name"))?;
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
        entries.push(FileEntry {
            url: format!("{}/{name}", base.trim_end_matches('/')),
            name,
            size,
            sha256: hex(&hash.finalize()),
        });
    }
    let image = ImageEntry {
        version: version.clone(),
        android_major: major
            .parse()
            .map_err(|_| format!("{major:?} is not a major version"))?,
        build: build
            .parse()
            .map_err(|_| format!("{build:?} is not a build number"))?,
        files: entries,
    };
    Ok(serde_json::to_string_pretty(&image).expect("an entry serialises"))
}

fn run(args: &[String]) -> Result<String, String> {
    match args {
        [command, key] if command == "keygen" => keygen(key),
        [command, key] if command == "public-key" => {
            Ok(hex(read_key(key)?.verifying_key().as_bytes()))
        }
        [command, rest @ ..] if command == "entry" => entry(rest),
        [command, key, list] if command == "sign" => {
            let bytes = std::fs::read(list).map_err(|e| format!("reading {list}: {e}"))?;
            let signature = hex(&read_key(key)?.sign(&bytes).to_bytes());
            let out = format!("{list}.sig");
            std::fs::write(&out, format!("{signature}\n"))
                .map_err(|e| format!("writing {out}: {e}"))?;
            Ok(format!("wrote {out}"))
        }
        [command, public, list] if command == "verify" => {
            let bytes = std::fs::read(list).map_err(|e| format!("reading {list}: {e}"))?;
            let sig_path = format!("{list}.sig");
            let signature = std::fs::read_to_string(&sig_path)
                .map_err(|e| format!("reading {sig_path}: {e}"))?;
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
