//! sRGB <-> linear color conversion helpers shared by texture mip generation and patch albedo.

use std::sync::LazyLock;

/// Every `u8` sRGB channel value pre-converted to linear light.
///
/// Mip downsampling converts four source texels per output texel per channel, so the
/// `powf` in `srgb_sample_to_linear` dominates that loop. Entries are produced by that
/// same function, making lookups bit-identical to computing them inline.
static SRGB_U8_TO_LINEAR: LazyLock<[f32; 256]> =
    LazyLock::new(|| std::array::from_fn(|value| srgb_sample_to_linear(value as f32)));

/// Returns the 256-entry sRGB `u8` -> linear table.
///
/// Hoist this out of per-pixel loops: each call pays `LazyLock`'s initialization check.
pub fn srgb_u8_to_linear_table() -> &'static [f32; 256] {
    &SRGB_U8_TO_LINEAR
}

/// Converts an sRGB channel sample in [0, 255] to linear-light `f32`.
pub fn srgb_sample_to_linear(value: f32) -> f32 {
    let value = value.clamp(0.0, 255.0) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Converts a linear-light `f32` in [0, 1] to an sRGB `u8`.
pub fn linear_to_srgb_u8(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let srgb = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}
