use super::*;
use crate::DistantStatics;
use crate::model::{DistantStatic, Subset, Vertex};
use std::env;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const TEST_MAX_TEXTURE_DIM: u32 = 512;
const TEST_GPU_MAX: u32 = crate::DEFAULT_STATIC_ATLAS_MAX_SIZE;

fn cwd_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn with_temp_cwd<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _guard = cwd_lock().lock().unwrap();
    let original = env::current_dir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    env::set_current_dir(temp.path()).unwrap();
    let result = f(temp.path());
    env::set_current_dir(original).unwrap();
    result
}

fn write_texture(root: &Path, rel_path: &str, bytes: &[u8]) -> String {
    let path = root.join("textures").join(rel_path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    rel_path.to_owned()
}

fn write_rgba_texture(root: &Path, rel_path: &str, color: [u8; 4]) -> String {
    write_sized_rgba_texture(root, rel_path, 40, color)
}

fn write_sized_rgba_texture(root: &Path, rel_path: &str, size: u32, color: [u8; 4]) -> String {
    let path = root.join("textures").join(rel_path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbaImage::from_pixel(size, size, image::Rgba(color))
        .save(path)
        .unwrap();
    rel_path.to_owned()
}

/// Runs the production prepare, pack, and compose sequence for pixel-level tests.
fn compose_packed_pages(
    textures: &IndexSet<String>,
    vfs: &Vfs,
    dims: &HashMap<String, u32>,
    dedupe_mode: TextureDedupeMode,
) -> tex_packer_core::error::Result<Vec<image::RgbaImage>> {
    let (prepared, _alias) = super::pack::prepare_textures(textures, vfs, dims, dedupe_mode, AtlasDomain::Opaque);
    let atlas = super::pack::pack_layout_only(prepared.layout_items, TEST_GPU_MAX)?;
    Ok(atlas
        .pages
        .iter()
        .map(|page| super::pack::compose_planned_page(&prepared.images, page))
        .collect())
}

#[test]
fn pack_decodes_tga_sources_instead_of_substituting_a_placeholder() {
    // TGA carries no magic bytes, so this passes only if the packer selects the decoder from the
    // resolved key's extension. `prepare_textures` swallows a decode failure into a 1x1 magenta
    // placeholder, so assert the packed pixels. Packing "succeeding" proves nothing here.
    with_temp_cwd(|temp| {
        const COLOR: [u8; 4] = [12, 200, 64, 255];
        let texture = write_sized_rgba_texture(temp, "regress\\solid.tga", 32, COLOR);

        let vfs = make_test_vfs(temp);
        let textures: IndexSet<String> = [texture].into_iter().collect();
        let dims: HashMap<String, u32> = textures.iter().map(|key| (key.clone(), 64)).collect();
        let pages = compose_packed_pages(&textures, &vfs, &dims, TextureDedupeMode::Off).unwrap();

        assert!(
            pages.iter().any(|page| page.pixels().any(|pixel| pixel.0 == COLOR)),
            "packed atlas should contain the decoded TGA color, not a placeholder"
        );
    });
}

#[test]
fn visible_content_fingerprint_ignores_pixels_outside_trimmed_source() {
    let mut first = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 0]));
    for y in 1..3 {
        for x in 1..3 {
            first.put_pixel(x, y, image::Rgba([40, 80, 120, 255]));
        }
    }
    let source = Rect::new(1, 1, 2, 2);
    let fingerprint = super::pack::visible_content_fingerprint(&first, source);

    let mut outside_changed = first.clone();
    outside_changed.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    assert_eq!(
        fingerprint,
        super::pack::visible_content_fingerprint(&outside_changed, source)
    );

    let mut visible_changed = first;
    visible_changed.put_pixel(1, 1, image::Rgba([0, 255, 0, 255]));
    assert_ne!(
        fingerprint,
        super::pack::visible_content_fingerprint(&visible_changed, source)
    );
}

fn make_test_vfs(dir: &Path) -> Vfs {
    use crate::vfs::directory_map::build_directory_map;
    let maps = build_directory_map(&[dir.to_path_buf()]).unwrap();
    Vfs {
        ini_path: dir.join("Morrowind.ini"),
        data_dirs: vec![dir.to_path_buf()],
        active_plugins: vec![],
        archives: vec![],
        maps,
    }
}

fn make_subset(vfs: &Vfs, texture: &str, has_alpha: bool) -> Subset {
    make_subset_with_flags(vfs, texture, has_alpha, false)
}

fn make_subset_with_flags(vfs: &Vfs, texture: &str, has_alpha: bool, has_uv_controller: bool) -> Subset {
    let mut subset = Subset::default();
    subset.texture = crate::SubsetTexture::Source(vfs.resolve_static_texture_sym_or_error(texture));
    subset.has_alpha = has_alpha;
    subset.has_uv_controller = has_uv_controller;
    subset.vertices.push(Vertex {
        uv_bound: UvBound {
            min_y: 0.0,
            max_x: 1.0,
            min_x: 0.0,
            max_y: 1.0,
        },
        ..Vertex::default()
    });
    subset
}

fn make_statics(vfs: &Vfs) -> DistantStatics {
    let mut statics = DistantStatics::default();
    let mut ds = DistantStatic::default();
    ds.subsets.push(make_subset(vfs, "shared\\leaf.dds", false));
    ds.subsets.push(make_subset(vfs, "shared\\leaf.dds", true));
    statics.insert("test".into(), ds);
    statics
}

fn make_atlas_cache(
    opaque_bounds: Vec<CachedUvBound>,
    alpha_bounds: Vec<CachedUvBound>,
    opaque_page_count: u32,
    alpha_page_count: u32,
) -> AtlasCache {
    fn family(bindings: Vec<CachedUvBound>, page_count: u32) -> CachedAtlasFamily {
        CachedAtlasFamily {
            family_config: AtlasFamilyConfig::default(),
            layout_input_digest: [0; 32],
            next_slot_id: 0,
            pages: (0..page_count).map(|_| CachedAtlasPage { width: 1, height: 1 }).collect(),
            slots: Vec::new(),
            key_slots: Vec::new(),
            bindings,
            texture_fingerprints: Vec::new(),
        }
    }
    AtlasCache {
        version: ATLAS_CACHE_VERSION,
        shared_config: AtlasSharedConfig::current(
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
        ),
        opaque: family(opaque_bounds, opaque_page_count),
        alpha: family(alpha_bounds, alpha_page_count),
    }
}

fn make_structural_family(
    textures: &IndexSet<String>,
    fingerprints: &[(u64, Hash)],
    family_config: AtlasFamilyConfig,
) -> CachedAtlasFamily {
    if textures.is_empty() {
        return CachedAtlasFamily {
            family_config,
            layout_input_digest: [0; 32],
            next_slot_id: 0,
            pages: Vec::new(),
            slots: Vec::new(),
            key_slots: Vec::new(),
            bindings: Vec::new(),
            texture_fingerprints: Vec::new(),
        };
    }
    let pages = vec![CachedAtlasPage { width: 128, height: 64 }];
    let slots: Vec<_> = textures
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let x = 8 + index as u32 * 40;
            CachedAtlasSlot {
                slot_id: index as u64,
                page_id: 0,
                reserved_rect: CachedAtlasRect {
                    x,
                    y: 8,
                    width: 33,
                    height: 33,
                },
                destination: CachedAtlasRect {
                    x: x + 16,
                    y: 24,
                    width: 1,
                    height: 1,
                },
                source: CachedAtlasRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                source_size: [1, 1],
                rotated: false,
                trimmed: false,
                provider_key: path.clone(),
                content_fingerprint: [index as u8; 32],
            }
        })
        .collect();
    let key_slots: Vec<_> = textures
        .iter()
        .enumerate()
        .map(|(index, path)| CachedAtlasKeySlot {
            path: path.clone(),
            slot_id: index as u64,
        })
        .collect();
    let bindings = super::reconcile::bindings_from_relations(&pages, &slots, &key_slots).unwrap();
    CachedAtlasFamily {
        family_config,
        layout_input_digest: [7; 32],
        next_slot_id: slots.len() as u64,
        pages,
        slots,
        key_slots,
        bindings: map_to_cached_bounds(&bindings),
        texture_fingerprints: build_fingerprint_entries(textures, fingerprints),
    }
}

fn make_structural_cache(
    textures: &AtlasTextureSet<IndexSet<String>>,
    fingerprints: &AtlasTextureSet<Vec<(u64, Hash)>>,
    plan: &SizingPlan,
) -> AtlasCache {
    AtlasCache {
        version: ATLAS_CACHE_VERSION,
        shared_config: AtlasSharedConfig::current(plan, TextureDedupeMode::Exact, TEST_GPU_MAX),
        opaque: make_structural_family(
            &textures.opaque,
            &fingerprints.opaque,
            AtlasFamilyConfig::current(plan, AtlasDomain::Opaque),
        ),
        alpha: make_structural_family(
            &textures.alpha,
            &fingerprints.alpha,
            AtlasFamilyConfig::current(plan, AtlasDomain::Alpha),
        ),
    }
}

fn make_atlas_texture_dir(dir: &Path) -> PathBuf {
    let texture_dir = dir.join("distantland").join("statics").join("textures");
    fs::create_dir_all(&texture_dir).unwrap();
    texture_dir
}

fn atlas_inventory_paths(cache: &AtlasCache) -> IndexSet<String> {
    [
        (OPAQUE_ATLAS_PREFIX, cache.opaque.pages.len()),
        (ALPHA_ATLAS_PREFIX, cache.alpha.pages.len()),
    ]
    .into_iter()
    .flat_map(|(prefix, count)| (0..count).map(move |page| format!(r"statics\textures\{}", atlas_page_string(prefix, page))))
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn validate_cache_bytes(
    textures: &AtlasTextureSet<IndexSet<String>>,
    plan: &SizingPlan,
    dedupe_mode: TextureDedupeMode,
    gpu_max: u32,
    committed_atlas_paths: &IndexSet<String>,
    cache: &AtlasCache,
) -> AtlasTextureSet<Option<CachedAtlasFamily>> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(cache).unwrap();
    let decoded = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&bytes).unwrap();
    validate_cache(textures, plan, dedupe_mode, gpu_max, committed_atlas_paths, decoded)
}

fn both_rejected(prior: &AtlasTextureSet<Option<CachedAtlasFamily>>) -> bool {
    prior.opaque.is_none() && prior.alpha.is_none()
}

