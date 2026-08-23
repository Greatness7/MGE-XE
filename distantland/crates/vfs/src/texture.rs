//! Texture symbols and the embedded static error texture.

use super::Vfs;

/// Reserved VFS key for the generator-owned static error texture.
pub const STATIC_ERROR_TEXTURE_KEY: &str = "distantland\\error.dds";

/// A minimal 4x4 DXT1 DDS texture whose pixels decode as opaque magenta.
pub const STATIC_ERROR_TEXTURE_DDS: &[u8] = &[
    // DDS magic + DDS_HEADER.
    0x44, 0x44, 0x53, 0x20, 0x7c, 0x00, 0x00, 0x00, 0x07, 0x10, 0x08, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    0x08, 0x00, 0x00, 0x00, // depth, mip count, reserved1[11].
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // DDS_PIXELFORMAT: size, DDPF_FOURCC, "DXT1", then unused masks.
    0x20, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x44, 0x58, 0x54, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // caps + reserved2.
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // One BC1 block: magenta endpoint, blue endpoint, all selectors = endpoint 0.
    0x1f, 0xf8, 0x1f, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Index into one [`Vfs`] instance's texture map.
///
/// Texture symbols are snapshot-local and are never serialized. In debug builds
/// the producing VFS address is retained so accidental cross-snapshot use fails
/// loudly at resolution boundaries.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextureSym {
    index: u32,
    #[cfg(debug_assertions)]
    vfs_addr: usize,
}

impl TextureSym {
    /// Reserved sentinel representing an uninitialized texture.
    pub const EMPTY: Self = Self {
        index: u32::MAX,
        #[cfg(debug_assertions)]
        vfs_addr: 0,
    };

    #[inline]
    pub fn is_empty(self) -> bool {
        self.index == u32::MAX
    }

    #[allow(unused_variables)]
    pub(super) fn from_index_for_vfs(index: usize, vfs: &Vfs) -> Option<Self> {
        let index = u32::try_from(index).ok()?;
        if index == u32::MAX {
            return None;
        }

        Some(Self {
            index,
            #[cfg(debug_assertions)]
            vfs_addr: vfs as *const Vfs as usize,
        })
    }

    #[allow(unused_variables)]
    pub(super) fn index_for_vfs(self, vfs: &Vfs) -> Option<usize> {
        if self.is_empty() {
            return None;
        }
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.vfs_addr, vfs as *const Vfs as usize,
            "TextureSym used with a different VFS snapshot than the one that produced it"
        );
        Some(self.index as usize)
    }
}
