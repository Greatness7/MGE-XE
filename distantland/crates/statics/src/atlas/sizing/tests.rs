use super::*;

/// The 48-texel page-fit floor for the default 16384 page, used where the exact value matters.
const USABLE_16K: u32 = 16336;

#[test]
fn equal_caps_reproduce_a_single_longest_side_cap() {
    let caps = TextureAxisCaps::uniform(1024);
    // Square, elongated, and already-under-cap sources all behave as a plain longest-side clamp.
    assert_eq!(caps.longest_for(4096, 4096), 1024);
    assert_eq!(caps.longest_for(512, 8192), 1024);
    assert_eq!(caps.longest_for(256, 128), 256);
}

#[test]
fn short_cap_bounds_the_shorter_axis_after_uniform_scaling() {
    let caps = TextureAxisCaps { long: 8192, short: 1024 };
    // 512x8192 is 16:1, so a 1024 short axis permits a 16384 long axis; the long cap binds first.
    assert_eq!(caps.longest_for(512, 8192), 8192);
    assert_eq!(limited_dimensions(512, 8192, caps.longest_for(512, 8192)), (512, 8192));

    // 512x16384 is 32:1: the long cap forces the short axis below its own cap.
    assert_eq!(caps.longest_for(512, 16384), 8192);
    assert_eq!(limited_dimensions(512, 16384, caps.longest_for(512, 16384)), (256, 8192));

    // A square source is governed entirely by the short cap.
    assert_eq!(caps.longest_for(4096, 4096), 1024);

    // Orientation must not matter.
    assert_eq!(caps.longest_for(8192, 512), caps.longest_for(512, 8192));
}

#[test]
fn short_cap_binds_before_the_long_cap_on_mild_aspect_ratios() {
    let caps = TextureAxisCaps { long: 8192, short: 1024 };
    // 2:1 at 4096x2048: the short axis reaches 1024 at a 2048 long axis.
    assert_eq!(caps.longest_for(4096, 2048), 2048);
    assert_eq!(limited_dimensions(4096, 2048, 2048), (2048, 1024));
}

#[test]
fn snap_to_mip_step_is_a_no_op_on_power_of_two_targets() {
    for &source in &[512u32, 1024, 2048, 4096, 8192] {
        for &cap in crate::SUPPORTED_STATIC_TEXTURE_LONG_SIZES {
            assert_eq!(
                snap_to_mip_step(source, cap),
                source.min(cap),
                "snap changed a power-of-two target: source {source}, cap {cap}"
            );
        }
    }
}

#[test]
fn snap_to_mip_step_halves_rather_than_shaving_at_the_page_fit_floor() {
    // The whole reason the snap exists: 8144 costs 0.6% of a page but forces a full resample.
    assert_eq!(snap_to_mip_step(8192, 8144), 4096);
    assert_eq!(snap_to_mip_step(16384, USABLE_16K), 8192);
    // Sources already inside the floor are untouched.
    assert_eq!(snap_to_mip_step(4096, USABLE_16K), 4096);
}

#[test]
fn snap_to_mip_step_never_reaches_zero() {
    assert_eq!(snap_to_mip_step(4096, 0), 1);
    assert_eq!(snap_to_mip_step(0, 0), 0);
}

#[test]
fn for_atlas_floors_both_caps_to_what_fits_one_page() {
    let caps = TextureAxisCaps::for_atlas(8192, 8192, 8192);
    assert_eq!(caps.long, usable_texture_dim(8192));
    assert_eq!(caps.short, usable_texture_dim(8192));
    // An 8192 source under an 8192 page halves instead of shaving 48 texels off both sides.
    assert_eq!(caps.longest_for(8192, 8192), 4096);
    // A cap already below the floor is untouched by it.
    assert_eq!(TextureAxisCaps::for_atlas(4096, 1024, 8192).short, 1024);
}

#[test]
fn defaults_give_stacked_atlases_sub_texture_parity_with_plain_art() {
    // The point of the shipped defaults: in a stacked pre-made atlas the shorter axis *is* the
    // sub-texture resolution, so at `long == 8 * short` every real atlas shape lands with its
    // shorter axis exactly at the short cap, the same ceiling equivalent plain art gets. Atlased
    // art must be neither penalized nor favored.
    let short = crate::DEFAULT_STATIC_TEXTURE_SHORT_SIZE;
    assert_eq!(
        crate::DEFAULT_STATIC_TEXTURE_LONG_SIZE,
        short * 8,
        "the defaults encode an 8:1 allowance; the parity cases below assume it"
    );
    let caps = TextureAxisCaps::for_atlas(
        crate::DEFAULT_STATIC_TEXTURE_LONG_SIZE,
        short,
        crate::DEFAULT_STATIC_ATLAS_MAX_SIZE,
    );

    // Real surveyed atlas shapes, both stacking orientations, at 8:1 and 4:1.
    for &(width, height) in &[(2048u32, 16384u32), (1024, 8192), (4096, 16384), (2048, 8192), (8192, 2048)] {
        let (baseline_width, baseline_height) = limited_dimensions(width, height, caps.longest_for(width, height));
        assert_eq!(
            baseline_width.min(baseline_height),
            short,
            "{width}x{height} lost sub-texture parity with plain art"
        );
    }

    // Plain art is bounded by the same cap, and art already at or under it is untouched.
    assert_eq!(limited_dimensions(2048, 2048, caps.longest_for(2048, 2048)), (short, short));
    assert_eq!(
        limited_dimensions(short, short, caps.longest_for(short, short)),
        (short, short)
    );
    assert_eq!(limited_dimensions(64, 64, caps.longest_for(64, 64)), (64, 64));

    // Past the 8:1 allowance the long cap binds instead, costing sub-texture resolution. Accepted:
    // surveyed content holds a single such file, and widening the allowance would double the page
    // floor for everything.
    assert_eq!(
        limited_dimensions(512, 8192, caps.longest_for(512, 8192)),
        (short / 2, short * 8)
    );
}

#[test]
fn unprobeable_sources_fall_back_to_the_long_cap() {
    let caps = TextureAxisCaps { long: 2048, short: 512 };
    assert_eq!(caps.longest_for(0, 0), 2048);
    assert_eq!(caps.longest_for(1024, 0), 2048);
}
