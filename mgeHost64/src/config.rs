use std::path::Path;

use mge_config::{ConfigDocument, FILE_NAME};

use crate::abi::USE_DISTANT_LAND;

/// `[min, max]` ranges for loaded and live settings; C++ and MWSE must mirror them.
pub use mge_config::{
    HORIZON_BIAS_Z_RANGE, HORIZON_BINS_RANGE, HORIZON_MAX_RANGE_RANGE, HORIZON_NEAR_UNITS_RANGE,
    HORIZON_OBJECT_BIAS_Z_RANGE, HORIZON_REBUILD_EYE_THRESHOLD_RANGE, HORIZON_RING_STEP_RANGE, HORIZON_SAMPLE_SPACING_RANGE,
};

#[derive(Clone, Copy, Debug)]
pub struct Configuration {
    pub mge_flags: u32,
    pub automatic_distant_land_rebuild: bool,
    pub horizon_culling: bool,
    pub horizon_bias_z: f32,
    pub horizon_object_bias_z: f32,
    pub horizon_near_units: f32,
    pub horizon_ring_step: f32,
    pub horizon_max_range: f32,
    pub horizon_bins: u32,
    /// Ray-march step in world units along each azimuth; live-tunable via `SetHorizonConfig`.
    ///
    /// Coarsening live march samples can lose culling but cannot over-cull.
    pub horizon_sample_spacing: f32,
    pub horizon_adaptive_gate: bool,
    /// Eye movement in world units that must accumulate before rebuilding the cached horizon table.
    ///
    /// Host-side only and not part of the live IPC horizon config; it prevents camera sway from
    /// rebuilding the cache.
    pub horizon_rebuild_eye_threshold: f32,
    /// Selects the hierarchical max-height-pyramid builder in place of the linear march.
    ///
    /// Load-time only; the linear builder remains the test oracle.
    pub horizon_hierarchical_march: bool,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            mge_flags: 0,
            automatic_distant_land_rebuild: false,
            horizon_culling: false,
            horizon_bias_z: 512.0,
            horizon_object_bias_z: 256.0,
            horizon_near_units: 2048.0,
            horizon_ring_step: 4096.0,
            horizon_max_range: 49152.0,
            horizon_bins: 512,
            horizon_sample_spacing: 512.0,
            horizon_adaptive_gate: true,
            horizon_rebuild_eye_threshold: 16.0,
            horizon_hierarchical_march: true,
        }
    }
}

impl Configuration {
    pub fn load() -> Self {
        Self::load_at(Path::new("."))
    }

    /// Missing or invalid files fall back to the shared crate's in-memory defaults.
    pub fn load_at(root: &Path) -> Self {
        let document = ConfigDocument::open(root.join(FILE_NAME));
        if let Some(diagnostic) = document.diagnostic() {
            tracing::error!(
                path = %document.path().display(),
                %diagnostic,
                "invalid MGE XE configuration; using built-in defaults"
            );
        }
        for warning in document.warnings() {
            tracing::warn!(
                path = %document.path().display(),
                setting = %warning.path,
                message = %warning.message,
                "MGE XE configuration value was clamped"
            );
        }
        let distant = &document.settings().distant_land;
        Self {
            mge_flags: if distant.enabled { USE_DISTANT_LAND } else { 0 },
            automatic_distant_land_rebuild: distant.automatic_rebuild,
            horizon_culling: distant.horizon.culling,
            horizon_bias_z: distant.horizon.height_bias,
            horizon_object_bias_z: distant.horizon.object_bias,
            horizon_near_units: distant.horizon.near_exclude,
            horizon_ring_step: distant.horizon.ring_step,
            horizon_max_range: distant.horizon.max_range,
            horizon_bins: distant.horizon.azimuth_bins,
            horizon_sample_spacing: distant.horizon.sample_spacing,
            horizon_adaptive_gate: distant.horizon.adaptive_gate,
            horizon_rebuild_eye_threshold: distant.horizon.rebuild_eye_threshold,
            horizon_hierarchical_march: distant.horizon.hierarchical_march,
        }
    }

    pub fn distant_land_enabled(&self) -> bool {
        (self.mge_flags & USE_DISTANT_LAND) != 0
    }

    pub fn disable_distant_land(&mut self) {
        self.mge_flags &= !USE_DISTANT_LAND;
    }

