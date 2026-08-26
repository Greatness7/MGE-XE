use std::borrow::Borrow;
use std::fmt;

/// Returns the meaningful bytes of a fixed-size C buffer.
///
/// The ABI uses fixed-width byte arrays that may be null-terminated or fully padded, so the
/// result is the slice up to the first NUL, or the whole buffer when there is none.
///
/// Bytes are returned undecoded. These fields carry the engine's own single-byte name bytes
/// in whatever codepage the install uses, which is not UTF-8: a lossy decode folds distinct
/// non-ASCII names onto the same replacement-character text, letting one world space match
/// another's statics. Comparing raw bytes is both correct and cheaper.
pub fn bytes_from_fixed(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|&byte| byte == 0) {
        Some(index) => &bytes[..index],
        None => bytes,
    }
}

/// An engine cell or world-space name, held as the bytes the engine itself uses.
///
/// Morrowind writes cell names as single-byte text in whatever codepage the install runs, and
/// `usage.data` and the IPC parameters carry those bytes through unchanged. Nothing in the host
/// knows that codepage, so the encoding stays deliberately unresolved and identity is byte
/// identity; [`bytes_from_fixed`] describes what decoding them would cost.
///
/// The exterior world space is recorded under the empty name, [`CellName::EXTERIOR`].
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct CellName(Vec<u8>);

impl CellName {
    /// The exterior world space, which `usage.data` records under an empty name.
    pub const EXTERIOR: Self = Self(Vec::new());

    /// Copies undecoded name bytes into an owned name.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    /// Returns the undecoded name bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Borrowed lookup, so `HashMap<CellName, _>::get` still accepts a plain `&[u8]` and the hot IPC
/// path never allocates just to test a name.
///
/// `Hash` is derived over the single `Vec<u8>` field and forwards to `<[u8]>::hash`, so a
/// `CellName` and its bytes hash identically, as `Borrow` requires. Adding a second field to this
/// struct would break that agreement and silently turn every borrowed lookup into a miss.
impl Borrow<[u8]> for CellName {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for CellName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", EscapedName(&self.0))
    }
}

/// Formats undecoded name bytes for diagnostics.
///
/// Runs that are valid UTF-8 print as themselves and everything else prints as `\xNN`, so an
/// ASCII name stays readable while a cp1251 one survives as recoverable escapes. A lossy decode
/// would render every non-ASCII name as the same run of replacement characters, which is worst
/// precisely where these names are hardest to diagnose.
///
/// [`CellName`] deliberately has no `Display`: no textual form is correct without knowing the
/// install's codepage, so each lossy rendering stays spelled out at its call site.
pub struct EscapedName<'a>(pub &'a [u8]);

impl fmt::Display for EscapedName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.0.utf8_chunks() {
            f.write_str(chunk.valid())?;
            for &byte in chunk.invalid() {
                write!(f, "\\x{byte:02x}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_the_first_nul() {
        assert_eq!(bytes_from_fixed(b"name\0\0\0"), b"name");
        assert_eq!(bytes_from_fixed(b"\0name"), b"");
    }

    #[test]
    fn returns_the_whole_buffer_when_full_width() {
        let full = [b'a'; 64];
        assert_eq!(bytes_from_fixed(&full), &full[..]);
    }

    #[test]
    fn keeps_distinct_high_byte_names_distinct() {
        // "Балмора" and "Балмара" as a Russian install's cp1251 bytes; a lossy UTF-8 decode
        // collapses both to replacement characters and makes them compare equal.
        let a = b"\xc1\xe0\xeb\xec\xee\xf0\xe0";
        let b = b"\xc1\xe0\xeb\xec\xe0\xf0\xe0";
        assert_ne!(bytes_from_fixed(a), bytes_from_fixed(b));
        assert_eq!(
            String::from_utf8_lossy(bytes_from_fixed(a)),
            String::from_utf8_lossy(bytes_from_fixed(b)),
            "guards the reason this helper does not decode"
        );
    }

    #[test]
    fn cell_name_looks_up_by_borrowed_bytes() {
        let mut map = hashbrown::HashMap::new();
        map.insert(CellName::from_bytes(b"\xc1\xe0\xeb\xec\xee\xf0\xe0"), 7_usize);
        assert_eq!(map.get(b"\xc1\xe0\xeb\xec\xee\xf0\xe0".as_slice()).copied(), Some(7));
        assert_eq!(map.get(b"\xc1\xe0\xeb\xec\xe0\xf0\xe0".as_slice()).copied(), None);
    }

    #[test]
    fn exterior_is_the_empty_name() {
        assert_eq!(CellName::EXTERIOR.as_bytes(), b"");
        assert_eq!(CellName::EXTERIOR, CellName::from_bytes(b""));
    }

    #[test]
    fn debug_escapes_undecodable_bytes_instead_of_replacing_them() {
        // The two cp1251 names from `keeps_distinct_high_byte_names_distinct`, which a lossy
        // decode renders identically. Escaped, they stay distinguishable in a log.
        let balmora = CellName::from_bytes(b"\xc1\xe0\xeb\xec\xee\xf0\xe0");
        let balmara = CellName::from_bytes(b"\xc1\xe0\xeb\xec\xe0\xf0\xe0");
        assert_eq!(format!("{balmora:?}"), r#""\xc1\xe0\xeb\xec\xee\xf0\xe0""#);
        assert_ne!(format!("{balmora:?}"), format!("{balmara:?}"));
    }

    #[test]
    fn debug_keeps_ascii_names_readable() {
        let name = CellName::from_bytes(b"Balmora, Guild of Mages");
        assert_eq!(format!("{name:?}"), r#""Balmora, Guild of Mages""#);
    }
}
