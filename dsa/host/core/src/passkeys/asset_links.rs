//! Digital Asset Links: whether a website lets an Android app use its passkeys.
//!
//! A relying party lets an app sign in with its passkeys by listing the app in
//! `https://<rp id>/.well-known/assetlinks.json`: a statement with the relation
//! `delegate_permission/common.get_login_creds` whose target is the app's package and the
//! SHA-256 fingerprint of its signing certificate. That is the check Android's own
//! credential providers make. A statement may instead point to another statement list
//! (`include`), which is followed as far as [`MAX_INCLUDES`] lists.
//!
//! What counts:
//! - Only the file at that exact place, fetched over HTTPS with the certificate checked, and
//!   with no redirect followed. A redirect could hand the check to another site.
//! - Only `delegate_permission/common.get_login_creds`. `handle_all_urls` lets an app open a
//!   site's links, which says nothing about its sign-in.
//! - Every certificate the app is signed with now must be listed. An app that has several
//!   signers is the set of them; one listed signer is not the app.
//! - A file that cannot be had, is larger than [`MAX_FILE_BYTES`], or is not a JSON list of
//!   statements verifies nothing, and the request is refused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The relation that lets an app use a site's sign-in credentials.
pub const GET_LOGIN_CREDS: &str = "delegate_permission/common.get_login_creds";

/// The largest statement list that is read.
pub const MAX_FILE_BYTES: usize = 128 * 1024;

/// The most statement lists one check reads, the site's own and those it includes.
pub const MAX_INCLUDES: usize = 10;

/// How long a site's answer is kept: a yes for ten minutes, a no for one, so a site that
/// fixes its file is not refused for long.
pub const VERIFIED_FOR: Duration = Duration::from_secs(600);
pub const REFUSED_FOR: Duration = Duration::from_secs(60);

/// Fetches a document over HTTPS. The platform's checks the server's certificate and never
/// follows a redirect: a redirect is an error.
pub trait HttpsGet: Send + Sync {
    /// The body of `url`, at most `limit` bytes; a longer one is an error.
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, String>;
}

/// The SHA-256 fingerprint of a certificate as asset links write it: hex pairs, colons
/// between them.
pub fn fingerprint(sha256: &[u8]) -> String {
    sha256
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn parse_fingerprint(text: &str) -> Option<Vec<u8>> {
    let bytes: Option<Vec<u8>> = text
        .split(':')
        .map(|pair| {
            (pair.len() == 2)
                .then(|| u8::from_str_radix(pair, 16).ok())
                .flatten()
        })
        .collect();
    bytes.filter(|b| b.len() == 32)
}

/// What one statement list says: the apps it lets use sign-in credentials, as
/// (package, fingerprints), and the lists it includes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Statements {
    pub apps: Vec<(String, Vec<Vec<u8>>)>,
    pub includes: Vec<String>,
}

/// Reads a statement list. Statements it cannot read are skipped, never taken as a yes.
pub fn parse_statements(data: &[u8]) -> Result<Statements, String> {
    let value: Value = serde_json::from_slice(data).map_err(|e| format!("it is not JSON ({e})"))?;
    let Value::Array(list) = value else {
        return Err("it is not a list of statements".into());
    };
    let mut statements = Statements::default();
    for statement in list {
        if let Some(include) = statement.get("include").and_then(Value::as_str) {
            statements.includes.push(include.to_string());
            continue;
        }
        let relations = statement
            .get("relation")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        if !relations.contains(&GET_LOGIN_CREDS) {
            continue;
        }
        let Some(target) = statement.get("target") else {
            continue;
        };
        if target.get("namespace").and_then(Value::as_str) != Some("android_app") {
            continue;
        }
        let Some(package) = target.get("package_name").and_then(Value::as_str) else {
            continue;
        };
        let fingerprints: Vec<Vec<u8>> = target
            .get("sha256_cert_fingerprints")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .filter_map(parse_fingerprint)
                    .collect()
            })
            .unwrap_or_default();
        statements.apps.push((package.to_string(), fingerprints));
    }
    Ok(statements)
}

/// Whether `statements` let the app `package`, signed by every certificate of `signers`, use
/// sign-in credentials: one statement must name the package and list every one of them.
pub fn lets_app(statements: &Statements, package: &str, signers: &[Vec<u8>]) -> bool {
    !signers.is_empty()
        && statements.apps.iter().any(|(listed, fingerprints)| {
            listed == package && signers.iter().all(|s| fingerprints.contains(s))
        })
}

/// The place a site's statement list is at.
pub fn statements_url(rp_id: &str) -> String {
    format!("https://{rp_id}/.well-known/assetlinks.json")
}

/// A relying party, a package and its signers: one question to a statement list.
type LinkQuestion = (String, String, Vec<Vec<u8>>);

/// An answer, and until when it is kept.
type KeptAnswer = (Instant, Result<(), String>);

/// Checks apps against relying parties' asset links, keeping each answer a while.
pub struct AssetLinks {
    fetch: Arc<dyn HttpsGet>,
    cache: Mutex<HashMap<LinkQuestion, KeptAnswer>>,
}

impl AssetLinks {
    pub fn new(fetch: Arc<dyn HttpsGet>) -> AssetLinks {
        AssetLinks {
            fetch,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// `Ok` when the relying party `rp_id` lets the app `package` signed by `signers` use
    /// its passkeys; `Err` says why not.
    pub fn verify(&self, rp_id: &str, package: &str, signers: &[Vec<u8>]) -> Result<(), String> {
        let key = (rp_id.to_string(), package.to_string(), signers.to_vec());
        let now = Instant::now();
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((until, answer)) = cache.get(&key) {
                if *until > now {
                    return answer.clone();
                }
            }
        }
        let answer = self.check(rp_id, package, signers);
        let keep = if answer.is_ok() {
            VERIFIED_FOR
        } else {
            REFUSED_FOR
        };
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|_, (until, _)| *until > now);
        cache.insert(key, (now + keep, answer.clone()));
        answer
    }

    fn check(&self, rp_id: &str, package: &str, signers: &[Vec<u8>]) -> Result<(), String> {
        let mut pending = vec![statements_url(rp_id)];
        let mut read = 0;
        let mut last_problem = None;
        while let Some(url) = pending.pop() {
            if read == MAX_INCLUDES {
                break;
            }
            read += 1;
            let statements = match self
                .fetch
                .get(&url, MAX_FILE_BYTES)
                .and_then(|data| parse_statements(&data))
            {
                Ok(statements) => statements,
                Err(why) => {
                    last_problem = Some(format!("{url} cannot be read: {why}"));
                    continue;
                }
            };
            if lets_app(&statements, package, signers) {
                return Ok(());
            }
            for include in statements.includes {
                if include.starts_with("https://") {
                    pending.push(include);
                }
            }
        }
        Err(last_problem.unwrap_or_else(|| {
            format!(
                "{rp_id} does not list {package} with its signing certificate ({}) for sign-in \
                 in {}",
                signers
                    .iter()
                    .map(|s| fingerprint(s))
                    .collect::<Vec<_>>()
                    .join(", "),
                statements_url(rp_id)
            )
        }))
    }
}
