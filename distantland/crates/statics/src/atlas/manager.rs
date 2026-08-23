use super::*;

impl AtlasManager {
    /// Builds an atlas manager without prior family evidence.
    ///
    /// Source textures keep their native resolution; the baseline plan caps them at the largest
    /// size that fits one atlas page.
    pub fn setup_without_cache(
        vfs: &Vfs,
        textures: &AtlasTextureSet<IndexSet<String>>,
        dedupe_mode: TextureDedupeMode,
        texture_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            prior: AtlasTextureSet::new(None, None),
            fingerprints: AtlasTextureSet::new(
                collect_fingerprints(&textures.opaque, vfs),
                collect_fingerprints(&textures.alpha, vfs),
            ),
            // The page size itself is not a packable texture size: padding and extrusion consume
            // the difference, so this has to be the usable dimension, not the raw page cap.
            plan: SizingPlan::uniform(usable_texture_dim(crate::DEFAULT_STATIC_ATLAS_MAX_SIZE)),
            dedupe_mode,
            atlas_max_size: crate::DEFAULT_STATIC_ATLAS_MAX_SIZE,
            texture_dir: texture_dir.into(),
        }
    }

    /// Builds an atlas manager from independently validated cache evidence.
    ///
    /// Decode failure or an absent cache yields no prior family evidence.
    pub fn setup_with_cache_state(
        textures: &AtlasTextureSet<IndexSet<String>>,
        plan: SizingPlan,
        dedupe_mode: TextureDedupeMode,
        atlas_max_size: u32,
        fingerprints: AtlasTextureSet<Vec<(u64, Hash)>>,
        texture_dir: PathBuf,
        cache_bytes: Option<&[u8]>,
        committed_atlas_paths: &IndexSet<String>,
    ) -> Self {
        let prior = match cache_bytes {
            Some(bytes) => match rkyv::from_bytes::<AtlasCache, rkyv::rancor::Error>(bytes) {
                Ok(cache) => validate_cache(textures, &plan, dedupe_mode, atlas_max_size, committed_atlas_paths, cache),
                Err(_) => AtlasTextureSet::new(None, None),
            },
            None => AtlasTextureSet::new(None, None),
        };
        Self {
            prior,
            fingerprints,
            plan,
            dedupe_mode,
            atlas_max_size,
            texture_dir,
        }
    }

    /// Clears both prior family entries for a force rebuild.
    pub fn clear_prior(&mut self) {
        self.prior.opaque = None;
        self.prior.alpha = None;
    }
}

/// Logs one domain's deduplication summary at info level.
pub(super) fn log_dedupe_stats(domain: &str, stats: &TextureDedupeDomainStats) {
    info!(
        "  Dedupe [{}]: inputs {} -> {} (source {}, decoded {})",
        domain, stats.input_count, stats.canonical_count, stats.source_alias_count, stats.decoded_alias_count,
    );
}
