/// Legacy 32-bit flag and size type used by the original MGE ABI.
pub type Dword = u32;
/// Identifier for a shared vector managed by the host.
pub type VecId = u32;
/// Sentinel value written into requests when no vector allocation succeeded.
pub const INVALID_VECTOR: VecId = u32::MAX;
/// Default one-minute timeout used by the shared-vector read protocol.
pub const MAX_WAIT: u32 = 60_000;

/// Includes near statics in a visible-set query.
pub const VIS_NEAR: Dword = 0x01;
/// Includes far statics in a visible-set query.
pub const VIS_FAR: Dword = 0x02;
/// Includes very-far statics in a visible-set query.
pub const VIS_VERY_FAR: Dword = 0x04;
/// Includes grass instances in a visible-set query.
pub const VIS_GRASS: Dword = 0x08;
/// Includes distant landscape tiles in a visible-set query.
pub const VIS_LAND: Dword = 0x10;
/// Convenience mask covering all static-detail visibility buckets.
pub const VIS_STATIC: Dword = VIS_NEAR | VIS_FAR | VIS_VERY_FAR;

/// Bit in `mge_flags` that enables distant-land rendering.
pub const USE_DISTANT_LAND: Dword = 1 << 17;
/// On-disk distant-land asset version expected by this host build.
pub const MGE_DL_VERSION: u8 = 17;
/// Terrain file magic for `terrain.bin`.
pub const TERRAIN_FILE_MAGIC: [u8; 8] = *b"XELAND02";
/// Terrain file version.
pub const TERRAIN_FILE_VERSION: u32 = 2;
/// Serialized byte size of one terrain vertex in `terrain.bin`.
pub const SIZE_OF_TERRAIN_VERT: u32 = 20;
/// Current `terrain.bin` index format: per-mesh index width inferred from
/// `vertex_count` (`u16[3]` when `vertex_count <= 0xFFFF`, else `u32[3]`).
pub const TERRAIN_FILE_INDEX_FORMAT_AUTO: u32 = 2;

/// Lets the host choose the appropriate quadtree based on object size.
pub const STATIC_AUTO: u8 = 0;
/// Forces a static into the near-detail quadtree.
pub const STATIC_NEAR: u8 = 1;
/// Forces a static into the far-detail quadtree.
pub const STATIC_FAR: u8 = 2;
/// Forces a static into the very-far-detail quadtree.
pub const STATIC_VERY_FAR: u8 = 3;
/// Marks a static as grass.
pub const STATIC_GRASS: u8 = 4;
/// Marks a static as a tree, which still uses automatic range bucketing.
pub const STATIC_TREE: u8 = 5;
/// Marks a static as a building, which receives slightly different bounds handling.
pub const STATIC_BUILDING: u8 = 6;

#[cfg(test)]
mod tests {
    /// The C++ runtime's `mgeversion.h`, which is what `d3d8.dll` actually reads
    /// generated trees with. It is a plain header, so this is the only way to
    /// hold it to the same contract from Rust.
    const MGE_VERSION_HEADER: &str = include_str!("../../../d3d8/cpp/mge/mgeversion.h");

    fn header_define(name: &str) -> u32 {
        MGE_VERSION_HEADER
            .lines()
            .find_map(|line| {
                let rest = line.trim().strip_prefix("#define ")?.strip_prefix(name)?;
                // Guard against matching a longer name with a shared prefix.
                rest.starts_with(char::is_whitespace)
                    .then(|| rest.split_whitespace().next())?
            })
            .unwrap_or_else(|| panic!("mgeversion.h must define {name}"))
            .parse()
            .unwrap_or_else(|error| panic!("{name} must be an integer literal: {error}"))
    }

    #[test]
    fn generator_and_host_output_versions_match() {
        assert_eq!(super::MGE_DL_VERSION, distantland::MGE_DL_VERSION);
    }

    /// Closes the last unguarded copy of the distant-land format version. The
    /// GUI no longer keeps one, so these three are now the whole set.
    #[test]
    fn cpp_runtime_and_generator_output_versions_match() {
        assert_eq!(header_define("MGE_DL_VERSION"), u32::from(distantland::MGE_DL_VERSION));
    }

    #[test]
    fn cpp_runtime_and_generator_shard_counts_match() {
        assert_eq!(
            header_define("MGE_STATIC_MESH_SHARD_COUNT") as usize,
            distantland::STATIC_MESH_SHARD_COUNT
        );
    }
}
