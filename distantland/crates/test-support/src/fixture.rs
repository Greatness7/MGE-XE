//! Programmatic hermetic fixture worlds for integration scenarios.
//!
//! Fixtures are constructed entirely in code: plugins through `tes3::esp`, meshes
//! through `tes3::nif`, and textures through the in-repo DDS encoder plus the `image` crate.
//! Construction is a pure function of the fixture identifier. It uses no RNG or wall-clock content.
//! and every generated element is sized to survive the generator's silent-drop rules (size gate,
//! deep-water and buried culls, NIF visibility/geometry/texture requirements, LAND flags).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use glam::{Vec2, Vec3};
use image::{Rgba, RgbaImage};
use image_dds::ddsfile::D3DFormat;
use tes3::esp::{
    Cell, CellFlags, Dialogue, FileType, Header, Landscape, LandscapeFlags, LandscapeTexture, Plugin, Reference, Static,
    TES3Object,
};
use tes3::nif::{
    Map, NiAlphaProperty, NiMaterialProperty, NiSourceTexture, NiStream, NiTexturingProperty, NiTriShape, NiTriShapeData,
    TextureMap, TextureSource,
};

use crate::{GenerationJob, GenerationSettings};

/// Identifier of the v1 hermetic baseline fixture world.
pub const BASELINE_WORLD_V1: &str = "baseline-world-v1";

/// Baseline-world variant with two large independent opaque textures for page-reuse assertions.
pub const BASELINE_WORLD_V1_ATLAS: &str = "baseline-world-v1-atlas";

/// Baseline-world variant with a distinct `_dist.nif` companion already present.
pub const BASELINE_WORLD_V1_DIST: &str = "baseline-world-v1-dist";

/// Baseline-world variant with a distinct `x*.nif` + `x*.kf` pair already present.
pub const BASELINE_WORLD_V1_XKF: &str = "baseline-world-v1-xkf";

/// Baseline-world variant with a main-load-order grass plugin containing repeated native
/// reference indices across cells.
pub const BASELINE_WORLD_V1_MAINGRASS: &str = "baseline-world-v1-maingrass";

/// Baseline-world variant with generator-only grass plugins outside the active load order.
pub const BASELINE_WORLD_V1_GRASSLIST: &str = "baseline-world-v1-grasslist";

/// Baseline-world variant whose contiguous 5-by-2 LAND block spans an absolute chunk boundary.
pub const BASELINE_WORLD_V1_CHUNKSPAN: &str = "baseline-world-v1-chunkspan";

/// Baseline-world variant whose contiguous 9-by-2 LAND block spans three absolute terrain chunks.
///
/// Nine cells wide is the minimum that yields three mesh work items (starting at absolute cells
/// x = 0, 4, and 8) and three absolute chunks (x = 0, 1, 2), which is what makes halo behaviour
/// observable: an edit at x = 3 dirties chunks 0 and 1 but leaves chunk 2 clean, so exactly two of
/// the three work items must rebuild and the third must be reused.
pub const BASELINE_WORLD_V1_HALOSPAN: &str = "baseline-world-v1-halospan";

/// Baseline-world variant whose LAND block straddles the deep-water mesh-emission threshold.
///
/// A contiguous 16-by-2 block spanning four mesh work items (starting at absolute cells x = 0, 4,
/// 8, and 12). The middle of the block is uniformly flat at exactly `DEEP_WATER_Z`, so the work at
/// x = 4 and x = 8 collapses to the default-cell fast path and emits *no* terrain record, while the
/// work at x = 0 and x = 12 straddles non-deep-water cells and emits one. Perturbing a height in
/// the deep-water middle makes that cell non-default, forcing its work item onto the dense path and
/// inserting a record where none existed.
pub const BASELINE_WORLD_V1_THRESHOLD: &str = "baseline-world-v1-threshold";

/// File name of the plugin authored by the hermetic fixtures.
pub const FIXTURE_PLUGIN_NAME: &str = "fixture.esp";

/// Dialogue-only load-order anchor placed after the world plugin for plugin-insertion scenarios.
pub const FIXTURE_TAIL_PLUGIN_NAME: &str = "fixture-tail.esp";

/// File name of the main-load-order grass plugin used by stable-identity scenarios.
pub const FIXTURE_GRASS_PLUGIN_NAME: &str = "fixture-grass.esp";

/// File name of the active content master that owns the grass-list fixture's static definition.
pub const FIXTURE_GRASS_MASTER_NAME: &str = "fixture-grass-master.esm";

/// File name of the dormant second grass plugin used by the grass-list add scenario.
pub const FIXTURE_SECOND_GRASS_PLUGIN_NAME: &str = "fixture-grass-second.esp";

