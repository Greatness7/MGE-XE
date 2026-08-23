//! Shared framing for checksummed binary payloads.
//!
//! A frame is a fixed 16-byte prefix (magic, little-endian version, zeroed reserved field), a
//! 32-byte BLAKE3 digest of the payload immediately before the payload itself, and the payload.

use std::fmt;

/// Bytes occupied by the common archive frame.
///
/// A multiple of 16, so a payload serialized directly into a reserved header prefix lands at the
/// same relative alignment it would have had at offset zero. See [`seal`].
pub const HEADER_LEN: usize = 48;

/// Bytes occupied by the payload digest that always ends an archive header.
const DIGEST_LEN: usize = 32;

/// Failure while validating a framed archive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The frame does not begin with the caller's magic.
    InvalidMagic,
    /// The frame version does not match the caller's schema version.
    VersionMismatch {
        /// Version encoded in the frame.
        found: u32,
        /// Version required by this decoder.
        expected: u32,
    },
    /// The frame is truncated or otherwise invalid.
    Corrupt(&'static str),
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => formatter.write_str("archive magic is invalid"),
            Self::VersionMismatch { found, expected } => {
                write!(formatter, "unsupported archive version {found}; expected {expected}")
            }
            Self::Corrupt(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for FrameError {}

/// Writes the frame header in front of a payload already serialized into `bytes`.
///
/// Callers allocate [`HEADER_LEN`] zeroed bytes, serialize the payload after them, then call this
/// to fill in magic, version, the zeroed reserved field, and the payload digest. Writing the
/// payload straight into the framed buffer avoids allocating and copying it a second time; because
/// rkyv pads relative to the writer position rather than the buffer address, the 16-aligned
/// [`HEADER_LEN`] makes that output byte-identical to serializing into a fresh buffer.
///
/// # Panics
///
/// Panics if `bytes` is shorter than [`HEADER_LEN`].
pub fn seal(bytes: &mut [u8], magic: &[u8; 8], version: u32) {
    assert!(bytes.len() >= HEADER_LEN, "framed archive buffer is shorter than its header");
    let digest = blake3::hash(&bytes[HEADER_LEN..]);
    bytes[..8].copy_from_slice(magic);
    bytes[8..12].copy_from_slice(&version.to_le_bytes());
    bytes[12..16].fill(0);
    bytes[HEADER_LEN - DIGEST_LEN..HEADER_LEN].copy_from_slice(digest.as_bytes());
}

/// Validates a common archive frame and returns its checksummed payload.
pub fn decode<'a>(bytes: &'a [u8], magic: &[u8; 8], expected_version: u32) -> Result<&'a [u8], FrameError> {
    let Some((header, payload)) = bytes.split_first_chunk::<HEADER_LEN>() else {
        return Err(FrameError::Corrupt("archive header is truncated"));
    };
    let found_magic: &[u8; 8] = header[..8].try_into().expect("eight-byte magic");
    if found_magic != magic {
        return Err(FrameError::InvalidMagic);
    }
    let version = u32::from_le_bytes(header[8..12].try_into().expect("four-byte version"));
    if version != expected_version {
        return Err(FrameError::VersionMismatch {
            found: version,
            expected: expected_version,
        });
    }
    if header[12..16] != [0; 4] {
        return Err(FrameError::Corrupt("archive reserved header field must be zero"));
    }
    let expected_digest: &[u8; 32] = header[HEADER_LEN - DIGEST_LEN..]
        .try_into()
        .expect("32-byte digest ends the header");
    if blake3::hash(payload).as_bytes() != expected_digest {
        return Err(FrameError::Corrupt("archive payload digest mismatch"));
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: &[u8; 8] = b"TESTFRM1";

    fn frame(version: u32, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; HEADER_LEN];
        bytes.extend_from_slice(payload);
        seal(&mut bytes, MAGIC, version);
        bytes
    }

    #[test]
    fn round_trip_and_failure_classes_are_distinct() {
        let bytes = frame(7, b"payload");
        assert_eq!(decode(&bytes, MAGIC, 7).unwrap(), b"payload");
        assert_eq!(
            decode(&bytes, MAGIC, 8),
            Err(FrameError::VersionMismatch { found: 7, expected: 8 })
        );
        assert_eq!(decode(&bytes, b"OTHERFRM", 7), Err(FrameError::InvalidMagic));
        assert_eq!(
            decode(&bytes[..HEADER_LEN - 1], MAGIC, 7),
            Err(FrameError::Corrupt("archive header is truncated"))
        );

        let mut reserved = frame(7, b"payload");
        reserved[12] = 1;
        assert_eq!(
            decode(&reserved, MAGIC, 7),
            Err(FrameError::Corrupt("archive reserved header field must be zero"))
        );

        let mut corrupt = bytes;
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            decode(&corrupt, MAGIC, 7),
            Err(FrameError::Corrupt("archive payload digest mismatch"))
        );
    }
}
