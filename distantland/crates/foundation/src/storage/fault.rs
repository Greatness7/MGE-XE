//! Minimal process-interruption hooks for representative storage integration tests.
//!
//! Hooks are compiled into production builds and remain inert unless the environment arms one.

use std::sync::OnceLock;

pub(crate) const FAULT_EXIT_CODE: i32 = 42;
const FAULT_EXIT_ENV: &str = "TES3_DL_FAULT_EXIT";

const NAMED_POINTS: [&str; 2] = ["after_state_invalidation", "mid_terrain_dds"];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct FaultConfig {
    point: Option<String>,
}

impl FaultConfig {
    fn from_values(point: Option<String>) -> Self {
        Self {
            point: point.filter(|point| NAMED_POINTS.contains(&point.as_str())),
        }
    }
}

fn config() -> &'static FaultConfig {
    static CONFIG: OnceLock<FaultConfig> = OnceLock::new();
    CONFIG.get_or_init(|| FaultConfig::from_values(std::env::var(FAULT_EXIT_ENV).ok()))
}

/// Returns whether the named representative fault boundary is armed.
pub fn arm(point: &str) -> bool {
    debug_assert!(NAMED_POINTS.contains(&point), "unknown fault point {point}");
    config().point.as_deref() == Some(point)
}

/// Exits with the stable injected-fault code when `point` is armed.
pub fn exit_if_requested(point: &str) {
    if arm(point) {
        std::process::exit(FAULT_EXIT_CODE);
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn configuration_accepts_only_retained_points() {
        assert_eq!(
            FaultConfig::from_values(Some("mid_terrain_dds".into())),
            FaultConfig {
                point: Some("mid_terrain_dds".into()),
            }
        );
        assert_eq!(FaultConfig::from_values(Some("after_intent".into())), FaultConfig::default());
        assert_eq!(
            FaultConfig::from_values(Some("after_state_invalidation".into())),
            FaultConfig {
                point: Some("after_state_invalidation".into()),
            }
        );
    }

    #[test]
    fn requested_fault_exits_with_the_pinned_code() {
        const CHILD_ENV: &str = "TES3_DL_FAULT_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            exit_if_requested("after_state_invalidation");
            panic!("configured fault did not exit");
        }

        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::fault::tests::requested_fault_exits_with_the_pinned_code"])
            .env(CHILD_ENV, "1")
            .env(FAULT_EXIT_ENV, "after_state_invalidation")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(FAULT_EXIT_CODE));
    }
}
