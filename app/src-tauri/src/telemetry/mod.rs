//! v1.2 Agent 遥测层：Adapter → UnifiedState → 桌宠表现层。
//!
//! 设计约束（`docs/v1_contract.md` §9）：
//! - 采集全部在 Rust 后台线程，前端只消费 `telemetry` 事件；
//! - `TelemetryAdapter` 是唯一扩展点，未来接 Claude Code / Cursor 只需新增实现；
//! - `UnifiedState` 与 `app/src/shared/ipc.ts` 严格 camelCase 同构；
//! - 只有序列化结果相对上次有变化才推送（忽略 `updatedAtMs` 抖动）；
//! - 429 时额度轮询指数退避到 120s–300s，绝不继续高频自伤。

pub mod antigravity;

use serde::{Deserialize, Serialize};

use crate::scheduler::SchedulerEvent;
use crate::stats::now_ms;

/* ------------------------------------------------------------------ */
/* UnifiedState（与 ipc.ts 同构）                                      */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryStatus {
    #[default]
    Offline,
    Idle,
    Streaming,
    ToolRunning,
    WaitingApproval,
    Error,
}

/// v2.1：Antigravity 全生命周期细粒度场景标识（与前端 `AgentActivityKind` 同构）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityKind {
    ProcessOffline,
    ServiceConnecting,
    ServiceReconnecting,
    SessionInit,
    Idle,
    TtftWaiting,
    PlanningMode,
    DeepReasoning,
    ResearchLookup,
    CodeStreaming,
    CodeDiffing,
    TerminalRunning,
    BuildLongRunning,
    TerminalError,
    BrowserRunning,
    McpToolCalling,
    WorkflowRunning,
    ApprovalTerminal,
    ApprovalOutsideDir,
    ArtifactReviewReady,
    TaskCompleted,
    ContextDeltaSpike,
    ContextWarning80,
    QuotaLowWarning,
    QuotaExhausted,
    ApiAuthRateError,
}

impl Default for AgentActivityKind {
    fn default() -> Self {
        Self::ProcessOffline
    }
}

impl AgentActivityKind {
    /// 是否属于持续锁定态：锁定期间调度器强制循环该段，直至 Agent 切换下一个动作或被人类直接交互抢占。
    pub fn is_held(self) -> bool {
        matches!(
            self,
            Self::ProcessOffline
                | Self::ServiceReconnecting
                | Self::PlanningMode
                | Self::DeepReasoning
                | Self::ResearchLookup
                | Self::CodeStreaming
                | Self::CodeDiffing
                | Self::TerminalRunning
                | Self::BuildLongRunning
                | Self::BrowserRunning
                | Self::McpToolCalling
                | Self::WorkflowRunning
                | Self::ApprovalTerminal
                | Self::ApprovalOutsideDir
                | Self::ArtifactReviewReady
                | Self::QuotaExhausted
                | Self::ApiAuthRateError
        )
    }

    /// 5 级仲裁优先级（1 最高、5 最低），对齐工程落地规范。
    pub fn tier(self) -> u8 {
        match self {
            Self::ApprovalTerminal
            | Self::ApprovalOutsideDir
            | Self::ApiAuthRateError
            | Self::QuotaExhausted => 1,
            Self::ArtifactReviewReady
            | Self::TaskCompleted
            | Self::ContextWarning80
            | Self::QuotaLowWarning
            | Self::ContextDeltaSpike
            | Self::TerminalError => 2,
            Self::BrowserRunning
            | Self::TerminalRunning
            | Self::BuildLongRunning
            | Self::McpToolCalling
            | Self::WorkflowRunning
            | Self::ResearchLookup => 3,
            Self::CodeStreaming
            | Self::CodeDiffing
            | Self::PlanningMode
            | Self::DeepReasoning
            | Self::TtftWaiting => 4,
            Self::ServiceConnecting
            | Self::ServiceReconnecting
            | Self::SessionInit
            | Self::ProcessOffline
            | Self::Idle => 5,
        }
    }

    /// 是否属于主动工作态（L1-L4 中的工作/阻塞/推理类动作，排除一次性里程碑与阈值弹窗）。
    pub fn is_active_work(self) -> bool {
        matches!(
            self,
            Self::TtftWaiting
                | Self::PlanningMode
                | Self::DeepReasoning
                | Self::ResearchLookup
                | Self::CodeStreaming
                | Self::CodeDiffing
                | Self::TerminalRunning
                | Self::BuildLongRunning
                | Self::TerminalError
                | Self::BrowserRunning
                | Self::McpToolCalling
                | Self::WorkflowRunning
                | Self::ApprovalTerminal
                | Self::ApprovalOutsideDir
                | Self::ArtifactReviewReady
                | Self::ApiAuthRateError
        )
    }

