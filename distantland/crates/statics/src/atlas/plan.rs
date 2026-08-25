use super::*;

use anyhow::Context;
use tex_packer_core::Page;

use crate::atlas::cache::fingerprints_match;
use crate::atlas::manager::log_dedupe_stats;
use crate::atlas::pack::{AtlasAliasRelation, compose_planned_page, prepare_textures};
use crate::atlas::reconcile::{
    PreparedAtlasGroup, allocation_stats, apply_provider_renames, build_prepared_groups, fresh_allocate_family,
    reconcile_family,
};
use crate::atlas::uv::{BindingDelta, atlas_binding_digest, binding_delta};
use crate::dds::encode_d3d_bcn_dds_with_mips;

type FrameMap = HashMap<String, (usize, UvBound)>;

enum AtlasPagePublishPlan {
    Carry { page_id: usize },
    Build(Page),
}

struct AtlasDomainPublishPlan {
    prefix: &'static str,
    pages: Vec<AtlasPagePublishPlan>,
    images: HashMap<String, image::RgbaImage>,
    prepared_source_bytes: u64,
}

struct PlannedFamily {
    publish: Option<AtlasDomainPublishPlan>,
    bindings: FrameMap,
    cache: CachedAtlasFamily,
    layout_hit: bool,
    decoded_texture_count: usize,
    carried_page_count: usize,
    built_page_count: usize,
    plan_mode: AtlasFamilyPlanMode,
    binding_delta: Option<BindingDelta>,
    metrics: AtlasFamilyMetrics,
}

/// Immutable atlas decision and prepared publication recipes produced before output mutation.
pub struct AtlasPublishPlan {
    opaque: Option<AtlasDomainPublishPlan>,
    alpha: Option<AtlasDomainPublishPlan>,
    texture_dir: PathBuf,
    cache_hit: bool,
    binding_digest: [u8; 32],
    binding_deltas: AtlasTextureSet<Option<BindingDelta>>,
    metrics: AtlasPlanMetrics,
    cache_bytes: Vec<u8>,
}

/// Fully rendered atlas page ready for publication by the pipeline's storage authority.
pub struct AtlasPageWrite {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

impl AtlasPublishPlan {
    pub fn cache_hit(&self) -> bool {
        self.cache_hit
    }

    pub fn metrics(&self) -> &AtlasPlanMetrics {
        &self.metrics
    }

    pub fn binding_deltas(&self) -> &AtlasTextureSet<Option<BindingDelta>> {
        &self.binding_deltas
    }

    pub fn binding_digest(&self) -> [u8; 32] {
        self.binding_digest
    }

    pub fn cache_bytes(&self) -> &[u8] {
        &self.cache_bytes
    }

    pub fn carried_relative_paths(&self) -> Vec<String> {
        [self.opaque.as_ref(), self.alpha.as_ref()]
            .into_iter()
            .flatten()
            .flat_map(|domain| {
                domain.pages.iter().filter_map(|page| match page {
                    AtlasPagePublishPlan::Carry { page_id } => {
                        Some(format!(r"statics\textures\{}", atlas_page_string(domain.prefix, *page_id)))
                    }
                    AtlasPagePublishPlan::Build(_) => None,
                })
            })
            .collect()
    }

