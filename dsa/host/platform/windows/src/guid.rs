//! GUIDs as Hyper-V sockets use them: VM IDs and service IDs.
//!
//! This module has no Windows dependency, so the parsing and the vsock mapping are tested
//! on every platform.

use std::fmt;
use std::str::FromStr;

/// A GUID with the in-memory layout of the Windows `GUID` structure.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    pub const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Guid {
        Guid {
            data1,
            data2,
            data3,
            data4,
        }
    }

    /// Any partition. A listener bound to it accepts from every VM it is allowed to.
    pub const WILDCARD: Guid = Guid::new(0, 0, 0, [0; 8]);

    /// The host itself, for connections from the host to the host.
    pub const LOOPBACK: Guid = Guid::new(
        0xe0e1_6197,
        0xdd56,
        0x4a10,
        [0x91, 0x95, 0x5e, 0xe7, 0xa1, 0x55, 0xa8, 0x38],
    );

    /// Every child partition of this host.
    pub const CHILDREN: Guid = Guid::new(
        0x90db_8b89,
        0x0d35,
        0x4f79,
        [0x8c, 0xe9, 0x49, 0xea, 0x0a, 0xc8, 0xb7, 0xcd],
    );

    /// The parent partition, as seen from a VM.
    pub const PARENT: Guid = Guid::new(
        0xa42e_7cda,
        0xd03f,
        0x480c,
        [0x9c, 0xc2, 0xa4, 0xde, 0x20, 0xab, 0xb8, 0x78],
    );

    /// The Hyper-V service ID that a Linux guest reaches as vsock port `port`.
    ///
    /// Hyper-V maps vsock ports onto service IDs of the form
    /// `<port>-facb-11e6-bd58-64006a7986d3`, so a guest that connects to vsock port 5000
    /// reaches the host service `00001388-facb-11e6-bd58-64006a7986d3`.
    pub const fn vsock_service(port: u32) -> Guid {
        Guid::new(
            port,
            0xfacb,
            0x11e6,
            [0xbd, 0x58, 0x64, 0x00, 0x6a, 0x79, 0x86, 0xd3],
        )
    }

    /// The vsock port this service ID stands for, if it is one of the vsock mapped IDs.
    pub fn vsock_port(&self) -> Option<u32> {
        let template = Guid::vsock_service(self.data1);
        (*self == template).then_some(self.data1)
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = &self.data4;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.data1, self.data2, self.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        )
    }
}

/// Why a string is not a GUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseGuidError(pub String);

impl fmt::Display for ParseGuidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not a GUID (expected xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)",
            self.0
        )
    }
}

impl std::error::Error for ParseGuidError {}

impl FromStr for Guid {
    type Err = ParseGuidError;

    /// Parses `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, with or without braces, in any case.
    fn from_str(text: &str) -> Result<Guid, ParseGuidError> {
        let error = || ParseGuidError(text.to_string());
        let inner = text.trim();
        let inner = inner
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .unwrap_or(inner);
        let groups: Vec<&str> = inner.split('-').collect();
        let lengths = [8, 4, 4, 4, 12];
        if groups.len() != lengths.len()
            || groups.iter().zip(lengths).any(|(group, len)| {
                group.len() != len || !group.chars().all(|c| c.is_ascii_hexdigit())
            })
        {
            return Err(error());
        }
        let data1 = u32::from_str_radix(groups[0], 16).map_err(|_| error())?;
        let data2 = u16::from_str_radix(groups[1], 16).map_err(|_| error())?;
        let data3 = u16::from_str_radix(groups[2], 16).map_err(|_| error())?;
        let tail = format!("{}{}", groups[3], groups[4]);
        let mut data4 = [0u8; 8];
        for (i, byte) in data4.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&tail[i * 2..i * 2 + 2], 16).map_err(|_| error())?;
        }
        Ok(Guid {
            data1,
            data2,
            data3,
            data4,
        })
    }
}