/// Half-extent of Morrowind exterior cells in engine units (cells are 8192 wide).
const CELL_SIZE: f32 = 8192.0;

/// Input paths produced by building a hermetic fixture, in the shape scenario jobs consume.
#[derive(Clone, Debug)]
pub struct HermeticFixturePaths {
    /// Workspace-local minimal `Morrowind.ini`.
    pub morrowind_ini: PathBuf,
    /// The fixture's single data directory.
    pub data_dir: PathBuf,
    /// Plugin file names to activate, resolved against `data_dir` by the job.
    pub plugin_names: Vec<PathBuf>,
    /// Generator-only grass plugin file names resolved against `data_dir` by the job.
    pub grass_plugin_names: Vec<PathBuf>,
    /// Fixture-authored static override used by trust scenarios.
    pub override_file: PathBuf,
}

/// Builds the named hermetic fixture into `inputs_root`, creating `Morrowind.ini` and a
/// `Data Files` tree beneath it.
///
/// # Errors
///
/// Returns an error for an unknown fixture identifier or when any fixture file cannot be
/// encoded or written.
pub fn build_hermetic_fixture(fixture: &str, inputs_root: &Path) -> Result<HermeticFixturePaths> {
    match fixture {
        BASELINE_WORLD_V1 => build_baseline_world_v1(inputs_root),
        BASELINE_WORLD_V1_ATLAS => build_atlas_variant(inputs_root),
        BASELINE_WORLD_V1_DIST => {
            let paths = build_baseline_world_v1(inputs_root)?;
            save_resolution_companion(&paths.data_dir.join("Meshes/fixture_opaque_dist.nif"))?;
            Ok(paths)
        }
        BASELINE_WORLD_V1_XKF => {
            let paths = build_baseline_world_v1(inputs_root)?;
            save_resolution_companion(&paths.data_dir.join("Meshes/xfixture_opaque.nif"))?;
            fs::write(paths.data_dir.join("Meshes/xfixture_opaque.kf"), b"").context("writing fixture x-mesh KF marker")?;
            Ok(paths)
        }
        BASELINE_WORLD_V1_MAINGRASS => build_main_grass_variant(inputs_root),
        BASELINE_WORLD_V1_GRASSLIST => build_grass_list_variant(inputs_root),
        BASELINE_WORLD_V1_CHUNKSPAN => build_chunkspan_variant(inputs_root),
        BASELINE_WORLD_V1_HALOSPAN => build_halospan_variant(inputs_root),
        BASELINE_WORLD_V1_THRESHOLD => build_threshold_variant(inputs_root),
        other => bail!("unknown hermetic fixture id {other:?}"),
    }
}

/// Builds a hermetic scenario job with explicit INI/data-dir/plugin paths, default settings, and
/// the terrain package enabled.
pub fn hermetic_generation_job(fixture: &HermeticFixturePaths, output_root: &Path) -> GenerationJob {
    GenerationJob {
        morrowind_ini: Some(fixture.morrowind_ini.clone()),
        data_dirs: Some(vec![fixture.data_dir.clone()]),
        plugins: Some(fixture.plugin_names.clone()),
        grass_plugins: (!fixture.grass_plugin_names.is_empty()).then(|| fixture.grass_plugin_names.clone()),
        output_root: Some(output_root.to_path_buf()),
        // The fixture's plugin list is the scenario; re-deriving it from the fixture INI would
        // make the leg depend on file mtimes inside the temporary tree.
        auto_sync_plugins: false,
        settings: GenerationSettings {
            generate_terrain: true,
            ..GenerationSettings::default()
        },
    }
}

