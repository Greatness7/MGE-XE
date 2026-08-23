use std::borrow::{Borrow, Cow};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;

/// Returns whether `text` is already lowercase and uses backslash separators.
#[inline]
pub fn is_normalized(text: &str) -> bool {
    text.chars().all(|c| c != '/' && !c.is_ascii_uppercase())
}

/// Normalizes a path-like string to lowercase with backslash separators.
#[inline]
pub fn normalize(text: &str) -> Cow<'_, str> {
    if is_normalized(text) {
        Cow::Borrowed(text)
    } else {
        let mut normalized = text.replace('/', "\\");
        normalized.make_ascii_lowercase();
        Cow::Owned(normalized)
    }
}

/// A path string known to be lowercase with backslash separators.
#[repr(transparent)]
pub struct NormalizedStr(str);

impl NormalizedStr {
    /// Reinterprets an already-normalized string as [`NormalizedStr`].
    #[inline]
    pub fn from_normalized(text: &str) -> &Self {
        debug_assert!(is_normalized(text));
        // SAFETY: NormalizedStr is a transparent wrapper around str, and the caller
        // contract is enforced by the debug assertion above during development.
        unsafe { &*(text as *const str as *const Self) }
    }

    /// Canonicalizes an arbitrary path, borrowing when the input already satisfies
    /// the normalized-string invariant.
    #[inline]
    pub fn new(text: &str) -> Cow<'_, NormalizedStr> {
        if is_normalized(text) {
            Cow::Borrowed(Self::from_normalized(text))
        } else {
            Cow::Owned(NormalizedString(normalize(text).into_owned().into_boxed_str()))
        }
    }

    /// Returns the normalized path text.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Hashes normalized path text independent of how callers segment it.
#[inline]
pub(crate) fn hash_normalized_text<H: Hasher>(state: &mut H, text: &str) {
    debug_assert!(is_normalized(text));
    for byte in text.bytes() {
        state.write_u8(byte);
    }
}

impl fmt::Debug for NormalizedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

impl Hash for NormalizedStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_normalized_text(state, self.as_str());
        state.write_u8(0xff);
    }
}

impl PartialEq for NormalizedStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for NormalizedStr {}

/// Owned normalized path string.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NormalizedString(Box<str>);

impl NormalizedString {
    /// Builds an owned normalized string from text that already satisfies the invariant.
    #[inline]
    pub fn from_normalized(text: impl Into<Box<str>>) -> Self {
        let text = text.into();
        debug_assert!(is_normalized(&text));
        Self(text)
    }

    /// Normalizes a texture key, strips optional `Data Files\` and `Textures\`
    /// prefixes, and rejects unsupported texture extensions.
    #[inline]
    pub fn texture_key(raw: &str) -> Option<Self> {
        Some(Self(normalize_texture_key(raw)?.into_owned().into_boxed_str()))
    }

    /// Returns the normalized path text.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for NormalizedString {
    #[inline]
    fn from(value: String) -> Self {
        Self::from_normalized(value.into_boxed_str())
    }
}

impl From<Box<str>> for NormalizedString {
    #[inline]
    fn from(value: Box<str>) -> Self {
        Self::from_normalized(value)
    }
}

impl Hash for NormalizedString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_normalized_text(state, self.as_str());
        state.write_u8(0xff);
    }
}

impl ToOwned for NormalizedStr {
    type Owned = NormalizedString;

    fn to_owned(&self) -> Self::Owned {
        NormalizedString(self.as_str().into())
    }
}

impl Borrow<NormalizedStr> for NormalizedString {
    #[inline]
    fn borrow(&self) -> &NormalizedStr {
        NormalizedStr::from_normalized(self.as_str())
    }
}

impl Deref for NormalizedString {
    type Target = NormalizedStr;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.borrow()
    }
}

impl AsRef<NormalizedStr> for NormalizedString {
    #[inline]
    fn as_ref(&self) -> &NormalizedStr {
        self
    }
}

fn has_supported_texture_extension(path: &str) -> bool {
    path.ends_with(".dds") || path.ends_with(".tga") || path.ends_with(".bmp")
}