fn both_accepted(prior: &AtlasTextureSet<Option<CachedAtlasFamily>>) -> bool {
    prior.opaque.is_some() && prior.alpha.is_some()
}

struct StructuralFixture {
    textures: AtlasTextureSet<IndexSet<String>>,
    plan: SizingPlan,
    cache: AtlasCache,
    inventory: IndexSet<String>,
}

fn two_family_structural_fixture() -> StructuralFixture {
    let textures = AtlasTextureSet::new(
        IndexSet::from_iter(["a.dds".to_owned(), "b.dds".to_owned()]),
        IndexSet::from_iter(["alpha.dds".to_owned()]),
    );
    let fingerprints = AtlasTextureSet::new(
        vec![(4, Hash::from_bytes([1; 32])), (4, Hash::from_bytes([2; 32]))],
        vec![(4, Hash::from_bytes([3; 32]))],
    );
    let plan = SizingPlan::uniform(TEST_MAX_TEXTURE_DIM);
    let cache = make_structural_cache(&textures, &fingerprints, &plan);
    let inventory = atlas_inventory_paths(&cache);
    StructuralFixture {
        textures,
        plan,
        cache,
        inventory,
    }
}

#[test]
fn cache_validation_rejects_old_version_globally() {
    let mut fixture = two_family_structural_fixture();
    assert!(
        both_accepted(&validate_cache_bytes(
            &fixture.textures,
            &fixture.plan,
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &fixture.inventory,
            &fixture.cache
        )),
        "baseline structural cache must validate both families"
    );

    fixture.cache.version -= 1;
    assert!(both_rejected(&validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &fixture.cache
    )));
}

#[test]
fn cache_validation_rejects_opaque_structural_failures_independently() {
    let fixture = two_family_structural_fixture();
    assert!(both_accepted(&validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &fixture.cache
    )));

    let assert_opaque_only = |cache: &AtlasCache| {
        let prior = validate_cache_bytes(
            &fixture.textures,
            &fixture.plan,
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &fixture.inventory,
            cache,
        );
        assert!(prior.opaque.is_none(), "opaque must be rejected");
        assert!(prior.alpha.is_some(), "alpha sibling must still be accepted");
    };

    let mut cache = fixture.cache.clone();
    cache.opaque.texture_fingerprints[0].path = "z.dds".into();
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.pages.clear();
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.pages[0].width = 0;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.pages[0].width = 3;
    cache.opaque.pages[0].height = 3;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.pages[0].width = TEST_GPU_MAX + 1;
    cache.opaque.pages[0].height = TEST_GPU_MAX + 1;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.slots[0].destination.width = 64;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.slots[0].rotated = true;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.bindings[0].max_x = 0.5;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.key_slots[0].slot_id = u64::MAX;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.slots[1].slot_id = cache.opaque.slots[0].slot_id;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.next_slot_id = cache.opaque.slots.last().unwrap().slot_id;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.slots[0].provider_key = "missing-provider.dds".into();
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.slots[1].reserved_rect = cache.opaque.slots[0].reserved_rect;
    cache.opaque.slots[1].destination = cache.opaque.slots[0].destination;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.slots[0].reserved_rect.x = 0;
    cache.opaque.slots[0].destination.x = 16;
    assert_opaque_only(&cache);

    let mut cache = fixture.cache.clone();
    cache.opaque.key_slots[1].path = cache.opaque.key_slots[0].path.clone();
    assert_opaque_only(&cache);
}

#[test]
fn cache_validation_rejects_alpha_structural_failures_independently() {
    let fixture = two_family_structural_fixture();
    assert!(both_accepted(&validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &fixture.cache
    )));

    let mut cache = fixture.cache.clone();
    cache.alpha.bindings[0].max_x = 0.5;
    let prior = validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &cache,
    );
    assert!(prior.opaque.is_some(), "opaque sibling must still be accepted");
    assert!(prior.alpha.is_none(), "alpha must be rejected");

    let mut cache = fixture.cache.clone();
    cache.alpha.slots[0].rotated = true;
    let prior = validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &cache,
    );
    assert!(prior.opaque.is_some());
    assert!(prior.alpha.is_none());
}

#[test]
fn cache_validation_uses_prior_internal_coverage_across_membership_changes() {
    let fixture = two_family_structural_fixture();
    assert!(both_accepted(&validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &fixture.cache
    )));

    let mut opaque_changed = fixture.textures.clone();
    opaque_changed.opaque.insert("extra_opaque.dds".into());
    let prior = validate_cache_bytes(
        &opaque_changed,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &fixture.cache,
    );
    assert!(both_accepted(&prior));

    let mut alpha_changed = fixture.textures.clone();
    alpha_changed.alpha.insert("extra_alpha.dds".into());
    let prior = validate_cache_bytes(
        &alpha_changed,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &fixture.cache,
    );
    assert!(both_accepted(&prior));
}

#[test]
fn cache_validation_rejects_inventory_per_family() {
    let fixture = two_family_structural_fixture();
    assert!(both_accepted(&validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &fixture.cache
    )));

    let mut extra_opaque = fixture.inventory.clone();
    extra_opaque.insert(format!(r"statics\textures\{}", atlas_page_string(OPAQUE_ATLAS_PREFIX, 1)));
    let prior = validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &extra_opaque,
        &fixture.cache,
    );
    assert!(prior.opaque.is_none());
    assert!(prior.alpha.is_some());

    let mut missing_opaque = fixture.inventory.clone();
    missing_opaque.shift_remove(&format!(r"statics\textures\{}", atlas_page_string(OPAQUE_ATLAS_PREFIX, 0)));
    let prior = validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &missing_opaque,
        &fixture.cache,
    );
    assert!(prior.opaque.is_none());
    assert!(prior.alpha.is_some());

    let mut extra_alpha = fixture.inventory.clone();
    extra_alpha.insert(format!(r"statics\textures\{}", atlas_page_string(ALPHA_ATLAS_PREFIX, 1)));
    let prior = validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &extra_alpha,
        &fixture.cache,
    );
    assert!(prior.opaque.is_some());
    assert!(prior.alpha.is_none());

    let mut missing_alpha = fixture.inventory.clone();
    missing_alpha.shift_remove(&format!(r"statics\textures\{}", atlas_page_string(ALPHA_ATLAS_PREFIX, 0)));
    let prior = validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &missing_alpha,
        &fixture.cache,
    );
    assert!(prior.opaque.is_some());
    assert!(prior.alpha.is_none());
}

#[test]
fn cache_validation_rejects_unclassifiable_inventory_globally() {
    let fixture = two_family_structural_fixture();
    assert!(both_accepted(&validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &fixture.inventory,
        &fixture.cache
    )));

    let mut bad = fixture.inventory.clone();
    bad.insert(r"statics\textures\not_an_atlas_page.dds".into());
    assert!(both_rejected(&validate_cache_bytes(
        &fixture.textures,
        &fixture.plan,
        TextureDedupeMode::Exact,
        TEST_GPU_MAX,
        &bad,
        &fixture.cache
    )));
}

#[test]
fn classify_atlas_page_path_matches_grammar_and_alpha_precedence() {
    use super::cache::{classify_atlas_page_path, partition_committed_atlas_paths};

    assert_eq!(
        classify_atlas_page_path(&format!(r"statics\textures\{}", atlas_page_string(OPAQUE_ATLAS_PREFIX, 0))),
        Some(AtlasDomain::Opaque)
    );
    assert_eq!(
        classify_atlas_page_path(&format!(r"statics\textures\{}", atlas_page_string(OPAQUE_ATLAS_PREFIX, 2))),
        Some(AtlasDomain::Opaque)
    );
    assert_eq!(
        classify_atlas_page_path(&format!(r"statics\textures\{}", atlas_page_string(ALPHA_ATLAS_PREFIX, 0))),
        Some(AtlasDomain::Alpha)
    );
    assert_eq!(
        classify_atlas_page_path(&format!(r"statics\textures\{}", atlas_page_string(ALPHA_ATLAS_PREFIX, 3))),
        Some(AtlasDomain::Alpha)
    );
    assert_eq!(classify_atlas_page_path(r"statics\textures\_mge_xe_atlas_extra.dds"), None);
    assert_eq!(classify_atlas_page_path(r"statics\textures\other.dds"), None);
    // Numbered grammar matches storage::path::parse_numbered_atlas_page exactly.
    assert_eq!(classify_atlas_page_path(r"statics\textures\_mge_xe_atlas_0.dds"), None);
    assert_eq!(classify_atlas_page_path(r"statics\textures\_mge_xe_atlas_00.dds"), None);
    assert_eq!(classify_atlas_page_path(r"statics\textures\_mge_xe_atlas_01.dds"), None);
    assert_eq!(classify_atlas_page_path(r"statics\textures\_mge_xe_atlas_alpha_0.dds"), None);
    assert_eq!(classify_atlas_page_path(r"statics\textures\_mge_xe_atlas_alpha_01.dds"), None);
    // u32::MAX + 1 as a decimal string overflows and must stay unclassifiable.
    assert_eq!(
        classify_atlas_page_path(r"statics\textures\_mge_xe_atlas_4294967296.dds"),
        None
    );

    let paths = IndexSet::from_iter([
        format!(r"statics\textures\{}", atlas_page_string(ALPHA_ATLAS_PREFIX, 0)),
        format!(r"statics\textures\{}", atlas_page_string(OPAQUE_ATLAS_PREFIX, 0)),
        format!(r"statics\textures\{}", atlas_page_string(OPAQUE_ATLAS_PREFIX, 1)),
    ]);
    let partitioned = partition_committed_atlas_paths(&paths).expect("atlas paths partition");
    assert_eq!(partitioned.opaque.len(), 2);
    assert_eq!(partitioned.alpha.len(), 1);
}