/// Builds the baseline world: a 2×2 exterior LAND block, two landscape textures (one
/// DDS, one TGA), and three statics (opaque, alpha-tested, building-classified) referenced
/// across two cells with one mergeable same-cell pair.
fn build_baseline_world_v1(inputs_root: &Path) -> Result<HermeticFixturePaths> {
    let data_dir = inputs_root.join("Data Files");
    let meshes_dir = data_dir.join("Meshes");
    let textures_dir = data_dir.join("Textures");
    fs::create_dir_all(meshes_dir.join("x")).context("creating fixture mesh directories")?;
    fs::create_dir_all(&textures_dir).context("creating fixture texture directory")?;

    // Static-mesh source textures: one DDS and one non-DDS path through the loaders.
    write_bc1_dds(&textures_dir.join("fixture_opaque.dds"), &gradient_image(64, 0x11))?;
    write_tga(&textures_dir.join("fixture_alpha.tga"), &gradient_image(64, 0x2f))?;
    // Landscape-texture sources: likewise one DDS and one TGA.
    write_bc1_dds(&textures_dir.join("fixture_land_a.dds"), &gradient_image(64, 0x53))?;
    write_tga(&textures_dir.join("fixture_land_b.tga"), &gradient_image(64, 0x71))?;

    // Sizes clear the default gate (min_static_size 150.0 against the bounding-sphere radius,
    // buildings ×2) with far more than the required 25% margin: the 300-unit box has radius
    // ≈519, the 200-unit building box ≈374 (×2 = 748). Heights keep tops well above terrain
    // (bounded within ±~100 units here) so the buried and deep-water culls cannot trigger.
    save_box_mesh(
        &meshes_dir.join("fixture_opaque.nif"),
        300.0,
        600.0,
        "fixture_opaque.dds",
        false,
    )?;
    save_box_mesh(&meshes_dir.join("fixture_alpha.nif"), 300.0, 600.0, "fixture_alpha.tga", true)?;
    // The `x\` mesh-path prefix classifies this static as a building (2× size multiplier).
    save_box_mesh(
        &meshes_dir.join("x").join("fixture_building.nif"),
        200.0,
        800.0,
        "fixture_opaque.dds",
        false,
    )?;

    let mut plugin = baseline_world_v1_plugin();
    plugin
        .save_path(data_dir.join(FIXTURE_PLUGIN_NAME))
        .context("writing fixture plugin")?;
    let mut tail = dialogue_only_plugin("fixture_tail_topic");
    tail.save_path(data_dir.join(FIXTURE_TAIL_PLUGIN_NAME))
        .context("writing fixture tail plugin")?;

    // The proven hermetic setup uses an empty INI with explicit data_dirs/plugins.
    let morrowind_ini = inputs_root.join("Morrowind.ini");
    fs::write(&morrowind_ini, "").context("writing fixture Morrowind.ini")?;
    let override_file = data_dir.join("fixture.ovr");
    fs::write(&override_file, b"meshes\\fixture_opaque.nif = far\n").context("writing fixture override")?;

    Ok(HermeticFixturePaths {
        morrowind_ini,
        data_dir,
        plugin_names: vec![PathBuf::from(FIXTURE_PLUGIN_NAME), PathBuf::from(FIXTURE_TAIL_PLUGIN_NAME)],
        grass_plugin_names: vec![],
        override_file,
    })
}

fn build_atlas_variant(inputs_root: &Path) -> Result<HermeticFixturePaths> {
    let paths = build_baseline_world_v1(inputs_root)?;
    let textures_dir = paths.data_dir.join("Textures");
    write_bc1_dds(&textures_dir.join("fixture_opaque.dds"), &gradient_image(1536, 0x11))?;
    write_bc1_dds(&textures_dir.join("fixture_opaque_other.dds"), &gradient_image(1536, 0x21))?;
    save_box_mesh(
        &paths.data_dir.join("Meshes/x/fixture_building.nif"),
        200.0,
        800.0,
        "fixture_opaque_other.dds",
        false,
    )?;
    Ok(paths)
}

fn build_chunkspan_variant(inputs_root: &Path) -> Result<HermeticFixturePaths> {
    let paths = build_baseline_world_v1(inputs_root)?;
    let mut plugin = baseline_world_v1_plugin();
    for x in 2..=4 {
        for y in 0..=1 {
            plugin.objects.push(TES3Object::Landscape(fixture_landscape((x, y), false)));
        }
    }
    plugin
        .save_path(paths.data_dir.join(FIXTURE_PLUGIN_NAME))
        .context("writing chunk-span fixture plugin")?;
    Ok(paths)
}

/// Widens the baseline LAND block to a contiguous 9-by-2 rectangle spanning three absolute chunks.
fn build_halospan_variant(inputs_root: &Path) -> Result<HermeticFixturePaths> {
    let paths = build_baseline_world_v1(inputs_root)?;
    let mut plugin = baseline_world_v1_plugin();
    // The baseline already covers x 0..=1; extend to x 8 so the row is nine cells wide.
    for x in 2..=8 {
        for y in 0..=1 {
            plugin.objects.push(TES3Object::Landscape(fixture_landscape((x, y), false)));
        }
    }
    plugin
        .save_path(paths.data_dir.join(FIXTURE_PLUGIN_NAME))
        .context("writing halo-span fixture plugin")?;
    Ok(paths)
}

/// World-space height the mesh builder treats as deep water and refuses to emit a record for.
///
/// Mirrors `terrain::mesh::DEEP_WATER_Z`, which is private to that module.
const FIXTURE_DEEP_WATER_Z: f32 = -2048.0;