    pub fn from_status(status: TelemetryStatus) -> Self {
        match status {
            TelemetryStatus::Offline => Self::ProcessOffline,
            TelemetryStatus::Idle => Self::Idle,
            TelemetryStatus::Streaming => Self::CodeStreaming,
            TelemetryStatus::ToolRunning => Self::TerminalRunning,
            TelemetryStatus::WaitingApproval => Self::ApprovalTerminal,
            TelemetryStatus::Error => Self::ApiAuthRateError,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSceneSignal {
    pub status: TelemetryStatus,
    pub activity: AgentActivityKind,
    pub detail: Option<String>,
    pub bubble: Option<String>,
}

impl TelemetryStatus {
    pub fn is_agent_active(self) -> bool {
        matches!(
            self,
            TelemetryStatus::Streaming
                | TelemetryStatus::ToolRunning
                | TelemetryStatus::WaitingApproval
                | TelemetryStatus::Error
        )
    }

    pub fn is_generating(self) -> bool {
        matches!(
            self,
            TelemetryStatus::Streaming | TelemetryStatus::ToolRunning
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryConfidence {
    Exact,
    Estimated,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryErrorKind {
    Auth,
    Region,
    RateLimit,
    Network,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryCapabilities {
    pub has_quota: bool,
    pub has_multi_pool: bool,
    pub has_realtime_speed: bool,
    pub has_ttft: bool,
    pub has_context: bool,
    pub has_approval: bool,
}

impl Default for TelemetryCapabilities {
    fn default() -> Self {
        Self {
            has_quota: false,
            has_multi_pool: false,
            has_realtime_speed: false,
            has_ttft: false,
            has_context: false,
            has_approval: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryModelInfo {
    pub name: Option<String>,
    pub context_limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryQuotaPool {
    pub id: String,
    pub label: String,
    pub remaining_pct: f64,
    pub reset_at_ms: Option<i64>,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryQuotaInfo {
    pub remaining_pct: Option<f64>,
    pub reset_at_ms: Option<i64>,
    pub pools: Vec<TelemetryQuotaPool>,
    pub confidence: TelemetryConfidence,
}

impl Default for TelemetryQuotaInfo {
    fn default() -> Self {
        Self {
            remaining_pct: None,
            reset_at_ms: None,
            pools: Vec::new(),
            confidence: TelemetryConfidence::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySpeedInfo {
    pub tokens_per_sec: Option<f64>,
    pub ttft_ms: Option<u64>,
    pub confidence: TelemetryConfidence,
}

impl Default for TelemetrySpeedInfo {
    fn default() -> Self {
        Self {
            tokens_per_sec: None,
            ttft_ms: None,
            confidence: TelemetryConfidence::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryContextInfo {
    pub used_tokens: Option<u64>,
    pub limit_tokens: Option<u64>,
    pub used_pct: Option<f64>,
    pub delta_tokens: Option<u64>,
    pub confidence: TelemetryConfidence,
}

impl Default for TelemetryContextInfo {
    fn default() -> Self {
        Self {
            used_tokens: None,
            limit_tokens: None,
            used_pct: None,
            delta_tokens: None,
            confidence: TelemetryConfidence::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryError {
    pub code: Option<i64>,
    pub kind: TelemetryErrorKind,
    pub message: String,
}

impl TelemetryError {
    pub fn new(code: Option<u16>, kind: TelemetryErrorKind, message: impl Into<String>) -> Self {
        Self {
            code: code.map(i64::from),
            kind,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryAlerts {
    pub context_high: bool,
    pub quota_low: bool,
    pub waiting_approval: bool,
    pub has_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedState {
    pub source: String,
    pub available_sources: Vec<String>,
    pub status: TelemetryStatus,
    pub status_since_ms: i64,
    pub active_step_label: Option<String>,
    pub activity: AgentActivityKind,
    pub active_agents: u32,
    pub completed_agents: u32,
    pub workspace_name: Option<String>,
    pub step_detail: Option<String>,
    pub capabilities: TelemetryCapabilities,
    pub model: TelemetryModelInfo,
    pub quota: TelemetryQuotaInfo,
    pub speed: TelemetrySpeedInfo,
    pub context: TelemetryContextInfo,
    pub error: Option<TelemetryError>,
    pub alerts: TelemetryAlerts,
    pub updated_at_ms: i64,
    pub stale: bool,
}

impl UnifiedState {
    pub fn offline(source: &str, now: i64) -> Self {
        Self {
            source: source.to_string(),
            available_sources: vec![source.to_string()],
            status: TelemetryStatus::Offline,
            status_since_ms: now,
            active_step_label: None,
            activity: AgentActivityKind::ProcessOffline,
            active_agents: 0,
            completed_agents: 0,
            workspace_name: None,
            step_detail: None,
            capabilities: TelemetryCapabilities::default(),
            model: TelemetryModelInfo::default(),
            quota: TelemetryQuotaInfo::default(),
            speed: TelemetrySpeedInfo::default(),
            context: TelemetryContextInfo::default(),
            error: None,
            alerts: TelemetryAlerts::default(),
            updated_at_ms: now,
            stale: false,
        }
    }

    /// Diff 去重键：忽略时间戳与 stale，只比较会改变 UI 的内容。
    pub fn diff_key(&self) -> String {
        let mut probe = self.clone();
        probe.updated_at_ms = 0;
        probe.stale = false;
        serde_json::to_string(&probe).unwrap_or_default()
    }
}

/* ------------------------------------------------------------------ */
/* Adapter 接口                                                        */
/* ------------------------------------------------------------------ */

/// 一次状态采样的结果（额度由独立低频接口负责）。
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub status: TelemetryStatus,
    pub active_step_label: Option<String>,
    pub activity: AgentActivityKind,
    pub active_agents: u32,
    pub workspace_name: Option<String>,
    pub step_detail: Option<String>,
    pub model: TelemetryModelInfo,
    pub speed: TelemetrySpeedInfo,
    pub context: TelemetryContextInfo,
    /// 当前活跃会话 id；用于识别「Turn 结束」与跨会话 Delta 重置。
    pub trajectory_id: Option<String>,
    pub error: Option<TelemetryError>,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            status: TelemetryStatus::Offline,
            active_step_label: None,
            activity: AgentActivityKind::ProcessOffline,
            active_agents: 0,
            workspace_name: None,
            step_detail: None,
            model: TelemetryModelInfo::default(),
            speed: TelemetrySpeedInfo::default(),
            context: TelemetryContextInfo::default(),
            trajectory_id: None,
            error: None,
        }
    }
}

/// 未来接入其他工具只需实现该 trait 并注册到 `TelemetryManager`。
pub trait TelemetryAdapter: Send {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> TelemetryCapabilities;
    fn poll_status(&mut self, now_ms: i64) -> Result<StatusSnapshot, TelemetryError>;
    fn poll_quota(&mut self, now_ms: i64) -> Result<TelemetryQuotaInfo, TelemetryError>;
}

/* ------------------------------------------------------------------ */
/* Manager：分级轮询、Diff 推送、告警联动                              */
/* ------------------------------------------------------------------ */

const STATUS_INTERVAL_MS: i64 = 1_000;
const ACTIVE_INTERVAL_MS: i64 = 600;
const STREAMING_INTERVAL_MS: i64 = 300;
const QUOTA_INTERVAL_MS: i64 = 45_000;
const QUOTA_BACKOFF_MIN_MS: i64 = 120_000;
const QUOTA_BACKOFF_MAX_MS: i64 = 300_000;
const STALE_AFTER_MS: i64 = 10_000;
const TRANSIENT_ERROR_THRESHOLD: u32 = 3;
const RECONNECT_GRACE_MS: i64 = 20_000;
const LONG_BUILD_THRESHOLD_MS: i64 = 15_000;
const TASK_COMPLETED_MIN_WORK_MS: i64 = 5_000;
const CONTEXT_BUBBLE_MIN_DELTA_TOKENS: u64 = 10_000;
const CONTEXT_BUBBLE_COOLDOWN_MS: i64 = 3_000;
const TRANSIENT_SCENE_TTL_MS: i64 = 2_600;
pub const TICK_INTERVAL_MS: u64 = 150;

fn status_poll_interval(status: TelemetryStatus) -> i64 {
    match status {
        TelemetryStatus::Streaming => STREAMING_INTERVAL_MS,
        TelemetryStatus::ToolRunning | TelemetryStatus::WaitingApproval => ACTIVE_INTERVAL_MS,
        TelemetryStatus::Offline | TelemetryStatus::Idle | TelemetryStatus::Error => {
            STATUS_INTERVAL_MS
        }
    }
}

pub struct TickOutcome {
    pub state: UnifiedState,
    pub changed: bool,
    pub scheduler_events: Vec<SchedulerEvent>,
}

pub struct RefreshOutcome {
    pub state: UnifiedState,
    pub changed: bool,
    pub scheduler_events: Vec<SchedulerEvent>,
}

/// 手动强刷与后台 tick 共用的脏标记。
pub struct TelemetryManager {
    adapter: Box<dyn TelemetryAdapter>,
    state: UnifiedState,
    last_emit_key: String,
    last_success_ms: i64,
    next_status_ms: i64,
    next_quota_ms: i64,
    quota_backoff_ms: i64,
    quota_forced: bool,
    last_alerts: TelemetryAlerts,
    published_once: bool,
    force_quota_after_turn: bool,
    last_trajectory: Option<String>,
    consecutive_transient_errors: u32,
    had_connection: bool,
    work_started_ms: Option<i64>,
    terminal_started_ms: Option<i64>,
    peak_swarm_agents: u32,
    last_emitted_activity: Option<AgentActivityKind>,
    last_emitted_web_search: bool,
    last_bubbled_used_tokens: Option<u64>,
    last_context_bubble_ms: i64,
    transient_scene: Option<(AgentActivityKind, i64, Option<String>)>,
}

impl TelemetryManager {
    pub fn new(adapter: Box<dyn TelemetryAdapter>) -> Self {
        let now = now_ms();
        let state = UnifiedState::offline(adapter.id(), now);
        Self {
            adapter,
            state,
            last_emit_key: String::new(),
            last_success_ms: 0,
            next_status_ms: 0,
            next_quota_ms: 0,
            quota_backoff_ms: QUOTA_BACKOFF_MIN_MS,
            quota_forced: true,
            last_alerts: TelemetryAlerts::default(),
            published_once: false,
            force_quota_after_turn: false,
            last_trajectory: None,
            consecutive_transient_errors: 0,
            had_connection: false,
            work_started_ms: None,
            terminal_started_ms: None,
            peak_swarm_agents: 0,
            last_emitted_activity: None,
            last_emitted_web_search: false,
            last_bubbled_used_tokens: None,
            last_context_bubble_ms: 0,
            transient_scene: None,
        }
    }

    pub fn snapshot(&mut self) -> UnifiedState {
        let now = now_ms();
        let mut state = self.state.clone();
        state.stale = state.status != TelemetryStatus::Offline
            && self.last_success_ms > 0
            && now - self.last_success_ms > STALE_AFTER_MS;
        state
    }

    pub fn available_sources(&self) -> Vec<String> {
        vec![self.adapter.id().to_string()]
    }

    pub fn source(&self) -> &'static str {
        self.adapter.id()
    }

    /// 手动强刷：状态 + 额度立即各拉一次。失败也返回带 error 的快照。
    pub fn refresh(&mut self) -> RefreshOutcome {
        let now = now_ms();
        let mut events = Vec::new();

        match self.adapter.poll_status(now) {
            Ok(snapshot) => self.apply_snapshot(snapshot, now, &mut events),
            Err(err) => self.apply_error(err, now, &mut events),
        }
        self.next_status_ms = now + status_poll_interval(self.state.status);

        match self.adapter.poll_quota(now) {
            Ok(quota) => {
                self.apply_quota(quota, now);
                self.quota_backoff_ms = QUOTA_BACKOFF_MIN_MS;
                self.next_quota_ms = now + QUOTA_INTERVAL_MS;
                if let Some(err) = &self.state.error {
                    if matches!(
                        err.kind,
                        TelemetryErrorKind::RateLimit | TelemetryErrorKind::Network
                    ) {
                        self.state.error = None;
                    }
                }
                if let Some(pct) = self.state.quota.remaining_pct {
                    events.push(SchedulerEvent::QuotaBarometer {
                        remaining_pct: pct,
                        announce: self.state.status == TelemetryStatus::Idle,
                    });
                }
            }
            Err(err) => self.apply_quota_error(err, now, &mut events),
        }

        self.synthesize_scene(now, &mut events);
        self.recalculate_alerts();
        let mut threshold_events = self.drain_threshold_alerts();
        events.append(&mut threshold_events);
        self.quota_forced = false;
        let changed = self.update_emit_key();
        RefreshOutcome {
            state: self.snapshot(),
            changed,
            scheduler_events: events,
        }
    }

    /// 150ms 心跳；内部按 300ms / 600ms / 1s / 45s 分级采样。
    pub fn tick(&mut self, now: i64) -> TickOutcome {
        let mut events = Vec::new();

        if now >= self.next_status_ms {
            let was_active = self.state.status.is_agent_active();
            match self.adapter.poll_status(now) {
                Ok(snapshot) => {
                    let ended_turn = was_active && !snapshot.status.is_agent_active();
                    let switched_trajectory = snapshot.trajectory_id != self.last_trajectory;
                    self.apply_snapshot(snapshot, now, &mut events);
                    if ended_turn || switched_trajectory {
                        self.force_quota_after_turn = true;
                    }
                }
                Err(err) => self.apply_error(err, now, &mut events),
            }
            self.next_status_ms = now + status_poll_interval(self.state.status);
            if self.force_quota_after_turn {
                self.force_quota_after_turn = false;
                self.next_quota_ms = 0;
            }
        }

        if self.quota_forced || now >= self.next_quota_ms {
            let prev_pct = self.state.quota.remaining_pct;
            match self.adapter.poll_quota(now) {
                Ok(quota) => {
                    self.apply_quota(quota, now);
                    self.quota_backoff_ms = QUOTA_BACKOFF_MIN_MS;
                    self.next_quota_ms = now + QUOTA_INTERVAL_MS;
                    if let Some(err) = &self.state.error {
                        if matches!(
                            err.kind,
                            TelemetryErrorKind::RateLimit | TelemetryErrorKind::Network
                        ) {
                            self.state.error = None;
                        }
                    }
                    if let Some(pct) = self.state.quota.remaining_pct {
                        let recharged = prev_pct.is_some_and(|old| pct - old >= 30.0);
                        events.push(SchedulerEvent::QuotaBarometer {
                            remaining_pct: pct,
                            announce: recharged && self.state.status == TelemetryStatus::Idle,
                        });
                    }
                }
                Err(err) => self.apply_quota_error(err, now, &mut events),
            }
            self.quota_forced = false;
            self.recalculate_alerts();
        }

        self.synthesize_scene(now, &mut events);
        self.recalculate_alerts();
        let mut threshold_events = self.drain_threshold_alerts();
        events.append(&mut threshold_events);
        let changed = self.update_emit_key();
        TickOutcome {
            state: self.snapshot(),
            changed,
            scheduler_events: events,
        }
    }

    fn apply_snapshot(
        &mut self,
        snapshot: StatusSnapshot,
        now: i64,
        events: &mut Vec<SchedulerEvent>,
    ) {
        let prev_status = self.state.status;
        let prev_trajectory = self.last_trajectory.clone();
        let was_active_work = prev_status.is_generating();
        let work_elapsed = self
            .work_started_ms
            .map(|started| now.saturating_sub(started))
            .unwrap_or(0);

        self.consecutive_transient_errors = 0;
        if snapshot.status != TelemetryStatus::Offline {
            self.had_connection = true;
        }

        if snapshot.status != self.state.status {
            self.state.status = snapshot.status;
            self.state.status_since_ms = now;
        }

        if snapshot.status == TelemetryStatus::Offline {
            self.work_started_ms = None;
            self.terminal_started_ms = None;
            self.peak_swarm_agents = 0;
            self.last_bubbled_used_tokens = None;
        } else if snapshot.status.is_generating() {
            if self.work_started_ms.is_none() {
                self.work_started_ms = Some(now);
            }
        } else if snapshot.status == TelemetryStatus::Idle {
            self.work_started_ms = None;
            self.terminal_started_ms = None;
        }

        if snapshot.activity == AgentActivityKind::TerminalRunning {
            if self.terminal_started_ms.is_none() {
                self.terminal_started_ms = Some(now);
            }
        } else if snapshot.activity != AgentActivityKind::BuildLongRunning {
            self.terminal_started_ms = None;
        }

        if snapshot.active_agents >= 2 {
            self.peak_swarm_agents = self.peak_swarm_agents.max(snapshot.active_agents);
            self.state.completed_agents = self
                .peak_swarm_agents
                .saturating_sub(snapshot.active_agents);
        } else {
            self.peak_swarm_agents = 0;
            self.state.completed_agents = 0;
        }

        let switched_traj = snapshot.trajectory_id.is_some()
            && prev_trajectory.is_some()
            && snapshot.trajectory_id != prev_trajectory;
        if switched_traj {
            self.last_bubbled_used_tokens = None;
        }

        self.last_trajectory = snapshot.trajectory_id.clone();
        self.state.active_step_label = snapshot.active_step_label;
        self.state.activity = snapshot.activity;
        self.state.active_agents = snapshot.active_agents;
        self.state.workspace_name = snapshot.workspace_name.clone();
        self.state.step_detail = snapshot.step_detail.clone();
        self.state.model = snapshot.model;
        self.state.speed = snapshot.speed;
        self.state.context = snapshot.context;
        self.state.error = snapshot.error;
        self.state.stale = false;
        self.state.updated_at_ms = now;
        self.last_success_ms = now;
        self.state.capabilities = self.adapter.capabilities();

        // 瞬态生命周期事件（只在上升沿触发短暂覆写，不打断真正运行中动作）
        if prev_status == TelemetryStatus::Offline && self.state.status != TelemetryStatus::Offline {
            self.transient_scene = Some((
                AgentActivityKind::ServiceConnecting,
                now + TRANSIENT_SCENE_TTL_MS,
                self.state.model.name.clone(),
            ));
        } else if self.state.status == TelemetryStatus::Idle
            && was_active_work
            && work_elapsed >= TASK_COMPLETED_MIN_WORK_MS
            && self.state.error.is_none()
        {
            self.transient_scene = Some((
                AgentActivityKind::TaskCompleted,
                now + TRANSIENT_SCENE_TTL_MS,
                None,
            ));
        } else if self.state.status == TelemetryStatus::Idle && switched_traj {
            self.transient_scene = Some((
                AgentActivityKind::SessionInit,
                now + TRANSIENT_SCENE_TTL_MS,
                self.state
                    .workspace_name
                    .clone()
                    .or_else(|| self.state.model.name.clone()),
            ));
        }

        if let (Some(delta), Some(used)) = (
            self.state.context.delta_tokens,
            self.state.context.used_tokens,
        ) {
            let is_new_used = self.last_bubbled_used_tokens != Some(used);
            let cooled =
                now.saturating_sub(self.last_context_bubble_ms) >= CONTEXT_BUBBLE_COOLDOWN_MS;
            if delta >= CONTEXT_BUBBLE_MIN_DELTA_TOKENS && is_new_used && cooled {
                self.last_bubbled_used_tokens = Some(used);
                self.last_context_bubble_ms = now;
                let k_text = if delta >= 100_000 {
                    format!("{}K", (delta + 500) / 1000)
                } else {
                    let tenths = (delta + 50) / 100;
                    if tenths % 10 == 0 {
                        format!("{}K", tenths / 10)
                    } else {
                        format!("{}.{}K", tenths / 10, tenths % 10)
                    }
                };
                let pct_suffix = self
                    .state
                    .context
                    .used_pct
                    .map(|pct| format!("（当前 {}%）", pct.round() as u64))
                    .unwrap_or_default();
                events.push(SchedulerEvent::AgentBubble {
                    text: format!("本步上下文 +{k_text} Tokens{pct_suffix}"),
                    ttl_ms: 4_200,
                });
            }
        }
    }

    fn apply_quota(&mut self, quota: TelemetryQuotaInfo, now: i64) {
        self.state.quota = quota;
        self.state.updated_at_ms = now;
        self.last_success_ms = now;
        self.state.stale = false;
    }

    fn apply_error(&mut self, err: TelemetryError, now: i64, _events: &mut Vec<SchedulerEvent>) {
        let hard_error = matches!(
            err.kind,
            TelemetryErrorKind::Auth | TelemetryErrorKind::Region | TelemetryErrorKind::RateLimit
        );
        self.state.updated_at_ms = now;
        if hard_error {
            self.consecutive_transient_errors = 0;
            self.state.error = Some(err);
            self.state.stale = true;
            self.state.activity = AgentActivityKind::ApiAuthRateError;
            if self.state.status != TelemetryStatus::Error {
                self.state.status = TelemetryStatus::Error;
                self.state.status_since_ms = now;
            }
            return;
        }

        // 单次或偶发本地 RPC 超时/端口切换抖动绝不立刻标红「连接异常」，
        // 连续 >=3 次失败才进入静默重连（ServiceReconnecting），超时再自然回落 Offline。
        self.consecutive_transient_errors = self.consecutive_transient_errors.saturating_add(1);
        if self.consecutive_transient_errors < TRANSIENT_ERROR_THRESHOLD {
            self.state.stale = self.last_success_ms > 0 && now - self.last_success_ms > STALE_AFTER_MS;
            return;
        }

        if self.had_connection
            && self.last_success_ms > 0
            && now - self.last_success_ms <= RECONNECT_GRACE_MS
        {
            self.state.stale = true;
            self.state.error = None;
            self.state.activity = AgentActivityKind::ServiceReconnecting;
            self.state.active_step_label = Some("正在重连".to_string());
        } else {
            self.state.error = None;
            self.state.stale = false;
            self.state.activity = AgentActivityKind::ProcessOffline;
            self.state.active_step_label = None;
            self.state.active_agents = 0;
            self.state.completed_agents = 0;
            if self.state.status != TelemetryStatus::Offline {
                self.state.status = TelemetryStatus::Offline;
                self.state.status_since_ms = now;
            }
            self.work_started_ms = None;
            self.terminal_started_ms = None;
        }
    }

    fn apply_quota_error(
        &mut self,
        err: TelemetryError,
        now: i64,
        _events: &mut Vec<SchedulerEvent>,
    ) {
        let rate_limited = err.kind == TelemetryErrorKind::RateLimit;
        let hard_error = matches!(
            err.kind,
            TelemetryErrorKind::Auth | TelemetryErrorKind::Region | TelemetryErrorKind::RateLimit
        );
        if hard_error {
            self.state.error = Some(err);
            self.state.activity = AgentActivityKind::ApiAuthRateError;
            if self.state.status != TelemetryStatus::Error {
                // 401/403/429 必须走 AGENT_ALERT；调度器收到 Error 会提升警示并弹泡。
                self.state.status = TelemetryStatus::Error;
                self.state.status_since_ms = now;
            }
        } else {
            // 仅额度接口偶发网络抖动：不覆盖健康状态、不弹出「连接异常」横幅，保留上次已知额度。
            self.state.stale = self.last_success_ms > 0 && now - self.last_success_ms > STALE_AFTER_MS;
        }
        if rate_limited {
            self.quota_backoff_ms = (self.quota_backoff_ms * 2).min(QUOTA_BACKOFF_MAX_MS);
            self.next_quota_ms = now + self.quota_backoff_ms;
        } else {
            self.next_quota_ms = now + QUOTA_INTERVAL_MS;
        }
    }

    /// 合成最终精细场景并向调度器发射去重后的 `AgentScene`。
    fn synthesize_scene(&mut self, now: i64, events: &mut Vec<SchedulerEvent>) {
        // 1. 长耗时终端构建自动升格（>= 15s）
        if self.state.status == TelemetryStatus::ToolRunning
            && self.state.activity == AgentActivityKind::TerminalRunning
        {
            if let Some(started) = self.terminal_started_ms {
                if now - started >= LONG_BUILD_THRESHOLD_MS {
                    self.state.activity = AgentActivityKind::BuildLongRunning;
                    self.state.active_step_label = Some("长耗时构建".to_string());
                }
            }
        }

        // 2. 过期瞬态清理；仅当仍为低冲突态（Idle / 非 L1-L3）时应用瞬态叠加
        if let Some((t_kind, expires_at, ref detail)) = self.transient_scene.clone() {
            if now >= expires_at
                || (self.state.activity.tier() <= 3
                    && self.state.activity != AgentActivityKind::ServiceReconnecting)
            {
                self.transient_scene = None;
                if self.state.status == TelemetryStatus::Idle && self.state.activity == t_kind {
                    self.state.activity = AgentActivityKind::Idle;
                }
            } else if self.state.status == TelemetryStatus::Idle {
                self.state.activity = t_kind;
                if let Some(d) = detail {
                    self.state.step_detail = Some(d.clone());
                }
            }
        }

        // 3. 空闲态额度耗尽冷却态
        if self.state.status == TelemetryStatus::Idle
            && self.state.activity == AgentActivityKind::Idle
        {
            let exhausted = self
                .state
                .quota
                .remaining_pct
                .map(|p| p <= 0.01)
                .unwrap_or(false)
                && self.state.quota.reset_at_ms.is_some();
            if exhausted {
                self.state.activity = AgentActivityKind::QuotaExhausted;
            }
        }

        let current_activity = self.state.activity;
        let current_status = self.state.status;
        let is_web_search = current_activity == AgentActivityKind::ResearchLookup
            && self.state.step_detail.as_deref() == Some("web_search");
        if self.last_emitted_activity != Some(current_activity)
            || self.last_emitted_web_search != is_web_search
            || !self.published_once
        {
            let bubble = match current_activity {
                AgentActivityKind::ApprovalOutsideDir => {
                    Some("Agent 请求访问工作区外路径，去 IDE 看一眼吧。".to_string())
                }
                AgentActivityKind::ArtifactReviewReady => {
                    Some("方案/演示成果已就绪，等你批注审阅。".to_string())
                }
                _ => None,
            };
            events.push(SchedulerEvent::AgentScene(AgentSceneSignal {
                status: current_status,
                activity: current_activity,
                detail: self.state.step_detail.clone(),
                bubble,
            }));
            self.last_emitted_activity = Some(current_activity);
            self.last_emitted_web_search = is_web_search;
        }
    }

    fn recalculate_alerts(&mut self) {
        let context_high = self
            .state
            .context
            .used_pct
            .map(|pct| pct >= 80.0)
            .unwrap_or(false);
        let quota_low = self
            .state
            .quota
            .remaining_pct
            .map(|pct| pct <= 10.0)
            .unwrap_or(false);
        self.state.alerts = TelemetryAlerts {
            context_high,
            quota_low,
            waiting_approval: self.state.status == TelemetryStatus::WaitingApproval,
            has_error: self.state.status == TelemetryStatus::Error && self.state.error.is_some(),
        };
    }

    /// 阈值告警只在上升沿发一次；调度器再叠加 60s 同类冷却。
    fn drain_threshold_alerts(&mut self) -> Vec<SchedulerEvent> {
        let mut events = Vec::new();
        if self.state.alerts.context_high && !self.last_alerts.context_high {
            events.push(SchedulerEvent::AgentAlert {
                key: "context_high".to_string(),
                bubble: "上下文快满了，点一下进度条复制续命提示词吧。".to_string(),
            });
        }
        if self.state.alerts.quota_low && !self.last_alerts.quota_low {
            let alt = self
                .state
                .quota
                .pools
                .iter()
                .filter(|p| !p.is_active && p.remaining_pct > 15.0)
                .max_by(|a, b| a.remaining_pct.total_cmp(&b.remaining_pct))
                .map(|p| format!("（备选 {} 还有 {}%）", p.label, p.remaining_pct.round() as i64))
                .unwrap_or_default();
            events.push(SchedulerEvent::AgentAlert {
                key: "quota_low".to_string(),
                bubble: format!("当前模型池额度见底了，考虑切个备选池{alt}。"),
            });
        }
        self.last_alerts = self.state.alerts.clone();
        events
    }

    fn update_emit_key(&mut self) -> bool {
        let key = self.state.diff_key();
        if !self.published_once || key != self.last_emit_key {
            self.last_emit_key = key;
            self.published_once = true;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_code(status: TelemetryStatus) -> u8 {
        match status {
            TelemetryStatus::Offline => 0,
            TelemetryStatus::Idle => 1,
            TelemetryStatus::Streaming => 2,
            TelemetryStatus::ToolRunning => 3,
            TelemetryStatus::WaitingApproval => 4,
            TelemetryStatus::Error => 5,
        }
    }

    fn code_status(code: u8) -> TelemetryStatus {
        match code {
            0 => TelemetryStatus::Offline,
            2 => TelemetryStatus::Streaming,
            3 => TelemetryStatus::ToolRunning,
            4 => TelemetryStatus::WaitingApproval,
            5 => TelemetryStatus::Error,
            _ => TelemetryStatus::Idle,
        }
    }

    struct FakeAdapter {
        status_slot: std::sync::Arc<std::sync::atomic::AtomicU8>,
        activity_override: Option<AgentActivityKind>,
        status_error: Option<TelemetryError>,
        used_pct: Option<f64>,
        quota_pct: Option<f64>,
        quota_error: Option<TelemetryError>,
    }

    impl FakeAdapter {
        fn new() -> Self {
            Self {
                status_slot: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(1)),
                activity_override: None,
                status_error: None,
                used_pct: None,
                quota_pct: Some(64.0),
                quota_error: None,
            }
        }

        fn with_status(status: TelemetryStatus) -> Self {
            let adapter = Self::new();
            adapter
                .status_slot
                .store(status_code(status), std::sync::atomic::Ordering::SeqCst);
            adapter
        }
    }

    impl TelemetryAdapter for FakeAdapter {
        fn id(&self) -> &'static str {
            "antigravity"
        }

        fn capabilities(&self) -> TelemetryCapabilities {
            TelemetryCapabilities {
                has_quota: true,
                has_multi_pool: true,
                has_realtime_speed: true,
                has_ttft: true,
                has_context: true,
                has_approval: true,
            }
        }

        fn poll_status(&mut self, _now: i64) -> Result<StatusSnapshot, TelemetryError> {
            if let Some(err) = &self.status_error {
                return Err(err.clone());
            }
            let status = code_status(self.status_slot.load(std::sync::atomic::Ordering::SeqCst));
            let activity = self
                .activity_override
                .unwrap_or_else(|| AgentActivityKind::from_status(status));
            Ok(StatusSnapshot {
                status,
                active_step_label: None,
                activity,
                active_agents: if status.is_agent_active() { 1 } else { 0 },
                workspace_name: Some("Deskpet".to_string()),
                step_detail: None,
                model: TelemetryModelInfo {
                    name: Some("Gemini 3 Pro".to_string()),
                    context_limit: Some(1_000_000),
                },
                speed: TelemetrySpeedInfo {
                    tokens_per_sec: Some(42.0),
                    ttft_ms: Some(800),
                    confidence: TelemetryConfidence::Estimated,
                },
                context: TelemetryContextInfo {
                    used_tokens: self.used_pct.map(|pct| (pct * 10_000.0) as u64),
                    limit_tokens: Some(1_000_000),
                    used_pct: self.used_pct,
                    delta_tokens: Some(120),
                    confidence: TelemetryConfidence::Exact,
                },
                trajectory_id: Some("traj-1".to_string()),
                error: None,
            })
        }

        fn poll_quota(&mut self, _now: i64) -> Result<TelemetryQuotaInfo, TelemetryError> {
            if let Some(err) = &self.quota_error {
                return Err(err.clone());
            }
            Ok(TelemetryQuotaInfo {
                remaining_pct: self.quota_pct,
                reset_at_ms: Some(1_800_000_000_000),
                pools: vec![TelemetryQuotaPool {
                    id: "gemini".to_string(),
                    label: "Gemini".to_string(),
                    remaining_pct: self.quota_pct.unwrap_or(0.0),
                    reset_at_ms: Some(1_800_000_000_000),
                    is_active: true,
                }],
                confidence: TelemetryConfidence::Exact,
            })
        }
    }

    fn manager_with(fake: FakeAdapter) -> TelemetryManager {
        TelemetryManager::new(Box::new(fake))
    }

    #[test]
    fn diff_dedup_ignores_only_timestamp() {
        let mut manager = manager_with(FakeAdapter::new());
        let first = manager.tick(1_000);
        assert!(first.changed);
        let second = manager.tick(1_001);
        assert!(!second.changed, "相同内容不应重复推送");
        manager.state.context.used_pct = Some(12.0);
        assert!(manager.update_emit_key());
    }

    #[test]
    fn alert_rising_edges_only_fire_once() {
        let mut manager = manager_with(FakeAdapter::new());
        manager.state.context.used_pct = Some(85.0);
        manager.state.quota.remaining_pct = Some(8.0);
        manager.recalculate_alerts();
        let alerts = manager.drain_threshold_alerts();
        assert_eq!(alerts.len(), 2);
        assert!(manager.drain_threshold_alerts().is_empty());
    }

    #[test]
    fn rate_limit_backoff_grows_into_range() {
        let mut fake = FakeAdapter::new();
        fake.quota_error = Some(TelemetryError::new(
            Some(429),
            TelemetryErrorKind::RateLimit,
            "限流",
        ));
        let mut manager = manager_with(fake);
        let first_tick = manager.tick(1_000);
        assert!(first_tick.scheduler_events.iter().any(|event| matches!(
            event,
            SchedulerEvent::AgentScene(sig) if sig.status == TelemetryStatus::Error
        )));
        assert!(manager.next_quota_ms >= 1_000 + 120_000);
        let first = manager.next_quota_ms;
        manager.next_quota_ms = 0;
        manager.tick(2_000);
        assert!(manager.next_quota_ms > first, "第二次退避必须更长");
        assert!(manager.quota_backoff_ms <= 300_000);
        assert_eq!(
            manager.state.error.as_ref().map(|err| err.code),
            Some(Some(429))
        );
    }

    #[test]
    fn turn_end_forces_quota_refresh() {
        let fake = FakeAdapter::with_status(TelemetryStatus::Streaming);
        let slot = fake.status_slot.clone();
        let mut manager = manager_with(fake);
        manager.tick(1_000);
        // 下一轮把状态置 idle，Turn 结束应立刻补一次额度同步。
        slot.store(1, std::sync::atomic::Ordering::SeqCst);
        manager.next_status_ms = 0;
        manager.tick(2_000);
        assert_eq!(manager.state.status, TelemetryStatus::Idle);
        assert_eq!(manager.next_quota_ms, 2_000 + QUOTA_INTERVAL_MS);
    }

    #[test]
    fn transient_network_errors_do_not_flash_error_status() {
        let mut adapter = FakeAdapter::new();
        adapter.quota_error = Some(TelemetryError::new(
            None,
            TelemetryErrorKind::Network,
            "偶发网络超时",
        ));
        let mut manager = manager_with(adapter);
        let first = manager.tick(1_000);
        assert_eq!(first.state.status, TelemetryStatus::Idle);
        assert!(first.state.error.is_none());
        assert!(!first.state.alerts.has_error);

        // 连续状态接口出现本地网络错误：前 2 次保持原态，第 3 次切 ServiceReconnecting，始终不为 Error
        let mut err_adapter = FakeAdapter::new();
        err_adapter.status_error = Some(TelemetryError::new(
            None,
            TelemetryErrorKind::Network,
            "连不上",
        ));
        let mut m2 = manager_with(err_adapter);
        m2.had_connection = true;
        m2.last_success_ms = 1_000;
        m2.state.status = TelemetryStatus::Idle;
        m2.state.activity = AgentActivityKind::Idle;

        m2.next_status_ms = 0;
        let t1 = m2.tick(2_000);
        assert_eq!(t1.state.activity, AgentActivityKind::Idle);
        assert_ne!(t1.state.status, TelemetryStatus::Error);

        m2.next_status_ms = 0;
        let t2 = m2.tick(3_000);
        assert_eq!(t2.state.activity, AgentActivityKind::Idle);

        m2.next_status_ms = 0;
        let t3 = m2.tick(4_000);
        assert_eq!(t3.state.activity, AgentActivityKind::ServiceReconnecting);
        assert_ne!(t3.state.status, TelemetryStatus::Error);
        assert!(t3.state.error.is_none());
    }

    #[test]
    fn terminal_upgrades_to_build_long_running_after_15s() {
        let fake = FakeAdapter::with_status(TelemetryStatus::ToolRunning);
        let mut manager = manager_with(fake);
        let t1 = manager.tick(1_000);
        assert_eq!(t1.state.activity, AgentActivityKind::TerminalRunning);
        manager.next_status_ms = 0;
        let t2 = manager.tick(17_000);
        assert_eq!(t2.state.activity, AgentActivityKind::BuildLongRunning);
    }

    #[test]
    fn completed_work_over_5s_emits_task_completed() {
        let fake = FakeAdapter::with_status(TelemetryStatus::Streaming);
        let slot = fake.status_slot.clone();
        let mut manager = manager_with(fake);
        manager.tick(1_000);
        slot.store(1, std::sync::atomic::Ordering::SeqCst);
        manager.next_status_ms = 0;
        let done = manager.tick(7_000);
        assert_eq!(done.state.status, TelemetryStatus::Idle);
        assert_eq!(done.state.activity, AgentActivityKind::TaskCompleted);
        assert!(done.scheduler_events.iter().any(|ev| matches!(
            ev,
            SchedulerEvent::AgentScene(sig) if sig.activity == AgentActivityKind::TaskCompleted
        )));
    }

    #[test]
    fn context_delta_bubble_emits_during_tool_running_without_interrupting_activity() {
        let mut fake = FakeAdapter::with_status(TelemetryStatus::ToolRunning);
        fake.used_pct = Some(38.0);
        let mut manager = manager_with(fake);
        // FakeAdapter 默认 used_tokens = 38.0 * 10_000 = 380_000, delta_tokens = 120；
        // 先手动验证 18_500 delta + 120_000 used_tokens 的纯气泡发射逻辑。
        let snap = StatusSnapshot {
            status: TelemetryStatus::ToolRunning,
            active_step_label: Some("执行命令".to_string()),
            activity: AgentActivityKind::TerminalRunning,
            active_agents: 1,
            workspace_name: Some("Deskpet".to_string()),
            step_detail: None,
            model: TelemetryModelInfo {
                name: Some("Gemini 4".to_string()),
                context_limit: Some(1_000_000),
            },
            speed: TelemetrySpeedInfo::default(),
            context: TelemetryContextInfo {
                used_tokens: Some(120_000),
                limit_tokens: Some(1_000_000),
                used_pct: Some(38.0),
                delta_tokens: Some(18_500),
                confidence: TelemetryConfidence::Exact,
            },
            trajectory_id: Some("traj-1".to_string()),
            error: None,
        };
        let mut events = Vec::new();
        manager.apply_snapshot(snap.clone(), 5_000, &mut events);
        manager.synthesize_scene(5_000, &mut events);

        assert_eq!(manager.state.activity, AgentActivityKind::TerminalRunning);
        assert!(events.iter().any(|ev| matches!(
            ev,
            SchedulerEvent::AgentBubble { text, .. }
            if text.contains("+18.5K Tokens") && text.contains("当前 38%")
        )));

        // 同一步 used_tokens 重复轮询不会重复弹泡
        let mut dup_events = Vec::new();
        manager.apply_snapshot(snap, 9_000, &mut dup_events);
        assert!(!dup_events
            .iter()
            .any(|ev| matches!(ev, SchedulerEvent::AgentBubble { .. })));
    }
}