#[test]
fn cached_uv_bounds_apply_alpha_specific_entries() {
    with_temp_cwd(|temp| {
        write_texture(temp, "shared\\leaf.dds", b"shared");
        let vfs = make_test_vfs(temp);
        let mut statics = make_statics(&vfs);
        let cache = make_atlas_cache(
            vec![CachedUvBound {
                path: "shared\\leaf.dds".into(),
                page: 0,
                min_x: 0.1,
                max_x: 0.2,
                min_y: 0.3,
                max_y: 0.4,
            }],
            vec![CachedUvBound {
                path: "shared\\leaf.dds".into(),
                page: 0,
                min_x: 0.6,
                max_x: 0.7,
                min_y: 0.8,
                max_y: 0.9,
            }],
            1,
            1,
        );
        let opaque_map = cached_uv_map(&cache.opaque.bindings);
        let alpha_map = cached_uv_map(&cache.alpha.bindings);
        update_uv_bounds_from_maps(&mut statics, &opaque_map, &alpha_map, &vfs).unwrap();

        let subsets = &statics["test"].subsets;
        assert_eq!(subsets[0].vertices[0].uv_bound.min_x, 0.1);
        assert_eq!(subsets[0].vertices[0].uv_bound.max_y, 0.4);
        assert_eq!(subsets[0].texture, crate::SubsetTexture::AtlasPage(0));
        assert_eq!(subsets[1].vertices[0].uv_bound.min_x, 0.6);
        assert_eq!(subsets[1].vertices[0].uv_bound.max_y, 0.9);
        assert_eq!(subsets[1].texture, crate::SubsetTexture::AtlasPage(0));
    });
}

#[test]
fn update_uv_bounds_from_maps_uses_page_id_in_texture_path() {
    with_temp_cwd(|temp| {
        write_texture(temp, "shared\\leaf.dds", b"shared");
        let vfs = make_test_vfs(temp);
        let mut statics = make_statics(&vfs);

        let opaque_map: HashMap<String, (usize, UvBound)> = HashMap::from_iter([(
            "shared\\leaf.dds".to_string(),
            (
                0,
                UvBound {
                    min_x: 0.1,
                    max_x: 0.2,
                    min_y: 0.3,
                    max_y: 0.4,
                },
            ),
        )]);
        let alpha_map: HashMap<String, (usize, UvBound)> = HashMap::from_iter([(
            "shared\\leaf.dds".to_string(),
            (
                1,
                UvBound {
                    min_x: 0.6,
                    max_x: 0.7,
                    min_y: 0.8,
                    max_y: 0.9,
                },
            ),
        )]);

        update_uv_bounds_from_maps(&mut statics, &opaque_map, &alpha_map, &vfs).unwrap();

        let subsets = &statics["test"].subsets;
        assert_eq!(subsets[0].vertices[0].uv_bound.min_x, 0.1);
        assert_eq!(subsets[0].texture, crate::SubsetTexture::AtlasPage(0));
        assert_eq!(subsets[1].vertices[0].uv_bound.min_x, 0.6);
        assert_eq!(subsets[1].texture, crate::SubsetTexture::AtlasPage(1));
    });
}

#[test]
fn update_uv_bounds_from_maps_reuses_page_texture_paths() {
    with_temp_cwd(|temp| {
        let first = write_texture(temp, "shared\\leaf.dds", b"first");
        let second = write_texture(temp, "shared\\bark.dds", b"second");
        let vfs = make_test_vfs(temp);
        let mut statics = DistantStatics::default();
        let mut ds = DistantStatic::default();
        ds.subsets.push(make_subset(&vfs, &first, false));
        ds.subsets.push(make_subset(&vfs, &second, false));
        statics.insert("test".into(), ds);

        let opaque_map: HashMap<String, (usize, UvBound)> = HashMap::from_iter([
            (
                "shared\\leaf.dds".to_string(),
                (
                    0,
                    UvBound {
                        min_x: 0.1,
                        max_x: 0.2,
                        min_y: 0.3,
                        max_y: 0.4,
                    },
                ),
            ),
            (
                "shared\\bark.dds".to_string(),
                (
                    0,
                    UvBound {
                        min_x: 0.5,
                        max_x: 0.6,
                        min_y: 0.7,
                        max_y: 0.8,
                    },
                ),
            ),
        ]);

        update_uv_bounds_from_maps(&mut statics, &opaque_map, &HashMap::new(), &vfs).unwrap();

        let subsets = &statics["test"].subsets;
        assert_eq!(subsets[0].texture, crate::SubsetTexture::AtlasPage(0));
        assert_eq!(subsets[1].texture, crate::SubsetTexture::AtlasPage(0));
    });
}

#[test]
fn try_load_cache_requires_alpha_file_when_alpha_textures_exist() {
    with_temp_cwd(|temp| {
        let shared_opaque = write_texture(temp, "shared\\leaf.dds", b"opaque");
        let shared_alpha = write_texture(temp, "shared\\leaf2.dds", b"alpha");
        let atlas_paths = make_atlas_texture_dir(temp);
        fs::write(atlas_page_path(&atlas_paths, OPAQUE_ATLAS_PREFIX, 0), b"dds").unwrap();

        let textures = AtlasTextureSet::new([shared_opaque].into_iter().collect(), [shared_alpha].into_iter().collect());
        let vfs = make_test_vfs(temp);
        let fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&textures.opaque, &vfs),
            collect_fingerprints(&textures.alpha, &vfs),
        );

        let cache = make_structural_cache(&textures, &fingerprints, &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM));
        let committed_atlas_paths =
            IndexSet::from_iter([format!(r"statics\textures\{}", atlas_page_string(OPAQUE_ATLAS_PREFIX, 0))]);

        let prior = validate_cache_bytes(
            &textures,
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &committed_atlas_paths,
            &cache,
        );
        // Opaque inventory matches while the missing alpha page rejects only alpha.
        assert!(prior.opaque.is_some());
        assert!(prior.alpha.is_none());
    });
}

#[test]
fn try_load_cache_allows_empty_alpha_pool_without_alpha_file() {
    with_temp_cwd(|temp| {
        let opaque = write_texture(temp, "shared\\leaf.dds", b"opaque");
        let atlas_paths = make_atlas_texture_dir(temp);
        fs::write(atlas_page_path(&atlas_paths, OPAQUE_ATLAS_PREFIX, 0), b"dds").unwrap();

        let textures = AtlasTextureSet::new([opaque].into_iter().collect(), IndexSet::default());
        let vfs = make_test_vfs(temp);
        let fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&textures.opaque, &vfs),
            collect_fingerprints(&textures.alpha, &vfs),
        );

        let cache = make_structural_cache(&textures, &fingerprints, &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM));
        let committed_atlas_paths = atlas_inventory_paths(&cache);

        assert!(both_accepted(&validate_cache_bytes(
            &textures,
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &committed_atlas_paths,
            &cache,
        )));
    });
}

#[test]
fn try_load_cache_invalidates_on_config_change() {
    with_temp_cwd(|temp| {
        let opaque = write_texture(temp, "shared\\leaf.dds", b"opaque");
        let atlas_paths = make_atlas_texture_dir(temp);
        fs::write(atlas_page_path(&atlas_paths, OPAQUE_ATLAS_PREFIX, 0), b"dds").unwrap();

        let textures = AtlasTextureSet::new([opaque].into_iter().collect(), IndexSet::default());
        let vfs = make_test_vfs(temp);
        let fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&textures.opaque, &vfs),
            collect_fingerprints(&textures.alpha, &vfs),
        );

        let mut stale_config = AtlasSharedConfig::current(
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
        );
        stale_config.gpu_max = 2048; // different from TEST_GPU_MAX
        let mut cache = make_structural_cache(&textures, &fingerprints, &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM));
        cache.shared_config = stale_config;
        let committed_atlas_paths = atlas_inventory_paths(&cache);

        assert!(both_rejected(&validate_cache_bytes(
            &textures,
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &committed_atlas_paths,
            &cache,
        )));
    });
}

#[test]
fn try_load_cache_invalidates_on_trim_change() {
    with_temp_cwd(|temp| {
        let opaque = write_texture(temp, "shared\\leaf.dds", b"opaque");
        let atlas_paths = make_atlas_texture_dir(temp);
        fs::write(atlas_page_path(&atlas_paths, OPAQUE_ATLAS_PREFIX, 0), b"dds").unwrap();

        let textures = AtlasTextureSet::new([opaque].into_iter().collect(), IndexSet::default());
        let vfs = make_test_vfs(temp);
        let fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&textures.opaque, &vfs),
            collect_fingerprints(&textures.alpha, &vfs),
        );

        let mut stale_config = AtlasSharedConfig::current(
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
        );
        stale_config.trim = !stale_config.trim;
        let mut cache = make_structural_cache(&textures, &fingerprints, &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM));
        cache.shared_config = stale_config;
        let committed_atlas_paths = atlas_inventory_paths(&cache);

        assert!(both_rejected(&validate_cache_bytes(
            &textures,
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &committed_atlas_paths,
            &cache,
        )));
    });
}

#[test]
fn try_load_cache_invalidates_on_max_texture_dim_change() {
    with_temp_cwd(|temp| {
        let opaque = write_texture(temp, "shared\\leaf.dds", b"opaque");
        let atlas_paths = make_atlas_texture_dir(temp);
        fs::write(atlas_page_path(&atlas_paths, OPAQUE_ATLAS_PREFIX, 0), b"dds").unwrap();

        let textures = AtlasTextureSet::new([opaque].into_iter().collect(), IndexSet::default());
        let vfs = make_test_vfs(temp);
        let fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&textures.opaque, &vfs),
            collect_fingerprints(&textures.alpha, &vfs),
        );

        let cache = make_structural_cache(&textures, &fingerprints, &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM));
        let committed_atlas_paths = atlas_inventory_paths(&cache);

        assert!(both_rejected(&validate_cache_bytes(
            &textures,
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM * 2),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &committed_atlas_paths,
            &cache,
        )));
    });
}

#[test]
fn try_load_cache_keeps_structural_evidence_on_family_sizing_change() {
    with_temp_cwd(|temp| {
        let opaque = write_texture(temp, "shared\\leaf.dds", b"opaque");
        let atlas_paths = make_atlas_texture_dir(temp);
        fs::write(atlas_page_path(&atlas_paths, OPAQUE_ATLAS_PREFIX, 0), b"dds").unwrap();

        let textures = AtlasTextureSet::new([opaque.clone()].into_iter().collect(), IndexSet::default());
        let vfs = make_test_vfs(temp);
        let fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&textures.opaque, &vfs),
            collect_fingerprints(&textures.alpha, &vfs),
        );

        let cache = make_structural_cache(&textures, &fingerprints, &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM));
        let committed_atlas_paths = atlas_inventory_paths(&cache);

        let mut reduced = SizingPlan::uniform(TEST_MAX_TEXTURE_DIM);
        reduced.opaque_overrides.insert(opaque, TEST_MAX_TEXTURE_DIM / 2);
        let prior = validate_cache_bytes(
            &textures,
            &reduced,
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &committed_atlas_paths,
            &cache,
        );
        assert!(both_accepted(&prior));
        assert_ne!(
            prior.opaque.unwrap().family_config,
            AtlasFamilyConfig::current(&reduced, AtlasDomain::Opaque)
        );

        assert!(both_accepted(&validate_cache_bytes(
            &textures,
            &SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            &committed_atlas_paths,
            &cache,
        )));
    });
}

