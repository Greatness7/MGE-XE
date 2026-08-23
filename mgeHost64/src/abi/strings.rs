/// Converts a fixed-size C buffer into Rust text, borrowing valid UTF-8.
///
/// The ABI uses fixed-width byte arrays that may be null-terminated or fully padded.
/// Invalid UTF-8 is lossily decoded so malformed input does not break IPC processing.
pub fn c_string_from_fixed(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    let slice = match std::ffi::CStr::from_bytes_until_nul(bytes) {
        Ok(c_str) => c_str.to_bytes(),
        Err(_) => bytes,
    };
    String::from_utf8_lossy(slice)
}
