use super::super::pack::AtlasAliasRelation;
use super::*;
use hashbrown::HashMap;
use itertools::Itertools;

fn test_shared() -> AtlasSharedConfig {
    AtlasSharedConfig::current(&SizingPlan::uniform(128), TextureDedupeMode::Exact, 128)
}

fn test_group(key: &str) -> PreparedAtlasGroup {
    PreparedAtlasGroup {
        group_key: key.into(),
        logical_keys: vec![key.into()],
        item: LayoutItem {
            key: key.into(),
            w: 40,
            h: 40,
            source: Some(Rect::new(0, 0, 40, 40)),
            source_size: Some((40, 40)),
            trimmed: false,
        },
        content_fingerprint: [0; 32],
    }
}

fn test_prior(entries: &[(&str, u32, u64)], page_count: usize, next_slot_id: u64) -> CachedAtlasFamily {
    let pages = vec![CachedAtlasPage { width: 128, height: 128 }; page_count];
    let slots = entries
        .iter()
        .map(|(key, page_id, slot_id)| CachedAtlasSlot {
            slot_id: *slot_id,
            page_id: *page_id,
            reserved_rect: CachedAtlasRect {
                x: 8,
                y: 8,
                width: 72,
                height: 72,
            },
            destination: CachedAtlasRect {
                x: 24,
                y: 24,
                width: 40,
                height: 40,
            },
            source: CachedAtlasRect {
                x: 0,
                y: 0,
                width: 40,
                height: 40,
            },
            source_size: [40, 40],
            rotated: false,
            trimmed: false,
            provider_key: (*key).into(),
            content_fingerprint: [0; 32],
        })
        .collect_vec();
    let mut key_slots = entries
        .iter()
        .map(|(key, _, slot_id)| CachedAtlasKeySlot {
            path: (*key).into(),
            slot_id: *slot_id,
        })
        .collect_vec();
    key_slots.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    let bindings = bindings_from_relations(&pages, &slots, &key_slots).unwrap();
    CachedAtlasFamily {
        family_config: AtlasFamilyConfig::default(),
        layout_input_digest: [0; 32],
        next_slot_id,
        pages,
        slots,
        key_slots,
        bindings: map_to_cached_bounds(&bindings),
        texture_fingerprints: entries
            .iter()
            .map(|(key, _, _)| CachedTextureFingerprint {
                path: (*key).into(),
                fingerprint: TextureFingerprint { size: 0, hash: [0; 32] },
            })
            .collect(),
    }
}

#[test]
fn weighted_matching_beats_greedy_split_join_claiming() {
    // g0-s0 retains three keys, while the tempting lower-slot g1-s0 edge retains two.
    // Greedily claiming g1-s0 leaves only one retained binding; the optimum keeps four.
    let edges = vec![(1, 0, 2), (0, 0, 3), (0, 1, 2)];
    let matching = min_cost_positive_matching(2, 2, &edges).unwrap();
    assert_eq!(matching, vec![(0, 1), (1, 0)]);
}

#[test]
fn matching_retains_one_compatible_overlap() {
    let groups = vec![PreparedAtlasGroup {
        group_key: "a".into(),
        logical_keys: vec!["a".into()],
        item: LayoutItem {
            key: "a".into(),
            w: 40,
            h: 40,
            source: Some(Rect::new(0, 0, 40, 40)),
            source_size: Some((40, 40)),
            trimmed: true,
        },
        content_fingerprint: [0; 32],
    }];
    let slot = CachedAtlasSlot {
        slot_id: 0,
        page_id: 0,
        reserved_rect: CachedAtlasRect {
            x: 8,
            y: 8,
            width: 72,
            height: 72,
        },
        destination: CachedAtlasRect {
            x: 24,
            y: 24,
            width: 40,
            height: 40,
        },
        source: CachedAtlasRect {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        },
        source_size: [40, 40],
        rotated: false,
        trimmed: true,
        provider_key: "a".into(),
        content_fingerprint: [0; 32],
    };
    let prior = vec![PriorGroup {
        slot: &slot,
        logical_keys: vec!["a".into()],
    }];
    let shared = AtlasSharedConfig::current(&SizingPlan::uniform(128), TextureDedupeMode::Exact, 128);
    assert_eq!(maximum_weight_matching(&groups, &prior, &shared).unwrap(), vec![(0, 0)]);
}

#[test]
fn reservation_capture_matches_fresh_packer_geometry() {
    let shared = AtlasSharedConfig::current(&SizingPlan::uniform(128), TextureDedupeMode::Exact, 128);
    let atlas = pack_layout_only(
        vec![LayoutItem {
            key: "a".into(),
            w: 40,
            h: 24,
            source: Some(Rect::new(0, 0, 40, 24)),
            source_size: Some((40, 24)),
            trimmed: false,
        }],
        128,
    )
    .unwrap();
    let page = &atlas.pages[0];
    let frame = &page.frames[0];
    let reserved = capture_reservation(frame.frame, page.width, page.height, &shared).unwrap();
    assert_eq!(reserved.x, frame.frame.x - ATLAS_TEXTURE_EXTRUSION);
    assert_eq!(reserved.y, frame.frame.y - ATLAS_TEXTURE_EXTRUSION);
    assert_eq!(reserved.width, frame.frame.w + ATLAS_TEXTURE_EXTRUSION * 2);
    assert_eq!(reserved.height, frame.frame.h + ATLAS_TEXTURE_EXTRUSION * 2);
}