#[test]
fn family_config_sizing_overrides_are_strictly_ascending() {
    let mut plan = SizingPlan::uniform(TEST_MAX_TEXTURE_DIM);
    for key in ["zeta.dds", "alpha.dds", "mid.dds"] {
        plan.opaque_overrides.insert(key.to_owned(), 64);
        plan.alpha_overrides.insert(key.to_owned(), 64);
    }

    for domain in [AtlasDomain::Opaque, AtlasDomain::Alpha] {
        let overrides = AtlasFamilyConfig::current(&plan, domain).sizing_overrides;
        assert_eq!(overrides.len(), 3, "{domain:?} projected every override");
        assert!(
            overrides.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "{domain:?} sizing overrides must be strictly ascending, got {overrides:?}"
        );
    }
}

#[test]
fn setup_with_cache_state_rejects_malformed_bytes() {
    with_temp_cwd(|temp| {
        let opaque = write_texture(temp, "shared\\leaf.dds", b"opaque");
        let textures = AtlasTextureSet::new([opaque].into_iter().collect(), IndexSet::default());
        let vfs = make_test_vfs(temp);
        let fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&textures.opaque, &vfs),
            collect_fingerprints(&textures.alpha, &vfs),
        );
        let manager = AtlasManager::setup_with_cache_state(
            &textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            TEST_GPU_MAX,
            fingerprints,
            make_atlas_texture_dir(temp),
            Some(b"not-a-valid-atlas-cache"),
            &IndexSet::default(),
        );
        assert!(manager.prior.opaque.is_none());
        assert!(manager.prior.alpha.is_none());
    });
}

#[test]
fn collect_textures_skips_uv_animated_subsets() {
    with_temp_cwd(|temp| {
        let animated_only = write_texture(temp, "animated\\ghost.dds", b"animated");
        let normal_only = write_texture(temp, "static\\stone.dds", b"normal");
        let vfs = make_test_vfs(temp);
        let mut statics = DistantStatics::default();
        let mut ds = DistantStatic::default();
        ds.subsets.push(make_subset_with_flags(&vfs, &animated_only, false, true));
        ds.subsets.push(make_subset_with_flags(&vfs, &normal_only, false, false));
        statics.insert("test".into(), ds);

        let textures = AtlasTextureSet::from_distant_statics(&vfs, &statics);

        assert_eq!(
            textures.opaque,
            IndexSet::from_iter([vfs.resolve_texture_key(&normal_only).unwrap().to_owned()])
        );
        assert!(textures.alpha.is_empty());
    });
}

#[test]
fn update_uv_bounds_from_maps_leaves_uv_animated_subsets_unmodified() {
    with_temp_cwd(|temp| {
        let shared = write_texture(temp, "shared\\leaf.dds", b"shared");
        let vfs = make_test_vfs(temp);
        let mut statics = DistantStatics::default();
        let mut ds = DistantStatic::default();
        ds.subsets.push(make_subset_with_flags(&vfs, &shared, false, true));
        ds.subsets.push(make_subset_with_flags(&vfs, &shared, false, false));
        statics.insert("test".into(), ds);
        let animated_texture = statics["test"].subsets[0].texture;

        let opaque_map: HashMap<String, (usize, UvBound)> = HashMap::from_iter([(
            "shared\\leaf.dds".to_string(),
            (
                0,
                UvBound {
                    min_x: 0.1,
                    max_x: 0.2,
                    min_y: 0.3,
                    max_y: 0.4,
                },
            ),
        )]);

        update_uv_bounds_from_maps(&mut statics, &opaque_map, &HashMap::new(), &vfs).unwrap();

        let subsets = &statics["test"].subsets;
        assert_eq!(animated_texture, subsets[0].texture);
        let animated_bound = subsets[0].vertices[0].uv_bound;
        assert_eq!(animated_bound.min_x, 0.0);
        assert_eq!(animated_bound.max_x, 1.0);
        assert_eq!(animated_bound.min_y, 0.0);
        assert_eq!(animated_bound.max_y, 1.0);
        assert_eq!(subsets[1].vertices[0].uv_bound.min_x, 0.1);
        assert_eq!(subsets[1].vertices[0].uv_bound.max_y, 0.4);
        assert_eq!(subsets[1].texture, crate::SubsetTexture::AtlasPage(0));
    });
}

#[test]
fn collect_textures_skips_grass_statics() {
    with_temp_cwd(|temp| {
        let grass_only = write_texture(temp, "grass\\blade.dds", b"grass");
        let normal_first = write_texture(temp, "static\\z.dds", b"normal-z");
        let normal_second = write_texture(temp, "static\\a.dds", b"normal-a");
        let vfs = make_test_vfs(temp);
        let mut statics = DistantStatics::default();

        let mut grass = DistantStatic::default();
        grass.static_type = StaticType::StaticGrass;
        grass.subsets.push(make_subset(&vfs, &grass_only, false));
        statics.insert("grass".into(), grass);

        let mut normal = DistantStatic::default();
        for texture in [&normal_first, &normal_second, &normal_first] {
            normal.subsets.push(make_subset(&vfs, texture, false));
        }
        statics.insert("normal".into(), normal);

        let textures = AtlasTextureSet::from_distant_statics(&vfs, &statics);

        assert_eq!(
            textures.opaque.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                vfs.resolve_texture_key(&normal_second).unwrap(),
                vfs.resolve_texture_key(&normal_first).unwrap(),
            ]
        );
        assert!(textures.alpha.is_empty());
    });
}

#[test]
fn update_uv_bounds_from_maps_leaves_grass_statics_unmodified() {
    with_temp_cwd(|temp| {
        let shared = write_texture(temp, "shared\\leaf.dds", b"shared");
        let vfs = make_test_vfs(temp);
        let mut statics = DistantStatics::default();

        let mut grass = DistantStatic::default();
        grass.static_type = StaticType::StaticGrass;
        grass.subsets.push(make_subset(&vfs, &shared, false));
        statics.insert("grass".into(), grass);

        let mut normal = DistantStatic::default();
        normal.subsets.push(make_subset(&vfs, &shared, false));
        statics.insert("normal".into(), normal);

        let grass_texture = statics["grass"].subsets[0].texture;

        let opaque_map: HashMap<String, (usize, UvBound)> = HashMap::from_iter([(
            "shared\\leaf.dds".to_string(),
            (
                0,
                UvBound {
                    min_x: 0.1,
                    max_x: 0.2,
                    min_y: 0.3,
                    max_y: 0.4,
                },
            ),
        )]);

        update_uv_bounds_from_maps(&mut statics, &opaque_map, &HashMap::new(), &vfs).unwrap();

        // Grass keeps its source texture and identity uv_bound; the normal static is atlased.
        let grass_subsets = &statics["grass"].subsets;
        assert_eq!(grass_texture, grass_subsets[0].texture);
        let grass_bound = grass_subsets[0].vertices[0].uv_bound;
        assert_eq!(grass_bound.min_x, 0.0);
        assert_eq!(grass_bound.max_x, 1.0);
        assert_eq!(grass_bound.min_y, 0.0);
        assert_eq!(grass_bound.max_y, 1.0);

        let normal_subsets = &statics["normal"].subsets;
        assert_eq!(normal_subsets[0].vertices[0].uv_bound.min_x, 0.1);
        assert_eq!(normal_subsets[0].vertices[0].uv_bound.max_y, 0.4);
        assert_eq!(normal_subsets[0].texture, crate::SubsetTexture::AtlasPage(0));
    });
}

#[test]
fn missing_static_texture_error_key_is_collected_in_both_atlas_pools() {
    with_temp_cwd(|temp| {
        let vfs = make_test_vfs(temp);
        let mut statics = DistantStatics::default();
        let mut ds = DistantStatic::default();
        ds.subsets.push(make_subset(&vfs, "missing_opaque.dds", false));
        ds.subsets.push(make_subset(&vfs, "missing_alpha.dds", true));
        statics.insert("test".into(), ds);

        let textures = AtlasTextureSet::from_distant_statics(&vfs, &statics);

        assert_eq!(
            textures.opaque,
            IndexSet::from_iter([crate::vfs::STATIC_ERROR_TEXTURE_KEY.to_string()])
        );
        assert_eq!(
            textures.alpha,
            IndexSet::from_iter([crate::vfs::STATIC_ERROR_TEXTURE_KEY.to_string()])
        );

        let opaque_map: HashMap<String, (usize, UvBound)> = HashMap::from_iter([(
            crate::vfs::STATIC_ERROR_TEXTURE_KEY.to_string(),
            (
                0,
                UvBound {
                    min_x: 0.1,
                    max_x: 0.2,
                    min_y: 0.3,
                    max_y: 0.4,
                },
            ),
        )]);
        let alpha_map: HashMap<String, (usize, UvBound)> = HashMap::from_iter([(
            crate::vfs::STATIC_ERROR_TEXTURE_KEY.to_string(),
            (
                1,
                UvBound {
                    min_x: 0.5,
                    max_x: 0.6,
                    min_y: 0.7,
                    max_y: 0.8,
                },
            ),
        )]);

        update_uv_bounds_from_maps(&mut statics, &opaque_map, &alpha_map, &vfs).unwrap();

        let subsets = &statics["test"].subsets;
        assert_eq!(subsets[0].vertices[0].uv_bound.min_x, 0.1);
        assert_eq!(subsets[0].texture, crate::SubsetTexture::AtlasPage(0));
        assert_eq!(subsets[1].vertices[0].uv_bound.min_x, 0.5);
        assert_eq!(subsets[1].texture, crate::SubsetTexture::AtlasPage(1));
    });
}

#[test]
fn embedded_error_texture_fingerprint_is_collected() {
    with_temp_cwd(|temp| {
        let vfs = make_test_vfs(temp);
        let textures = IndexSet::from_iter([crate::vfs::STATIC_ERROR_TEXTURE_KEY.to_string()]);

        let fingerprints = collect_fingerprints(&textures, &vfs);

        assert_eq!(fingerprints.len(), 1);
        assert_eq!(fingerprints[0].0, crate::vfs::STATIC_ERROR_TEXTURE_DDS.len() as u64);
    });
}