#[inline]
pub(crate) fn normalize_byte(byte: u8) -> u8 {
    match byte {
        b'/' => b'\\',
        b'A'..=b'Z' => byte + 32,
        _ => byte,
    }
}

/// Checks if a string begins with `prefix` considering path separator and case normalization.
///
/// The `prefix` MUST already be normalized (lowercase and backslash separators).
fn starts_with_normalized(text: &str, prefix: &str) -> bool {
    debug_assert!(is_normalized(prefix));
    text.len() >= prefix.len()
        && text
            .bytes()
            .zip(prefix.bytes())
            .take(prefix.len())
            .all(|(text, prefix)| normalize_byte(text) == prefix)
}

/// Removes a specific prefix from a string after applying normalization rules to the prefix comparison.
///
/// The `prefix` MUST already be normalized (lowercase and backslash separators).
#[inline]
pub(crate) fn trim_normalized_prefix<'a>(text: &'a str, prefix: &str) -> &'a str {
    debug_assert!(is_normalized(prefix));
    if starts_with_normalized(text, prefix) {
        &text[prefix.len()..]
    } else {
        text
    }
}

#[inline]
pub(crate) fn trim_mesh_override_path(path: &str) -> &str {
    let trimmed = trim_normalized_prefix(path, "data files\\");
    trim_normalized_prefix(trimmed, "meshes\\")
}

/// Normalizes a mesh override key and strips optional `Data Files\` and `Meshes\` prefixes.
#[inline]
pub fn normalize_mesh_override_key(path: &str) -> Cow<'_, str> {
    normalize(trim_mesh_override_path(path))
}

/// Normalizes a texture key, stripping optional `Data Files\` and `Textures\` prefixes
/// and rejecting unsupported texture extensions, borrowing when already normalized.
#[inline]
pub fn normalize_texture_key(path: &str) -> Option<Cow<'_, str>> {
    let trimmed = trim_normalized_prefix(path, "data files\\");
    let trimmed = trim_normalized_prefix(trimmed, "textures\\");
    let normalized = normalize(trimmed);
    has_supported_texture_extension(&normalized).then_some(normalized)
}

/// Normalizes a mutable string in place.
///
/// # Safety
///
/// The implementation only swaps ASCII `/` bytes for `\`, so it preserves UTF-8 validity.
#[inline]
pub fn make_normalized(text: &mut str) {
    text.make_ascii_lowercase();
    // SAFETY: Both '/' and '\\' are single-byte ASCII characters
    // that cannot appear as part of a multi-byte utf8 sequence.
    unsafe {
        for byte in text.as_bytes_mut() {
            if *byte == b'/' {
                *byte = b'\\';
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_mesh_override_key_trims_prefixes_and_normalizes() {
        assert_eq!(
            normalize_mesh_override_key("Data Files/Meshes/Foo/Bar.NIF").as_ref(),
            "foo\\bar.nif"
        );
        assert_eq!(normalize_mesh_override_key("meshes\\Foo\\Bar.NIF").as_ref(), "foo\\bar.nif");
        assert_eq!(normalize_mesh_override_key("Foo\\Bar.NIF").as_ref(), "foo\\bar.nif");
    }

    #[test]
    fn trim_mesh_override_path_trims_without_allocating() {
        assert_eq!(trim_mesh_override_path("Data Files/Meshes/Foo/Bar.NIF"), "Foo/Bar.NIF");
        assert_eq!(trim_mesh_override_path("meshes\\Foo/Bar.NIF"), "Foo/Bar.NIF");
        assert_eq!(trim_mesh_override_path("Foo/Bar.NIF"), "Foo/Bar.NIF");
    }

    #[test]
    fn in_place_normalized_asset_paths_are_borrowed() {
        let mut mesh = "Data Files/Meshes/Foo/Bar.NIF".to_owned();
        make_normalized(&mut mesh);
        assert!(matches!(normalize_mesh_override_key(&mesh), Cow::Borrowed("foo\\bar.nif")));

        let mut texture = "Data Files/Textures/Foo/Bar.TGA".to_owned();
        make_normalized(&mut texture);
        assert!(matches!(normalize_texture_key(&texture), Some(Cow::Borrowed("foo\\bar.tga"))));
    }
}
