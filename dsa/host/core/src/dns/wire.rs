//! DNS messages on the wire (RFC 1035, with EDNS from RFC 6891): reading a program's query,
//! reading a resolver's response, and writing the answer the program gets.
//!
//! Names are kept as their uncompressed wire form ([`Name`]), so a label may hold any byte,
//! including a dot. Reading follows compression pointers, with a bound on how many, so a
//! message that points in a loop is refused rather than followed forever. Writing compresses
//! names again, the owner names and the names inside the record types RFC 1035 lets a server
//! compress (`CNAME`, `NS`, `PTR`, `MX`, `SOA`); every other record's data goes out as it came.
//!
//! Nothing here panics on what a peer sends: a short or malformed message is a [`WireError`].

use std::fmt;

pub const TYPE_A: u16 = 1;
pub const TYPE_NS: u16 = 2;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_SOA: u16 = 6;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_MX: u16 = 15;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_SRV: u16 = 33;
pub const TYPE_DNAME: u16 = 39;
pub const TYPE_OPT: u16 = 41;

pub const CLASS_IN: u16 = 1;

/// The header's length.
pub const HEADER_LEN: usize = 12;

/// The largest answer over UDP to a program that does not say it takes more (RFC 1035).
pub const CLASSIC_UDP_LIMIT: usize = 512;

/// The UDP size the answer's EDNS record says the host takes (RFC 9715's advice).
pub const EDNS_UDP_SIZE: u16 = 1232;

/// The largest DNS message, which is what TCP's two length bytes allow.
pub const MAX_MESSAGE_LEN: usize = 65535;

/// The longest name on the wire, its length bytes and the root's zero included.
const MAX_NAME_LEN: usize = 255;

/// How many compression pointers one name may follow.
const MAX_POINTERS: usize = 64;

const FLAG_QR: u16 = 0x8000;
const FLAG_AA: u16 = 0x0400;
const FLAG_TC: u16 = 0x0200;
const FLAG_RD: u16 = 0x0100;
const FLAG_RA: u16 = 0x0080;
const FLAG_AD: u16 = 0x0020;
const FLAG_CD: u16 = 0x0010;
const OPCODE_MASK: u16 = 0x7800;

/// The EDNS flag that asks for DNSSEC records.
const EDNS_DO: u32 = 0x8000;

/// A response code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rcode {
    NoError,
    FormErr,
    ServFail,
    NxDomain,
    NotImp,
    Refused,
    /// Any other code, as its number.
    Other(u8),
}

impl Rcode {
    pub fn from_code(code: u8) -> Rcode {
        match code {
            0 => Rcode::NoError,
            1 => Rcode::FormErr,
            2 => Rcode::ServFail,
            3 => Rcode::NxDomain,
            4 => Rcode::NotImp,
            5 => Rcode::Refused,
            other => Rcode::Other(other),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Rcode::NoError => 0,
            Rcode::FormErr => 1,
            Rcode::ServFail => 2,
            Rcode::NxDomain => 3,
            Rcode::NotImp => 4,
            Rcode::Refused => 5,
            // A code that needs EDNS's extended bits cannot be written in the header alone.
            Rcode::Other(code) if code <= 0x0f => code,
            Rcode::Other(_) => 2,
        }
    }
}

/// Why a message could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub String);

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WireError {}

fn error<T>(why: impl Into<String>) -> Result<T, WireError> {
    Err(WireError(why.into()))
}

/// A domain name in its uncompressed wire form: each label with its length byte, then the
/// root's zero.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Name(Vec<u8>);

impl Name {
    /// The root name.
    pub fn root() -> Name {
        Name(vec![0])
    }

    /// A name from its dotted text (`www.example.com`, with or without the final dot). `None`
    /// for an empty label, a label longer than 63 bytes or a name longer than 255.
    pub fn from_dotted(text: &str) -> Option<Name> {
        let text = text.strip_suffix('.').unwrap_or(text);
        let mut wire = Vec::with_capacity(text.len() + 2);
        if !text.is_empty() {
            for label in text.split('.') {
                if label.is_empty() || label.len() > 63 {
                    return None;
                }
                wire.push(label.len() as u8);
                wire.extend_from_slice(label.as_bytes());
            }
        }
        wire.push(0);
        (wire.len() <= MAX_NAME_LEN).then_some(Name(wire))
    }

    /// The wire form.
    pub fn wire(&self) -> &[u8] {
        &self.0
    }