/// Authors a trivially default LAND cell sitting flat at `height`.
///
/// Every value `TerrainCell::is_default` checks lands on its default without authoring any of them:
///
/// - `USES_VERTEX_HEIGHTS_AND_NORMALS` is required or the VHGT subrecord is not written at all, but
///   the zeroed normal data it also enables decodes to `Vec3::Z`. `decode_vertex_normals`
///   substitutes the unit up vector for vectors it cannot normalize.
/// - Omitting `USES_VERTEX_COLORS` and `USES_TEXTURES` makes `TerrainCell::from_landscape` supply
///   default colors and zeroed VTEX indices.
/// - Heights decode as `(offset + cumulative deltas) * 8`, so zero deltas put every vertex at
///   exactly `offset * 8`.
fn flat_default_landscape(grid: (i32, i32), height: f32) -> Landscape {
    let mut land = Landscape::default();
    land.grid = grid;
    land.landscape_flags = LandscapeFlags::USES_VERTEX_HEIGHTS_AND_NORMALS;
    land.vertex_heights.offset = height / 8.0;
    land
}

/// Widens the baseline LAND block into a 16-by-2 rectangle straddling the deep-water threshold.
///
/// Cells `2..=12` are flat at exactly `DEEP_WATER_Z`; cells `13..=15` sit at zero. Combined with
/// the baseline's non-default cells at `0..=1`, that gives four mesh work items with a deliberate
/// mix of emitting and non-emitting outcomes. See [`BASELINE_WORLD_V1_THRESHOLD`].
fn build_threshold_variant(inputs_root: &Path) -> Result<HermeticFixturePaths> {
    let paths = build_baseline_world_v1(inputs_root)?;
    let mut plugin = baseline_world_v1_plugin();
    // A solid rectangle keeps the atlas packer to one region, which keeps work-item start cells on
    // the absolute multiples of four the scenario's expectations name.
    for x in 2..=15 {
        for y in 0..=1 {
            let height = if x <= 12 { FIXTURE_DEEP_WATER_Z } else { 0.0 };
            plugin
                .objects
                .push(TES3Object::Landscape(flat_default_landscape((x, y), height)));
        }
    }
    plugin
        .save_path(paths.data_dir.join(FIXTURE_PLUGIN_NAME))
        .context("writing deep-water threshold fixture plugin")?;
    Ok(paths)
}

/// Adds a density-thinned grass plugin to the baseline load order before its tail anchor.
fn build_main_grass_variant(inputs_root: &Path) -> Result<HermeticFixturePaths> {
    let mut paths = build_baseline_world_v1(inputs_root)?;
    let grass_mesh_dir = paths.data_dir.join("Meshes/grass");
    fs::create_dir_all(&grass_mesh_dir).context("creating fixture grass mesh directory")?;
    save_box_mesh(
        &grass_mesh_dir.join("fixture_grass.nif"),
        32.0,
        96.0,
        "fixture_alpha.tga",
        true,
    )?;
    let mut plugin = main_grass_plugin();
    plugin
        .save_path(paths.data_dir.join(FIXTURE_GRASS_PLUGIN_NAME))
        .context("writing fixture grass plugin")?;
    fs::write(
        paths.data_dir.join("fixture-grass-metadata.toml"),
        "[tools.mge-xe.distantland.statics]\n'grass\\fixture_grass.nif' = { type = \"grass\", grass_density = 50 }\n",
    )
    .context("writing fixture grass metadata")?;

    paths.plugin_names.insert(1, PathBuf::from(FIXTURE_GRASS_PLUGIN_NAME));
    Ok(paths)
}

fn build_grass_list_variant(inputs_root: &Path) -> Result<HermeticFixturePaths> {
    let mut paths = build_baseline_world_v1(inputs_root)?;
    let grass_mesh_dir = paths.data_dir.join("Meshes/grass");
    fs::create_dir_all(&grass_mesh_dir).context("creating fixture grass mesh directory")?;
    save_box_mesh(
        &grass_mesh_dir.join("fixture_grass.nif"),
        32.0,
        96.0,
        "fixture_alpha.tga",
        true,
    )?;
    save_box_mesh(
        &grass_mesh_dir.join("fixture_grass_second.nif"),
        36.0,
        104.0,
        "fixture_alpha.tga",
        true,
    )?;

    let mut master = grass_master_plugin();
    master
        .save_path(paths.data_dir.join(FIXTURE_GRASS_MASTER_NAME))
        .context("writing fixture active grass-definition master")?;
    let mut primary = grass_list_plugin_with_ignored_content();
    primary
        .save_path(paths.data_dir.join(FIXTURE_GRASS_PLUGIN_NAME))
        .context("writing fixture grass-list plugin")?;
    let mut second = second_grass_plugin();
    second
        .save_path(paths.data_dir.join(FIXTURE_SECOND_GRASS_PLUGIN_NAME))
        .context("writing second fixture grass-list plugin")?;

    paths.plugin_names.insert(0, PathBuf::from(FIXTURE_GRASS_MASTER_NAME));
    paths.grass_plugin_names = vec![PathBuf::from(FIXTURE_GRASS_PLUGIN_NAME)];
    Ok(paths)
}

