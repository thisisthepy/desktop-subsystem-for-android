//! Decoding an LZ4 block, as erofs stores its compressed clusters.
//!
//! An LZ4 block is a run of sequences: a token (literal length in its high four bits, match
//! length less four in its low four, fifteen meaning more bytes follow), the literals, then a
//! two-byte offset back into the output and the match. The last sequence has literals only.
//! erofs decodes a cluster until it has the bytes the extent needs, which can be fewer than
//! the block holds (a deduplicated extent uses the start of another's cluster), so decoding
//! stops as soon as `out` is full.

/// Decodes `input` into `out` until `out` is full. Fails when the block ends first, or when
/// it is not a valid LZ4 block.
pub fn decode_into(input: &[u8], out: &mut [u8]) -> Result<(), String> {
    let want = out.len();
    let mut at = 0usize;
    let mut done = 0usize;
    let short = || "the LZ4 block ends before the data it should hold".to_string();
    while done < want {
        let token = *input.get(at).ok_or_else(short)?;
        at += 1;
        let mut literals = usize::from(token >> 4);
        if literals == 15 {
            loop {
                let more = *input.get(at).ok_or_else(short)?;
                at += 1;
                literals += usize::from(more);
                if more != 255 {
                    break;
                }
            }
        }
        let take = literals.min(want - done);
        let source = input.get(at..at + take).ok_or_else(short)?;
        out[done..done + take].copy_from_slice(source);
        done += take;
        at += literals;
        if done == want {
            break;
        }
        if at > input.len() {
            return Err(short());
        }
        let offset = match input.get(at..at + 2) {
            Some(bytes) => usize::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            None => return Err(short()),
        };
        at += 2;
        if offset == 0 || offset > done {
            return Err(format!(
                "the LZ4 block refers {offset} bytes back, before its data starts"
            ));
        }
        let mut length = usize::from(token & 15) + 4;
        if token & 15 == 15 {
            loop {
                let more = *input.get(at).ok_or_else(short)?;
                at += 1;
                length += usize::from(more);
                if more != 255 {
                    break;
                }
            }
        }
        let take = length.min(want - done);
        // The match may overlap what it copies, so it goes byte by byte when it does.
        let from = done - offset;
        if offset >= take {
            out.copy_within(from..from + take, done);
        } else {
            for i in 0..take {
                out[done + i] = out[from + i];
            }
        }
        done += take;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr1_an_lz4_block_of_literals_and_matches_decodes() {
        // "abcd" then a match of 8 at offset 4, then "xyz".
        let block = [0x44, b'a', b'b', b'c', b'd', 4, 0, 0x30, b'x', b'y', b'z'];
        let mut out = vec![0u8; 15];
        decode_into(&block, &mut out).unwrap();
        assert_eq!(&out, b"abcdabcdabcdxyz");
        // An overlapping match repeats one byte.
        let run = [0x1f, b'z', 1, 0, 6, 0x10, b'!'];
        let mut out = vec![0u8; 27];
        decode_into(&run, &mut out).unwrap();
        assert_eq!(&out[..26], [b'z'; 26].as_slice());
        assert_eq!(out[26], b'!');
    }

    #[test]
    fn nfr1_decoding_stops_once_the_extent_has_its_bytes() {
        let block = [0x44, b'a', b'b', b'c', b'd', 4, 0, 0x30, b'x', b'y', b'z'];
        let mut out = vec![0u8; 6];
        decode_into(&block, &mut out).unwrap();
        assert_eq!(&out, b"abcdab");
    }

    #[test]
    fn nfr1_a_short_or_bad_lz4_block_is_refused() {
        let mut out = vec![0u8; 20];
        assert!(decode_into(&[0x30, b'x', b'y', b'z'], &mut out).is_err());
        // A match before the start of the data.
        assert!(decode_into(&[0x14, b'a', 9, 0], &mut out).is_err());
        assert!(decode_into(&[], &mut out).is_err());
    }
}