#[test]
fn missing_cached_uv_bound_returns_contextual_error() {
    with_temp_cwd(|temp| {
        let texture = write_texture(temp, "shared\\leaf.dds", b"shared");
        let vfs = make_test_vfs(temp);
        let mut statics = DistantStatics::default();
        let mut ds = DistantStatic::default();
        ds.subsets.push(make_subset(&vfs, &texture, false));
        statics.insert("test".into(), ds);

        let error = update_uv_bounds_from_maps(&mut statics, &HashMap::new(), &HashMap::new(), &vfs)
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing opaque atlas UV bounds"));
        assert!(error.contains("shared\\leaf.dds"));
    });
}

#[test]
fn large_texture_does_not_fail_atlas_packing() {
    with_temp_cwd(|temp| {
        let img = image::RgbaImage::new(4200, 1);
        let mut tga_bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut tga_bytes), image::ImageFormat::Tga)
            .unwrap();

        let texture = write_texture(temp, "large.tga", &tga_bytes);

        let vfs = make_test_vfs(temp);
        let mut textures = crate::IndexSet::default();
        textures.insert(texture);

        let dims: HashMap<String, u32> = textures.iter().map(|key| (key.clone(), 4096)).collect();
        let result = compose_packed_pages(&textures, &vfs, &dims, TextureDedupeMode::Exact);
        assert!(result.is_ok(), "Failed to pack large texture: {:?}", result.err());
    });
}

fn uv_tuple(bound: &UvBound) -> (f32, f32, f32, f32) {
    (bound.min_x, bound.max_x, bound.min_y, bound.max_y)
}

fn two_opaque_statics(vfs: &Vfs, a: &str, b: &str) -> DistantStatics {
    opaque_statics(vfs, &[a, b])
}

fn opaque_statics(vfs: &Vfs, textures: &[&str]) -> DistantStatics {
    let mut statics = DistantStatics::default();
    let mut ds = DistantStatic::default();
    for texture in textures {
        ds.subsets.push(make_subset(vfs, texture, false));
    }
    statics.insert("test".into(), ds);
    statics
}

/// Builds a single distant static with two opaque subsets and one alpha subset, giving the opaque
/// family more than one page so a repack can be distinguished from single-page reuse.
fn two_opaque_one_alpha_statics(vfs: &Vfs, a: &str, b: &str, alpha: &str) -> DistantStatics {
    let mut statics = DistantStatics::default();
    let mut ds = DistantStatic::default();
    ds.subsets.push(make_subset(vfs, a, false));
    ds.subsets.push(make_subset(vfs, b, false));
    ds.subsets.push(make_subset(vfs, alpha, true));
    statics.insert("test".into(), ds);
    statics
}

fn opaque_alpha_statics(vfs: &Vfs, opaque: &str, alpha: &str) -> DistantStatics {
    let mut statics = DistantStatics::default();
    let mut ds = DistantStatic::default();
    ds.subsets.push(make_subset(vfs, opaque, false));
    ds.subsets.push(make_subset(vfs, alpha, true));
    statics.insert("test".into(), ds);
    statics
}

#[test]
fn render_streaming_stops_at_the_first_emitter_error() {
    with_temp_cwd(|temp| {
        const ATLAS_MAX: u32 = 128;
        let a = write_rgba_texture(temp, r"emit\one.bmp", [220, 30, 20, 255]);
        let b = write_rgba_texture(temp, r"emit\two.bmp", [20, 80, 220, 255]);
        let vfs = make_test_vfs(temp);
        let paths = make_atlas_texture_dir(temp);

        let mut statics = two_opaque_statics(&vfs, &a, &b);
        let textures = AtlasTextureSet::from_distant_statics(&vfs, &statics);
        let fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&textures.opaque, &vfs),
            collect_fingerprints(&textures.alpha, &vfs),
        );
        let manager = AtlasManager::setup_with_cache_state(
            &textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            fingerprints,
            paths,
            None,
            &IndexSet::default(),
        );
        let plan = manager.plan(&vfs, &mut statics, textures).unwrap();
        assert_eq!(
            plan.metrics().built_page_counts.opaque,
            2,
            "the fixture must build more than one page for the abort to be observable"
        );

        let mut emitted = 0_usize;
        let error = plan
            .render_streaming(|_| {
                emitted += 1;
                anyhow::bail!("emitter refused page {emitted}")
            })
            .unwrap_err();

        assert_eq!(emitted, 1, "no page may be composed after the first emitter error");
        assert!(error.to_string().contains("emitter refused page 1"), "{error}");
    });
}

fn publish_plan(plan: AtlasPublishPlan) -> Vec<PathBuf> {
    let mut written = Vec::new();
    plan.render_streaming(|write| {
        let AtlasPageWrite { path, bytes } = write;
        fs::write(&path, bytes).unwrap();
        written.push(path);
        Ok(())
    })
    .unwrap();
    written
}

fn apply_opaque_atlas(
    vfs: &Vfs,
    statics: &mut DistantStatics,
    temp: &Path,
    mode: TextureDedupeMode,
    cache_bytes: Option<&[u8]>,
) -> (AtlasManager, Vec<u8>) {
    let textures = AtlasTextureSet::from_distant_statics(vfs, statics);
    let paths = make_atlas_texture_dir(temp);
    let fingerprints = AtlasTextureSet::new(
        collect_fingerprints(&textures.opaque, vfs),
        collect_fingerprints(&textures.alpha, vfs),
    );
    let committed_atlas_paths = cache_bytes
        .and_then(|bytes| rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(bytes).ok())
        .map(|cache| atlas_inventory_paths(&cache))
        .unwrap_or_default();
    let manager = AtlasManager::setup_with_cache_state(
        &textures,
        SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
        mode,
        crate::DEFAULT_STATIC_ATLAS_MAX_SIZE,
        fingerprints,
        paths.clone(),
        cache_bytes,
        &committed_atlas_paths,
    );
    let plan = manager.plan(vfs, statics, textures).unwrap();
    let next_cache = plan.cache_bytes().to_vec();
    publish_plan(plan);
    (manager, next_cache)
}

#[test]
fn planned_page_composes_trimmed_sources_at_their_frame_origin() {
    // Trim and compose are only correct together: the packer allocates a frame sized to the visible
    // rectangle, and compositing must land exactly those pixels at the frame origin. A source with
    // a fully transparent border is the case where an off-by-one in either half shows up.
    with_temp_cwd(|temp| {
        const VISIBLE: image::Rgba<u8> = image::Rgba([40, 80, 220, 255]);
        let texture_root = temp.join("textures").join("streaming");
        fs::create_dir_all(&texture_root).unwrap();
        let mut trimmed = image::RgbaImage::from_pixel(10, 8, image::Rgba([0, 0, 0, 0]));
        for y in 2..6 {
            for x in 3..8 {
                trimmed.put_pixel(x, y, VISIBLE);
            }
        }
        trimmed.save(texture_root.join("trimmed.tga")).unwrap();

        let vfs = make_test_vfs(temp);
        let textures: IndexSet<String> = ["streaming\\trimmed.tga".to_string()].into_iter().collect();
        let dims: HashMap<String, u32> = textures.iter().map(|key| (key.clone(), 64)).collect();

        let (prepared, _alias) =
            super::pack::prepare_textures(&textures, &vfs, &dims, TextureDedupeMode::Off, AtlasDomain::Opaque);
        let images = prepared.images;
        let atlas = super::pack::pack_layout_only(prepared.layout_items, TEST_GPU_MAX).unwrap();

        let page = atlas.pages.first().expect("one texture should pack into one page");
        let frame = page.frames.first().expect("the page should hold the packed texture");
        assert!(frame.trimmed);
        assert_eq!(
            (frame.source.x, frame.source.y, frame.source.w, frame.source.h),
            (3, 2, 5, 4),
            "frame should cover only the visible rectangle of the source"
        );

        let composed = super::pack::compose_planned_page(&images, page);
        for y in 0..frame.source.h {
            for x in 0..frame.source.w {
                assert_eq!(*composed.get_pixel(frame.frame.x + x, frame.frame.y + y), VISIBLE);
            }
        }
    });
}

#[test]
fn exact_dedupe_aliases_identical_source_bytes() {
    with_temp_cwd(|temp| {
        // Two distinct keys, byte-identical on disk: Tier-0 source-byte identity.
        write_texture(temp, "dedupe\\a.dds", b"identical-bytes");
        write_texture(temp, "dedupe\\b.dds", b"identical-bytes");
        let vfs = make_test_vfs(temp);
        let mut statics = two_opaque_statics(&vfs, "dedupe\\a.dds", "dedupe\\b.dds");

        apply_opaque_atlas(&vfs, &mut statics, temp, TextureDedupeMode::Exact, None);

        let subsets = &statics["test"].subsets;
        assert_eq!(
            uv_tuple(&subsets[0].vertices[0].uv_bound),
            uv_tuple(&subsets[1].vertices[0].uv_bound)
        );
        assert_eq!(subsets[0].texture, subsets[1].texture);
    });
}

#[test]
fn exact_dedupe_aliases_decoded_identical_but_off_keeps_separate() {
    // Two different-on-disk textures that both fail to decode fall back to the identical 1x1
    // magenta placeholder, so they feed identical decoded pixels while differing in source bytes:
    // the Tier-1 decoded-identity case. Under Off they must stay on separate frames.
    let uvs = |statics: &DistantStatics| {
        let subsets = &statics["test"].subsets;
        (
            uv_tuple(&subsets[0].vertices[0].uv_bound),
            uv_tuple(&subsets[1].vertices[0].uv_bound),
        )
    };

    with_temp_cwd(|temp| {
        write_texture(temp, "dedupe\\a.dds", b"alpha-bytes");
        write_texture(temp, "dedupe\\b.dds", b"bravo-bytes");
        let vfs = make_test_vfs(temp);
        let mut statics = two_opaque_statics(&vfs, "dedupe\\a.dds", "dedupe\\b.dds");
        apply_opaque_atlas(&vfs, &mut statics, temp, TextureDedupeMode::Exact, None);
        let (a, b) = uvs(&statics);
        assert_eq!(a, b, "decoded-identical textures should alias under Exact");
    });

    with_temp_cwd(|temp| {
        write_texture(temp, "dedupe\\a.dds", b"alpha-bytes");
        write_texture(temp, "dedupe\\b.dds", b"bravo-bytes");
        let vfs = make_test_vfs(temp);
        let mut statics = two_opaque_statics(&vfs, "dedupe\\a.dds", "dedupe\\b.dds");
        apply_opaque_atlas(&vfs, &mut statics, temp, TextureDedupeMode::Off, None);
        let (a, b) = uvs(&statics);
        assert_ne!(a, b, "Off must keep distinct textures on separate frames");
    });
}