    /// Renders pages marked `Build` without mutating the output tree, handing each encoded page
    /// to `emit` as it is produced.
    ///
    /// Pages are streamed rather than collected so at most one encoded page is resident at a time.
    /// `emit` is called in the fixed page order of the opaque family followed by the alpha family.
    ///
    /// # Errors
    ///
    /// Returns an error if a page fails to encode, or propagates the first error `emit` returns.
    /// An emitter error aborts before the next page's canvas is composed.
    pub fn render_streaming(self, mut emit: impl FnMut(AtlasPageWrite) -> anyhow::Result<()>) -> anyhow::Result<()> {
        for domain in [self.opaque, self.alpha].into_iter().flatten() {
            render_domain(domain, &self.texture_dir, &mut emit)?;
        }
        Ok(())
    }
}

impl AtlasManager {
    /// Computes immutable bindings, family-local reconciliation, and page publication work.
    pub fn plan(
        &self,
        vfs: &Vfs,
        distant_statics: &mut DistantStatics,
        textures: AtlasTextureSet<IndexSet<String>>,
    ) -> anyhow::Result<AtlasPublishPlan> {
        let span = info_span!("atlas.plan", report = true);
        let _guard = span.enter();
        let shared_config = AtlasSharedConfig::current(&self.plan, self.dedupe_mode, self.atlas_max_size);

        let opaque = self.plan_domain(
            vfs,
            &textures.opaque,
            &self.fingerprints.opaque,
            OPAQUE_ATLAS_PREFIX,
            AtlasDomain::Opaque,
            self.prior.opaque.as_ref(),
            &shared_config,
        )?;
        let alpha = self.plan_domain(
            vfs,
            &textures.alpha,
            &self.fingerprints.alpha,
            ALPHA_ATLAS_PREFIX,
            AtlasDomain::Alpha,
            self.prior.alpha.as_ref(),
            &shared_config,
        )?;

        let binding_digest = atlas_binding_digest(&opaque.bindings, &alpha.bindings);
        update_uv_bounds_from_maps(distant_statics, &opaque.bindings, &alpha.bindings, vfs)?;
        // Move the families into the cache after capturing their page counts.
        let opaque_page_count = opaque.cache.pages.len() as u32;
        let alpha_page_count = alpha.cache.pages.len() as u32;
        let cache = AtlasCache {
            version: ATLAS_CACHE_VERSION,
            shared_config,
            opaque: opaque.cache,
            alpha: alpha.cache,
        };
        let cache_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cache)
            .context("failed to serialize static atlas cache")?
            .to_vec();
        let domains = [opaque.publish.as_ref(), alpha.publish.as_ref()];
        let prepared_source_bytes = domains
            .into_iter()
            .flatten()
            .map(|domain| domain.prepared_source_bytes)
            .sum::<u64>();
        let max_page_bytes = domains
            .into_iter()
            .flatten()
            .flat_map(|domain| &domain.pages)
            .filter_map(build_page)
            .map(page_rgba_bytes)
            .max()
            .unwrap_or(0);
        let publication_bytes_estimate = domains
            .into_iter()
            .flatten()
            .flat_map(|domain| &domain.pages)
            .filter_map(build_page)
            .map(|page| 256u64.saturating_add(page_rgba_bytes(page)))
            .sum();
        let cache_hit = opaque.plan_mode == AtlasFamilyPlanMode::ExactCarry
            && alpha.plan_mode == AtlasFamilyPlanMode::ExactCarry
            && opaque.built_page_count == 0
            && alpha.built_page_count == 0;
        let zero_page_write_reconciliation_count = [
            (opaque.plan_mode, opaque.built_page_count),
            (alpha.plan_mode, alpha.built_page_count),
        ]
        .into_iter()
        .filter(|(mode, built)| *mode == AtlasFamilyPlanMode::Reconciled && *built == 0)
        .count();

