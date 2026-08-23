use super::*;

fn vertex(position: Vec3) -> Vertex {
    Vertex {
        position,
        ..Vertex::default()
    }
}

#[test]
fn nif_identity_survives_parse_failure() {
    let temp = tempfile::tempdir().unwrap();
    let mesh_dir = temp.path().join("meshes");
    std::fs::create_dir_all(&mesh_dir).unwrap();
    let bytes = b"not a nif";
    std::fs::write(mesh_dir.join("broken.nif"), bytes).unwrap();
    let vfs = Vfs {
        ini_path: temp.path().join("Morrowind.ini"),
        data_dirs: vec![temp.path().to_path_buf()],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::build_directory_map(&[temp.path().to_path_buf()]).unwrap(),
    };

    let extraction =
        DistantStatic::from_nif_with_identity("broken.nif", &vfs, 1.0, 0.0, false, 1.0, false, &StaticOverrides::default());

    assert!(extraction.distant_static.is_none());
    assert_eq!(extraction.identity, Some(ContentIdentity::from_bytes(bytes)));
    assert_eq!(
        extraction.resolution,
        Some(MeshResolutionFact {
            rule: distantland_foundation::identity::MeshResolutionRule::Original,
            resolved_key: "broken.nif".to_string(),
        })
    );
}

#[test]
fn sanitize_triangles_keeps_valid_and_counts_invalid_reasons() {
    let vertices = vec![
        vertex(Vec3::new(0.0, 0.0, 0.0)),
        vertex(Vec3::new(1.0, 0.0, 0.0)),
        vertex(Vec3::new(0.0, 1.0, 0.0)),
        vertex(Vec3::new(f32::NAN, 0.0, 0.0)),
    ];
    let result = sanitize_triangles(&[[0, 1, 2], [0, 1, 9], [0, 1, 3], [0, 0, 1]], &vertices);

    assert_eq!(result.triangles, vec![[0, 1, 2]]);
    assert_eq!(result.dropped_out_of_bounds, 1);
    assert_eq!(result.dropped_non_finite_position, 1);
    assert_eq!(result.dropped_degenerate, 1);
    assert_eq!(result.dropped_total(), 3);
}

#[test]
fn sanitize_triangles_skips_subset_when_all_triangles_are_invalid() {
    let vertices = vec![
        vertex(Vec3::new(0.0, 0.0, 0.0)),
        vertex(Vec3::new(1.0, 0.0, 0.0)),
        vertex(Vec3::new(0.0, 1.0, 0.0)),
    ];
    let result = sanitize_triangles(&[[0, 1, 9], [0, 0, 1]], &vertices);

    assert!(result.triangles.is_empty());
    assert_eq!(result.dropped_out_of_bounds, 1);
    assert_eq!(result.dropped_degenerate, 1);
}
