use super::*;
use crate::StaticTextureSizingMode;

fn settings(mode: StaticTextureSizingMode, protected: f32) -> StaticTextureSizingSettings {
    StaticTextureSizingSettings {
        mode,
        protected_density: protected,
        min_texture_size: 64,
        max_mip_reduction: 4,
    }
}

#[test]
fn select_longest_reduces_by_whole_mips_and_respects_floor() {
    let s = settings(StaticTextureSizingMode::DownscaleOpaque, 1.0);
    // limiting density 8 -> required_scale 1/8 -> reduction 3 -> 512>>3 = 64.
    assert_eq!(select_longest(512, 8.0, &s), 64);
    // limiting density 2 -> required_scale 1/2 -> reduction 1 -> 256.
    assert_eq!(select_longest(512, 2.0, &s), 256);
    // limiting density 1 -> required_scale 1 -> no reduction.
    assert_eq!(select_longest(512, 1.0, &s), 512);
}

#[test]
fn select_longest_clamps_to_max_mip_reduction_and_min_size() {
    let s = settings(StaticTextureSizingMode::DownscaleOpaque, 1.0);
    // Enormous density would reduce far, but max_mip_reduction caps it at 4 and the floor at 64.
    assert_eq!(select_longest(512, 100000.0, &s), 64);
    // A sub-floor baseline never upscales and never drops below itself.
    assert_eq!(select_longest(32, 100000.0, &s), 32);
}

fn aggregate(area_density: f32, valid: u64) -> TextureUsageAggregate {
    let mut agg = TextureUsageAggregate::default();
    agg.area_density_min = area_density;
    agg.valid_count = valid;
    agg.limiting_use = Some(super::super::analyze::UseId {
        static_key: "s.nif".into(),
        subset_index: 0,
        triangle_index: 0,
    });
    agg
}

fn source(width: u32, height: u32) -> SourceTextureInfo {
    SourceTextureInfo {
        size: 1,
        hash: [0; 32],
        width,
        height,
        mip_count: 1,
        is_dds: true,
    }
}

fn usage(
    domain: AtlasDomain,
    key: &str,
    agg: TextureUsageAggregate,
) -> AtlasTextureSet<HashMap<String, TextureUsageAggregate>> {
    let mut set = AtlasTextureSet::<HashMap<String, TextureUsageAggregate>>::default();
    match domain {
        AtlasDomain::Opaque => set.opaque.insert(key.to_owned(), agg),
        AtlasDomain::Alpha => set.alpha.insert(key.to_owned(), agg),
    };
    set
}

#[test]
fn downscale_opaque_writes_overlay_report_only_does_not() {
    let mut info = HashMap::new();
    info.insert("a.dds".to_owned(), source(1024, 1024));
    let agg = aggregate(8.0, 10);
    let u = usage(AtlasDomain::Opaque, "a.dds", agg);

    let (plan_dl, m_dl) = plan_static_texture_resolutions(
        &info,
        &u,
        &settings(StaticTextureSizingMode::DownscaleOpaque, 1.0),
        TextureAxisCaps::uniform(512),
    );
    assert_eq!(plan_dl.opaque_overrides.get("a.dds"), Some(&64));
    assert_eq!(m_dl.reduced_texture_count, 1);

    let (plan_rep, m_rep) = plan_static_texture_resolutions(
        &info,
        &u,
        &settings(StaticTextureSizingMode::Report, 1.0),
        TextureAxisCaps::uniform(512),
    );
    assert!(
        plan_rep.opaque_overrides.is_empty(),
        "Report mode must not populate the overlay"
    );
    assert_eq!(m_rep.reduced_texture_count, 1, "Report still projects the reduction");
}

#[test]
fn alpha_is_reduced_only_in_downscale_mode() {
    let mut info = HashMap::new();
    info.insert("leaf.dds".to_owned(), source(1024, 1024));
    let u = usage(AtlasDomain::Alpha, "leaf.dds", aggregate(8.0, 10));

    // DownscaleOpaque still measures and projects alpha, but applies nothing.
    let (plan, metrics) = plan_static_texture_resolutions(
        &info,
        &u,
        &settings(StaticTextureSizingMode::DownscaleOpaque, 1.0),
        TextureAxisCaps::uniform(512),
    );
    assert!(plan.alpha_overrides.is_empty());
    assert_eq!(metrics.reduced_texture_count, 1, "the reduction is still projected");

    // Downscale applies it.
    let (plan, _metrics) = plan_static_texture_resolutions(
        &info,
        &u,
        &settings(StaticTextureSizingMode::Downscale, 1.0),
        TextureAxisCaps::uniform(512),
    );
    assert_eq!(plan.alpha_overrides.get("leaf.dds"), Some(&64));
}

