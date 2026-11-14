//! How a backup file is encrypted, and the recovery key that carries the backup key to
//! another computer.
//!
//! A file is a header and then the data in chunks of [`CHUNK`] bytes, each sealed with
//! XChaCha20-Poly1305 (the STREAM construction): the nonce is the file's random prefix, the
//! chunk's number and whether it is the last one. A chunk cannot be moved, dropped or added,
//! and the file cannot be cut short, without the read failing. Every chunk is also bound to
//! the file's slot (which package, which kind of data), so one app's file put in another's
//! place does not open either.
//!
//! ```text
//! "DSABAK1\0" | key id (8) | nonce prefix (19) | sealed chunks...
//! ```
//!
//! The key id lets a read say "this folder was made with another key" rather than only that
//! the data is damaged. It is derived from the key one way, so it says nothing of the key.

use std::io::{self, BufRead, BufReader, Read, Write};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use dsa_protocol::auth::hmac_sha256;
use sha2::{Digest, Sha256};

use crate::host_keys::{Secret, SECRET_LEN};

/// What every backup file starts with.
pub const MAGIC: &[u8; 8] = b"DSABAK1\0";

/// The plaintext bytes in each chunk but the last.
pub const CHUNK: usize = 64 * 1024;

const TAG: usize = 16;
const PREFIX: usize = 19;
/// The header's size: the magic, the key id and the nonce prefix.
pub const HEADER: usize = 8 + 8 + PREFIX;

/// The keys a backup key gives: one encrypts, the other names the key.
#[derive(Clone)]
pub struct BackupKey {
    cipher: XChaCha20Poly1305,
    id: [u8; 8],
}

impl BackupKey {
    pub fn new(secret: &Secret) -> BackupKey {
        let mut encryption = hmac_sha256(secret.bytes(), b"DSA backup encryption key v1");
        let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&encryption));
        encryption.fill(0);
        let id = hmac_sha256(secret.bytes(), b"DSA backup key id v1");
        BackupKey {
            cipher,
            id: id[..8].try_into().expect("8 of 32 bytes"),
        }
    }

    /// The key's id, as the folder's metadata records it.
    pub fn id(&self) -> [u8; 8] {
        self.id
    }

    pub fn id_hex(&self) -> String {
        dsa_protocol::auth::to_hex(&self.id)
    }
}

fn nonce(prefix: &[u8; PREFIX], counter: u32, last: bool) -> XNonce {
    let mut nonce = [0u8; 24];
    nonce[..PREFIX].copy_from_slice(prefix);
    nonce[PREFIX..PREFIX + 4].copy_from_slice(&counter.to_be_bytes());
    nonce[23] = last as u8;
    *XNonce::from_slice(&nonce)
}

/// Writes data encrypted to `inner`, in chunks. [`finish`](Self::finish) seals the last chunk;
/// a writer dropped without it leaves a file no read accepts.
pub struct EncryptingWriter<W: Write> {
    inner: W,
    key: BackupKey,
    prefix: [u8; PREFIX],
    aad: Vec<u8>,
    counter: u32,
    buffer: Vec<u8>,
    written: u64,
}

impl<W: Write> EncryptingWriter<W> {
    /// Starts a file for the slot `slot` (such as `com.example/full`), writing its header.
    pub fn new(mut inner: W, key: &BackupKey, slot: &str) -> io::Result<EncryptingWriter<W>> {
        let mut prefix = [0u8; PREFIX];
        getrandom::getrandom(&mut prefix).map_err(|e| io::Error::other(e.to_string()))?;
        let mut header = Vec::with_capacity(HEADER);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&key.id);
        header.extend_from_slice(&prefix);
        inner.write_all(&header)?;
        let mut aad = header;
        aad.extend_from_slice(slot.as_bytes());
        Ok(EncryptingWriter {
            inner,
            key: key.clone(),
            prefix,
            aad,
            counter: 0,
            buffer: Vec::with_capacity(CHUNK),
            written: 0,
        })
    }

    /// The plaintext bytes written so far.
    pub fn written(&self) -> u64 {
        self.written
    }

    fn seal(&mut self, last: bool) -> io::Result<()> {
        if self.counter == u32::MAX {
            return Err(io::Error::other("a backup file is limited to 256 TiB"));
        }
        let sealed = self
            .key
            .cipher
            .encrypt(
                &nonce(&self.prefix, self.counter, last),
                Payload {
                    msg: &self.buffer,
                    aad: &self.aad,
                },
            )
            .map_err(|_| io::Error::other("encrypting a backup chunk failed"))?;
        self.inner.write_all(&sealed)?;
        self.counter += 1;
        self.buffer.clear();
        Ok(())
    }

    /// Seals the last chunk and hands back the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.seal(true)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for EncryptingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut taken = 0;
        while taken < data.len() {
            if self.buffer.len() == CHUNK {
                // A full chunk is sealed only once more data comes, so the last chunk is
                // never a full one followed by nothing.
                self.seal(false)?;
            }
            let room = CHUNK - self.buffer.len();
            let n = room.min(data.len() - taken);
            self.buffer.extend_from_slice(&data[taken..taken + n]);
            taken += n;
        }
        self.written += data.len() as u64;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Why a backup file did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// It is not a backup file DSA made.
    NotBackup,
    /// It was encrypted with another key (the id it records).
    OtherKey(String),
    /// It was changed or cut short, or belongs to another slot.
    Damaged,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotBackup => f.write_str("it is not a DSA backup file"),
            OpenError::OtherKey(id) => write!(
                f,
                "it was encrypted with another backup key (key {id}); enter that key's recovery \
                 key to read it"
            ),
            OpenError::Damaged => f.write_str("it was changed or damaged after it was written"),
        }
    }
}