/// Builds the minimal valid dialogue-only plugin used as a fixture load-order anchor.
pub fn dialogue_only_plugin(topic: &str) -> Plugin {
    let mut plugin = Plugin::new();
    let mut header = Header::default();
    header.file_type = FileType::Esp;
    plugin.objects.push(TES3Object::Header(header));
    plugin.objects.push(TES3Object::Dialogue(Dialogue {
        id: topic.to_owned(),
        ..Dialogue::default()
    }));
    plugin
}

/// Assembles the baseline-world-v1 plugin records.
fn baseline_world_v1_plugin() -> Plugin {
    let mut plugin = Plugin::new();

    let mut header = Header::default();
    header.file_type = FileType::Esp;
    plugin.objects.push(TES3Object::Header(header));

    for (id, mesh) in [
        ("fixture_opaque", "fixture_opaque.nif"),
        ("fixture_alpha", "fixture_alpha.nif"),
        ("fixture_building", "x\\fixture_building.nif"),
    ] {
        plugin.objects.push(TES3Object::Static(Static {
            id: id.to_owned(),
            mesh: mesh.to_owned(),
            ..Static::default()
        }));
    }

    // LTEX `index` values are referenced from `texture_indices` as `index + 1`; raw value 0
    // selects the engine default texture.
    for (id, index, file_name) in [
        ("fixture_ltex_a", 0, "fixture_land_a.dds"),
        ("fixture_ltex_b", 1, "fixture_land_b.tga"),
    ] {
        plugin.objects.push(TES3Object::LandscapeTexture(LandscapeTexture {
            id: id.to_owned(),
            index,
            file_name: file_name.to_owned(),
            ..LandscapeTexture::default()
        }));
    }

    // Cell (0, 0): a mergeable same-static pair plus the alpha static. Cell (1, 0): the
    // building. All placements sit at z = 0, far above the deep-water threshold and clearly
    // exposed above the gentle fixture terrain.
    plugin.objects.push(TES3Object::Cell(exterior_cell(
        (0, 0),
        vec![
            placed_reference(1, "fixture_opaque", [2048.0, 2048.0, 0.0]),
            placed_reference(2, "fixture_opaque", [2648.0, 2048.0, 0.0]),
            placed_reference(3, "fixture_alpha", [5120.0, 5120.0, 0.0]),
        ],
    )));
    plugin.objects.push(TES3Object::Cell(exterior_cell(
        (1, 0),
        vec![placed_reference(4, "fixture_building", [CELL_SIZE + 4096.0, 3072.0, 0.0])],
    )));

    // 2×2 LAND block. Cells (0,0), (0,1), and (1,1) carry populated texture indices; (1,0)
    // relies on the raw index-0 default.
    for grid in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        // Cell (0,1) is populated so the material-edit scenario can mutate a serialized index and
        // exercise all four current/south/east/southeast consumers.
        let textured = grid == (0, 0) || grid == (0, 1) || grid == (1, 1);
        plugin.objects.push(TES3Object::Landscape(fixture_landscape(grid, textured)));
    }

    plugin
}

/// Builds the active content master that supplies the dedicated grass plugin's STAT.
fn grass_master_plugin() -> Plugin {
    let mut plugin = Plugin::new();
    let mut header = Header::default();
    header.file_type = FileType::Esm;
    plugin.objects.push(TES3Object::Header(header));
    plugin.objects.push(TES3Object::Static(Static {
        id: "fixture_grass".to_owned(),
        mesh: "grass\\fixture_grass.nif".to_owned(),
        ..Static::default()
    }));
    plugin
}

