use std::time::{Duration, Instant};

use tracing::{debug, info};

use crate::abi::D3dxVector3;

pub const GATE_EVAL_WINDOW: u32 = 120;
const GATE_CULL_FLOOR: f32 = 16.0;
const GATE_PRUNE_FLOOR: f32 = 8.0;
const GATE_SUSPEND_WINDOWS: u32 = 2;
/// Delay before the first resume probe after a low-benefit suspend.
const GATE_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// Maximum resume-probe backoff while idle-suspended.
const GATE_PROBE_BACKOFF_CAP: Duration = Duration::from_secs(16);
const GATE_PROBE_MOVE_TRIGGER: f32 = 512.0;
const GATE_FAST_WINDOW: usize = 64;
const GATE_FAST_ENTER_COUNT: u32 = 32;
const GATE_FAST_EXIT_COUNT: u32 = 8;
const GATE_WARM_TIMEOUT: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GateState {
    Inactive,
    Active { probing: bool },
    SuspendedLowBenefit,
    SuspendedFast,
    Warming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HorizonGateMode {
    Active,
    Warming,
    Suspended,
}

#[derive(Clone, Copy, Debug, Default)]
struct EvalWindow {
    frames: u32,
    culled: u64,
    pruned: u64,
}

impl EvalWindow {
    fn add(&mut self, culled: u32, pruned: u32) {
        self.frames = self.frames.saturating_add(1);
        self.culled = self.culled.saturating_add(u64::from(culled));
        self.pruned = self.pruned.saturating_add(u64::from(pruned));
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn averages(self) -> (f32, f32) {
        if self.frames == 0 {
            return (0.0, 0.0);
        }
        (
            self.culled as f32 / self.frames as f32,
            self.pruned as f32 / self.frames as f32,
        )
    }

    fn has_benefit(self) -> bool {
        let (avg_culled, avg_pruned) = self.averages();
        avg_culled >= GATE_CULL_FLOOR || avg_pruned >= GATE_PRUNE_FLOOR
    }
}

#[derive(Debug)]
pub struct HorizonGate {
    enabled: bool,
    state: GateState,
    eval: EvalWindow,
    low_benefit_windows: u32,
    last_eye: Option<D3dxVector3>,
    fast_bits: u64,
    fast_samples: usize,
    cumulative_move_since_probe: f32,
    /// Entry time for `SuspendedLowBenefit`.
    suspended_since: Option<Instant>,
    /// Delay before the next resume probe.
    probe_backoff: Duration,
    warming_frames: u32,
    warm_build_posted: bool,
}

impl HorizonGate {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            state: GateState::Inactive,
            eval: EvalWindow::default(),
            low_benefit_windows: 0,
            last_eye: None,
            fast_bits: 0,
            fast_samples: 0,
            cumulative_move_since_probe: 0.0,
            suspended_since: None,
            probe_backoff: GATE_PROBE_INTERVAL,
            warming_frames: 0,
            warm_build_posted: false,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.reset_tracking();
        self.state = GateState::Active { probing: false };
        info!(enabled, "Terrain horizon adaptive gate toggled");
    }

    pub fn on_config_change(&mut self) {
        if !self.enabled {
            return;
        }
        self.enter_warming("config-change");
    }

    pub fn on_context_lost(&mut self) {
        if !self.enabled {
            return;
        }
        self.transition(GateState::Inactive, "context-lost", None);
        self.reset_tracking();
    }

    pub fn on_context_available(&mut self) {
        if !self.enabled {
            return;
        }
        if self.state == GateState::Inactive {
            self.enter_warming("context-restored");
        }
    }

    pub fn mode(&self) -> HorizonGateMode {
        if !self.enabled {
            return HorizonGateMode::Active;
        }
        match self.state {
            GateState::Inactive | GateState::SuspendedLowBenefit | GateState::SuspendedFast => HorizonGateMode::Suspended,
            GateState::Warming => HorizonGateMode::Warming,
            GateState::Active { .. } => HorizonGateMode::Active,
        }
    }

    pub fn is_warming(&self) -> bool {
        self.enabled && self.state == GateState::Warming
    }

    pub fn should_post_warm_build(&self) -> bool {
        self.is_warming() && !self.warm_build_posted
    }

    pub fn on_warm_build_posted(&mut self) {
        if self.is_warming() {
            self.warm_build_posted = true;
        }
    }

    pub fn on_warm_adopted(&mut self) {
        if self.enabled && self.state == GateState::Warming {
            self.eval.reset();
            self.warm_build_posted = false;
            self.transition(GateState::Active { probing: true }, "warm-table-adopted", None);
        }
    }

    pub fn on_frame(&mut self, now: Instant, eye: D3dxVector3, frame_culled: u32, frame_pruned: u32, fast_distance: f32) {
        if !self.enabled || self.state == GateState::Inactive {
            self.last_eye = Some(eye);
            return;
        }

        let displacement = self.last_eye.map(|last| eye_distance(last, eye)).unwrap_or(0.0);
        self.last_eye = Some(eye);
        self.note_fast_sample(displacement > fast_distance);

        if self.state != GateState::SuspendedFast
            && self.fast_samples == GATE_FAST_WINDOW
            && self.fast_bits.count_ones() >= GATE_FAST_ENTER_COUNT
        {
            self.transition(GateState::SuspendedFast, "fast-motion-enter", None);
            self.reset_eval_and_probe_counters();
            return;
        }

        match self.state {
            GateState::Active { probing } => self.tick_active_window(now, frame_culled, frame_pruned, probing),
            GateState::SuspendedLowBenefit => self.tick_low_benefit_suspended(now, displacement),
            GateState::SuspendedFast => self.tick_fast_suspended(),
            GateState::Warming => self.tick_warming_timeout(now),
            GateState::Inactive => {}
        }
    }

    /// Numeric state code used by tests.
    #[cfg(test)]
    pub fn state_code(&self) -> u32 {
        if !self.enabled {
            return 1;
        }
        match self.state {
            GateState::Inactive => 0,
            GateState::Active { .. } => 1,
            GateState::Warming => 2,
            GateState::SuspendedLowBenefit => 3,
            GateState::SuspendedFast => 4,
        }
    }

    fn tick_active_window(&mut self, now: Instant, frame_culled: u32, frame_pruned: u32, probing: bool) {
        self.eval.add(frame_culled, frame_pruned);
        if self.eval.frames < GATE_EVAL_WINDOW {
            return;
        }

        let averages = self.eval.averages();
        if self.eval.has_benefit() {
            self.low_benefit_windows = 0;
            if probing {
                self.probe_backoff = GATE_PROBE_INTERVAL;
                self.transition(GateState::Active { probing: false }, "probe-benefit", Some(averages));
            }
        } else if probing {
            self.enter_low_benefit_suspended(now, "probe-no-benefit", Some(averages), true);
        } else {
            self.low_benefit_windows = self.low_benefit_windows.saturating_add(1);
            if self.low_benefit_windows >= GATE_SUSPEND_WINDOWS {
                self.enter_low_benefit_suspended(now, "low-benefit-windows", Some(averages), false);
            }
        }
        self.eval.reset();
    }

    fn tick_low_benefit_suspended(&mut self, now: Instant, displacement: f32) {
        self.cumulative_move_since_probe += displacement;
        if self.cumulative_move_since_probe >= GATE_PROBE_MOVE_TRIGGER {
            self.probe_backoff = GATE_PROBE_INTERVAL;
            self.enter_warming("movement-probe");
        } else if let Some(since) = self.suspended_since
            && now.saturating_duration_since(since) >= self.probe_backoff
        {
            self.enter_warming("timer-probe");
        }
    }

    fn tick_fast_suspended(&mut self) {
        if self.fast_samples == GATE_FAST_WINDOW && self.fast_bits.count_ones() <= GATE_FAST_EXIT_COUNT {
            self.enter_warming("fast-motion-exit");
        }
    }

    fn tick_warming_timeout(&mut self, now: Instant) {
        self.warming_frames = self.warming_frames.saturating_add(1);
        if self.warming_frames >= GATE_WARM_TIMEOUT {
            self.enter_low_benefit_suspended(now, "warm-timeout", None, true);
        }
    }

    fn enter_warming(&mut self, reason: &'static str) {
        self.reset_eval_and_probe_counters();
        self.transition(GateState::Warming, reason, None);
    }

    fn enter_low_benefit_suspended(
        &mut self,
        now: Instant,
        reason: &'static str,
        averages: Option<(f32, f32)>,
        failed_probe: bool,
    ) {
        if failed_probe {
            self.probe_backoff = (self.probe_backoff * 2).min(GATE_PROBE_BACKOFF_CAP);
        }
        self.reset_eval_and_probe_counters();
        self.suspended_since = Some(now);
        self.transition(GateState::SuspendedLowBenefit, reason, averages);
    }

    fn transition(&mut self, next: GateState, reason: &'static str, averages: Option<(f32, f32)>) {
        if self.state == next {
            return;
        }
        let previous = self.state;
        self.state = next;
        if let Some((avg_culled, avg_pruned)) = averages {
            debug!(
                ?previous,
                ?next,
                reason,
                avg_culled,
                avg_pruned,
                "Terrain horizon adaptive gate transition"
            );
        } else {
            debug!(?previous, ?next, reason, "Terrain horizon adaptive gate transition");
        }
    }

    fn note_fast_sample(&mut self, fast: bool) {
        self.fast_bits = (self.fast_bits << 1) | u64::from(fast);
        self.fast_samples = (self.fast_samples + 1).min(GATE_FAST_WINDOW);
    }

    fn reset_tracking(&mut self) {
        self.reset_eval_and_probe_counters();
        self.last_eye = None;
        self.fast_bits = 0;
        self.fast_samples = 0;
        self.probe_backoff = GATE_PROBE_INTERVAL;
    }

    fn reset_eval_and_probe_counters(&mut self) {
        self.eval.reset();
        self.low_benefit_windows = 0;
        self.cumulative_move_since_probe = 0.0;
        self.suspended_since = None;
        self.warming_frames = 0;
        self.warm_build_posted = false;
    }
}

fn eye_distance(a: D3dxVector3, b: D3dxVector3) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eye(x: f32) -> D3dxVector3 {
        D3dxVector3 { x, y: 0.0, z: 0.0 }
    }

    /// Advances `now` by a tiny, fixed increment per simulated frame so tests exercising
    /// frame-count-based behavior (eval windows, fast-motion, warm timeout) never accidentally
    /// cross a real-time probe threshold.
    fn tick_many(gate: &mut HorizonGate, now: &mut Instant, count: u32, culled: u32, pruned: u32) {
        for frame in 0..count {
            *now += Duration::from_micros(1);
            gate.on_frame(*now, eye(frame as f32), culled, pruned, 64.0);
        }
    }

    fn active_gate() -> HorizonGate {
        let mut gate = HorizonGate::new(true);
        gate.state = GateState::Active { probing: false };
        gate
    }

    #[test]
    fn gate_suspends_after_two_low_benefit_windows() {
        let mut gate = active_gate();
        let mut now = Instant::now();

        tick_many(&mut gate, &mut now, GATE_EVAL_WINDOW * GATE_SUSPEND_WINDOWS, 0, 0);

        assert_eq!(gate.mode(), HorizonGateMode::Suspended);
        assert_eq!(gate.state_code(), 3);
    }

    #[test]
    fn gate_stays_active_with_cull_or_prune_benefit() {
        let mut gate = active_gate();
        let mut now = Instant::now();
        tick_many(
            &mut gate,
            &mut now,
            GATE_EVAL_WINDOW * GATE_SUSPEND_WINDOWS,
            GATE_CULL_FLOOR as u32,
            0,
        );
        assert_eq!(gate.mode(), HorizonGateMode::Active);
        assert_eq!(gate.state, GateState::Active { probing: false });

        let mut gate = active_gate();
        let mut now = Instant::now();
        tick_many(
            &mut gate,
            &mut now,
            GATE_EVAL_WINDOW * GATE_SUSPEND_WINDOWS,
            0,
            GATE_PRUNE_FLOOR as u32,
        );
        assert_eq!(gate.mode(), HorizonGateMode::Active);
        assert_eq!(gate.state, GateState::Active { probing: false });
    }

    #[test]
    fn suspended_probe_uses_backoff_and_movement_trigger_resets_it() {
        let mut gate = active_gate();
        let mut now = Instant::now();
        tick_many(&mut gate, &mut now, GATE_EVAL_WINDOW * GATE_SUSPEND_WINDOWS, 0, 0);
        assert_eq!(gate.state_code(), 3);

        // A large displacement forces an immediate probe regardless of elapsed real time.
        now += Duration::from_millis(1);
        gate.on_frame(now, eye(9999.0), 0, 0, 64.0);
        assert_eq!(gate.state_code(), 2);

        tick_many(&mut gate, &mut now, GATE_WARM_TIMEOUT, 0, 0);
        assert_eq!(gate.state_code(), 3);
        assert_eq!(gate.probe_backoff, GATE_PROBE_INTERVAL * 2);
    }

    #[test]
    fn suspended_probe_uses_real_elapsed_time_not_frame_count() {
        // Regression guard: the probe delay is wall-clock time, not a frame count. At a very high
        // frame rate, tens of thousands of frames can elapse without reaching the real-time probe
        // interval, and the gate must stay suspended throughout.
        let mut gate = active_gate();
        let mut now = Instant::now();
        tick_many(&mut gate, &mut now, GATE_EVAL_WINDOW * GATE_SUSPEND_WINDOWS, 0, 0);
        assert_eq!(gate.state_code(), 3);

        for _ in 0..100_000u32 {
            now += Duration::from_micros(1);
            gate.on_frame(now, eye(0.0), 0, 0, 64.0);
        }
        assert_eq!(
            gate.state_code(),
            3,
            "100ms of high-fps frames must not trip a frame-count probe"
        );

        // Advancing real time past the probe interval (with no further displacement) must trigger
        // the timer probe on the very next frame.
        now += GATE_PROBE_INTERVAL;
        gate.on_frame(now, eye(0.0), 0, 0, 64.0);
        assert_eq!(gate.state_code(), 2);
    }

    #[test]
    fn probe_backoff_doubles_and_caps() {
        let mut gate = HorizonGate::new(true);
        gate.on_context_available();
        let mut now = Instant::now();
        gate.enter_low_benefit_suspended(now, "test", None, false);

        for _ in 0..8 {
            gate.enter_warming("test");
            tick_many(&mut gate, &mut now, GATE_WARM_TIMEOUT, 0, 0);
        }

        assert_eq!(gate.probe_backoff, GATE_PROBE_BACKOFF_CAP);
    }

    #[test]
    fn fast_motion_enters_and_exits_on_fractions() {
        let mut gate = active_gate();
        let mut now = Instant::now();
        for frame in 0..GATE_FAST_WINDOW {
            now += Duration::from_micros(1);
            gate.on_frame(now, eye(frame as f32 * 128.0), 8, 0, 64.0);
        }
        assert_eq!(gate.state_code(), 4);

        for frame in 0..GATE_FAST_WINDOW {
            now += Duration::from_micros(1);
            gate.on_frame(now, eye(10_000.0 + frame as f32), 0, 0, 64.0);
        }
        assert_eq!(gate.state_code(), 2);
    }

    #[test]
    fn warming_adoption_enters_probing_active_and_success_clears_probe() {
        let mut gate = HorizonGate::new(true);
        gate.on_context_available();
        assert_eq!(gate.state_code(), 2);

        gate.on_warm_adopted();
        assert_eq!(gate.state_code(), 1);
        assert_eq!(gate.state, GateState::Active { probing: true });
        let mut now = Instant::now();
        tick_many(&mut gate, &mut now, GATE_EVAL_WINDOW, GATE_CULL_FLOOR as u32, 0);
        assert_eq!(gate.state_code(), 1);
        assert_eq!(gate.mode(), HorizonGateMode::Active);
        assert_eq!(gate.state, GateState::Active { probing: false });
    }

    #[test]
    fn gate_disabled_is_active() {
        let mut gate = HorizonGate::new(false);
        gate.on_context_available();
        let mut now = Instant::now();
        tick_many(&mut gate, &mut now, GATE_EVAL_WINDOW * GATE_SUSPEND_WINDOWS, 0, 0);

        assert_eq!(gate.mode(), HorizonGateMode::Active);
        assert_eq!(gate.state_code(), 1);
    }
}
