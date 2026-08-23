use super::*;
use distantland_foundation::units::TerrainChunkUnitKey;

fn make_region(min_x: i32, max_x: i32, min_y: i32, max_y: i32) -> TerrainAtlasRegion {
    TerrainAtlasRegion {
        min_x,
        max_x,
        min_y,
        max_y,
        offset_x: 0,
        offset_y: 0,
    }
}

fn layout_of(regions: Vec<TerrainAtlasRegion>) -> TerrainAtlasLayout {
    TerrainAtlasLayout {
        regions,
        span_x: 0,
        span_y: 0,
    }
}

fn key(start_cell_x: i32, start_cell_y: i32) -> TerrainMeshWorkKey {
    TerrainMeshWorkKey {
        start_cell_x,
        start_cell_y,
        cells_per_side: MESH_CHUNK_CELLS_PER_SIDE as u32,
    }
}

#[test]
fn work_keys_are_absolute_including_negative_regions() {
    let work = enumerate_terrain_mesh_work(&layout_of(vec![make_region(-8, -1, -5, -4)])).unwrap();
    let mut keys: Vec<_> = work.iter().map(|item| item.key).collect();
    keys.sort_unstable();
    // Work squares are anchored at the region origin, not on the absolute chunk grid: x -8..=-1 is
    // two patches and y -5..=-4 is one, all starting from the region minimum.
    assert_eq!(keys, [key(-8, -5), key(-4, -5)]);

    // Those unaligned squares still map onto the absolute chunk grid by Euclidean division.
    assert_eq!(
        work.iter().find(|item| item.key == key(-8, -5)).unwrap().dependencies,
        // Cells x -8..=-5 all divide to chunk -2; cells y -5..=-2 straddle chunks -2 and -1.
        [TerrainChunkUnitKey::new(-2, -2), TerrainChunkUnitKey::new(-2, -1)]
    );
}

#[test]
fn work_item_dependencies_cover_the_chunks_its_reads_can_touch() {
    let work = enumerate_terrain_mesh_work(&layout_of(vec![make_region(3, 3, 0, 0)])).unwrap();
    assert_eq!(work.len(), 1);
    // The owned rectangle is clipped to the single cell 3,0, but the nominal square reaches x = 6
    // and its fringe reads cell 7, so chunk 1 must still be declared.
    assert_eq!(
        work[0].dependencies,
        [TerrainChunkUnitKey::new(0, 0), TerrainChunkUnitKey::new(1, 0)]
    );
}

#[test]
fn overlapping_regions_producing_one_key_twice_are_rejected_when_enumerating() {
    let layout = layout_of(vec![make_region(0, 3, 0, 3), make_region(0, 3, 0, 3)]);
    let error = enumerate_terrain_mesh_work(&layout).unwrap_err().to_string();
    assert!(error.contains("duplicate mesh work key"), "{error}");
}
