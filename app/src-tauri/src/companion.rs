//! IDE 状态跃迁沿语音与智能防打扰自主陪伴语音节流阀。

use std::collections::HashMap;

use crate::scheduler::SchedulerEvent;
use crate::telemetry::{AgentActivityKind, TelemetryStatus, UnifiedState};

pub struct IdeVoiceTracker {
    initialized: bool,
    prev_status: TelemetryStatus,
    prev_activity: AgentActivityKind,
    prev_context_high: bool,
    cooldowns: HashMap<&'static str, i64>,
    work_start_toggle: bool,
    done_toggle: bool,
}

impl IdeVoiceTracker {
    const APPROVAL_COOLDOWN_MS: i64 = 45_000;
    const ERROR_COOLDOWN_MS: i64 = 60_000;
    const CONTEXT_HIGH_COOLDOWN_MS: i64 = 180_000;
    const DONE_COOLDOWN_MS: i64 = 60_000;
    const WORK_START_COOLDOWN_MS: i64 = 120_000;

    pub fn new() -> Self {
        Self {
            initialized: false,
            prev_status: TelemetryStatus::Offline,
            prev_activity: AgentActivityKind::ProcessOffline,
            prev_context_high: false,
            cooldowns: HashMap::new(),
            work_start_toggle: false,
            done_toggle: false,
        }
    }

    fn try_claim(&mut self, key: &'static str, cooldown_ms: i64, now_ms: i64) -> bool {
        if let Some(&last) = self.cooldowns.get(key) {
            if now_ms >= last && now_ms - last < cooldown_ms {
                return false;
            }
        }
        self.cooldowns.insert(key, now_ms);
        true
    }

    pub fn evaluate(
        &mut self,
        state: &UnifiedState,
        events: &[SchedulerEvent],
        now_ms: i64,
    ) -> Option<&'static str> {
        if !self.initialized {
            self.initialized = true;
            self.prev_status = state.status;
            self.prev_activity = state.activity;
            self.prev_context_high = state.alerts.context_high;
            return None;
        }

        let approval_edge = state.status == TelemetryStatus::WaitingApproval
            && self.prev_status != TelemetryStatus::WaitingApproval;
        let error_edge = (state.status == TelemetryStatus::Error
            && self.prev_status != TelemetryStatus::Error)
            || (state.activity == AgentActivityKind::TerminalError
                && self.prev_activity != AgentActivityKind::TerminalError);
        let context_high_edge = state.alerts.context_high && !self.prev_context_high;
        let done_edge = events.iter().any(|ev| {
            matches!(
                ev,
                SchedulerEvent::AgentScene(sig) if sig.activity == AgentActivityKind::TaskCompleted
            )
        }) || (state.activity == AgentActivityKind::TaskCompleted
            && self.prev_activity != AgentActivityKind::TaskCompleted);
        let work_start_edge =
            state.status.is_generating() && !self.prev_status.is_generating();

        self.prev_status = state.status;
        self.prev_activity = state.activity;
        self.prev_context_high = state.alerts.context_high;

        if approval_edge && self.try_claim("approval", Self::APPROVAL_COOLDOWN_MS, now_ms) {
            return Some("approval");
        }
        if error_edge && self.try_claim("error", Self::ERROR_COOLDOWN_MS, now_ms) {
            return Some("error");
        }
        if context_high_edge
            && self.try_claim("context_high", Self::CONTEXT_HIGH_COOLDOWN_MS, now_ms)
        {
            return Some("context_high");
        }
        if done_edge && self.try_claim("done", Self::DONE_COOLDOWN_MS, now_ms) {
            self.done_toggle = !self.done_toggle;
            return Some(if self.done_toggle { "done" } else { "done_2" });
        }
        if work_start_edge && self.try_claim("work_start", Self::WORK_START_COOLDOWN_MS, now_ms) {
            self.work_start_toggle = !self.work_start_toggle;
            return Some(if self.work_start_toggle {
                "work_start"
            } else {
                "work_start_2"
            });
        }
        None
    }
}