    /// The labels, in order, without their length bytes.
    pub fn labels(&self) -> Vec<&[u8]> {
        let mut labels = Vec::new();
        let mut at = 0;
        while at < self.0.len() && self.0[at] != 0 {
            let len = self.0[at] as usize;
            labels.push(&self.0[at + 1..at + 1 + len]);
            at += 1 + len;
        }
        labels
    }

    /// The dotted text, without the final dot; a dot or backslash inside a label is escaped
    /// with a backslash, and an unprintable byte as `\DDD`. The root is `.`.
    pub fn to_dotted(&self) -> String {
        let labels = self.labels();
        if labels.is_empty() {
            return ".".into();
        }
        let mut text = String::new();
        for (i, label) in labels.iter().enumerate() {
            if i > 0 {
                text.push('.');
            }
            for &byte in *label {
                match byte {
                    b'.' | b'\\' => {
                        text.push('\\');
                        text.push(byte as char);
                    }
                    0x21..=0x7e => text.push(byte as char),
                    _ => text.push_str(&format!("\\{byte:03}")),
                }
            }
        }
        text
    }

    /// The same name in lower case, which is how DNS compares names (ASCII only). The length
    /// bytes are never letters, so lowering the whole wire form lowers the labels alone.
    pub fn lowercase(&self) -> Name {
        Name(self.0.to_ascii_lowercase())
    }

    /// Whether the two names are the same name, as DNS compares them.
    pub fn same(&self, other: &Name) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Name({})", self.to_dotted())
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_dotted())
    }
}

/// A question: what a program asks about which name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: Name,
    pub qtype: u16,
    pub qclass: u16,
}

/// What a query's EDNS record says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edns {
    /// The largest UDP answer the program takes.
    pub udp_size: u16,
    /// Whether it asks for DNSSEC records.
    pub dnssec_ok: bool,
}

/// A record's data. The record types whose data holds names a server may compress are read
/// into their parts, so the names can be written again; every other type keeps its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rdata {
    /// `CNAME`, `NS`, `PTR` and `DNAME`: one name.
    Name(Name),
    Mx {
        preference: u16,
        exchange: Name,
    },
    Soa {
        mname: Name,
        rname: Name,
        serial: u32,
        refresh: u32,
        retry: u32,
        expire: u32,
        minimum: u32,
    },
    /// `SRV`, whose target RFC 2782 forbids compressing, read so its name is checked.
    Srv {
        priority: u16,
        weight: u16,
        port: u16,
        target: Name,
    },
    /// Any other type, as its bytes.
    Raw(Vec<u8>),
}

/// One resource record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: Name,
    pub rtype: u16,
    pub class: u16,
    pub ttl: u32,
    pub data: Rdata,
}

impl Record {
    /// An `A` record.
    pub fn a(name: &Name, ttl: u32, address: std::net::Ipv4Addr) -> Record {
        Record {
            name: name.clone(),
            rtype: TYPE_A,
            class: CLASS_IN,
            ttl,
            data: Rdata::Raw(address.octets().to_vec()),
        }
    }

    /// An `AAAA` record.
    pub fn aaaa(name: &Name, ttl: u32, address: std::net::Ipv6Addr) -> Record {
        Record {
            name: name.clone(),
            rtype: TYPE_AAAA,
            class: CLASS_IN,
            ttl,
            data: Rdata::Raw(address.octets().to_vec()),
        }
    }

    /// A record of a type that holds one name (`CNAME`, `NS`, `PTR`, `DNAME`).
    pub fn with_name(name: &Name, rtype: u16, ttl: u32, target: Name) -> Record {
        Record {
            name: name.clone(),
            rtype,
            class: CLASS_IN,
            ttl,
            data: Rdata::Name(target),
        }
    }
}

/// A whole message: a query or a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    /// The header's flags and opcode, without the response code.
    pub flags: u16,
    pub rcode: Rcode,
    pub questions: Vec<Question>,
    pub answers: Vec<Record>,
    pub authority: Vec<Record>,
    /// The additional records, without the EDNS record, which is in [`Message::edns`].
    pub additional: Vec<Record>,
    pub edns: Option<Edns>,
}

impl Message {
    pub fn is_response(&self) -> bool {
        self.flags & FLAG_QR != 0
    }

    pub fn opcode(&self) -> u8 {
        ((self.flags & OPCODE_MASK) >> 11) as u8
    }

    pub fn recursion_desired(&self) -> bool {
        self.flags & FLAG_RD != 0
    }

    pub fn checking_disabled(&self) -> bool {
        self.flags & FLAG_CD != 0
    }