/// Builds a grass plugin whose native reference indices repeat across exterior cells.
fn main_grass_plugin() -> Plugin {
    let mut plugin = Plugin::new();
    let mut header = Header::default();
    header.file_type = FileType::Esp;
    plugin.objects.push(TES3Object::Header(header));
    plugin.objects.push(TES3Object::Static(Static {
        id: "fixture_grass".to_owned(),
        mesh: "grass\\fixture_grass.nif".to_owned(),
        ..Static::default()
    }));

    for (cell_x, cell_y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let mut references = Vec::with_capacity(16);
        for index in 1..=16 {
            let local_x = 512.0 + ((index - 1) % 4) as f32 * 1536.0;
            let local_y = 512.0 + ((index - 1) / 4) as f32 * 1536.0;
            references.push(placed_reference(
                index,
                "fixture_grass",
                [
                    cell_x as f32 * CELL_SIZE + local_x,
                    cell_y as f32 * CELL_SIZE + local_y,
                    128.0,
                ],
            ));
        }
        plugin
            .objects
            .push(TES3Object::Cell(exterior_cell((cell_x, cell_y), references)));
    }

    plugin
}

/// Builds a STAT-less grass plugin whose definition comes from an active content master.
///
/// The extra content-master reference is deliberately non-grass and verifies that ordinary
/// non-delete dependencies on active content do not produce warning noise.
fn grass_list_plugin_with_ignored_content() -> Plugin {
    let mut plugin = main_grass_plugin();
    plugin
        .objects
        .retain(|object| !matches!(object, TES3Object::Static(Static { id, .. }) if id == "fixture_grass"));
    let header = plugin.header_mut().expect("fixture plugin has a header");
    header.masters.push((FIXTURE_GRASS_MASTER_NAME.to_owned(), 0));
    header.masters.push((FIXTURE_PLUGIN_NAME.to_owned(), 0));
    plugin.objects.push(TES3Object::Static(Static {
        id: "fixture_non_grass".to_owned(),
        mesh: "fixture_opaque.nif".to_owned(),
        ..Static::default()
    }));

    let mut master_reference = placed_reference(100, "fixture_opaque", [40_000.0, 40_000.0, 0.0]);
    master_reference.mast_index = 2;
    plugin.objects.push(TES3Object::Cell(exterior_cell(
        (5, 5),
        vec![
            master_reference,
            placed_reference(101, "fixture_non_grass", [41_000.0, 40_000.0, 0.0]),
        ],
    )));

    let mut interior = Cell::default();
    interior.name = "Fixture Grass Interior".to_owned();
    interior.data.flags.insert(CellFlags::IS_INTERIOR);
    let interior_reference = placed_reference(102, "fixture_grass", [0.0, 0.0, 0.0]);
    interior.references.insert((0, 102), interior_reference);
    plugin.objects.push(TES3Object::Cell(interior));
    plugin.objects.push(TES3Object::Landscape(fixture_landscape((5, 5), false)));
    plugin.objects.push(TES3Object::LandscapeTexture(LandscapeTexture {
        index: 99,
        file_name: "fixture_land_a.dds".to_owned(),
        ..LandscapeTexture::default()
    }));
    plugin
}

/// Builds the dormant grass plugin activated by the grass-list-add scenario.
fn second_grass_plugin() -> Plugin {
    let mut plugin = Plugin::new();
    let mut header = Header::default();
    header.file_type = FileType::Esp;
    plugin.objects.push(TES3Object::Header(header));
    plugin.objects.push(TES3Object::Static(Static {
        id: "fixture_grass_second".to_owned(),
        mesh: "grass\\fixture_grass_second.nif".to_owned(),
        ..Static::default()
    }));
    plugin.objects.push(TES3Object::Cell(exterior_cell(
        (2, 0),
        vec![
            placed_reference(1, "fixture_grass_second", [17_000.0, 1_000.0, 128.0]),
            placed_reference(2, "fixture_grass_second", [18_000.0, 2_000.0, 128.0]),
        ],
    )));
    plugin
}

/// Builds an exterior cell (default flags are non-interior) holding `references`.
fn exterior_cell(grid: (i32, i32), references: Vec<Reference>) -> Cell {
    let mut cell = Cell::default();
    cell.data.grid = grid;
    for reference in references {
        cell.references
            .insert((reference.mast_index, reference.refr_index), reference);
    }
    cell
}

/// Builds one placed reference with deterministic identity and default rotation/scale.
fn placed_reference(refr_index: u32, id: &str, translation: [f32; 3]) -> Reference {
    Reference {
        mast_index: 0,
        refr_index,
        id: id.to_owned(),
        translation,
        ..Reference::default()
    }
}