#[test]
fn dedupe_cache_hit_reproduces_per_original_uvs() {
    with_temp_cwd(|temp| {
        write_texture(temp, "dedupe\\a.dds", b"identical-bytes");
        write_texture(temp, "dedupe\\b.dds", b"identical-bytes");
        let vfs = make_test_vfs(temp);

        let mut fresh = two_opaque_statics(&vfs, "dedupe\\a.dds", "dedupe\\b.dds");
        let (first, cache_bytes) = apply_opaque_atlas(&vfs, &mut fresh, temp, TextureDedupeMode::Exact, None);
        assert!(
            first.prior.opaque.is_none() && first.prior.alpha.is_none(),
            "first run should miss the cache"
        );

        let mut cached = two_opaque_statics(&vfs, "dedupe\\a.dds", "dedupe\\b.dds");
        let (second, _) = apply_opaque_atlas(&vfs, &mut cached, temp, TextureDedupeMode::Exact, Some(&cache_bytes));
        assert!(
            second.prior.opaque.is_some() && second.prior.alpha.is_some(),
            "second run should hit the cache"
        );

        let f = &fresh["test"].subsets;
        let c = &cached["test"].subsets;
        // The cache stores per-original UV bounds, so both deduped originals are reproduced.
        assert_eq!(uv_tuple(&c[0].vertices[0].uv_bound), uv_tuple(&f[0].vertices[0].uv_bound));
        assert_eq!(uv_tuple(&c[1].vertices[0].uv_bound), uv_tuple(&f[1].vertices[0].uv_bound));
        assert_eq!(uv_tuple(&c[0].vertices[0].uv_bound), uv_tuple(&c[1].vertices[0].uv_bound));
    });
}

#[test]
fn stable_layout_edit_builds_only_the_consuming_page_then_hits_cache() {
    with_temp_cwd(|temp| {
        const ATLAS_MAX: u32 = 128;
        let a = write_rgba_texture(temp, r"stable\a.bmp", [220, 30, 20, 255]);
        let b = write_rgba_texture(temp, r"stable\b.bmp", [20, 80, 220, 255]);
        let vfs = make_test_vfs(temp);
        let paths = make_atlas_texture_dir(temp);

        let mut first_statics = two_opaque_statics(&vfs, &a, &b);
        let first_textures = AtlasTextureSet::from_distant_statics(&vfs, &first_statics);
        let first_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&first_textures.opaque, &vfs),
            collect_fingerprints(&first_textures.alpha, &vfs),
        );
        let first_manager = AtlasManager::setup_with_cache_state(
            &first_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            first_fingerprints,
            paths.clone(),
            None,
            &IndexSet::default(),
        );
        let first_plan = first_manager.plan(&vfs, &mut first_statics, first_textures).unwrap();
        let first_binding_digest = first_plan.binding_digest();
        let first_cache_bytes = first_plan.cache_bytes().to_vec();
        let first_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&first_cache_bytes).unwrap();
        assert_eq!(
            first_plan.metrics().built_page_counts.opaque,
            2,
            "packed pages: {:?}, frames: {:?}",
            first_cache.opaque.pages,
            first_cache.opaque.slots
        );
        let a_slot = first_cache
            .opaque
            .key_slots
            .iter()
            .find(|relation| relation.path == a)
            .unwrap()
            .slot_id;
        let a_page = first_cache
            .opaque
            .slots
            .iter()
            .find(|slot| slot.slot_id == a_slot)
            .unwrap()
            .page_id as usize;
        assert_eq!(publish_plan(first_plan).len(), 2);

        write_rgba_texture(temp, r"stable\a.bmp", [30, 220, 80, 255]);
        let mut second_statics = two_opaque_statics(&vfs, &a, &b);
        let second_textures = AtlasTextureSet::from_distant_statics(&vfs, &second_statics);
        let second_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&second_textures.opaque, &vfs),
            collect_fingerprints(&second_textures.alpha, &vfs),
        );
        let first_inventory = atlas_inventory_paths(&first_cache);
        let second_manager = AtlasManager::setup_with_cache_state(
            &second_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            second_fingerprints,
            paths.clone(),
            Some(&first_cache_bytes),
            &first_inventory,
        );
        let second_plan = second_manager.plan(&vfs, &mut second_statics, second_textures).unwrap();
        assert!(second_plan.metrics().layout_hits.opaque);
        assert!(second_plan.metrics().layout_hits.alpha);
        assert_eq!(second_plan.metrics().built_page_counts.opaque, 1);
        assert_eq!(second_plan.metrics().carried_page_counts.opaque, 1);
        assert_eq!(second_plan.metrics().built_page_counts.alpha, 0);
        assert_eq!(second_plan.binding_digest(), first_binding_digest);
        assert!(!second_plan.cache_hit());
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.added, 0);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.removed, 0);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.changed, 0);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.unchanged, 2);
        let second_cache_bytes = second_plan.cache_bytes().to_vec();
        let written = publish_plan(second_plan);
        assert_eq!(written, vec![atlas_page_path(&paths, OPAQUE_ATLAS_PREFIX, a_page)]);

        let second_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&second_cache_bytes).unwrap();
        let mut third_statics = two_opaque_statics(&vfs, &a, &b);
        let third_textures = AtlasTextureSet::from_distant_statics(&vfs, &third_statics);
        let third_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&third_textures.opaque, &vfs),
            collect_fingerprints(&third_textures.alpha, &vfs),
        );
        let third_manager = AtlasManager::setup_with_cache_state(
            &third_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            third_fingerprints,
            paths,
            Some(&second_cache_bytes),
            &atlas_inventory_paths(&second_cache),
        );
        let third_plan = third_manager.plan(&vfs, &mut third_statics, third_textures).unwrap();
        assert!(third_plan.cache_hit());
        assert_eq!(third_plan.metrics().dirty_page_count, 0);
        assert_eq!(third_plan.metrics().decoded_texture_count, 0);
    });
}

#[test]
fn exact_alias_addition_reuses_the_existing_slot_without_page_writes() {
    with_temp_cwd(|temp| {
        const ATLAS_MAX: u32 = 128;
        let a = write_rgba_texture(temp, r"alias\a.bmp", [80, 120, 200, 255]);
        let b = write_rgba_texture(temp, r"alias\b.bmp", [80, 120, 200, 255]);
        let vfs = make_test_vfs(temp);
        let paths = make_atlas_texture_dir(temp);

        let mut first_statics = opaque_statics(&vfs, &[&a]);
        let first_textures = AtlasTextureSet::from_distant_statics(&vfs, &first_statics);
        let first_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&first_textures.opaque, &vfs),
            collect_fingerprints(&first_textures.alpha, &vfs),
        );
        let first_manager = AtlasManager::setup_with_cache_state(
            &first_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            first_fingerprints,
            paths.clone(),
            None,
            &IndexSet::default(),
        );
        let first_plan = first_manager.plan(&vfs, &mut first_statics, first_textures).unwrap();
        let first_cache_bytes = first_plan.cache_bytes().to_vec();
        let first_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&first_cache_bytes).unwrap();
        let first_slot = first_cache.opaque.key_slots[0].slot_id;

        let mut second_statics = opaque_statics(&vfs, &[&a, &b]);
        let second_textures = AtlasTextureSet::from_distant_statics(&vfs, &second_statics);
        let second_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&second_textures.opaque, &vfs),
            collect_fingerprints(&second_textures.alpha, &vfs),
        );
        let second_manager = AtlasManager::setup_with_cache_state(
            &second_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            second_fingerprints,
            paths.clone(),
            Some(&first_cache_bytes),
            &atlas_inventory_paths(&first_cache),
        );
        let second_plan = second_manager.plan(&vfs, &mut second_statics, second_textures).unwrap();
        let second_cache_bytes = second_plan.cache_bytes().to_vec();
        let second_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&second_cache_bytes).unwrap();
        assert_eq!(second_cache.opaque.slots.len(), 1);
        assert!(
            second_cache
                .opaque
                .key_slots
                .iter()
                .all(|relation| relation.slot_id == first_slot)
        );
        assert_eq!(second_plan.metrics().built_page_counts.opaque, 0);
        assert_eq!(second_plan.metrics().carried_page_counts.opaque, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.retained_slots, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.allocated_slots, 0);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.added, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.changed, 0);
        assert!(!second_plan.cache_hit());

        let mut third_statics = opaque_statics(&vfs, &[&a, &b]);
        let third_textures = AtlasTextureSet::from_distant_statics(&vfs, &third_statics);
        let third_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&third_textures.opaque, &vfs),
            collect_fingerprints(&third_textures.alpha, &vfs),
        );
        let third_manager = AtlasManager::setup_with_cache_state(
            &third_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            third_fingerprints,
            paths,
            Some(&second_cache_bytes),
            &atlas_inventory_paths(&second_cache),
        );
        let third_plan = third_manager.plan(&vfs, &mut third_statics, third_textures).unwrap();
        assert!(third_plan.cache_hit());
        assert_eq!(third_plan.metrics().dirty_page_count, 0);
    });
}