    pub fn authentic_data(&self) -> bool {
        self.flags & FLAG_AD != 0
    }

    pub fn truncated(&self) -> bool {
        self.flags & FLAG_TC != 0
    }

    /// Every record's TTL less `seconds`, at 0 at least, as a cached answer ages.
    pub fn age(&mut self, seconds: u32) {
        for record in self
            .answers
            .iter_mut()
            .chain(self.authority.iter_mut())
            .chain(self.additional.iter_mut())
        {
            record.ttl = record.ttl.saturating_sub(seconds);
        }
    }
}

/// Reads a message. `Err` for one too short or malformed to read.
pub fn parse(bytes: &[u8]) -> Result<Message, WireError> {
    let mut reader = Reader { bytes, at: 0 };
    let id = reader.u16()?;
    let flags_and_rcode = reader.u16()?;
    let qdcount = reader.u16()?;
    let ancount = reader.u16()?;
    let nscount = reader.u16()?;
    let arcount = reader.u16()?;
    let mut questions = Vec::new();
    for _ in 0..qdcount {
        let name = reader.name()?;
        let qtype = reader.u16()?;
        let qclass = reader.u16()?;
        questions.push(Question {
            name,
            qtype,
            qclass,
        });
    }
    let mut rcode = (flags_and_rcode & 0x000f) as u8;
    let mut answers = Vec::new();
    for _ in 0..ancount {
        answers.push(reader.record()?);
    }
    let mut authority = Vec::new();
    for _ in 0..nscount {
        authority.push(reader.record()?);
    }
    let mut additional = Vec::new();
    let mut edns = None;
    for _ in 0..arcount {
        let record = reader.record()?;
        if record.rtype == TYPE_OPT {
            // The extended response code's upper bits are in the TTL's top byte.
            let extended = (record.ttl >> 24) as u8;
            if extended != 0 {
                rcode = rcode.wrapping_add(extended.wrapping_shl(4));
            }
            edns = Some(Edns {
                udp_size: record.class,
                dnssec_ok: record.ttl & EDNS_DO != 0,
            });
        } else {
            additional.push(record);
        }
    }
    Ok(Message {
        id,
        flags: flags_and_rcode & 0xfff0,
        rcode: Rcode::from_code(rcode),
        questions,
        answers,
        authority,
        additional,
        edns,
    })
}

/// The ID of a message, from its first two bytes; `None` for a message shorter than that.
pub fn message_id(bytes: &[u8]) -> Option<u16> {
    (bytes.len() >= 2).then(|| u16::from_be_bytes([bytes[0], bytes[1]]))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let end = self
            .at
            .checked_add(len)
            .filter(|&end| end <= self.bytes.len());
        match end {
            Some(end) => {
                let bytes = &self.bytes[self.at..end];
                self.at = end;
                Ok(bytes)
            }
            None => error(format!("the message ends early, at byte {}", self.at)),
        }
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A name at the reader's place, following compression pointers into the whole message.
    fn name(&mut self) -> Result<Name, WireError> {
        let (name, end) = read_name(self.bytes, self.at)?;
        self.at = end;
        Ok(name)
    }

    fn record(&mut self) -> Result<Record, WireError> {
        let name = self.name()?;
        let rtype = self.u16()?;
        let class = self.u16()?;
        let ttl = self.u32()?;
        let len = self.u16()? as usize;
        let start = self.at;
        let raw = self.take(len)?;
        let end = start + len;
        // Names inside the data may point anywhere earlier in the message, so they are read
        // against the whole message and must end where the data does.
        let mut inner = Reader {
            bytes: &self.bytes[..end],
            at: start,
        };
        let data = match rtype {
            TYPE_CNAME | TYPE_NS | TYPE_PTR | TYPE_DNAME => Rdata::Name(inner.name()?),
            TYPE_MX => Rdata::Mx {
                preference: inner.u16()?,
                exchange: inner.name()?,
            },
            TYPE_SOA => Rdata::Soa {
                mname: inner.name()?,
                rname: inner.name()?,
                serial: inner.u32()?,
                refresh: inner.u32()?,
                retry: inner.u32()?,
                expire: inner.u32()?,
                minimum: inner.u32()?,
            },
            TYPE_SRV => Rdata::Srv {
                priority: inner.u16()?,
                weight: inner.u16()?,
                port: inner.u16()?,
                target: inner.name()?,
            },
            _ => {
                inner.at = end;
                Rdata::Raw(raw.to_vec())
            }
        };
        if inner.at != end {
            return error(format!(
                "a type {rtype} record's data is {len} bytes, but its parts take {}",
                inner.at - start
            ));
        }
        Ok(Record {
            name,
            rtype,
            class,
            ttl,
            data,
        })
    }
}