/// Builds one LAND record with deterministic non-uniform heights, normals, and colors.
///
/// Height deltas are sparse ±1 values, so decoded world heights stay within roughly ±100
/// units. This is non-uniform terrain that can never bury the fixture statics. Every populated
/// channel sets its matching `LandscapeFlags` bit, or the data would decode to defaults.
fn fixture_landscape(grid: (i32, i32), textured: bool) -> Landscape {
    // Grid-dependent salt keeps the four cells' contents distinct from each other.
    let salt = (grid.0 + 2 * grid.1) as usize;

    let mut land = Landscape::default();
    land.grid = grid;
    land.landscape_flags = LandscapeFlags::USES_VERTEX_HEIGHTS_AND_NORMALS | LandscapeFlags::USES_VERTEX_COLORS;
    land.vertex_heights.offset = 0.0;

    for (y, row) in land.vertex_heights.data.iter_mut().enumerate() {
        for (x, delta) in row.iter_mut().enumerate() {
            *delta = match (x * 7 + y * 13 + salt) % 31 {
                0 => 1,
                15 => -1,
                _ => 0,
            };
        }
    }
    for (y, row) in land.vertex_normals.data.iter_mut().enumerate() {
        for (x, normal) in row.iter_mut().enumerate() {
            let tilt = ((x + 2 * y + salt) % 5) as i8;
            *normal = [tilt, 0, 120];
        }
    }
    for (y, row) in land.vertex_colors.data.iter_mut().enumerate() {
        for (x, color) in row.iter_mut().enumerate() {
            *color = [200 - x as u8, 180 - (salt as u8) * 8, 160 + (y % 64) as u8];
        }
    }

    if textured {
        land.landscape_flags |= LandscapeFlags::USES_TEXTURES;
        for (parent, row) in land.texture_indices.data.iter_mut().enumerate() {
            for (shape, index) in row.iter_mut().enumerate() {
                // Cycle raw values 0 (default), 1 (fixture_ltex_a), 2 (fixture_ltex_b).
                *index = ((parent + shape + salt) % 3) as u16;
            }
        }
    }

    land
}

/// Writes a textured axis-aligned box NIF: a root `NiTriShape` (flags 0 = not app-culled)
/// spanning `±half_extent` in X/Y and `0..height` in Z, with one UV set matching the vertex
/// count and an external texture source. This is the exact shape the extractor requires.
pub(crate) fn save_box_mesh(
    path: &Path,
    half_extent: f32,
    height: f32,
    texture_path: &str,
    alpha_tested: bool,
) -> Result<()> {
    let mut stream = NiStream::new();
    let shape = stream.insert(NiTriShape::default());
    let material = stream.insert(NiMaterialProperty::default());
    let tex_prop = stream.insert(NiTexturingProperty::default());
    let texture = stream.insert(NiSourceTexture::default());
    let data = stream.insert(NiTriShapeData::default());

    stream.roots.push(shape.cast());

    if let Some(shape) = stream.get_mut(shape) {
        shape.geometry_data = data.cast();
        shape.properties.push(material.cast());
        shape.properties.push(tex_prop.cast());
    }

    if let Some(material) = stream.get_mut(material) {
        material.ambient_color = Vec3::ONE;
        material.diffuse_color = Vec3::ONE;
        material.alpha = 1.0;
    }

    if let Some(tex_prop) = stream.get_mut(tex_prop) {
        tex_prop.texture_maps.resize(7, None);
        tex_prop.texture_maps[0] = Some(TextureMap::Map(Map {
            texture: texture.cast(),
            ..Map::default()
        }));
    }

    if let Some(texture) = stream.get_mut(texture) {
        texture.source = TextureSource::External(texture_path.to_owned());
    }

    if alpha_tested {
        let alpha = stream.insert(NiAlphaProperty::default());
        if let Some(alpha) = stream.get_mut(alpha) {
            // 0x0200 enables alpha testing only; the extractor treats testing or blending
            // as an alpha-family subset.
            alpha.flags = 0x0200;
            alpha.test_ref = 128;
        }
        if let Some(shape) = stream.get_mut(shape) {
            shape.properties.push(alpha.cast());
        }
    }

    if let Some(data) = stream.get_mut(data) {
        let (h, z0, z1) = (half_extent, 0.0, height);
        data.vertices = vec![
            Vec3::new(-h, -h, z0),
            Vec3::new(h, -h, z0),
            Vec3::new(h, h, z0),
            Vec3::new(-h, h, z0),
            Vec3::new(-h, -h, z1),
            Vec3::new(h, -h, z1),
            Vec3::new(h, h, z1),
            Vec3::new(-h, h, z1),
        ];
        data.uv_sets = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 0.0),
        ];
        data.triangles = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        data.update_center_radius();
    }

    stream
        .save_path(path)
        .with_context(|| format!("writing fixture mesh {}", path.display()))
}