#[test]
fn a_few_uncertain_triangles_do_not_block_reduction() {
    let mut info = HashMap::new();
    info.insert("a.dds".to_owned(), source(1024, 1024));

    // 1 unmeasurable triangle out of 100 is noise, not evidence the measurement is wrong.
    let mut agg = aggregate(8.0, 99);
    agg.uncertain_count = 1;
    let u = usage(AtlasDomain::Opaque, "a.dds", agg);
    let (plan, m) = plan_static_texture_resolutions(
        &info,
        &u,
        &settings(StaticTextureSizingMode::Downscale, 1.0),
        TextureAxisCaps::uniform(512),
    );
    assert_eq!(plan.opaque_overrides.get("a.dds"), Some(&64));
    assert_eq!(m.reduced_texture_count, 1);

    // Past the ratio we have genuinely not measured the texture.
    let mut agg = aggregate(8.0, 50);
    agg.uncertain_count = 50;
    let u = usage(AtlasDomain::Opaque, "a.dds", agg);
    let (plan, m) = plan_static_texture_resolutions(
        &info,
        &u,
        &settings(StaticTextureSizingMode::Downscale, 1.0),
        TextureAxisCaps::uniform(512),
    );
    assert!(plan.opaque_overrides.is_empty());
    assert_eq!(
        m.reduced_texture_count, 0,
        "falling back to baseline proposes no reduction at all, unlike Report mode"
    );
}

#[test]
fn unknown_texture_dimensions_force_baseline_fallback() {
    let mut info = HashMap::new();
    info.insert("x.dds".to_owned(), source(1024, 1024));
    let mut agg = aggregate(8.0, 10);
    agg.needs_baseline = true;
    let u = usage(AtlasDomain::Opaque, "x.dds", agg);
    let (plan, m) = plan_static_texture_resolutions(
        &info,
        &u,
        &settings(StaticTextureSizingMode::DownscaleOpaque, 1.0),
        TextureAxisCaps::uniform(512),
    );
    assert!(plan.opaque_overrides.is_empty());
    assert_eq!(
        m.reduced_texture_count, 0,
        "unknown dimensions propose no reduction at all, unlike Report mode"
    );
}

#[test]
fn dedupe_reconciliation_lifts_group_to_max_selected() {
    // Two byte-identical opaque textures, one reduced to 64, the other kept at baseline 512.
    let mut info = HashMap::new();
    info.insert("a.dds".to_owned(), source(512, 512));
    info.insert("b.dds".to_owned(), source(512, 512));
    let textures = AtlasTextureSet::new(
        ["a.dds".to_owned(), "b.dds".to_owned()].into_iter().collect(),
        IndexSet::default(),
    );

    let mut plan = SizingPlan::uniform(512);
    plan.opaque_overrides.insert("a.dds".to_owned(), 64);
    // "b.dds" has no override (kept at baseline 512).

    merge_dedupe_alias_requirements(&textures, &info, TextureDedupeMode::Exact, &mut plan);
    // Group max selected is 512 (b kept baseline), so a's reduction is cleared.
    assert!(plan.opaque_overrides.is_empty());

    // Now both want a reduction; the larger (256) wins for both.
    let mut plan2 = SizingPlan::uniform(512);
    plan2.opaque_overrides.insert("a.dds".to_owned(), 64);
    plan2.opaque_overrides.insert("b.dds".to_owned(), 256);
    merge_dedupe_alias_requirements(&textures, &info, TextureDedupeMode::Exact, &mut plan2);
    assert_eq!(plan2.opaque_overrides.get("a.dds"), Some(&256));
    assert_eq!(plan2.opaque_overrides.get("b.dds"), Some(&256));

    // Off mode leaves the plan untouched.
    let mut plan3 = SizingPlan::uniform(512);
    plan3.opaque_overrides.insert("a.dds".to_owned(), 64);
    merge_dedupe_alias_requirements(&textures, &info, TextureDedupeMode::Off, &mut plan3);
    assert_eq!(plan3.opaque_overrides.get("a.dds"), Some(&64));
}