#[test]
fn removing_dedupe_provider_promotes_survivor_without_moving_or_writing() {
    with_temp_cwd(|temp| {
        const ATLAS_MAX: u32 = 128;
        let a = write_rgba_texture(temp, r"provider\a.bmp", [160, 90, 40, 255]);
        let b = write_rgba_texture(temp, r"provider\b.bmp", [160, 90, 40, 255]);
        let vfs = make_test_vfs(temp);
        let paths = make_atlas_texture_dir(temp);

        let mut first_statics = opaque_statics(&vfs, &[&a, &b]);
        let first_textures = AtlasTextureSet::from_distant_statics(&vfs, &first_statics);
        let first_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&first_textures.opaque, &vfs),
            collect_fingerprints(&first_textures.alpha, &vfs),
        );
        let first_manager = AtlasManager::setup_with_cache_state(
            &first_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            first_fingerprints,
            paths.clone(),
            None,
            &IndexSet::default(),
        );
        let first_plan = first_manager.plan(&vfs, &mut first_statics, first_textures).unwrap();
        let first_cache_bytes = first_plan.cache_bytes().to_vec();
        let first_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&first_cache_bytes).unwrap();
        assert_eq!(first_cache.opaque.slots[0].provider_key, a);
        let first_slot = first_cache.opaque.slots[0].clone();

        let mut second_statics = opaque_statics(&vfs, &[&b]);
        let second_textures = AtlasTextureSet::from_distant_statics(&vfs, &second_statics);
        let second_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&second_textures.opaque, &vfs),
            collect_fingerprints(&second_textures.alpha, &vfs),
        );
        let second_manager = AtlasManager::setup_with_cache_state(
            &second_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            second_fingerprints,
            paths,
            Some(&first_cache_bytes),
            &atlas_inventory_paths(&first_cache),
        );
        let second_plan = second_manager.plan(&vfs, &mut second_statics, second_textures).unwrap();
        let second_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(second_plan.cache_bytes()).unwrap();
        let promoted = &second_cache.opaque.slots[0];
        assert_eq!(promoted.slot_id, first_slot.slot_id);
        assert_eq!(promoted.page_id, first_slot.page_id);
        assert_eq!(promoted.reserved_rect, first_slot.reserved_rect);
        assert_eq!(promoted.destination, first_slot.destination);
        assert_eq!(promoted.provider_key, b);
        assert_eq!(second_plan.metrics().built_page_counts.opaque, 0);
        assert_eq!(second_plan.metrics().carried_page_counts.opaque, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.provider_promoted_slots, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.removed, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.changed, 0);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.unchanged, 1);
        assert!(!second_plan.cache_hit());
    });
}

#[test]
fn dimension_change_repacks_only_the_affected_family() {
    with_temp_cwd(|temp| {
        const ATLAS_MAX: u32 = 128;
        let a = write_rgba_texture(temp, r"repack\a.bmp", [220, 30, 20, 255]);
        let b = write_rgba_texture(temp, r"repack\b.bmp", [20, 80, 220, 255]);
        let alpha = write_rgba_texture(temp, r"repack\alpha.bmp", [20, 80, 220, 128]);
        let vfs = make_test_vfs(temp);
        let paths = make_atlas_texture_dir(temp);

        let mut first_statics = two_opaque_one_alpha_statics(&vfs, &a, &b, &alpha);
        let first_textures = AtlasTextureSet::from_distant_statics(&vfs, &first_statics);
        let first_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&first_textures.opaque, &vfs),
            collect_fingerprints(&first_textures.alpha, &vfs),
        );
        let first_manager = AtlasManager::setup_with_cache_state(
            &first_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            first_fingerprints,
            paths.clone(),
            None,
            &IndexSet::default(),
        );
        let first_plan = first_manager.plan(&vfs, &mut first_statics, first_textures).unwrap();
        let first_binding_digest = first_plan.binding_digest();
        let first_cache_bytes = first_plan.cache_bytes().to_vec();
        let first_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&first_cache_bytes).unwrap();
        assert_eq!(
            first_plan.metrics().built_page_counts.opaque,
            2,
            "packed pages: {:?}",
            first_cache.opaque.pages
        );
        assert_eq!(first_plan.metrics().built_page_counts.alpha, 1);
        publish_plan(first_plan);

        // Re-author one opaque source at a different dimension. Its layout item geometry changes, so
        // the opaque family's layout-input digest no longer matches its committed evidence while the
        // untouched alpha family still matches on exact fingerprints alone.
        write_sized_rgba_texture(temp, r"repack\a.bmp", 64, [220, 30, 20, 255]);
        let mut second_statics = two_opaque_one_alpha_statics(&vfs, &a, &b, &alpha);
        let second_textures = AtlasTextureSet::from_distant_statics(&vfs, &second_statics);
        let second_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&second_textures.opaque, &vfs),
            collect_fingerprints(&second_textures.alpha, &vfs),
        );
        let second_manager = AtlasManager::setup_with_cache_state(
            &second_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            second_fingerprints,
            paths.clone(),
            Some(&first_cache_bytes),
            &atlas_inventory_paths(&first_cache),
        );
        let second_plan = second_manager.plan(&vfs, &mut second_statics, second_textures).unwrap();

        assert!(
            !second_plan.metrics().layout_hits.opaque,
            "the resized family must miss its cached layout"
        );
        assert!(
            second_plan.metrics().layout_hits.alpha,
            "the untouched family must keep its cached layout"
        );
        assert_eq!(second_plan.metrics().built_page_counts.opaque, 1);
        assert_eq!(second_plan.metrics().carried_page_counts.opaque, 1);
        assert_eq!(second_plan.metrics().built_page_counts.alpha, 0);
        assert_eq!(second_plan.metrics().carried_page_counts.alpha, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.relocated_slots, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.changed, 1);
        assert_ne!(second_plan.binding_digest(), first_binding_digest);
        assert!(!second_plan.cache_hit());

        // Only the repacked family's pages are rewritten; the carried alpha page stays untouched.
        let second_cache_bytes = second_plan.cache_bytes().to_vec();
        let written = publish_plan(second_plan);
        assert_eq!(written.len(), 1, "written: {written:?}");
        assert!(
            !written.contains(&atlas_page_path(&paths, ALPHA_ATLAS_PREFIX, 0)),
            "the carried alpha page must not be rewritten"
        );

        // The repacked evidence is reusable in turn: an unchanged follow-up carries every page.
        let second_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&second_cache_bytes).unwrap();
        let mut third_statics = two_opaque_one_alpha_statics(&vfs, &a, &b, &alpha);
        let third_textures = AtlasTextureSet::from_distant_statics(&vfs, &third_statics);
        let third_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&third_textures.opaque, &vfs),
            collect_fingerprints(&third_textures.alpha, &vfs),
        );
        let third_manager = AtlasManager::setup_with_cache_state(
            &third_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            third_fingerprints,
            paths,
            Some(&second_cache_bytes),
            &atlas_inventory_paths(&second_cache),
        );
        let third_plan = third_manager.plan(&vfs, &mut third_statics, third_textures).unwrap();
        assert!(third_plan.cache_hit());
        assert_eq!(third_plan.metrics().dirty_page_count, 0);
    });
}

#[test]
fn alpha_only_stable_edit_carries_the_opaque_family() {
    with_temp_cwd(|temp| {
        const ATLAS_MAX: u32 = 128;
        let opaque = write_rgba_texture(temp, r"stable\opaque.bmp", [220, 30, 20, 255]);
        let alpha = write_rgba_texture(temp, r"stable\alpha.bmp", [20, 80, 220, 128]);
        let vfs = make_test_vfs(temp);
        let paths = make_atlas_texture_dir(temp);

        let mut first_statics = opaque_alpha_statics(&vfs, &opaque, &alpha);
        let first_textures = AtlasTextureSet::from_distant_statics(&vfs, &first_statics);
        let first_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&first_textures.opaque, &vfs),
            collect_fingerprints(&first_textures.alpha, &vfs),
        );
        let first_manager = AtlasManager::setup_with_cache_state(
            &first_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            first_fingerprints,
            paths.clone(),
            None,
            &IndexSet::default(),
        );
        let first_plan = first_manager.plan(&vfs, &mut first_statics, first_textures).unwrap();
        let first_digest = first_plan.binding_digest();
        let first_cache_bytes = first_plan.cache_bytes().to_vec();
        let first_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&first_cache_bytes).unwrap();

        write_rgba_texture(temp, r"stable\alpha.bmp", [220, 180, 20, 128]);
        let mut second_statics = opaque_alpha_statics(&vfs, &opaque, &alpha);
        let second_textures = AtlasTextureSet::from_distant_statics(&vfs, &second_statics);
        let second_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&second_textures.opaque, &vfs),
            collect_fingerprints(&second_textures.alpha, &vfs),
        );
        let second_manager = AtlasManager::setup_with_cache_state(
            &second_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            second_fingerprints,
            paths,
            Some(&first_cache_bytes),
            &atlas_inventory_paths(&first_cache),
        );
        let second_plan = second_manager.plan(&vfs, &mut second_statics, second_textures).unwrap();

        assert!(second_plan.metrics().layout_hits.opaque);
        assert!(second_plan.metrics().layout_hits.alpha);
        assert_eq!(second_plan.metrics().carried_page_counts.opaque, 1);
        assert_eq!(second_plan.metrics().built_page_counts.opaque, 0);
        assert_eq!(second_plan.metrics().carried_page_counts.alpha, 0);
        assert_eq!(second_plan.metrics().built_page_counts.alpha, 1);
        assert_eq!(second_plan.binding_digest(), first_digest);
    });
}