/// Writes the distinct resolution companion used by companion add/remove scenarios.
pub fn save_resolution_companion(path: &Path) -> Result<()> {
    save_box_mesh(path, 360.0, 660.0, "fixture_opaque.dds", false)
}

/// Builds a deterministic gradient test image, salted so distinct fixture textures differ.
pub(crate) fn gradient_image(size: u32, salt: u8) -> RgbaImage {
    RgbaImage::from_fn(size, size, |x, y| {
        let alpha = ((x + y) * 2).min(255) as u8;
        Rgba([(x * 4) as u8 ^ salt, (y * 4) as u8, salt, alpha])
    })
}

/// Encodes `image` as a mipped BC1 legacy DDS file at `path`.
pub(crate) fn write_bc1_dds(path: &Path, image: &RgbaImage) -> Result<()> {
    let bytes = crate::dds::encode_d3d_bcn_dds_with_mips(image, D3DFormat::DXT1)
        .with_context(|| format!("encoding fixture DDS {}", path.display()))?;
    fs::write(path, bytes).with_context(|| format!("writing fixture DDS {}", path.display()))
}

/// Writes `image` as a TGA file at `path` (the non-DDS loader path).
fn write_tga(path: &Path, image: &RgbaImage) -> Result<()> {
    image
        .save_with_format(path, image::ImageFormat::Tga)
        .with_context(|| format!("writing fixture TGA {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recursively collects `(relative path, bytes)` for every file under `root`.
    fn tree_contents(root: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    let relative = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                    out.push((relative, fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    #[test]
    fn baseline_world_v1_creates_expected_files() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = build_hermetic_fixture(BASELINE_WORLD_V1, temp.path()).unwrap();

        assert!(fixture.morrowind_ini.is_file());
        for relative in [
            "fixture.esp",
            "Meshes\\fixture_opaque.nif",
            "Meshes\\fixture_alpha.nif",
            "Meshes\\x\\fixture_building.nif",
            "Textures\\fixture_opaque.dds",
            "Textures\\fixture_alpha.tga",
            "Textures\\fixture_land_a.dds",
            "Textures\\fixture_land_b.tga",
        ] {
            assert!(fixture.data_dir.join(relative).is_file(), "missing fixture file {relative}");
        }
    }

    #[test]
    fn baseline_world_v1_is_byte_deterministic() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        build_hermetic_fixture(BASELINE_WORLD_V1, first.path()).unwrap();
        build_hermetic_fixture(BASELINE_WORLD_V1, second.path()).unwrap();
        assert_eq!(tree_contents(first.path()), tree_contents(second.path()));
    }

    #[test]
    fn nif_resolution_fact_tracks_s05_companions() {
        use distantland_foundation::identity::MeshResolutionRule;
        use distantland_statics::{DistantStatic, StaticOverrides, Vfs};

        let temp = tempfile::tempdir().unwrap();
        let meshes = temp.path().join("meshes");
        fs::create_dir_all(&meshes).unwrap();
        save_resolution_companion(&meshes.join("fixture.nif")).unwrap();

        let resolve = || {
            let vfs = Vfs {
                ini_path: temp.path().join("Morrowind.ini"),
                data_dirs: vec![temp.path().to_path_buf()],
                active_plugins: vec![],
                archives: vec![],
                maps: distantland_vfs::directory_map::build_directory_map(&[temp.path().to_path_buf()]).unwrap(),
            };
            DistantStatic::from_nif_with_identity(
                "fixture.nif",
                &vfs,
                1.0,
                f32::MAX,
                false,
                1.0,
                false,
                &StaticOverrides::default(),
            )
            .resolution
            .unwrap()
        };

        assert_eq!(resolve().rule, MeshResolutionRule::Original);

        save_resolution_companion(&meshes.join("fixture_dist.nif")).unwrap();
        let dist = resolve();
        assert_eq!(dist.rule, MeshResolutionRule::Dist);
        assert_eq!(dist.resolved_key, "fixture_dist.nif");

        fs::remove_file(meshes.join("fixture_dist.nif")).unwrap();
        save_resolution_companion(&meshes.join("xfixture.nif")).unwrap();
        fs::write(meshes.join("xfixture.kf"), []).unwrap();
        let x_with_kf = resolve();
        assert_eq!(x_with_kf.rule, MeshResolutionRule::XWithKf);
        assert_eq!(x_with_kf.resolved_key, "xfixture.nif");
    }

    #[test]
    fn unknown_fixture_ids_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        assert!(build_hermetic_fixture("no-such-fixture", temp.path()).is_err());
    }
}