        let built_page_counts = AtlasTextureSet::new(opaque.built_page_count, alpha.built_page_count);
        let metrics = AtlasPlanMetrics {
            page_counts: AtlasTextureSet::new(opaque_page_count, alpha_page_count),
            layout_hits: AtlasTextureSet::new(opaque.layout_hit, alpha.layout_hit),
            decoded_texture_count: opaque.decoded_texture_count + alpha.decoded_texture_count,
            carried_page_counts: AtlasTextureSet::new(opaque.carried_page_count, alpha.carried_page_count),
            dirty_page_count: built_page_counts.opaque + built_page_counts.alpha,
            built_page_counts,
            family_metrics: AtlasTextureSet::new(opaque.metrics, alpha.metrics),
            zero_page_write_reconciliation_count,
            publication_bytes_estimate,
            planning_peak_bytes: prepared_source_bytes,
            publication_peak_bytes: prepared_source_bytes.saturating_add(max_page_bytes.saturating_mul(4)),
        };
        Ok(AtlasPublishPlan {
            opaque: opaque.publish,
            alpha: alpha.publish,
            texture_dir: self.texture_dir.clone(),
            cache_hit,
            binding_digest,
            binding_deltas: AtlasTextureSet::new(opaque.binding_delta, alpha.binding_delta),
            metrics,
            cache_bytes,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_domain(
        &self,
        vfs: &Vfs,
        keys: &IndexSet<String>,
        fingerprints: &[(u64, Hash)],
        prefix: &'static str,
        domain: AtlasDomain,
        prior: Option<&CachedAtlasFamily>,
        shared: &AtlasSharedConfig,
    ) -> anyhow::Result<PlannedFamily> {
        let family_config = AtlasFamilyConfig::current(&self.plan, domain);
        if let Some(prior) = prior.filter(|prior| {
            prior.family_config == family_config && fingerprints_match(keys, fingerprints, &prior.texture_fingerprints)
        }) {
            let carried_page_count = prior.pages.len();
            let mut allocation = allocation_stats(prior, shared)?;
            allocation.retained_slots = prior.slots.len();
            let current_bindings = cached_uv_map(&prior.bindings);
            // Exact carry republishes the prior bindings verbatim.
            let delta = BindingDelta {
                unchanged: prior.bindings.len(),
                ..BindingDelta::default()
            };
            let metrics = family_metrics(
                AtlasFamilyPlanMode::ExactCarry,
                &allocation,
                carried_page_count,
                0,
                Some(&delta),
            );
            return Ok(PlannedFamily {
                publish: carry_publish(prefix, carried_page_count),
                bindings: current_bindings,
                cache: prior.clone(),
                layout_hit: true,
                decoded_texture_count: 0,
                carried_page_count,
                built_page_count: 0,
                plan_mode: AtlasFamilyPlanMode::ExactCarry,
                binding_delta: Some(delta),
                metrics,
            });
        }

        let (canon0, alias0): (Vec<String>, AtlasAliasRelation) = match self.dedupe_mode {
            TextureDedupeMode::Off => (keys.iter().cloned().collect(), AtlasAliasRelation::Identity),
            TextureDedupeMode::Exact => {
                let (canon0, alias) = build_alias_map(
                    keys.iter()
                        .cloned()
                        .zip(fingerprints.iter())
                        .map(|(key, (size, hash))| (key, source_fingerprint(*size, hash.as_bytes()))),
                );
                (canon0, AtlasAliasRelation::Complete(alias))
            }
        };
        let canon0_set: IndexSet<String> = canon0.iter().cloned().collect();
        let dims: HashMap<String, u32> = canon0_set
            .iter()
            .map(|key| (key.clone(), self.plan.dim_for(key, domain)))
            .collect();
        let (prepared, alias1) = prepare_textures(&canon0_set, vfs, &dims, self.dedupe_mode, domain);
        let groups = build_prepared_groups(keys, &alias0, &alias1, &prepared.layout_items, &prepared.content_fingerprints)?;
        let layout_input_digest = family_layout_digest(prefix, shared, &family_config, &groups)?;
        let decoded_texture_count = prepared.decoded_texture_count;
        let canonical_count = groups.len();
        let stats = TextureDedupeDomainStats {
            input_count: keys.len(),
            canonical_count,
            source_alias_count: keys.len() - canon0.len(),
            decoded_alias_count: canon0.len() - canonical_count,
            missing_to_default_count: 0,
        };
        log_dedupe_stats(domain.as_str(), &stats);
        let texture_fingerprints = build_fingerprint_entries(keys, fingerprints);
        let prepared_source_bytes = prepared.prepared_source_bytes;
        let mut images = prepared.images;
        let prior_bindings = prior.map(|family| family.bindings.clone());

        let (allocated, plan_mode) = if let Some(prior) = prior {
            match reconcile_family(
                prior,
                family_config.clone(),
                layout_input_digest,
                &groups,
                texture_fingerprints.clone(),
                shared,
            ) {
                Ok(allocated) => (allocated, AtlasFamilyPlanMode::Reconciled),
                Err(error) => {
                    // Reuse the prepared images instead of decoding the domain again.
                    trace!(domain = domain.as_str(), %error, "Atlas reconciliation failed closed; fresh-packing family");
                    (
                        fresh_allocate_family(
                            family_config.clone(),
                            layout_input_digest,
                            &groups,
                            texture_fingerprints.clone(),
                            shared,
                        )?,
                        AtlasFamilyPlanMode::Fresh,
                    )
                }
            }
        } else {
            (
                fresh_allocate_family(
                    family_config.clone(),
                    layout_input_digest,
                    &groups,
                    texture_fingerprints,
                    shared,
                )?,
                AtlasFamilyPlanMode::Fresh,
            )
        };
        apply_provider_renames(&mut images, &allocated.provider_renames)?;

        let binding_delta = prior_bindings
            .as_deref()
            .map(|previous| binding_delta(previous, &allocated.cache.bindings));
        let layout_hit = prior.is_some_and(|prior| prior.layout_input_digest == layout_input_digest);
        let built_page_count = allocated.pages.iter().filter(|page| page.dirty).count();
        let carried_page_count = allocated.pages.len() - built_page_count;
        let metrics = family_metrics(
            plan_mode,
            &allocated.stats,
            carried_page_count,
            built_page_count,
            binding_delta.as_ref(),
        );
        Ok(PlannedFamily {
            publish: domain_publish(prefix, allocated.pages, images, prepared_source_bytes),
            bindings: allocated.bindings,
            cache: allocated.cache,
            layout_hit,
            decoded_texture_count,
            carried_page_count,
            built_page_count,
            plan_mode,
            binding_delta,
            metrics,
        })
    }
}

fn domain_publish(
    prefix: &'static str,
    pages: Vec<crate::atlas::reconcile::ReconciledPage>,
    images: HashMap<String, image::RgbaImage>,
    prepared_source_bytes: u64,
) -> Option<AtlasDomainPublishPlan> {
    if pages.is_empty() {
        return None;
    }
    Some(AtlasDomainPublishPlan {
        prefix,
        pages: pages
            .into_iter()
            .map(|page| {
                if page.dirty {
                    AtlasPagePublishPlan::Build(page.page)
                } else {
                    AtlasPagePublishPlan::Carry { page_id: page.page.id }
                }
            })
            .collect(),
        images,
        prepared_source_bytes,
    })
}

fn carry_publish(prefix: &'static str, page_count: usize) -> Option<AtlasDomainPublishPlan> {
    if page_count == 0 {
        return None;
    }
    Some(AtlasDomainPublishPlan {
        prefix,
        pages: (0..page_count)
            .map(|page_id| AtlasPagePublishPlan::Carry { page_id })
            .collect(),
        images: HashMap::new(),
        prepared_source_bytes: 0,
    })
}

fn family_layout_digest(
    prefix: &str,
    shared: &AtlasSharedConfig,
    family: &AtlasFamilyConfig,
    groups: &[PreparedAtlasGroup],
) -> anyhow::Result<[u8; 32]> {
    let shared_bytes =
        rkyv::to_bytes::<rkyv::rancor::Error>(shared).context("failed to serialize shared atlas layout configuration")?;
    let family_bytes =
        rkyv::to_bytes::<rkyv::rancor::Error>(family).context("failed to serialize family atlas layout configuration")?;
    let mut hasher = Hasher::new();
    hasher.update(b"tes3-distantland-atlas-family-layout-v2");
    hash_bytes(&mut hasher, prefix.as_bytes());
    hash_bytes(&mut hasher, &shared_bytes);
    hash_bytes(&mut hasher, &family_bytes);
    for group in groups {
        hash_bytes(&mut hasher, group.group_key.as_bytes());
        hasher.update(&(group.logical_keys.len() as u64).to_le_bytes());
        for key in &group.logical_keys {
            hash_bytes(&mut hasher, key.as_bytes());
        }
        for value in [group.item.w, group.item.h] {
            hasher.update(&value.to_le_bytes());
        }
        let source = group
            .item
            .source
            .context("prepared atlas layout item is missing its source rectangle")?;
        for value in [source.x, source.y, source.w, source.h] {
            hasher.update(&value.to_le_bytes());
        }
        let source_size = group
            .item
            .source_size
            .context("prepared atlas layout item is missing its source size")?;
        hasher.update(&source_size.0.to_le_bytes());
        hasher.update(&source_size.1.to_le_bytes());
        hasher.update(&[u8::from(group.item.trimmed)]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn hash_bytes(hasher: &mut Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn family_metrics(
    plan_mode: AtlasFamilyPlanMode,
    allocation: &crate::atlas::reconcile::AtlasAllocationStats,
    carried_pages: usize,
    built_pages: usize,
    delta: Option<&BindingDelta>,
) -> AtlasFamilyMetrics {
    AtlasFamilyMetrics {
        plan_mode,
        retained_slots: allocation.retained_slots,
        allocated_slots: allocation.allocated_slots,
        freed_slots: allocation.freed_slots,
        relocated_slots: allocation.relocated_slots,
        provider_promoted_slots: allocation.provider_promoted_slots,
        active_area: allocation.active_area,
        reserved_area: allocation.reserved_area,
        page_area: allocation.page_area,
        usable_page_area: allocation.usable_page_area,
        fragmentation_ppm: allocation.fragmentation_ppm,
        carried_pages,
        built_pages,
        appended_pages: allocation.appended_pages,
        retained_empty_pages: allocation.retained_empty_pages,
        truncated_pages: allocation.truncated_pages,
        binding_delta: delta.map_or_else(AtlasBindingDeltaMetrics::default, |delta| AtlasBindingDeltaMetrics {
            available: true,
            added: delta.added.len(),
            removed: delta.removed.len(),
            changed: delta.changed.len(),
            unchanged: delta.unchanged,
        }),
    }
}

fn build_page(page: &AtlasPagePublishPlan) -> Option<&Page> {
    match page {
        AtlasPagePublishPlan::Carry { .. } => None,
        AtlasPagePublishPlan::Build(page) => Some(page),
    }
}

fn page_rgba_bytes(page: &Page) -> u64 {
    u64::from(page.width) * u64::from(page.height) * 4
}

fn render_domain(
    plan: AtlasDomainPublishPlan,
    texture_dir: &Path,
    emit: &mut impl FnMut(AtlasPageWrite) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let d3d_format = if plan.prefix == OPAQUE_ATLAS_PREFIX {
        D3DFormat::DXT1
    } else {
        D3DFormat::DXT5
    };
    for page in plan.pages {
        let AtlasPagePublishPlan::Build(page) = page else {
            continue;
        };
        let canvas = compose_planned_page(&plan.images, &page);
        let bytes =
            encode_d3d_bcn_dds_with_mips(&canvas, d3d_format).context("failed to encode planned static atlas DDS page")?;
        // The canvas is the largest single allocation here - a 16384x8192 page is 512 MiB of RGBA.
        // Free it before the emitter writes, so publication holds the encoded bytes alone.
        drop(canvas);
        emit(AtlasPageWrite {
            path: atlas_page_path(texture_dir, plan.prefix, page.id),
            bytes,
        })?;
    }
    Ok(())
}