/// Adding a distinct opaque key reconciles that family while alpha carries completely.
#[test]
fn opaque_membership_change_reconciles_opaque_and_carries_alpha() {
    with_temp_cwd(|temp| {
        const ATLAS_MAX: u32 = 128;
        let opaque = write_rgba_texture(temp, r"member\opaque.bmp", [220, 30, 20, 255]);
        let alpha = write_rgba_texture(temp, r"member\alpha.bmp", [20, 80, 220, 128]);
        let vfs = make_test_vfs(temp);
        let paths = make_atlas_texture_dir(temp);

        let mut first_statics = opaque_alpha_statics(&vfs, &opaque, &alpha);
        let first_textures = AtlasTextureSet::from_distant_statics(&vfs, &first_statics);
        let first_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&first_textures.opaque, &vfs),
            collect_fingerprints(&first_textures.alpha, &vfs),
        );
        let first_manager = AtlasManager::setup_with_cache_state(
            &first_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            first_fingerprints,
            paths.clone(),
            None,
            &IndexSet::default(),
        );
        let first_plan = first_manager.plan(&vfs, &mut first_statics, first_textures).unwrap();
        let first_cache_bytes = first_plan.cache_bytes().to_vec();
        let first_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&first_cache_bytes).unwrap();
        publish_plan(first_plan);

        let opaque_b = write_rgba_texture(temp, r"member\opaque_b.bmp", [30, 180, 40, 255]);
        let mut second_statics = two_opaque_one_alpha_statics(&vfs, &opaque, &opaque_b, &alpha);
        let second_textures = AtlasTextureSet::from_distant_statics(&vfs, &second_statics);
        let second_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&second_textures.opaque, &vfs),
            collect_fingerprints(&second_textures.alpha, &vfs),
        );
        let second_manager = AtlasManager::setup_with_cache_state(
            &second_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            second_fingerprints,
            paths.clone(),
            Some(&first_cache_bytes),
            &atlas_inventory_paths(&first_cache),
        );
        assert!(second_manager.prior.opaque.is_some());
        assert!(second_manager.prior.alpha.is_some());

        let second_plan = second_manager.plan(&vfs, &mut second_statics, second_textures).unwrap();
        assert!(
            !second_plan.metrics().layout_hits.opaque,
            "opaque membership change must miss the opaque layout"
        );
        assert!(
            second_plan.metrics().layout_hits.alpha,
            "unchanged alpha membership must keep its layout"
        );
        assert!(second_plan.metrics().built_page_counts.opaque > 0);
        assert_eq!(
            second_plan.metrics().family_metrics.opaque.plan_mode,
            crate::atlas::AtlasFamilyPlanMode::Reconciled
        );
        let second_cache_debug = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(second_plan.cache_bytes()).unwrap();
        let first_slot = first_cache
            .opaque
            .key_slots
            .iter()
            .find(|entry| entry.path == opaque)
            .unwrap()
            .slot_id;
        let second_slot = second_cache_debug
            .opaque
            .key_slots
            .iter()
            .find(|entry| entry.path == opaque)
            .unwrap()
            .slot_id;
        assert_eq!(
            second_slot, first_slot,
            "first={:?} second={:?}",
            first_cache.opaque, second_cache_debug.opaque
        );
        assert_eq!(second_plan.metrics().family_metrics.opaque.retained_slots, 1);
        assert_eq!(second_plan.binding_deltas().opaque.as_ref().unwrap().added.len(), 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.added, 1);
        assert_eq!(second_plan.metrics().family_metrics.opaque.binding_delta.changed, 0);
        assert_eq!(second_plan.metrics().carried_page_counts.alpha, first_cache.alpha.pages.len());
        assert_eq!(second_plan.metrics().built_page_counts.alpha, 0);
        assert!(!second_plan.cache_hit());

        let second_cache_bytes = second_plan.cache_bytes().to_vec();
        let written = publish_plan(second_plan);
        assert!(
            !written
                .iter()
                .any(|path| path.file_name().is_some_and(|name| name.to_string_lossy().contains("alpha"))),
            "written pages must exclude carried alpha pages: {written:?}"
        );

        let second_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&second_cache_bytes).unwrap();
        let mut third_statics = two_opaque_one_alpha_statics(&vfs, &opaque, &opaque_b, &alpha);
        let third_textures = AtlasTextureSet::from_distant_statics(&vfs, &third_statics);
        let third_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&third_textures.opaque, &vfs),
            collect_fingerprints(&third_textures.alpha, &vfs),
        );
        let third_manager = AtlasManager::setup_with_cache_state(
            &third_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            third_fingerprints,
            paths,
            Some(&second_cache_bytes),
            &atlas_inventory_paths(&second_cache),
        );
        let third_plan = third_manager.plan(&vfs, &mut third_statics, third_textures).unwrap();
        assert!(third_plan.cache_hit());
        assert_eq!(third_plan.metrics().dirty_page_count, 0);
    });
}

/// Adding a distinct alpha key reconciles that family while opaque carries completely.
#[test]
fn alpha_membership_change_reconciles_alpha_and_carries_opaque() {
    with_temp_cwd(|temp| {
        const ATLAS_MAX: u32 = 128;
        let opaque = write_rgba_texture(temp, r"member\opaque.bmp", [220, 30, 20, 255]);
        let alpha = write_rgba_texture(temp, r"member\alpha.bmp", [20, 80, 220, 128]);
        let vfs = make_test_vfs(temp);
        let paths = make_atlas_texture_dir(temp);

        let mut first_statics = opaque_alpha_statics(&vfs, &opaque, &alpha);
        let first_textures = AtlasTextureSet::from_distant_statics(&vfs, &first_statics);
        let first_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&first_textures.opaque, &vfs),
            collect_fingerprints(&first_textures.alpha, &vfs),
        );
        let first_manager = AtlasManager::setup_with_cache_state(
            &first_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            first_fingerprints,
            paths.clone(),
            None,
            &IndexSet::default(),
        );
        let first_plan = first_manager.plan(&vfs, &mut first_statics, first_textures).unwrap();
        let first_cache_bytes = first_plan.cache_bytes().to_vec();
        let first_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&first_cache_bytes).unwrap();
        publish_plan(first_plan);

        let alpha_b = write_rgba_texture(temp, r"member\alpha_b.bmp", [180, 40, 200, 128]);
        let mut second_statics = DistantStatics::default();
        let mut ds = DistantStatic::default();
        ds.subsets.push(make_subset(&vfs, &opaque, false));
        ds.subsets.push(make_subset(&vfs, &alpha, true));
        ds.subsets.push(make_subset(&vfs, &alpha_b, true));
        second_statics.insert("test".into(), ds);

        let second_textures = AtlasTextureSet::from_distant_statics(&vfs, &second_statics);
        let second_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&second_textures.opaque, &vfs),
            collect_fingerprints(&second_textures.alpha, &vfs),
        );
        let second_manager = AtlasManager::setup_with_cache_state(
            &second_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            second_fingerprints,
            paths.clone(),
            Some(&first_cache_bytes),
            &atlas_inventory_paths(&first_cache),
        );
        assert!(second_manager.prior.opaque.is_some());
        assert!(second_manager.prior.alpha.is_some());

        let second_plan = second_manager.plan(&vfs, &mut second_statics, second_textures).unwrap();
        assert!(second_plan.metrics().layout_hits.opaque);
        assert!(!second_plan.metrics().layout_hits.alpha);
        assert_eq!(second_plan.metrics().built_page_counts.opaque, 0);
        assert_eq!(
            second_plan.metrics().carried_page_counts.opaque,
            first_cache.opaque.pages.len()
        );
        assert!(second_plan.metrics().built_page_counts.alpha > 0);
        assert_eq!(
            second_plan.metrics().family_metrics.alpha.plan_mode,
            crate::atlas::AtlasFamilyPlanMode::Reconciled
        );
        let second_cache_debug = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(second_plan.cache_bytes()).unwrap();
        let first_slot = first_cache
            .alpha
            .key_slots
            .iter()
            .find(|entry| entry.path == alpha)
            .unwrap()
            .slot_id;
        let second_slot = second_cache_debug
            .alpha
            .key_slots
            .iter()
            .find(|entry| entry.path == alpha)
            .unwrap()
            .slot_id;
        assert_eq!(
            second_slot, first_slot,
            "first={:?} second={:?}",
            first_cache.alpha, second_cache_debug.alpha
        );
        assert_eq!(second_plan.metrics().family_metrics.alpha.retained_slots, 1);
        assert_eq!(second_plan.binding_deltas().alpha.as_ref().unwrap().added.len(), 1);
        assert_eq!(second_plan.metrics().family_metrics.alpha.binding_delta.added, 1);
        assert!(!second_plan.cache_hit());

        let second_cache_bytes = second_plan.cache_bytes().to_vec();
        let written = publish_plan(second_plan);
        assert!(
            !written.contains(&atlas_page_path(&paths, OPAQUE_ATLAS_PREFIX, 0)),
            "written pages must exclude carried opaque pages: {written:?}"
        );

        let second_cache = rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(&second_cache_bytes).unwrap();
        let mut third_statics = DistantStatics::default();
        let mut ds = DistantStatic::default();
        ds.subsets.push(make_subset(&vfs, &opaque, false));
        ds.subsets.push(make_subset(&vfs, &alpha, true));
        ds.subsets.push(make_subset(&vfs, &alpha_b, true));
        third_statics.insert("test".into(), ds);
        let third_textures = AtlasTextureSet::from_distant_statics(&vfs, &third_statics);
        let third_fingerprints = AtlasTextureSet::new(
            collect_fingerprints(&third_textures.opaque, &vfs),
            collect_fingerprints(&third_textures.alpha, &vfs),
        );
        let third_manager = AtlasManager::setup_with_cache_state(
            &third_textures,
            SizingPlan::uniform(TEST_MAX_TEXTURE_DIM),
            TextureDedupeMode::Exact,
            ATLAS_MAX,
            third_fingerprints,
            paths,
            Some(&second_cache_bytes),
            &atlas_inventory_paths(&second_cache),
        );
        let third_plan = third_manager.plan(&vfs, &mut third_statics, third_textures).unwrap();
        assert!(third_plan.cache_hit());
        assert_eq!(third_plan.metrics().dirty_page_count, 0);
    });
}

/// A texture sized at `usable_texture_dim` must pack, and one at the raw page cap must not.
///
/// The packer inflates every texture by border padding and edge extrusion, so the raw cap is
/// unreachable. A texture downscaled to it fails on every page, however empty, and generation
/// aborts with an out-of-space error. Locks the inset that keeps the sizing baseline packable.
#[test]
fn usable_texture_dim_is_the_largest_packable_size() {
    fn pack_square(dim: u32, gpu_max: u32) -> tex_packer_core::error::Result<tex_packer_core::Atlas> {
        pack::pack_layout_only(
            vec![tex_packer_core::LayoutItem {
                key: "tex".to_string(),
                w: dim,
                h: dim,
                source: None,
                source_size: None,
                trimmed: false,
            }],
            gpu_max,
        )
    }

    for &gpu_max in crate::SUPPORTED_STATIC_ATLAS_SIZES {
        let usable = usable_texture_dim(gpu_max);

        let atlas =
            pack_square(usable, gpu_max).unwrap_or_else(|e| panic!("{usable}px must fit a {gpu_max}px page, got {e}"));
        let page = &atlas.pages[0];
        assert_eq!(
            (page.width, page.height),
            (gpu_max, gpu_max),
            "a {usable}px texture should fill a {gpu_max}px page exactly, with no pow2 overshoot",
        );

        assert!(
            pack_square(usable + 1, gpu_max).is_err(),
            "{}px must not fit a {gpu_max}px page; usable_texture_dim is too generous",
            usable + 1,
        );
        assert!(
            pack_square(gpu_max, gpu_max).is_err(),
            "the raw {gpu_max}px cap must remain unpackable",
        );
    }
}