#[derive(Clone, Copy)]
pub struct CompanionTickContext {
    pub now_ms: i64,
    pub mode: &'static str,
    pub muted_or_tts_off: bool,
    pub dnd_or_fullscreen: bool,
    pub idle_input_ms: Option<u64>,
    pub agent_busy: bool,
    pub dragging: bool,
    pub last_any_voice_ms: i64,
    pub local_minute_of_day: i32,
}

pub struct CompanionVoiceThrottle {
    started_ms: i64,
    last_ambient_ms: i64,
    fired_in_hour: Vec<i64>,
    next_day_idx: usize,
    last_scene: Option<&'static str>,
}

impl CompanionVoiceThrottle {
    pub const ACTIVE_TYPING_DEFER_MS: u64 = 8_000;
    pub const USER_AFK_MAX_IDLE_MS: u64 = 300_000;

    pub fn new(now_ms: i64) -> Self {
        Self {
            started_ms: now_ms,
            last_ambient_ms: 0,
            fired_in_hour: Vec::new(),
            next_day_idx: 0,
            last_scene: None,
        }
    }

    pub fn tick(&mut self, ctx: CompanionTickContext) -> Option<&'static str> {
        let (interval_ms, global_cd_ms, hourly_cap) = match ctx.mode {
            "quiet" => return None,
            "lively" => (9 * 60_000i64, 90_000i64, 6usize),
            _ => (20 * 60_000i64, 180_000i64, 3usize),
        };

        if ctx.muted_or_tts_off
            || ctx.dnd_or_fullscreen
            || ctx.agent_busy
            || ctx.dragging
        {
            return None;
        }

        if let Some(idle_ms) = ctx.idle_input_ms {
            if idle_ms < Self::ACTIVE_TYPING_DEFER_MS || idle_ms > Self::USER_AFK_MAX_IDLE_MS {
                return None;
            }
        }

        let warmup_ms = (interval_ms / 2).min(5 * 60_000);
        if ctx.now_ms - self.started_ms < warmup_ms {
            return None;
        }

        if ctx.last_any_voice_ms > 0 && ctx.now_ms - ctx.last_any_voice_ms < global_cd_ms {
            return None;
        }

        if self.last_ambient_ms > 0 && ctx.now_ms - self.last_ambient_ms < interval_ms {
            return None;
        }

        self.fired_in_hour
            .retain(|&ts| ctx.now_ms >= ts && ctx.now_ms - ts < 3_600_000);
        if self.fired_in_hour.len() >= hourly_cap {
            return None;
        }

        let is_late_night =
            ctx.local_minute_of_day >= 22 * 60 + 30 || ctx.local_minute_of_day < 4 * 60 + 30;
        let scene = if is_late_night && self.last_scene != Some("ambient_4") {
            "ambient_4"
        } else {
            const DAY_SCENES: &[&str] = &["ambient_1", "ambient_2", "ambient_3"];
            let s = DAY_SCENES[self.next_day_idx % DAY_SCENES.len()];
            self.next_day_idx = (self.next_day_idx + 1) % DAY_SCENES.len();
            s
        };

