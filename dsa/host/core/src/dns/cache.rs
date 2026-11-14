//! The DNS service's small cache of answers.
//!
//! The guest's own resolver caches too, so this one only spares the host's resolver the
//! questions several programs in the guest ask at once, and the same question over UDP and
//! then TCP. An answer is kept for its smallest TTL, an empty answer or `NXDOMAIN` for its
//! SOA's negative TTL (RFC 2308), and none longer than [`MAX_TTL`]. The records' TTLs count
//! down while it is kept, so the guest never keeps a record longer than the host's resolver
//! said. A failure, a truncated answer and an answer with a TTL of 0 are never kept.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::wire::{Message, Question, Rcode, Rdata, TYPE_SOA};

/// The longest an answer is kept.
pub const MAX_TTL: u32 = 300;

/// How long `NXDOMAIN` or an empty answer is kept when the answer has no SOA to say.
pub const NEGATIVE_TTL: u32 = 30;

/// How many answers are kept at most.
pub const MAX_ENTRIES: usize = 1024;

/// What an answer is kept under: the question, and the flags that change what the answer
/// holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    name: Vec<u8>,
    qtype: u16,
    qclass: u16,
    dnssec_ok: bool,
    checking_disabled: bool,
}

impl CacheKey {
    pub fn of(question: &Question, query: &Message) -> CacheKey {
        CacheKey {
            name: question.name.lowercase().wire().to_vec(),
            qtype: question.qtype,
            qclass: question.qclass,
            dnssec_ok: query.edns.is_some_and(|e| e.dnssec_ok),
            checking_disabled: query.checking_disabled(),
        }
    }
}

struct Entry {
    response: Message,
    stored: Instant,
    ttl: u32,
}

/// The answers kept.
#[derive(Default)]
pub struct Cache {
    entries: HashMap<CacheKey, Entry>,
}

impl Cache {
    /// The answer kept under `key`, its TTLs counted down to now.
    pub fn get(&mut self, key: &CacheKey) -> Option<Message> {
        self.get_at(key, Instant::now())
    }

    /// [`Cache::get`] at `now`.
    pub fn get_at(&mut self, key: &CacheKey, now: Instant) -> Option<Message> {
        let entry = self.entries.get(key)?;
        let age = now.saturating_duration_since(entry.stored).as_secs();
        if age >= entry.ttl as u64 {
            self.entries.remove(key);
            return None;
        }
        let mut response = entry.response.clone();
        response.age(age as u32);
        Some(response)
    }

    /// Keeps `response` under `key`, if it may be kept.
    pub fn put(&mut self, key: CacheKey, response: &Message) {
        self.put_at(key, response, Instant::now());
    }

    /// [`Cache::put`] at `now`.
    pub fn put_at(&mut self, key: CacheKey, response: &Message, now: Instant) {
        let Some(ttl) = keep_for(response) else {
            return;
        };
        if self.entries.len() >= MAX_ENTRIES && !self.entries.contains_key(&key) {
            self.entries.retain(|_, entry| {
                now.saturating_duration_since(entry.stored) < Duration::from_secs(entry.ttl as u64)
            });
            if self.entries.len() >= MAX_ENTRIES {
                if let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.stored)
                    .map(|(key, _)| key.clone())
                {
                    self.entries.remove(&oldest);
                }
            }
        }
        self.entries.insert(
            key,
            Entry {
                response: response.clone(),
                stored: now,
                ttl,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// How long `response` may be kept, or `None` when it may not.
pub fn keep_for(response: &Message) -> Option<u32> {
    if response.truncated() {
        return None;
    }
    let ttl = match response.rcode {
        Rcode::NoError if !response.answers.is_empty() => {
            response.answers.iter().map(|r| r.ttl).min()?
        }
        Rcode::NoError | Rcode::NxDomain => response
            .authority
            .iter()
            .find_map(|record| match (&record.data, record.rtype) {
                (Rdata::Soa { minimum, .. }, TYPE_SOA) => Some(record.ttl.min(*minimum)),
                _ => None,
            })
            .unwrap_or(NEGATIVE_TTL),
        _ => return None,
    };
    (ttl > 0).then_some(ttl.min(MAX_TTL))
}