/// Reads the name at `at` in `message`, and returns it with where the bytes after it start.
fn read_name(message: &[u8], at: usize) -> Result<(Name, usize), WireError> {
    let mut wire = Vec::new();
    let mut pos = at;
    let mut end = None;
    let mut pointers = 0;
    loop {
        let Some(&len) = message.get(pos) else {
            return error(format!(
                "a name runs past the end of the message, at byte {pos}"
            ));
        };
        match len & 0xc0 {
            0x00 => {
                if len == 0 {
                    wire.push(0);
                    if wire.len() > MAX_NAME_LEN {
                        return error("a name is longer than 255 bytes");
                    }
                    return Ok((Name(wire), end.unwrap_or(pos + 1)));
                }
                let label = message
                    .get(pos + 1..pos + 1 + len as usize)
                    .ok_or_else(|| {
                        WireError(format!("a label runs past the end, at byte {pos}"))
                    })?;
                wire.push(len);
                wire.extend_from_slice(label);
                if wire.len() >= MAX_NAME_LEN {
                    return error("a name is longer than 255 bytes");
                }
                pos += 1 + len as usize;
            }
            0xc0 => {
                let Some(&low) = message.get(pos + 1) else {
                    return error("a compression pointer is cut off");
                };
                pointers += 1;
                if pointers > MAX_POINTERS {
                    return error("a name follows too many compression pointers");
                }
                end.get_or_insert(pos + 2);
                let target = (((len & 0x3f) as usize) << 8) | low as usize;
                if target >= pos {
                    // A pointer may only point back, or it could loop.
                    return error(format!(
                        "a compression pointer at byte {pos} points forward"
                    ));
                }
                pos = target;
            }
            _ => return error(format!("a label at byte {pos} has an unknown type")),
        }
    }
}

/// Writes messages, compressing names.
struct Writer {
    bytes: Vec<u8>,
    /// Each name suffix written so far, in lower case, and where it starts.
    suffixes: Vec<(Vec<u8>, usize)>,
}

