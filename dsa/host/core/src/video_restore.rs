//! What a stream needs to make its decoder again where it stood, after the decoder was let go.
//!
//! A stream that gets no packet for a while ([`crate::video::IDLE_AFTER`]) lets its whole
//! decoder go: on Windows the Media Foundation transform with its surfaces and sample pool,
//! not only the texture its pictures are read through. A feed of short videos leaves streams
//! that decoded one picture and then never another, each holding its decoder for minutes, and
//! a paused video holds one for as long as it stays paused.
//!
//! When the next packet comes, a new decoder is opened. It holds none of the pictures the
//! stream's next packets refer to, so a packet decoded on it as it is would come out green or
//! as blocks of an unrelated picture. Two ways keep that from happening:
//!
//! - The packets since the stream's last key frame are kept, compressed, with the codec
//!   configuration (H.264 SPS and PPS, HEVC VPS, SPS and PPS, AV1's configuration record)
//!   that was in effect at that key frame ([`Restore`]). The new decoder is given them first,
//!   and their pictures, which the guest already has, are not sent again. It then stands
//!   where the old one stood, and a paused video goes on from the picture it paused at. A
//!   picture the old decoder still held back (reordered, not yet out) comes out of the new one
//!   and goes to the guest as it would have.
//! - When they were not kept (the stream decoded more since its last key frame than
//!   [`MAX_RESTORE_BYTES`] or [`MAX_RESTORE_PACKETS`], a flush or a failed packet came since,
//!   or no key frame was ever recognized), the new decoder takes packets only from the next
//!   key frame on, as after a flush ([`crate::video_keyframe`]). Each packet before it is
//!   answered with a frame that carries no picture, which the guest finishes without output,
//!   so the app keeps showing its last good picture and sees no error.
//!
//! Nothing here names a host operating system API.

use crate::video::Packet;
use crate::video_keyframe::Access;

/// The most compressed bytes kept to restore a stream: a key frame and what follows it, for a
/// 4K stream of 20 Mbit/s over six seconds, longer than any streaming service's key frame
/// interval. A stream that outgrows it is restored from its next key frame instead.
pub const MAX_RESTORE_BYTES: usize = 16 * 1024 * 1024;

/// The most packets kept to restore a stream: 20 seconds at 30 frames a second.
pub const MAX_RESTORE_PACKETS: usize = 600;

/// The most bytes of codec configuration kept. Parameter sets are a few hundred bytes; a
/// guest that sends more than this is not sending parameter sets.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
struct Kept {
    packet: Packet,
    access: Access,
}

/// The packets a new decoder is given so it stands where the stream's old decoder stood.
///
/// The stream tells it of every packet its decoder takes, in the order the decoder takes them:
/// codec configuration ([`configuration`](Self::configuration)), and every other packet with
/// what it is to a decoder that starts at it ([`decoded`](Self::decoded)). A key frame starts
/// what is kept anew. Anything that leaves the decoder without its references (a flush, a
/// packet it failed on, a drain) is [`lost`](Self::lost).
#[derive(Debug, Default, Clone)]
pub struct Restore {
    /// The configuration packets the guest sent last, in order: the run of them since the last
    /// packet that was not one.
    config: Vec<Packet>,
    config_bytes: usize,
    /// No picture packet came since the last configuration packet: another one joins the run.
    config_open: bool,
    /// The configuration in effect at the key frame that starts `since_key`.
    key_config: Vec<Packet>,
    /// Every packet the decoder took from that key frame on, configuration included.
    since_key: Vec<Kept>,
    bytes: usize,
    /// `since_key` starts at a key frame and nothing was lost since.
    anchored: bool,
}

impl Restore {
    pub fn new() -> Restore {
        Restore::default()
    }

    /// The decoder took a codec configuration packet.
    pub fn configuration(&mut self, packet: &Packet) {
        if !self.config_open {
            self.config.clear();
            self.config_bytes = 0;
            self.config_open = true;
        }
        self.config_bytes += packet.data.len();
        if self.config_bytes > MAX_CONFIG_BYTES {
            // Not parameter sets: nothing kept could make a decoder stand where this one does.
            self.config.clear();
            self.lost();
            return;
        }
        self.config.push(packet.clone());
        if self.anchored {
            // Configuration that came between pictures, in its place among them.
            self.keep(packet, Access::Unknown);
        }
    }

    /// The decoder took `packet`, which is `access` to a decoder that starts at it.
    pub fn decoded(&mut self, packet: &Packet, access: Access) {
        self.config_open = false;
        match access {
            Access::Key | Access::OpenKey => {
                self.since_key.clear();
                self.bytes = 0;
                self.key_config.clone_from(&self.config);
                self.anchored = true;
                self.keep(packet, access);
            }
            _ if self.anchored => self.keep(packet, access),
            _ => {}
        }
    }

    /// The decoder lost its references: a flush, a packet it failed on, a drain. What was kept
    /// cannot make another one stand where it stood, so nothing is kept until the next key
    /// frame. The configuration stays: the guest does not always send it again.
    pub fn lost(&mut self) {
        self.since_key = Vec::new();
        self.key_config = Vec::new();
        self.bytes = 0;
        self.anchored = false;
    }

    /// Whether a new decoder given [`packets`](Self::packets) stands where the old one stood.
    pub fn can_restore(&self) -> bool {
        self.anchored && !self.since_key.is_empty()
    }

    /// The packets to give a new decoder, in order: the configuration in effect at the last key
    /// frame, the key frame, and every packet after it. When the key frame is an open one (an
    /// HEVC CRA), its leading pictures that refer to pictures before it are left out, as a
    /// decoder that starts there leaves them out; their pictures went to the guest long ago.
    /// Empty when the stream cannot be restored.
    pub fn packets(&self) -> Vec<Packet> {
        if !self.can_restore() {
            return Vec::new();
        }
        let open = self.since_key[0].access == Access::OpenKey;
        let mut leading = open;
        let mut out: Vec<Packet> = self.key_config.clone();
        for (at, kept) in self.since_key.iter().enumerate() {
            if at > 0 && leading {
                if kept.access == Access::Leading {
                    continue;
                }
                if !kept.packet.codec_config {
                    leading = false;
                }
            }
            out.push(kept.packet.clone());
        }
        out
    }

    /// The codec configuration the guest sent last, which a new decoder is given before it
    /// waits for the next key frame: the guest does not send it again, and a key frame of an
    /// H.264 or HEVC stream seldom carries its own parameter sets.
    pub fn config_packets(&self) -> Vec<Packet> {
        self.config.clone()
    }

    /// The packets kept, configuration included.
    pub fn kept_packets(&self) -> usize {
        if self.can_restore() {
            self.key_config.len() + self.since_key.len()
        } else {
            0
        }
    }

    /// The bytes this holds: what the stream keeps beside its decoder while it plays.
    pub fn held_bytes(&self) -> usize {
        let config: usize = self.config.iter().map(|p| p.data.len()).sum();
        let key_config: usize = self.key_config.iter().map(|p| p.data.len()).sum();
        config + key_config + self.bytes
    }

    fn keep(&mut self, packet: &Packet, access: Access) {
        let bytes = self.bytes + packet.data.len();
        if bytes > MAX_RESTORE_BYTES || self.since_key.len() >= MAX_RESTORE_PACKETS {
            // Too long since the last key frame to keep: the stream is restored from its next
            // one instead.
            self.lost();
            return;
        }
        self.bytes = bytes;
        self.since_key.push(Kept {
            packet: packet.clone(),
            access,
        });
    }
}