#[test]
fn seeded_free_list_is_independent_of_occupied_input_order() {
    let occupied = [
        (
            2,
            CachedAtlasRect {
                x: 48,
                y: 8,
                width: 32,
                height: 32,
            },
        ),
        (
            1,
            CachedAtlasRect {
                x: 8,
                y: 8,
                width: 32,
                height: 32,
            },
        ),
    ];
    let forward = SeededFreeList::new(128, 128, 8, occupied).unwrap();
    let reverse = SeededFreeList::new(128, 128, 8, occupied.into_iter().rev()).unwrap();
    assert_eq!(forward.free, reverse.free);
}

#[test]
fn middle_pages_survive_and_are_reused_before_append_while_empty_tails_truncate() {
    let shared = test_shared();
    let full_prior = test_prior(&[("a", 0, 0), ("b", 1, 1), ("c", 2, 2)], 3, 3);

    let middle_removed = vec![test_group("a"), test_group("c")];
    let middle = reconcile_family(
        &full_prior,
        AtlasFamilyConfig::default(),
        [1; 32],
        &middle_removed,
        Vec::new(),
        &shared,
    )
    .unwrap();
    assert_eq!(middle.cache.pages.len(), 3);
    assert_eq!(middle.pages[1].page.frames.len(), 0);
    assert!(!middle.pages[1].dirty);
    assert_eq!(middle.stats.retained_empty_pages, 1);
    assert_eq!(middle.stats.truncated_pages, 0);

    let with_new = vec![test_group("a"), test_group("c"), test_group("d")];
    let reused = reconcile_family(
        &middle.cache,
        AtlasFamilyConfig::default(),
        [2; 32],
        &with_new,
        Vec::new(),
        &shared,
    )
    .unwrap();
    let d_slot_id = reused.cache.key_slots.iter().find(|entry| entry.path == "d").unwrap().slot_id;
    let d_slot = reused.cache.slots.iter().find(|slot| slot.slot_id == d_slot_id).unwrap();
    assert_eq!(d_slot.page_id, 1);
    assert_eq!(reused.stats.appended_pages, 0);

    let tail_removed = vec![test_group("a"), test_group("b")];
    let truncated = reconcile_family(
        &full_prior,
        AtlasFamilyConfig::default(),
        [3; 32],
        &tail_removed,
        Vec::new(),
        &shared,
    )
    .unwrap();
    assert_eq!(truncated.cache.pages.len(), 2);
    assert_eq!(truncated.stats.truncated_pages, 1);
}

fn layout_item(key: &str) -> LayoutItem {
    LayoutItem {
        key: key.into(),
        w: 8,
        h: 8,
        source: Some(Rect::new(0, 0, 8, 8)),
        source_size: Some((8, 8)),
        trimmed: false,
    }
}

#[test]
fn build_prepared_groups_identity_one_group_per_key() {
    let keys: IndexSet<String> = ["b.dds", "a.dds"].into_iter().map(str::to_owned).collect();
    let items = [layout_item("a.dds"), layout_item("b.dds")];
    let mut fps = HashMap::new();
    fps.insert("a.dds".into(), [1; 32]);
    fps.insert("b.dds".into(), [2; 32]);
    let groups = build_prepared_groups(
        &keys,
        &AtlasAliasRelation::Identity,
        &AtlasAliasRelation::Identity,
        &items,
        &fps,
    )
    .unwrap();
    // BTreeMap orders by provider key; keys iteration order is insertion (b then a) within each group.
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].group_key, "a.dds");
    assert_eq!(groups[0].logical_keys, vec!["a.dds".to_owned()]);
    assert_eq!(groups[1].group_key, "b.dds");
    assert_eq!(groups[1].logical_keys, vec!["b.dds".to_owned()]);
}

#[test]
fn build_prepared_groups_two_tier_complete_composition() {
    // Production-order chain: sorted keys 0, a, b.
    // Tier-0 source first-seen: b→a (0 and a remain canonicals).
    // Tier-1 decoded first-seen among sorted canons: a→0.
    // Final providers: 0→0, a→0, b→0; one group with logical_keys in keys order.
    let keys: IndexSet<String> = ["0.dds", "a.dds", "b.dds"].into_iter().map(str::to_owned).collect();
    let mut tier0 = HashMap::new();
    tier0.insert("0.dds".into(), "0.dds".into());
    tier0.insert("a.dds".into(), "a.dds".into());
    tier0.insert("b.dds".into(), "a.dds".into());
    let mut tier1 = HashMap::new();
    tier1.insert("0.dds".into(), "0.dds".into());
    tier1.insert("a.dds".into(), "0.dds".into());
    let items = [layout_item("0.dds")];
    let mut fps = HashMap::new();
    fps.insert("0.dds".into(), [9; 32]);
    let groups = build_prepared_groups(
        &keys,
        &AtlasAliasRelation::Complete(tier0),
        &AtlasAliasRelation::Complete(tier1),
        &items,
        &fps,
    )
    .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].item.key, "0.dds");
    assert_eq!(
        groups[0].logical_keys,
        vec!["0.dds".to_owned(), "a.dds".to_owned(), "b.dds".to_owned()]
    );
    assert_eq!(groups[0].group_key, "0.dds");
}

#[test]
fn build_prepared_groups_complete_missing_is_error() {
    let keys: IndexSet<String> = ["a.dds"].into_iter().map(str::to_owned).collect();
    let result = build_prepared_groups(
        &keys,
        &AtlasAliasRelation::Complete(HashMap::new()),
        &AtlasAliasRelation::Identity,
        &[],
        &HashMap::new(),
    );
    let err = match result {
        Ok(_) => panic!("expected missing Complete entry to fail closed"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("tier-0"));
    assert!(err.to_string().contains("a.dds"));
}