impl Writer {
    fn new() -> Writer {
        Writer {
            bytes: Vec::with_capacity(512),
            suffixes: Vec::new(),
        }
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes `name`, pointing at an earlier copy of its longest suffix that was written.
    fn name(&mut self, name: &Name, compress: bool) {
        let wire = name.wire();
        let mut at = 0;
        while at < wire.len() && wire[at] != 0 {
            let suffix = wire[at..].to_ascii_lowercase();
            if compress {
                if let Some(&(_, offset)) = self.suffixes.iter().find(|(s, _)| *s == suffix) {
                    self.u16(0xc000 | offset as u16);
                    return;
                }
            }
            let offset = self.bytes.len();
            if offset < 0x4000 {
                self.suffixes.push((suffix, offset));
            }
            let len = wire[at] as usize;
            self.bytes.extend_from_slice(&wire[at..at + 1 + len]);
            at += 1 + len;
        }
        self.bytes.push(0);
    }

    fn record(&mut self, record: &Record) {
        self.name(&record.name, true);
        self.u16(record.rtype);
        self.u16(record.class);
        self.u32(record.ttl);
        let length_at = self.bytes.len();
        self.u16(0);
        let start = self.bytes.len();
        match &record.data {
            Rdata::Name(name) => self.name(name, true),
            Rdata::Mx {
                preference,
                exchange,
            } => {
                self.u16(*preference);
                self.name(exchange, true);
            }
            Rdata::Soa {
                mname,
                rname,
                serial,
                refresh,
                retry,
                expire,
                minimum,
            } => {
                self.name(mname, true);
                self.name(rname, true);
                for value in [serial, refresh, retry, expire, minimum] {
                    self.u32(*value);
                }
            }
            Rdata::Srv {
                priority,
                weight,
                port,
                target,
            } => {
                self.u16(*priority);
                self.u16(*weight);
                self.u16(*port);
                self.name(target, false);
            }
            Rdata::Raw(bytes) => self.bytes.extend_from_slice(bytes),
        }
        let len = (self.bytes.len() - start) as u16;
        self.bytes[length_at..length_at + 2].copy_from_slice(&len.to_be_bytes());
    }
}

/// Writes `message`. Its EDNS record, if it has one, goes last in the additional section.
pub fn encode(message: &Message) -> Vec<u8> {
    let mut writer = Writer::new();
    let rcode = message.rcode.code();
    writer.u16(message.id);
    writer.u16((message.flags & 0xfff0) | (rcode as u16 & 0x0f));
    writer.u16(message.questions.len() as u16);
    writer.u16(message.answers.len() as u16);
    writer.u16(message.authority.len() as u16);
    writer.u16(message.additional.len() as u16 + message.edns.is_some() as u16);
    for question in &message.questions {
        writer.name(&question.name, true);
        writer.u16(question.qtype);
        writer.u16(question.qclass);
    }
    for record in message
        .answers
        .iter()
        .chain(&message.authority)
        .chain(&message.additional)
    {
        writer.record(record);
    }
    if let Some(edns) = message.edns {
        writer.bytes.push(0);
        writer.u16(TYPE_OPT);
        writer.u16(edns.udp_size.max(CLASSIC_UDP_LIMIT as u16));
        writer.u32(if edns.dnssec_ok { EDNS_DO } else { 0 });
        writer.u16(0);
    }
    writer.bytes
}

/// The answer a program gets to `query`: `response`'s records and response code under the
/// query's ID and question, with recursion available, and an EDNS record when the query had
/// one. `limit` is the largest message the program takes; an answer larger than that goes out
/// with only its question and the TC flag, so the program asks again over TCP.
pub fn answer_for(query: &Message, response: &Message, limit: usize) -> Vec<u8> {
    let wants_dnssec = query.edns.is_some_and(|e| e.dnssec_ok) || query.authentic_data();
    let mut flags = FLAG_QR | FLAG_RA | (query.flags & (OPCODE_MASK | FLAG_RD | FLAG_CD));
    if wants_dnssec && response.authentic_data() {
        flags |= FLAG_AD;
    }
    if response.flags & FLAG_AA != 0 {
        flags |= FLAG_AA;
    }
    let mut message = Message {
        id: query.id,
        flags,
        rcode: response.rcode,
        questions: query.questions.clone(),
        answers: response.answers.clone(),
        authority: response.authority.clone(),
        additional: response.additional.clone(),
        edns: query.edns.map(|e| Edns {
            udp_size: EDNS_UDP_SIZE,
            dnssec_ok: e.dnssec_ok,
        }),
    };
    let bytes = encode(&message);
    if bytes.len() <= limit {
        return bytes;
    }
    // The additional section is only a help: an answer without it may still fit.
    message.additional.clear();
    let bytes = encode(&message);
    if bytes.len() <= limit {
        return bytes;
    }
    message.flags |= FLAG_TC;
    message.answers.clear();
    message.authority.clear();
    encode(&message)
}

/// The answer with only a response code, for a query the host cannot answer otherwise
/// (`SERVFAIL` when its resolver failed or took too long, `NOTIMP` for what it does not do).
pub fn error_answer(query: &Message, rcode: Rcode) -> Vec<u8> {
    let response = Message {
        id: query.id,
        flags: FLAG_QR,
        rcode,
        questions: Vec::new(),
        answers: Vec::new(),
        authority: Vec::new(),
        additional: Vec::new(),
        edns: None,
    };
    answer_for(query, &response, MAX_MESSAGE_LEN)
}

/// A response with `rcode` and `answers`, for a resolver that gives records rather than a
/// message.
pub fn response_with(rcode: Rcode, answers: Vec<Record>, authority: Vec<Record>) -> Message {
    Message {
        id: 0,
        flags: FLAG_QR | FLAG_RA,
        rcode,
        questions: Vec::new(),
        answers,
        authority,
        additional: Vec::new(),
        edns: None,
    }
}

/// A query for `name` and `qtype`, with recursion desired, as a program sends one.
pub fn query(id: u16, name: &Name, qtype: u16) -> Message {
    Message {
        id,
        flags: FLAG_RD,
        rcode: Rcode::NoError,
        questions: vec![Question {
            name: name.clone(),
            qtype,
            qclass: CLASS_IN,
        }],
        answers: Vec::new(),
        authority: Vec::new(),
        additional: Vec::new(),
        edns: None,
    }
}

/// The largest answer `query` takes over UDP: 512 bytes, or what its EDNS record says.
pub fn udp_limit(query: &Message) -> usize {
    match query.edns {
        Some(edns) => (edns.udp_size as usize).max(CLASSIC_UDP_LIMIT),
        None => CLASSIC_UDP_LIMIT,
    }
}