    /// Keeps loaded and live settings within shared ranges.
    pub fn clamp_horizon(&mut self) {
        self.horizon_bias_z = self.horizon_bias_z.clamp(HORIZON_BIAS_Z_RANGE.0, HORIZON_BIAS_Z_RANGE.1);
        self.horizon_object_bias_z = self
            .horizon_object_bias_z
            .clamp(HORIZON_OBJECT_BIAS_Z_RANGE.0, HORIZON_OBJECT_BIAS_Z_RANGE.1);
        self.horizon_near_units = self
            .horizon_near_units
            .clamp(HORIZON_NEAR_UNITS_RANGE.0, HORIZON_NEAR_UNITS_RANGE.1);
        self.horizon_ring_step = self
            .horizon_ring_step
            .clamp(HORIZON_RING_STEP_RANGE.0, HORIZON_RING_STEP_RANGE.1);
        self.horizon_max_range = self
            .horizon_max_range
            .clamp(HORIZON_MAX_RANGE_RANGE.0, HORIZON_MAX_RANGE_RANGE.1);
        self.horizon_bins = self.horizon_bins.clamp(HORIZON_BINS_RANGE.0, HORIZON_BINS_RANGE.1);
        self.horizon_sample_spacing = self
            .horizon_sample_spacing
            .clamp(HORIZON_SAMPLE_SPACING_RANGE.0, HORIZON_SAMPLE_SPACING_RANGE.1);
        self.horizon_rebuild_eye_threshold = self
            .horizon_rebuild_eye_threshold
            .clamp(HORIZON_REBUILD_EYE_THRESHOLD_RANGE.0, HORIZON_REBUILD_EYE_THRESHOLD_RANGE.1);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Configuration;

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn with_toml(name: &str, toml: &str) -> Self {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("mgehost64_config_{name}_{}_{}", std::process::id(), nanos));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join(mge_config::FILE_NAME), toml).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn missing_keys_use_shared_defaults() {
        let root = TestRoot::with_toml("missing", "schema_version = 1\n");
        let configuration = Configuration::load_at(root.path());
        assert!(!configuration.automatic_distant_land_rebuild);
        assert!(!configuration.distant_land_enabled());
        assert!(configuration.horizon_culling);
        assert_eq!(configuration.horizon_bins, 512);
    }

    #[test]
    fn host_fields_load_from_shared_toml_schema() {
        let root = TestRoot::with_toml(
            "host_fields",
            "schema_version = 1\n\
             [distant_land]\n\
             enabled = true\n\
             automatic_rebuild = false\n\
             [distant_land.horizon]\n\
             culling = true\n\
             height_bias = 1024\n\
             object_bias = 128\n\
             near_exclude = 1024\n\
             ring_step = 2048\n\
             max_range = 65536\n\
             azimuth_bins = 1024\n\
             sample_spacing = 256\n\
             adaptive_gate = false\n\
             rebuild_eye_threshold = 48\n\
             hierarchical_march = false\n",
        );
        let configuration = Configuration::load_at(root.path());
        assert!(configuration.distant_land_enabled());
        assert!(!configuration.automatic_distant_land_rebuild);
        assert!(configuration.horizon_culling);
        assert_eq!(configuration.horizon_bias_z, 1024.0);
        assert_eq!(configuration.horizon_object_bias_z, 128.0);
        assert_eq!(configuration.horizon_near_units, 1024.0);
        assert_eq!(configuration.horizon_ring_step, 2048.0);
        assert_eq!(configuration.horizon_max_range, 65536.0);
        assert_eq!(configuration.horizon_bins, 1024);
        assert_eq!(configuration.horizon_sample_spacing, 256.0);
        assert!(!configuration.horizon_adaptive_gate);
        assert_eq!(configuration.horizon_rebuild_eye_threshold, 48.0);
        assert!(!configuration.horizon_hierarchical_march);
    }

    #[test]
    fn invalid_toml_uses_built_in_defaults() {
        let root = TestRoot::with_toml(
            "invalid",
            "schema_version = 1\n[distant_land]\nautomatic_rebuild = \"maybe\"\n",
        );
        let configuration = Configuration::load_at(root.path());
        assert!(!configuration.automatic_distant_land_rebuild);
        assert!(!configuration.distant_land_enabled());
    }

    #[test]
    fn clamp_horizon_pins_out_of_range_fields_to_valid_bounds() {
        let mut configuration = Configuration {
            horizon_bias_z: -10.0,
            horizon_object_bias_z: 1.0e9,
            horizon_near_units: -1.0,
            horizon_ring_step: 0.0,
            horizon_max_range: 1.0e12,
            horizon_bins: 0,
            horizon_sample_spacing: 0.0,
            horizon_rebuild_eye_threshold: -5.0,
            ..Configuration::default()
        };
        configuration.clamp_horizon();
        assert_eq!(configuration.horizon_bias_z, super::HORIZON_BIAS_Z_RANGE.0);
        assert_eq!(configuration.horizon_object_bias_z, super::HORIZON_OBJECT_BIAS_Z_RANGE.1);
        assert_eq!(configuration.horizon_near_units, super::HORIZON_NEAR_UNITS_RANGE.0);
        assert_eq!(configuration.horizon_ring_step, super::HORIZON_RING_STEP_RANGE.0);
        assert_eq!(configuration.horizon_max_range, super::HORIZON_MAX_RANGE_RANGE.1);
        assert_eq!(configuration.horizon_bins, super::HORIZON_BINS_RANGE.0);
        assert_eq!(configuration.horizon_sample_spacing, super::HORIZON_SAMPLE_SPACING_RANGE.0);
        assert_eq!(
            configuration.horizon_rebuild_eye_threshold,
            super::HORIZON_REBUILD_EYE_THRESHOLD_RANGE.0
        );
    }
}