        self.last_ambient_ms = ctx.now_ms;
        self.fired_in_hour.push(ctx.now_ms);
        self.last_scene = Some(scene);
        Some(scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::AgentSceneSignal;

    #[test]
    fn ide_voice_tracker_fires_on_transitions_and_respects_cooldowns() {
        let mut tracker = IdeVoiceTracker::new();
        let mut st = UnifiedState::offline("antigravity", 1_000);
        // 冷启动首帧静默
        assert_eq!(tracker.evaluate(&st, &[], 1_000), None);

        // 进入工作：work_start
        st.status = TelemetryStatus::Streaming;
        st.activity = AgentActivityKind::CodeStreaming;
        assert_eq!(tracker.evaluate(&st, &[], 2_000), Some("work_start"));

        // 冷却期内再次进入工作：静默
        st.status = TelemetryStatus::Idle;
        st.activity = AgentActivityKind::Idle;
        assert_eq!(tracker.evaluate(&st, &[], 3_000), None);
        st.status = TelemetryStatus::Streaming;
        st.activity = AgentActivityKind::CodeStreaming;
        assert_eq!(tracker.evaluate(&st, &[], 10_000), None);

        // 超过 120s 冷却：交替 work_start_2
        st.status = TelemetryStatus::Idle;
        assert_eq!(tracker.evaluate(&st, &[], 125_000), None);
        st.status = TelemetryStatus::Streaming;
        assert_eq!(tracker.evaluate(&st, &[], 126_000), Some("work_start_2"));

        // 审批与报错独立冷却
        st.status = TelemetryStatus::WaitingApproval;
        assert_eq!(tracker.evaluate(&st, &[], 127_000), Some("approval"));
        st.status = TelemetryStatus::Error;
        assert_eq!(tracker.evaluate(&st, &[], 128_000), Some("error"));

        // 上下文 80% 告警
        st.alerts.context_high = true;
        assert_eq!(tracker.evaluate(&st, &[], 129_000), Some("context_high"));

        // 任务完成交替 done / done_2
        st.status = TelemetryStatus::Idle;
        st.activity = AgentActivityKind::TaskCompleted;
        let done_ev = [SchedulerEvent::AgentScene(AgentSceneSignal {
            status: TelemetryStatus::Idle,
            activity: AgentActivityKind::TaskCompleted,
            detail: None,
            bubble: None,
        })];
        assert_eq!(tracker.evaluate(&st, &done_ev, 130_000), Some("done"));
        st.activity = AgentActivityKind::Idle;
        assert_eq!(tracker.evaluate(&st, &[], 131_000), None);
        st.activity = AgentActivityKind::TaskCompleted;
        assert_eq!(tracker.evaluate(&st, &done_ev, 195_000), Some("done_2"));
    }

    #[test]
    fn companion_throttle_respects_dnd_typing_busy_and_budgets() {
        let mut th = CompanionVoiceThrottle::new(0);
        let base = CompanionTickContext {
            now_ms: 10 * 60_000,
            mode: "balanced",
            muted_or_tts_off: false,
            dnd_or_fullscreen: false,
            idle_input_ms: Some(15_000),
            agent_busy: false,
            dragging: false,
            last_any_voice_ms: 0,
            local_minute_of_day: 14 * 60,
        };

        // quiet 永久静默
        assert_eq!(
            th.tick(CompanionTickContext { mode: "quiet", ..base }),
            None
        );
        // 全屏免打扰静默
        assert_eq!(
            th.tick(CompanionTickContext {
                dnd_or_fullscreen: true,
                ..base
            }),
            None
        );
        // 高频键鼠输入 (<8s) 避让
        assert_eq!(
            th.tick(CompanionTickContext {
                idle_input_ms: Some(3_000),
                ..base
            }),
            None
        );
        // 久未操作 (>5min) 离开静默
        assert_eq!(
            th.tick(CompanionTickContext {
                idle_input_ms: Some(400_000),
                ..base
            }),
            None
        );
        // Agent 忙碌顺延，闲下来后立刻补播
        assert_eq!(
            th.tick(CompanionTickContext {
                agent_busy: true,
                ..base
            }),
            None
        );
        assert_eq!(th.tick(base), Some("ambient_1"));

        // 20 分钟间隔内不重复播
        assert_eq!(
            th.tick(CompanionTickContext {
                now_ms: 25 * 60_000,
                ..base
            }),
            None
        );
        // 深夜切 ambient_4
        assert_eq!(
            th.tick(CompanionTickContext {
                now_ms: 35 * 60_000,
                local_minute_of_day: 23 * 60 + 15,
                ..base
            }),
            Some("ambient_4")
        );
    }
}