fn damaged() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, OpenError::Damaged.to_string())
}

/// Reads the plaintext of a backup file. Every chunk is checked before any of its bytes are
/// given out, and a file that ends before its last chunk is an error, not a short read.
pub struct DecryptingReader<R: Read> {
    inner: BufReader<R>,
    key: BackupKey,
    prefix: [u8; PREFIX],
    aad: Vec<u8>,
    counter: u32,
    plain: Vec<u8>,
    at: usize,
    done: bool,
}

impl<R: Read> DecryptingReader<R> {
    /// Opens the file of slot `slot` with `key`, reading and checking its header.
    pub fn new(inner: R, key: &BackupKey, slot: &str) -> Result<DecryptingReader<R>, OpenError> {
        let mut inner = BufReader::with_capacity(CHUNK + TAG + 1, inner);
        let mut header = [0u8; HEADER];
        inner
            .read_exact(&mut header)
            .map_err(|_| OpenError::NotBackup)?;
        if &header[..8] != MAGIC {
            return Err(OpenError::NotBackup);
        }
        if header[8..16] != key.id {
            return Err(OpenError::OtherKey(dsa_protocol::auth::to_hex(
                &header[8..16],
            )));
        }
        let prefix: [u8; PREFIX] = header[16..].try_into().expect("19 bytes");
        let mut aad = header.to_vec();
        aad.extend_from_slice(slot.as_bytes());
        Ok(DecryptingReader {
            inner,
            key: key.clone(),
            prefix,
            aad,
            counter: 0,
            plain: Vec::new(),
            at: 0,
            done: false,
        })
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        let mut sealed = Vec::with_capacity(CHUNK + TAG);
        (&mut self.inner)
            .take((CHUNK + TAG) as u64)
            .read_to_end(&mut sealed)?;
        let last = self.inner.fill_buf()?.is_empty();
        if sealed.len() < TAG || (!last && sealed.len() != CHUNK + TAG) {
            return Err(damaged());
        }
        let plain = self
            .key
            .cipher
            .decrypt(
                &nonce(&self.prefix, self.counter, last),
                Payload {
                    msg: &sealed,
                    aad: &self.aad,
                },
            )
            .map_err(|_| damaged())?;
        self.counter = self.counter.checked_add(1).ok_or_else(damaged)?;
        self.plain = plain;
        self.at = 0;
        self.done = last;
        Ok(())
    }

    /// Reads everything, checking the whole file.
    pub fn read_all(mut self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.read_to_end(&mut out)?;
        Ok(out)
    }
}

impl<R: Read> Read for DecryptingReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.plain.len() {
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = out.len().min(self.plain.len() - self.at);
        out[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// The letters a recovery key is written in: Crockford's base 32, which leaves out I, L, O and
/// U so nothing written down reads as something else.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// How many characters each group of a recovery key holds.
const GROUP: usize = 5;

/// The recovery key of `secret`: the key and a two-byte check, in base 32, in groups of five.
pub fn recovery_key(secret: &Secret) -> String {
    let mut bytes = secret.bytes().to_vec();
    bytes.extend_from_slice(&Sha256::digest(secret.bytes())[..2]);
    let mut text = String::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for byte in bytes {
        acc = (acc << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            text.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        text.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(GROUP)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// The secret a recovery key carries. Spaces, dashes and case do not matter, and I, L and O
/// read as 1, 1 and 0. `Err` says what is wrong with the text.
pub fn parse_recovery_key(text: &str) -> Result<Secret, String> {
    let mut values = Vec::new();
    for c in text.chars() {
        let c = c.to_ascii_uppercase();
        let c = match c {
            '-' | ' ' | '\t' | '\n' | '\r' => continue,
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        };
        match ALPHABET.iter().position(|&a| a as char == c) {
            Some(value) => values.push(value as u32),
            None => {
                return Err(format!(
                    "{c:?} is not in a recovery key, which has only digits and letters other \
                     than U"
                ))
            }
        }
    }
    let want = (SECRET_LEN + 2) * 8;
    if values.len() != want.div_ceil(5) {
        return Err(format!(
            "a recovery key has {} letters and digits, not {}",
            want.div_ceil(5),
            values.len()
        ));
    }
    let mut bytes = Vec::with_capacity(SECRET_LEN + 2);
    let (mut acc, mut bits) = (0u32, 0u32);
    for value in values {
        acc = (acc << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push(((acc >> bits) & 0xff) as u8);
        }
    }
    bytes.truncate(SECRET_LEN + 2);
    let secret = Secret::from_slice(&bytes[..SECRET_LEN]).expect("32 bytes");
    if Sha256::digest(secret.bytes())[..2] != bytes[SECRET_LEN..] {
        return Err("the recovery key was not written down right: its check does not match".into());
    }
    Ok(secret)
}
