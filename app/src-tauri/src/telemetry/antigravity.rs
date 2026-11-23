//! Antigravity IDE 官方 `language_server` 只读适配器。
//!
//! 契约 v1.2 §9.3：
//! - 扫描 `%USERPROFILE%\.gemini\antigravity\daemon\hub_*.json` 自动发现端口；
//! - `csrfToken` 只驻留内存，用于本机请求头，不写日志、不落库、不出现在 UnifiedState；
//! - 直连 ConnectRPC 四个接口：GetUserStatus / GetAllCascadeTrajectories /
//!   GetCascadeTrajectorySteps / GetCascadeTrajectoryGeneratorMetadata；
//! - 所有解析器都对字段做容错（数字/字符串、数组/对象 map 两种形态）。

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::{
    AgentActivityKind, StatusSnapshot, TelemetryAdapter, TelemetryCapabilities, TelemetryConfidence,
    TelemetryContextInfo, TelemetryError, TelemetryErrorKind, TelemetryModelInfo,
    TelemetryQuotaInfo, TelemetryQuotaPool, TelemetrySpeedInfo, TelemetryStatus,
};

const SERVICE_PATH: &str = "/exa.language_server_pb.LanguageServerService/";
const CONNECT_TIMEOUT_MS: u64 = 500;
const IO_TIMEOUT_MS: u64 = 4_000;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/* ------------------------------------------------------------------ */
/* 本地 hub 发现                                                       */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone)]
struct HubInfo {
    pid: u32,
    http_port: u16,
    csrf_token: String,
}

fn hub_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .map(|home| home.join(".gemini").join("antigravity").join("daemon"))
}

/// 读取全部 hub_*.json，按修改时间倒序，返回第一个 pid 仍存活的记录。
fn discover_hub() -> Option<HubInfo> {
    let dir = hub_dir()?;
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("hub_") || !name.ends_with(".json") {
                return None;
            }
            let mtime = entry.metadata().ok()?.modified().ok()?;
            Some((mtime, entry.path()))
        })
        .collect();
    files.sort_by(|left, right| right.0.cmp(&left.0));

    for (_, path) in files {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let pid = value.get("pid").and_then(as_u64).unwrap_or(0) as u32;
        let http_port = value.get("httpPort").and_then(as_u64).unwrap_or(0) as u16;
        let csrf_token = value
            .get("csrfToken")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if pid == 0 || http_port == 0 || csrf_token.is_empty() {
            continue;
        }
        // 受限权限下 OpenProcess 可能被拒绝，但端口仍存活（本机实测会遇到）。
        // 因此 pid 检查失败时再用 TCP 探活兜底，避免把真实 LS 误判为 offline。
        if pid_alive(pid) || port_open(http_port) {
            return Some(HubInfo {
                pid,
                http_port,
                csrf_token,
            });
        }
    }
    None
}

/// 150ms 内能否连上本机端口；只用于 liveness，不发送任何数据。
fn port_open(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok()
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }
    // STILL_ACTIVE = 259（windows-sys 在不同版本里常量位置不一致，直接比较值）。
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        ok != 0 && exit_code == 259
    }
}

#[cfg(not(windows))]
fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/* ------------------------------------------------------------------ */
/* 最小 HTTP/1.1 客户端（纯 std，零新增依赖）                          */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone)]
struct HttpResponse {
    body: String,
}

fn post_json(
    port: u16,
    method: &str,
    csrf_token: &str,
    body: &str,
) -> Result<HttpResponse, TelemetryError> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_millis(CONNECT_TIMEOUT_MS)).map_err(
            |_| {
                TelemetryError::new(
                    None,
                    TelemetryErrorKind::Network,
                    "连不上 Antigravity 本地服务",
                )
            },
        )?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(IO_TIMEOUT_MS)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(IO_TIMEOUT_MS)));

    let request = format!(
        "POST {SERVICE_PATH}{method} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Connect-Protocol-Version: 1\r\n\
         X-Codeium-Csrf-Token: {csrf_token}\r\n\
         Accept: application/json\r\n\
         Accept-Encoding: identity\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n{body}",
        len = body.as_bytes().len()
    );
    stream.write_all(request.as_bytes()).map_err(|_| {
        TelemetryError::new(
            None,
            TelemetryErrorKind::Network,
            "向 Antigravity 本地服务写入失败",
        )
    })?;

    let mut raw = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut timed_out = false;
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                raw.extend_from_slice(&buffer[..read]);
                if raw.len() > MAX_RESPONSE_BYTES {
                    return Err(TelemetryError::new(
                        None,
                        TelemetryErrorKind::Unknown,
                        "Antigravity 本地服务响应过大",
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                timed_out = true;
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {
                timed_out = true;
                break;
            }
            Err(_) => {
                return Err(TelemetryError::new(
                    None,
                    TelemetryErrorKind::Network,
                    "读取 Antigravity 本地服务响应失败",
                ))
            }
        }
    }
    if timed_out && (raw.is_empty() || !raw.windows(4).any(|w| w == b"\r\n\r\n")) {
        return Err(TelemetryError::new(
            None,
            TelemetryErrorKind::Network,
            "读取 Antigravity 本地服务响应超时",
        ));
    }
    parse_http_response(&raw)
}

fn parse_http_response(raw: &[u8]) -> Result<HttpResponse, TelemetryError> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| {
            TelemetryError::new(
                None,
                TelemetryErrorKind::Network,
                "Antigravity 本地服务返回了空响应",
            )
        })?;
    let header_text = String::from_utf8_lossy(&raw[..split]);
    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            TelemetryError::new(
                None,
                TelemetryErrorKind::Network,
                "无法解析 Antigravity 本地服务响应状态",
            )
        })?;

    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }

    let payload = &raw[split + 4..];
    let body_bytes = if chunked {
        decode_chunked(payload).unwrap_or_else(|_| payload.to_vec())
    } else {
        payload.to_vec()
    };
    let body = String::from_utf8_lossy(&body_bytes).to_string();

    if !(200..300).contains(&status) {
        return Err(error_for_status(status));
    }
    // Connect 也可能用 HTTP 200 + {"code":"unauthenticated"}。
    if let Ok(value) = serde_json::from_str::<Value>(&body) {
        if let Some(code) = value.get("code").and_then(Value::as_str) {
            if let Some(err) = error_for_connect_code(code) {
                return Err(err);
            }
        }
    }
    Ok(HttpResponse { body })
}

fn decode_chunked(raw: &[u8]) -> Result<Vec<u8>, ()> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < raw.len() {
        let line_end = raw[offset..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or(())?
            + offset;
        let size_text = String::from_utf8_lossy(&raw[offset..line_end]);
        let size = usize::from_str_radix(size_text.trim(), 16).map_err(|_| ())?;
        if size == 0 {
            break;
        }
        let data_start = line_end + 2;
        let data_end = (data_start + size).min(raw.len());
        out.extend_from_slice(&raw[data_start..data_end]);
        offset = (data_end + 2).min(raw.len());
    }
    Ok(out)
}

fn error_for_status(status: u16) -> TelemetryError {
    match status {
        401 => TelemetryError::new(
            Some(401),
            TelemetryErrorKind::Auth,
            "Antigravity 登录已过期（401），请在 IDE 里重新登录。",
        ),
        403 => TelemetryError::new(
            Some(403),
            TelemetryErrorKind::Region,
            "Antigravity 拒绝了请求（403），可能是网络节点或 IP 区域变动。",
        ),
        429 => TelemetryError::new(
            Some(429),
            TelemetryErrorKind::RateLimit,
            "Antigravity 触发限流（429），额度轮询已自动退避。",
        ),
        other => TelemetryError::new(
            Some(other),
            TelemetryErrorKind::Unknown,
            format!("Antigravity 本地服务返回 HTTP {other}"),
        ),
    }
}

fn error_for_connect_code(code: &str) -> Option<TelemetryError> {
    let lower = code.to_ascii_lowercase();
    if lower.contains("unauthenticated") {
        return Some(error_for_status(401));
    }
    if lower.contains("permission_denied") {
        return Some(error_for_status(403));
    }
    if lower.contains("resource_exhausted") {
        return Some(error_for_status(429));
    }
    None
}

/* ------------------------------------------------------------------ */
/* Adapter                                                             */
/* ------------------------------------------------------------------ */

const SPEED_WINDOW_MS: i64 = 2_800;
const SPEED_EWMA_ALPHA: f64 = 0.45;
const SPEED_HOLD_FULL_MS: i64 = 1_200;
const SPEED_HOLD_MAX_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeedSourceKind {
    None,
    StepExact,
    MetaExact,
    StepEstimated,
}

#[derive(Debug, Clone)]
struct SpeedEstimator {
    samples: VecDeque<(i64, f64)>,
    source_kind: SpeedSourceKind,
    active_step_key: Option<String>,
    stream_started_ms: Option<i64>,
    last_progress_ms: i64,
    last_max_tokens: f64,
    last_meta_tokens: Option<u64>,
    meta_stalled: bool,
    ewma_tps: Option<f64>,
    pre_stream_meta_count: usize,
}

impl Default for SpeedEstimator {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            source_kind: SpeedSourceKind::None,
            active_step_key: None,
            stream_started_ms: None,
            last_progress_ms: 0,
            last_max_tokens: 0.0,
            last_meta_tokens: None,
            meta_stalled: false,
            ewma_tps: None,
            pre_stream_meta_count: 0,
        }
    }
}

impl SpeedEstimator {
    fn reset_all(&mut self) {
        *self = Self::default();
    }

    fn reset_stream_window(&mut self) {
        self.samples.clear();
        self.source_kind = SpeedSourceKind::None;
        self.active_step_key = None;
        self.stream_started_ms = None;
        self.last_progress_ms = 0;
        self.last_max_tokens = 0.0;
        self.last_meta_tokens = None;
        self.meta_stalled = false;
        self.ewma_tps = None;
    }

    fn update(
        &mut self,
        status: TelemetryStatus,
        obs: &StepObservation,
        sample: Option<&GeneratorSample>,
        switched: bool,
        now: i64,
    ) -> TelemetrySpeedInfo {
        let ttft_ms = sample.and_then(|s| s.ttft_ms);

        if switched {
            self.reset_all();
        }

        if status != TelemetryStatus::Streaming {
            if let Some(s) = sample {
                self.pre_stream_meta_count = s.metadata_count;
            }
            self.reset_stream_window();
            return TelemetrySpeedInfo {
                tokens_per_sec: Some(0.0),
                ttft_ms,
                confidence: TelemetryConfidence::Exact,
            };
        }

        if self.stream_started_ms.is_none() {
            self.stream_started_ms = Some(now);
        }

        // 同一次对话内切到新的 PLANNER_RESPONSE 步骤（例如工具调用完继续输出）时重置采样窗。
        if obs.streaming_step_key.is_some() && obs.streaming_step_key != self.active_step_key {
            if self.active_step_key.is_some() {
                self.samples.clear();
                self.source_kind = SpeedSourceKind::None;
                self.last_max_tokens = 0.0;
                self.last_progress_ms = 0;
                self.ewma_tps = None;
                self.stream_started_ms = Some(now);
            }
            self.active_step_key = obs.streaming_step_key.clone();
        }

        let is_new_meta = sample
            .map(|s| s.metadata_count > self.pre_stream_meta_count)
            .unwrap_or(false);
        let meta_tokens = if is_new_meta {
            sample.and_then(|s| s.output_tokens)
        } else {
            None
        };

        if let Some(mt) = meta_tokens {
            if let Some(prev) = self.last_meta_tokens {
                if mt > prev {
                    self.meta_stalled = false;
                } else if obs.streaming_est_tokens.is_some() {
                    self.meta_stalled = true;
                }
            }
            self.last_meta_tokens = Some(mt);
        }

        let (chosen_kind, chosen_val, is_exact) = if let Some(exact) = obs.streaming_exact_tokens {
            (SpeedSourceKind::StepExact, exact as f64, true)
        } else if let (Some(mt), false) = (meta_tokens, self.meta_stalled) {
            (SpeedSourceKind::MetaExact, mt as f64, true)
        } else if let Some(est) = obs.streaming_est_tokens {
            (SpeedSourceKind::StepEstimated, est, false)
        } else if let Some(mt) = meta_tokens {
            (SpeedSourceKind::MetaExact, mt as f64, true)
        } else {
            (SpeedSourceKind::None, 0.0, false)
        };

        if chosen_kind != SpeedSourceKind::None && chosen_kind != self.source_kind {
            self.samples.clear();
            self.source_kind = chosen_kind;
            self.last_max_tokens = 0.0;
            self.last_progress_ms = 0;
        }

        let mut made_progress = false;
        if chosen_kind != SpeedSourceKind::None {
            let is_first = self.samples.is_empty() && chosen_val > 0.0;
            let advanced = chosen_val > self.last_max_tokens + 0.05;
            if is_first || advanced {
                if self.last_progress_ms > 0 && now - self.last_progress_ms > SPEED_HOLD_MAX_MS {
                    self.samples.clear();
                    self.ewma_tps = None;
                }
                made_progress = true;
                self.last_max_tokens = chosen_val;
                self.last_progress_ms = now;

                if self
                    .samples
                    .back()
                    .map(|(ts, _)| *ts == now)
                    .unwrap_or(false)
                {
                    if let Some(last) = self.samples.back_mut() {
                        last.1 = last.1.max(chosen_val);
                    }
                } else {
                    self.samples.push_back((now, chosen_val));
                }
                while self
                    .samples
                    .front()
                    .map(|(ts, _)| now - *ts > SPEED_WINDOW_MS)
                    .unwrap_or(false)
                {
                    if self.samples.len() <= 2 {
                        break;
                    }
                    self.samples.pop_front();
                }
            }
        }

        let confidence = if is_exact {
            TelemetryConfidence::Exact
        } else {
            TelemetryConfidence::Estimated
        };

        // 1) 有新 token 推进时使用 >=250ms 的滑动窗口计算实时速度 + EWMA 平滑
        if made_progress {
            if let (Some((first_ts, first_tok)), Some((last_ts, last_tok))) =
                (self.samples.front().copied(), self.samples.back().copied())
            {
                let span_ms = last_ts - first_ts;
                let delta_tok = last_tok - first_tok;
                if span_ms >= 250 && delta_tok > 0.05 {
                    let raw_tps = delta_tok / (span_ms as f64 / 1000.0);
                    let smoothed = match self.ewma_tps {
                        Some(prev) => SPEED_EWMA_ALPHA * raw_tps + (1.0 - SPEED_EWMA_ALPHA) * prev,
                        None => raw_tps,
                    };
                    self.ewma_tps = Some(smoothed);
                    return TelemetrySpeedInfo {
                        tokens_per_sec: Some((smoothed * 10.0).round() / 10.0),
                        ttft_ms,
                        confidence,
                    };
                }
            }
        }

        // 2) 块间短停顿保持（<= 2.0s 内不闪烁归零；1.2s-2.0s 轻柔衰减）
        if let Some(prev_ewma) = self.ewma_tps {
            if self.last_progress_ms > 0 {
                let pause_ms = now - self.last_progress_ms;
                if pause_ms <= SPEED_HOLD_FULL_MS {
                    return TelemetrySpeedInfo {
                        tokens_per_sec: Some((prev_ewma * 10.0).round() / 10.0),
                        ttft_ms,
                        confidence,
                    };
                }
                if pause_ms <= SPEED_HOLD_MAX_MS {
                    let frac = (pause_ms - SPEED_HOLD_FULL_MS) as f64
                        / (SPEED_HOLD_MAX_MS - SPEED_HOLD_FULL_MS) as f64;
                    let decayed = prev_ewma * (1.0 - 0.35 * frac.clamp(0.0, 1.0));
                    return TelemetrySpeedInfo {
                        tokens_per_sec: Some((decayed * 10.0).round() / 10.0),
                        ttft_ms,
                        confidence,
                    };
                }
            }
            self.ewma_tps = None;
        }

        // 3) 滑窗不足 250ms 跨度时的启动兜底
        if is_new_meta {
            if let Some(s) = sample {
                if let (Some(output), Some(duration_ms)) = (s.output_tokens, s.streaming_duration_ms)
                {
                    if output > 0 && duration_ms > 0 {
                        let tps = output as f64 / (duration_ms as f64 / 1000.0);
                        return TelemetrySpeedInfo {
                            tokens_per_sec: Some((tps * 10.0).round() / 10.0),
                            ttft_ms,
                            confidence: TelemetryConfidence::Estimated,
                        };
                    }
                }
            }
        }

        if let Some(started_ms) = self.stream_started_ms {
            let elapsed_ms = now - started_ms;
            if self.last_max_tokens >= 2.0 && elapsed_ms >= 250 {
                let tps = self.last_max_tokens / (elapsed_ms as f64 / 1000.0);
                self.ewma_tps = Some(tps);
                return TelemetrySpeedInfo {
                    tokens_per_sec: Some((tps * 10.0).round() / 10.0),
                    ttft_ms,
                    confidence,
                };
            }
        }

        TelemetrySpeedInfo {
            tokens_per_sec: None,
            ttft_ms,
            confidence: TelemetryConfidence::Unavailable,
        }
    }
}

pub struct AntigravityAdapter {
    hub: Option<HubInfo>,
    next_discovery_ms: i64,
    model: TelemetryModelInfo,
    capabilities: TelemetryCapabilities,
    last_trajectory: Option<String>,
    last_used_tokens: Option<u64>,
    speed_estimator: SpeedEstimator,
    last_context: TelemetryContextInfo,
    last_metadata_ms: i64,
}

impl AntigravityAdapter {
    pub fn new() -> Self {
        Self {
            hub: discover_hub(),
            next_discovery_ms: 0,
            model: TelemetryModelInfo::default(),
            capabilities: TelemetryCapabilities {
                has_quota: true,
                has_multi_pool: true,
                has_realtime_speed: true,
                has_ttft: true,
                has_context: true,
                has_approval: true,
            },
            last_trajectory: None,
            last_used_tokens: None,
            speed_estimator: SpeedEstimator::default(),
            last_context: TelemetryContextInfo::default(),
            last_metadata_ms: 0,
        }
    }

    fn current_hub(&mut self) -> Option<HubInfo> {
        if let Some(hub) = &self.hub {
            if pid_alive(hub.pid) || port_open(hub.http_port) {
                return Some(hub.clone());
            }
            self.hub = None;
        }
        // 离线时每 2s 才重新扫一次 hub，避免每秒把 7 个陈旧文件都探一遍。
        let now = super::now_ms();
        if now < self.next_discovery_ms {
            return None;
        }
        self.next_discovery_ms = now + 2_000;
        self.hub = discover_hub();
        self.hub.clone()
    }

    fn request(&mut self, method: &str, body: &str) -> Result<Value, TelemetryError> {
        let Some(hub) = self.current_hub() else {
            return Err(TelemetryError::new(
                None,
                TelemetryErrorKind::Network,
                "未发现存活的 Antigravity language_server",
            ));
        };
        match post_json(hub.http_port, method, &hub.csrf_token, body) {
            Ok(response) => parse_json_body(&response.body),
            Err(first_error) => {
                // 端口变动 / 进程重启：立即清缓存与冷却，重扫一次 hub_*.json 再试。
                self.hub = None;
                self.next_discovery_ms = 0;
                if let Some(next) = self.current_hub() {
                    if next.pid != hub.pid || next.http_port != hub.http_port {
                        let response = post_json(next.http_port, method, &next.csrf_token, body)?;
                        return parse_json_body(&response.body);
                    }
                }
                Err(first_error)
            }
        }
    }

    fn poll_quota_inner(&mut self) -> Result<TelemetryQuotaInfo, TelemetryError> {
        let value = self.request(
            "GetUserStatus",
            r#"{"metadata":{"ideName":"antigravity","extensionName":"antigravity","locale":"en"}}"#,
        )?;
        Ok(parse_user_status(&value, self.model.name.as_deref()))
    }

    fn poll_status_inner(&mut self, now: i64) -> Result<StatusSnapshot, TelemetryError> {
        if self.current_hub().is_none() {
            return Ok(StatusSnapshot {
                status: TelemetryStatus::Offline,
                active_step_label: None,
                activity: AgentActivityKind::ProcessOffline,
                active_agents: 0,
                workspace_name: None,
                step_detail: None,
                model: self.model.clone(),
                speed: TelemetrySpeedInfo {
                    tokens_per_sec: Some(0.0),
                    ttft_ms: None,
                    confidence: TelemetryConfidence::Exact,
                },
                context: self.last_context.clone(),
                trajectory_id: None,
                error: None,
            });
        }

        let trajectories = self.request("GetAllCascadeTrajectories", "{}")?;
        let summaries = parse_trajectories(&trajectories);
        let active_agents = count_running_trajectories(&summaries);
        let Some(active) = pick_active_trajectory(&summaries) else {
            self.last_trajectory = None;
            self.last_used_tokens = None;
            self.speed_estimator.reset_all();
            return Ok(StatusSnapshot {
                status: TelemetryStatus::Idle,
                active_step_label: None,
                activity: AgentActivityKind::Idle,
                active_agents: 0,
                workspace_name: None,
                step_detail: None,
                model: self.model.clone(),
                speed: TelemetrySpeedInfo {
                    tokens_per_sec: Some(0.0),
                    ttft_ms: None,
                    confidence: TelemetryConfidence::Exact,
                },
                context: self.last_context.clone(),
                trajectory_id: None,
                error: None,
            });
        };

        let status_upper = active.status.to_ascii_uppercase();
        let actively_running = status_upper.contains("RUNNING")
            || status_upper.contains("WAITING")
            || status_upper.contains("GENERATING")
            || status_upper.contains("IN_PROGRESS");

        // 契约 §9.4：1s 轻量检测只拉轨迹列表，仅活跃时才查尾步。
        let obs = if actively_running {
            let start = active.step_count.saturating_sub(4);
            let steps = if active.step_count > 0 {
                let body = format!(
                    r#"{{"cascadeId":"{}","startIndex":{},"endIndex":{}}}"#,
                    escape_json(&active.id),
                    start,
                    active.step_count
                );
                self.request("GetCascadeTrajectorySteps", &body)
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            observe_step_state(&steps, &active.status, start)
        } else {
            StepObservation {
                status: TelemetryStatus::Idle,
                active_step_label: None,
                activity: AgentActivityKind::Idle,
                ..StepObservation::default()
            }
        };
        let status = obs.status;

        // 模型/上下文在 idle 时每 30s 刷新一次即可，避免每秒 3 个本机 RPC。
        let need_metadata = actively_running
            || self.last_trajectory.as_deref() != Some(active.id.as_str())
            || now - self.last_metadata_ms >= 30_000;
        let sample = if need_metadata {
            let metadata_body = format!(
                r#"{{"cascadeId":"{}","generatorMetadataOffset":0}}"#,
                escape_json(&active.id)
            );
            let sample = self
                .request("GetCascadeTrajectoryGeneratorMetadata", &metadata_body)
                .ok()
                .and_then(|value| parse_generator_metadata(&value));
            if sample.is_some() {
                self.last_metadata_ms = now;
            }
            sample
        } else {
            None
        };

        let (model, speed, context) =
            self.build_details(sample, &obs, Some(active.id.as_str()), now);

        let mut activity = obs.activity;
        let mut step_label = obs.active_step_label.clone();
        if status == TelemetryStatus::Streaming
            && activity == AgentActivityKind::DeepReasoning
            && !obs.streaming_thinking_only
            && speed.tokens_per_sec.is_some_and(|tps| tps > 0.0)
        {
            activity = AgentActivityKind::CodeStreaming;
            step_label = Some("生成中".to_string());
        }

        let step_error = if status == TelemetryStatus::Error {
            Some(TelemetryError::new(
                None,
                TelemetryErrorKind::Unknown,
                "Agent 执行中断，请在 IDE 查看详情。",
            ))
        } else {
            None
        };

        Ok(StatusSnapshot {
            status,
            active_step_label: step_label,
            activity,
            active_agents,
            workspace_name: active.workspace_name.clone(),
            step_detail: obs.step_detail,
            model,
            speed,
            context,
            trajectory_id: Some(active.id),
            error: step_error,
        })
    }

    /// 合并 GeneratorMetadata 与步骤实时观测，计算上下文占用、Delta 与平滑实时 t/s。
    fn build_details(
        &mut self,
        sample: Option<GeneratorSample>,
        obs: &StepObservation,
        trajectory_id: Option<&str>,
        now: i64,
    ) -> (TelemetryModelInfo, TelemetrySpeedInfo, TelemetryContextInfo) {
        let switched = self.last_trajectory.as_deref() != trajectory_id;
        let status = obs.status;

        let Some(sample) = sample else {
            if switched {
                self.last_context = TelemetryContextInfo::default();
                self.last_used_tokens = None;
            }
            self.last_trajectory = trajectory_id.map(str::to_string);
            let speed = self.speed_estimator.update(status, obs, None, switched, now);
            return (self.model.clone(), speed, self.last_context.clone());
        };

        if let Some(name) = sample.model_name.clone() {
            self.model.name = Some(name);
        }
        self.model.context_limit = self.model.name.as_deref().and_then(context_limit_for);

        let used = match (sample.input_tokens, sample.cache_read_tokens) {
            (Some(input), Some(cache)) => Some(input.saturating_add(cache)),
            (Some(input), None) => Some(input),
            (None, Some(cache)) => Some(cache),
            (None, None) => None,
        };
        let delta = if switched {
            None
        } else {
            match (self.last_used_tokens, used) {
                (Some(previous), Some(current)) if current >= previous => Some(current - previous),
                _ => None,
            }
        };
        self.last_used_tokens = used;
        let limit = self.model.context_limit;
        let used_pct = match (used, limit) {
            (Some(used), Some(limit)) if limit > 0 => {
                Some(((used as f64 / limit as f64) * 100.0).clamp(0.0, 100.0))
            }
            _ => None,
        };
        let context = TelemetryContextInfo {
            used_tokens: used,
            limit_tokens: limit,
            used_pct,
            delta_tokens: delta,
            confidence: if used.is_some() {
                TelemetryConfidence::Exact
            } else {
                TelemetryConfidence::Unavailable
            },
        };
        self.last_context = context.clone();

        let speed = self
            .speed_estimator
            .update(status, obs, Some(&sample), switched, now);
        self.last_trajectory = trajectory_id.map(str::to_string);
        (self.model.clone(), speed, context)
    }

    /// 只读探针：给 `cargo test -- --ignored` 或示例程序使用。
    /// 只输出池数量、状态与百分比，绝不打印 token / email。
    pub fn probe_summary(&mut self) -> Result<String, TelemetryError> {
        let quota = self.poll_quota_inner()?;
        let status = self.poll_status_inner(super::now_ms())?;
        Ok(format!(
            "status={:?} model={} pools={} remaining={:?} context={:?} tps={:?}",
            status.status,
            self.model.name.as_deref().unwrap_or("-"),
            quota.pools.len(),
            quota.remaining_pct,
            status.context.used_pct,
            status.speed.tokens_per_sec
        ))
    }
}

impl Default for AntigravityAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryAdapter for AntigravityAdapter {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn capabilities(&self) -> TelemetryCapabilities {
        self.capabilities.clone()
    }

    fn poll_status(&mut self, now_ms: i64) -> Result<StatusSnapshot, TelemetryError> {
        self.poll_status_inner(now_ms)
    }

    fn poll_quota(&mut self, _now_ms: i64) -> Result<TelemetryQuotaInfo, TelemetryError> {
        self.poll_quota_inner()
    }
}

fn parse_json_body(body: &str) -> Result<Value, TelemetryError> {
    if body.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str::<Value>(body).map_err(|_| {
        TelemetryError::new(
            None,
            TelemetryErrorKind::Unknown,
            "Antigravity 本地服务返回了无法解析的数据",
        )
    })
}

fn escape_json(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/* ------------------------------------------------------------------ */
/* JSON 解析（全部是纯函数，便于单测）                                 */
/* ------------------------------------------------------------------ */

fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_f64().map(|value| value.max(0.0) as u64)),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn get<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    Some(cursor)
}

fn first_object_with_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                if found.is_object() {
                    return Some(found);
                }
                if let Some(array) = found.as_array() {
                    if let Some(last) = array.iter().rev().find(|item| item.is_object()) {
                        return Some(last);
                    }
                }
            }
            for child in map.values() {
                if let Some(found) = first_object_with_key(child, key) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .rev()
            .find_map(|item| first_object_with_key(item, key)),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrajectorySummary {
    pub id: String,
    pub status: String,
    pub step_count: u32,
    pub last_modified_ms: i64,
    pub workspace_name: Option<String>,
}

fn leaf_workspace_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches(&['/', '\\'][..]);
    if trimmed.is_empty() {
        return None;
    }
    let leaf = trimmed
        .rsplit(&['/', '\\'][..])
        .next()
        .unwrap_or("")
        .trim();
    if leaf.is_empty() {
        return None;
    }
    let clean: String = leaf.chars().take(24).collect();
    Some(clean)
}

fn extract_workspace_name(item: &Value) -> Option<String> {
    if let Some(ws_list) = item.get("workspaces").and_then(Value::as_array) {
        for ws in ws_list {
            let cand = get_str(ws, &["workspaceFolderAbsoluteUri"])
                .or_else(|| get_str(ws, &["repository", "computedName"]))
                .or_else(|| get_str(ws, &["gitRootAbsoluteUri"]))
                .or_else(|| get_str(ws, &["name"]))
                .or_else(|| ws.as_str().map(str::to_string));
            if let Some(name) = cand.and_then(|s| leaf_workspace_name(&s)) {
                return Some(name);
            }
        }
    }
    get_str(item, &["workspaceUri"])
        .or_else(|| get_str(item, &["workspacePath"]))
        .or_else(|| get_str(item, &["cwd"]))
        .and_then(|s| leaf_workspace_name(&s))
}

fn value_items(value: Option<&Value>) -> Vec<(Option<String>, &Value)> {
    match value {
        Some(Value::Array(items)) => items.iter().map(|item| (None, item)).collect(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(key, item)| (Some(key.clone()), item))
            .collect(),
        _ => Vec::new(),
    }
}

pub fn parse_trajectories(value: &Value) -> Vec<TrajectorySummary> {
    let container = value
        .get("trajectorySummaries")
        .or_else(|| value.get("trajectories"))
        .unwrap_or(value);
    let mut out = Vec::new();
    for (key, item) in value_items(Some(container)) {
        if !item.is_object() {
            continue;
        }
        // trajectorySummaries 是 map 时，键就是后续 RPC 需要的 cascadeId；
        // 值里的 trajectoryId 可能是另一套内部 ID，不能优先使用。
        let id = key
            .or_else(|| get_str(item, &["cascadeId"]))
            .or_else(|| get_str(item, &["trajectoryId"]))
            .or_else(|| get_str(item, &["id"]))
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let status = get_str(item, &["status"]).unwrap_or_default();
        let step_count = get_u64(item, &["stepCount"])
            .or_else(|| get_u64(item, &["step_count"]))
            .unwrap_or(0) as u32;
        let last_modified_ms = get_str(item, &["lastModifiedTime"])
            .and_then(|text| parse_rfc3339_ms(&text))
            .or_else(|| get_i64(item, &["lastModifiedTime"]))
            .unwrap_or(0);
        let workspace_name = extract_workspace_name(item);
        out.push(TrajectorySummary {
            id,
            status,
            step_count,
            last_modified_ms,
            workspace_name,
        });
    }
    out
}

pub fn count_running_trajectories(items: &[TrajectorySummary]) -> u32 {
    items
        .iter()
        .filter(|item| {
            let status = item.status.to_ascii_uppercase();
            status.contains("RUNNING")
                || status.contains("WAITING")
                || status.contains("GENERATING")
                || status.contains("IN_PROGRESS")
        })
        .count() as u32
}

/// 优先等待确认 / 最新运行中轨迹；否则取最后修改时间最新的一条。
pub fn pick_active_trajectory(items: &[TrajectorySummary]) -> Option<TrajectorySummary> {
    let running = items
        .iter()
        .filter(|item| {
            let status = item.status.to_ascii_uppercase();
            status.contains("RUNNING")
                || status.contains("WAITING")
                || status.contains("GENERATING")
                || status.contains("IN_PROGRESS")
        })
        .max_by_key(|item| {
            let status = item.status.to_ascii_uppercase();
            (status.contains("WAITING"), item.last_modified_ms)
        });
    running.cloned().or_else(|| {
        items
            .iter()
            .max_by_key(|item| item.last_modified_ms)
            .cloned()
    })
}

pub fn label_for_step_type(step_type: &str) -> &'static str {
    let upper = step_type.to_ascii_uppercase();
    if upper.contains("RUN_COMMAND") {
        "执行命令"
    } else if upper.contains("SEARCH_WEB") {
        "搜索网页"
    } else if upper.contains("BROWSER") {
        "操作浏览器"
    } else if upper.contains("GREP") || upper.contains("SEARCH") {
        "检索代码"
    } else if upper.contains("LIST_DIRECTORY") || upper.contains("LIST_DIR") {
        "浏览目录"
    } else if upper.contains("VIEW_FILE") || upper.contains("READ_FILE") {
        "读取文件"
    } else if upper.contains("CODE_ACTION") || upper.contains("EDIT") {
        "修改代码"
    } else if upper.contains("PLANNER_RESPONSE") || upper.contains("PLANNER") {
        "生成中"
    } else if upper.contains("USER_INPUT") {
        "等待输入"
    } else {
        "执行工具"
    }
}

#[derive(Debug, Clone, Default)]
pub struct StepObservation {
    pub status: TelemetryStatus,
    pub active_step_label: Option<String>,
    pub activity: AgentActivityKind,
    pub step_detail: Option<String>,
    pub streaming_step_key: Option<String>,
    pub streaming_exact_tokens: Option<u64>,
    pub streaming_est_tokens: Option<f64>,
    pub streaming_thinking_only: bool,
}

pub fn estimate_tokens_from_text(text: &str) -> f64 {
    let mut total = 0.0;
    for ch in text.chars() {
        if ch.is_ascii() {
            total += 0.25;
        } else {
            total += 0.65;
        }
    }
    total
}

fn estimate_value_strings(value: &Value) -> f64 {
    match value {
        Value::String(text) => estimate_tokens_from_text(text),
        Value::Array(items) => items.iter().map(estimate_value_strings).sum(),
        Value::Object(map) => map
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "id" | "callId" | "toolCallId"))
            .map(|(_, v)| estimate_value_strings(v))
            .sum(),
        _ => 0.0,
    }
}

struct PlannerProgress {
    exact_tokens: Option<u64>,
    thinking_est: f64,
    output_est: f64,
}

fn inspect_planner_progress(step: &Value) -> PlannerProgress {
    let usage_candidates = [
        get(step, &["plannerResponse", "usage"]),
        get(step, &["metadata", "usage"]),
        get(step, &["usage"]),
        get(step, &["chatModel", "usage"]),
        get(step, &["plannerResponse"]),
    ];
    let exact_tokens = usage_candidates.into_iter().flatten().find_map(|cand| {
        get_u64(cand, &["outputTokens"])
            .or_else(|| get_u64(cand, &["completionTokens"]))
            .or_else(|| get_u64(cand, &["outputTokenCount"]))
    });

    let container = step.get("plannerResponse").unwrap_or(step);
    let thinking_est = get_str(container, &["thinking"])
        .or_else(|| get_str(container, &["thought"]))
        .or_else(|| get_str(container, &["thinkingContent"]))
        .or_else(|| get_str(container, &["reasoning"]))
        .or_else(|| get_str(container, &["reasoningContent"]))
        .or_else(|| get_str(step, &["thinking"]))
        .or_else(|| get_str(step, &["thought"]))
        .or_else(|| get_str(step, &["thinkingContent"]))
        .or_else(|| get_str(step, &["reasoning"]))
        .or_else(|| get_str(step, &["reasoningContent"]))
        .map(|s| estimate_tokens_from_text(&s))
        .unwrap_or(0.0);

    let resp_est = get_str(container, &["response"])
        .or_else(|| get_str(step, &["response"]))
        .map(|s| estimate_tokens_from_text(&s))
        .unwrap_or(0.0);
    let mod_resp_est = get_str(container, &["modifiedResponse"])
        .or_else(|| get_str(step, &["modifiedResponse"]))
        .map(|s| estimate_tokens_from_text(&s))
        .unwrap_or(0.0);
    let text_fallback_est = if resp_est == 0.0 && mod_resp_est == 0.0 {
        get_str(container, &["text"])
            .or_else(|| get_str(container, &["content"]))
            .or_else(|| get_str(container, &["partialResponse"]))
            .or_else(|| get_str(container, &["markdown"]))
            .or_else(|| get_str(container, &["message"]))
            .map(|s| estimate_tokens_from_text(&s))
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let response_tokens = resp_est.max(mod_resp_est).max(text_fallback_est);

    let tool_calls_tokens = container
        .get("toolCalls")
        .or_else(|| step.get("toolCalls"))
        .map(estimate_value_strings)
        .unwrap_or(0.0);

    PlannerProgress {
        exact_tokens,
        thinking_est,
        output_est: response_tokens + tool_calls_tokens,
    }
}

pub fn extract_step_token_progress(step: &Value) -> (Option<u64>, Option<f64>) {
    let p = inspect_planner_progress(step);
    let total = p.thinking_est + p.output_est;
    let est = if total > 0.0 { Some(total) } else { None };
    (p.exact_tokens, est)
}

fn classify_waiting_step(step: &Value, step_type_upper: &str) -> (AgentActivityKind, &'static str) {
    if step_type_upper.contains("NOTIFY_USER")
        || step_type_upper.contains("ARTIFACT")
        || step_type_upper.contains("WALKTHROUGH")
        || step_type_upper.contains("PLAN")
    {
        return (AgentActivityKind::ArtifactReviewReady, "等待审阅");
    }
    if step_type_upper.contains("VIEW_FILE")
        || step_type_upper.contains("READ_FILE")
        || step_type_upper.contains("WRITE_TO_FILE")
        || step_type_upper.contains("LIST_DIR")
        || step_type_upper.contains("OUTSIDE")
        || step.get("outsideWorkspace").and_then(Value::as_bool) == Some(true)
    {
        return (AgentActivityKind::ApprovalOutsideDir, "请求外部权限");
    }
    (AgentActivityKind::ApprovalTerminal, "等待确认")
}

fn extract_mcp_detail(step: &Value, step_type: &str) -> Option<String> {
    let raw = get_str(step, &["mcp", "serverName"])
        .or_else(|| get_str(step, &["mcp", "toolName"]))
        .or_else(|| get_str(step, &["toolName"]))
        .or_else(|| get_str(step, &["metadata", "toolName"]))
        .or_else(|| {
            step_type
                .strip_prefix("CASCADE_STEP_TYPE_MCP_")
                .map(str::to_string)
        })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(18).collect())
    }
}

fn step_targets_plan_doc(step: &Value) -> bool {
    let paths = [
        get_str(step, &["writeToFile", "targetFile"]),
        get_str(step, &["codeAction", "targetFile"]),
        get_str(step, &["targetFile"]),
        get_str(step, &["metadata", "targetFile"]),
    ];
    paths.into_iter().flatten().any(|p| {
        let low = p.to_ascii_lowercase();
        low.contains("implementation_plan") || low.contains("task.md") || low.contains("walkthrough")
    })
}

fn is_known_tool_step(step_type_upper: &str) -> bool {
    const TOOL_TOKENS: &[&str] = &[
        "RUN_COMMAND",
        "COMMAND_STATUS",
        "CODE_ACTION",
        "EDIT",
        "REPLACE",
        "WRITE_TO_FILE",
        "VIEW_FILE",
        "READ_FILE",
        "GREP",
        "SEARCH",
        "FIND",
        "LIST_DIRECTORY",
        "LIST_DIR",
        "KNOWLEDGE",
        "BROWSER",
        "MCP",
        "WORKFLOW",
        "SKILL",
        "TASK_BOUNDARY",
    ];
    TOOL_TOKENS.iter().any(|tok| step_type_upper.contains(tok))
}

fn classify_tool_step(
    step: &Value,
    step_type: &str,
    step_type_upper: &str,
) -> (AgentActivityKind, &'static str, Option<String>) {
    if step_type_upper.contains("BROWSER") {
        return (AgentActivityKind::BrowserRunning, "浏览器巡检", None);
    }
    if step_type_upper.contains("MCP") {
        return (
            AgentActivityKind::McpToolCalling,
            "MCP 工具",
            extract_mcp_detail(step, step_type),
        );
    }
    if step_type_upper.contains("WORKFLOW") || step_type_upper.contains("SKILL") {
        return (AgentActivityKind::WorkflowRunning, "执行工作流", None);
    }
    if step_type_upper.contains("TASK_BOUNDARY")
        || step_type_upper.contains("PLAN")
        || step_targets_plan_doc(step)
    {
        return (AgentActivityKind::PlanningMode, "规划方案", None);
    }
    if step_type_upper.contains("CODE_ACTION")
        || step_type_upper.contains("EDIT")
        || step_type_upper.contains("REPLACE")
        || step_type_upper.contains("WRITE_TO_FILE")
    {
        return (AgentActivityKind::CodeDiffing, "修改代码", None);
    }
    if step_type_upper.contains("RUN_COMMAND") || step_type_upper.contains("COMMAND_STATUS") {
        return (AgentActivityKind::TerminalRunning, "执行命令", None);
    }
    if step_type_upper.contains("SEARCH_WEB")
        || step_type_upper.contains("WEB_SEARCH")
        || step_type_upper.contains("READ_URL")
    {
        return (
            AgentActivityKind::ResearchLookup,
            "搜索网页",
            Some("web_search".to_string()),
        );
    }
    if step_type_upper.contains("GREP")
        || step_type_upper.contains("SEARCH")
        || step_type_upper.contains("FIND")
        || step_type_upper.contains("VIEW_FILE")
        || step_type_upper.contains("READ_FILE")
        || step_type_upper.contains("LIST_DIRECTORY")
        || step_type_upper.contains("LIST_DIR")
        || step_type_upper.contains("KNOWLEDGE")
    {
        return (
            AgentActivityKind::ResearchLookup,
            label_for_step_type(step_type),
            None,
        );
    }
    (
        AgentActivityKind::TerminalRunning,
        label_for_step_type(step_type),
        None,
    )
}

/// 根据末尾 steps + 轨迹状态判定精细状态机，并提取实时吐字进度。
///
/// 关键修复：绝不把末尾之前已经过去的历史 `ERROR`/`FAILED` 工具步当成当前「连接异常」。
pub fn observe_step_state(
    steps: &Value,
    trajectory_status: &str,
    start_index: u32,
) -> StepObservation {
    let items = value_items(steps.get("steps").or_else(|| steps.get("stepList")));
    if items.is_empty() && !steps.is_object() {
        let (status, active_step_label, activity) = fallback_state(trajectory_status);
        return StepObservation {
            status,
            active_step_label,
            activity,
            ..StepObservation::default()
        };
    }

    // 第一遍：仅倒序查找真正处于 WAITING 或运行中/排队中的活跃步骤（忽略历史已失败旧步）。
    for (idx, (_, step)) in items.iter().enumerate().rev() {
        if !step.is_object() {
            continue;
        }
        let step_type = get_str(step, &["type"]).unwrap_or_default();
        let step_type_upper = step_type.to_ascii_uppercase();
        let status = get_str(step, &["status"])
            .unwrap_or_default()
            .to_ascii_uppercase();

        if status.contains("WAITING") {
            let (activity, label) = classify_waiting_step(step, &step_type_upper);
            return StepObservation {
                status: TelemetryStatus::WaitingApproval,
                active_step_label: Some(label.to_string()),
                activity,
                ..StepObservation::default()
            };
        }

        let is_active_step = status.contains("RUNNING")
            || status.contains("IN_PROGRESS")
            || status.contains("GENERATING")
            || status.contains("STREAMING")
            || status.contains("EXECUTING")
            || status.contains("STARTING")
            || status.contains("PENDING")
            || status.contains("QUEUED");
        if is_active_step {
            let is_explicit_planner = step_type_upper.contains("PLANNER_RESPONSE")
                || step_type_upper.contains("PLANNER")
                || step.get("plannerResponse").is_some();
            let is_streaming = is_explicit_planner
                || ((status.contains("GENERATING") || status.contains("STREAMING"))
                    && !is_known_tool_step(&step_type_upper));
            if is_streaming {
                let step_key = get_str(step, &["stepId"])
                    .or_else(|| get_str(step, &["id"]))
                    .unwrap_or_else(|| {
                        format!("{}:{}", start_index.saturating_add(idx as u32), step_type)
                    });
                let progress = inspect_planner_progress(step);
                let total_est = progress.thinking_est + progress.output_est;
                let thinking_only = progress.thinking_est > 0.05
                    && progress.output_est <= 0.05
                    && progress.exact_tokens.is_none();
                let (activity, label) =
                    if progress.output_est > 0.05 || progress.exact_tokens.is_some() {
                        (AgentActivityKind::CodeStreaming, "生成中")
                    } else {
                        (AgentActivityKind::DeepReasoning, "深度思考")
                    };
                return StepObservation {
                    status: TelemetryStatus::Streaming,
                    active_step_label: Some(label.to_string()),
                    activity,
                    step_detail: None,
                    streaming_step_key: Some(step_key),
                    streaming_exact_tokens: progress.exact_tokens,
                    streaming_est_tokens: (total_est > 0.0).then_some(total_est),
                    streaming_thinking_only: thinking_only,
                };
            }
            let (activity, label, step_detail) =
                classify_tool_step(step, &step_type, &step_type_upper);
            return StepObservation {
                status: TelemetryStatus::ToolRunning,
                active_step_label: Some(label.to_string()),
                activity,
                step_detail,
                ..StepObservation::default()
            };
        }
    }

    // 第二遍：无等待/运行中步骤时，仅考察最后一步是否刚刚失败（且不再错误映射为连接异常）。
    let traj_upper = trajectory_status.to_ascii_uppercase();
    let traj_running = traj_upper.contains("RUNNING")
        || traj_upper.contains("IN_PROGRESS")
        || traj_upper.contains("GENERATING")
        || traj_upper.contains("STREAMING");
    let traj_failed = traj_upper.contains("ERROR") || traj_upper.contains("FAILED");

    if let Some((_, last_step)) = items.iter().rev().find(|(_, s)| s.is_object()) {
        let last_type = get_str(last_step, &["type"])
            .unwrap_or_default()
            .to_ascii_uppercase();
        let last_status = get_str(last_step, &["status"])
            .unwrap_or_default()
            .to_ascii_uppercase();
        if last_status.contains("ERROR") || last_status.contains("FAILED") {
            let is_cmd = last_type.contains("RUN_COMMAND") || last_type.contains("COMMAND");
            if traj_running {
                // 轨迹仍在运行：仅代表刚有工具/命令返回非零退出码，Agent 正准备分析，不标红 Error。
                return StepObservation {
                    status: TelemetryStatus::ToolRunning,
                    active_step_label: Some(if is_cmd { "命令报错" } else { "步骤重试" }.to_string()),
                    activity: AgentActivityKind::TerminalError,
                    ..StepObservation::default()
                };
            }
            if traj_failed {
                return StepObservation {
                    status: TelemetryStatus::Error,
                    active_step_label: Some("步骤失败".to_string()),
                    activity: if is_cmd {
                        AgentActivityKind::TerminalError
                    } else {
                        AgentActivityKind::ApiAuthRateError
                    },
                    ..StepObservation::default()
                };
            }
        }
    }

    let (status, active_step_label, activity) = fallback_state(trajectory_status);
    StepObservation {
        status,
        active_step_label,
        activity,
        ..StepObservation::default()
    }
}

/// 根据末尾 steps + 轨迹状态判定精细状态机。
pub fn parse_step_state(
    steps: &Value,
    trajectory_status: &str,
) -> (TelemetryStatus, Option<String>) {
    let obs = observe_step_state(steps, trajectory_status, 0);
    (obs.status, obs.active_step_label)
}

fn fallback_state(trajectory_status: &str) -> (TelemetryStatus, Option<String>, AgentActivityKind) {
    let upper = trajectory_status.to_ascii_uppercase();
    if upper.contains("WAITING") {
        (
            TelemetryStatus::WaitingApproval,
            Some("等待确认".to_string()),
            AgentActivityKind::ApprovalTerminal,
        )
    } else if upper.contains("ERROR") || upper.contains("FAILED") {
        (
            TelemetryStatus::Error,
            Some("步骤失败".to_string()),
            AgentActivityKind::ApiAuthRateError,
        )
    } else if upper.contains("RUNNING")
        || upper.contains("IN_PROGRESS")
        || upper.contains("GENERATING")
        || upper.contains("STREAMING")
    {
        (
            TelemetryStatus::Streaming,
            Some("深度思考".to_string()),
            AgentActivityKind::DeepReasoning,
        )
    } else {
        (TelemetryStatus::Idle, None, AgentActivityKind::Idle)
    }
}

#[derive(Debug, Clone, Default)]
pub struct GeneratorSample {
    pub model_name: Option<String>,
    pub input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub streaming_duration_ms: Option<u64>,
    pub metadata_count: usize,
}

pub fn parse_generator_metadata(value: &Value) -> Option<GeneratorSample> {
    // 实测真实响应：`generatorMetadata` 是数组，元素本身就是 chatModel，
    // 并没有再包一层 `chatModel` 字段；同时兼容旧假设的嵌套形态。
    let chat_model = first_object_with_key(value, "chatModel").or_else(|| {
        value.get("generatorMetadata").and_then(|meta| match meta {
            Value::Array(items) => items.iter().rev().find(|item| item.is_object()),
            Value::Object(map) => map.values().rev().find(|item| item.is_object()),
            _ => None,
        })
    })?;

    let metadata_count = match value.get("generatorMetadata") {
        Some(Value::Array(items)) => items.len(),
        Some(Value::Object(map)) => map.len(),
        _ => usize::from(chat_model.is_object()),
    };

    let model_name = get_str(chat_model, &["modelDisplayName"])
        .or_else(|| get_str(chat_model, &["modelTitle"]))
        .or_else(|| get_str(chat_model, &["displayName"]))
        .or_else(|| get_str(chat_model, &["model"]));

    let usage = chat_model.get("usage").unwrap_or(chat_model);
    Some(GeneratorSample {
        model_name: model_name.filter(|name| !name.trim().is_empty()),
        input_tokens: get_u64(usage, &["inputTokens"])
            .or_else(|| get_u64(usage, &["promptTokens"])),
        cache_read_tokens: get_u64(usage, &["cacheReadTokens"])
            .or_else(|| get_u64(usage, &["cachedInputTokens"])),
        output_tokens: get_u64(usage, &["outputTokens"])
            .or_else(|| get_u64(usage, &["completionTokens"])),
        // 实测 TTFT / streamingDuration 挂在 chatModel 顶层，而不是 usage 内。
        ttft_ms: get(chat_model, &["timeToFirstToken"])
            .or_else(|| get(usage, &["timeToFirstToken"]))
            .and_then(parse_duration_ms),
        streaming_duration_ms: get(chat_model, &["streamingDuration"])
            .or_else(|| get(usage, &["streamingDuration"]))
            .or_else(|| get(usage, &["generationDuration"]))
            .and_then(parse_duration_ms),
        metadata_count,
    })
}

/// 解析 Protobuf Duration JSON（`"1.23s"`）以及常见 `"123ms"` / `"0.8s"`。
pub fn parse_duration_ms(value: &Value) -> Option<u64> {
    if let Some(object) = value.as_object() {
        // 兼容 protobuf Duration 的结构化形态 {"seconds":"1","nanos":230000000}。
        let seconds = object.get("seconds").and_then(as_f64)?;
        let nanos = object.get("nanos").and_then(as_f64).unwrap_or(0.0);
        return Some((seconds * 1000.0 + nanos / 1_000_000.0).round() as u64);
    }
    if let Some(number) = value.as_f64() {
        if number <= 0.0 {
            return Some(0);
        }
        // 裸数字按毫秒处理；> 600 视为已经是毫秒。
        return Some(number.round() as u64);
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    if let Some(rest) = lower.strip_suffix("ms") {
        return rest.trim().parse::<f64>().ok().map(|v| v.round() as u64);
    }
    if let Some(rest) = lower.strip_suffix('s') {
        if let Some((minutes, seconds)) = rest.split_once('m') {
            let minutes = minutes.trim().parse::<f64>().ok()?;
            let seconds = seconds.trim().parse::<f64>().ok()?;
            return Some(((minutes * 60.0 + seconds) * 1000.0).round() as u64);
        }
        return rest
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v * 1000.0).round() as u64);
    }
    if let Some(rest) = lower.strip_suffix('m') {
        return rest
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v * 60_000.0).round() as u64);
    }
    lower
        .parse::<f64>()
        .ok()
        .map(|v| (v * 1000.0).round() as u64)
}

pub fn context_limit_for(model_name: &str) -> Option<u64> {
    let lower = model_name.to_ascii_lowercase();
    if lower.contains("gemini") {
        Some(512_000)
    } else if lower.contains("claude") {
        Some(200_000)
    } else if lower.contains("gpt") || lower.contains("o3") || lower.contains("o4") {
        Some(200_000)
    } else if lower.is_empty() {
        None
    } else {
        Some(128_000)
    }
}

/// 解析 `GetUserStatus` → 按模型系列聚合的池余量。
pub fn parse_user_status(value: &Value, active_model: Option<&str>) -> TelemetryQuotaInfo {
    let configs = find_array(value, "clientModelConfigs")
        .or_else(|| find_array(value, "modelConfigs"))
        .unwrap_or_default();

    struct Entry {
        label: String,
        remaining: f64,
        reset_at_ms: Option<i64>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    for config in configs {
        if !config.is_object() {
            continue;
        }
        let label = get_str(config, &["label"])
            .or_else(|| get_str(config, &["displayName"]))
            .or_else(|| get_str(config, &["modelDisplayName"]))
            .or_else(|| get_str(config, &["modelId"]))
            .or_else(|| get_str(config, &["model"]))
            .unwrap_or_else(|| "Model".to_string());
        let quota = config.get("quotaInfo").unwrap_or(config);
        let remaining = get(quota, &["remainingFraction"])
            .and_then(as_f64)
            .or_else(|| get(quota, &["remaining_fraction"]).and_then(as_f64));
        let Some(remaining) = remaining else {
            continue;
        };
        let pct = if remaining <= 1.0 {
            remaining * 100.0
        } else {
            remaining
        }
        .clamp(0.0, 100.0);
        let reset_at_ms = get_str(quota, &["resetTime"])
            .and_then(|text| parse_rfc3339_ms(&text))
            .or_else(|| get_i64(quota, &["resetTime"]));
        entries.push(Entry {
            label,
            remaining: pct,
            reset_at_ms,
        });
    }

    if entries.is_empty() {
        return TelemetryQuotaInfo::default();
    }

    // 按系列聚合成池（Gemini / Claude / GPT / 其他）。
    let mut families: Vec<(String, String, f64, Option<i64>, bool)> = Vec::new();
    for entry in &entries {
        let (id, label) = pool_family(&entry.label);
        let matches_active = active_model
            .map(|active| label_matches(active, &entry.label))
            .unwrap_or(false);
        match families.iter_mut().find(|item| item.0 == id) {
            Some(pool) => {
                pool.2 = pool.2.min(entry.remaining);
                pool.3 = merge_reset(pool.3, entry.reset_at_ms);
                pool.4 = pool.4 || matches_active;
            }
            None => families.push((
                id,
                label,
                entry.remaining,
                entry.reset_at_ms,
                matches_active,
            )),
        }
    }

    let active_index = families.iter().position(|item| item.4).unwrap_or(0);
    let pools: Vec<TelemetryQuotaPool> = families
        .iter()
        .enumerate()
        .map(
            |(index, (id, label, remaining, reset_at_ms, _))| TelemetryQuotaPool {
                id: id.clone(),
                label: label.clone(),
                remaining_pct: *remaining,
                reset_at_ms: *reset_at_ms,
                is_active: index == active_index,
            },
        )
        .collect();

    let active = &pools[active_index];
    TelemetryQuotaInfo {
        remaining_pct: Some(active.remaining_pct),
        reset_at_ms: active.reset_at_ms,
        pools,
        confidence: TelemetryConfidence::Exact,
    }
}

fn pool_family(label: &str) -> (String, String) {
    let lower = label.to_ascii_lowercase();
    if lower.contains("gemini") {
        ("gemini".to_string(), "Gemini".to_string())
    } else if lower.contains("claude") {
        ("claude".to_string(), "Claude".to_string())
    } else if lower.contains("gpt") || lower.contains("openai") {
        ("gpt".to_string(), "GPT".to_string())
    } else {
        ("other".to_string(), "其他".to_string())
    }
}

fn label_matches(active: &str, label: &str) -> bool {
    let active = active.to_ascii_lowercase();
    let label = label.to_ascii_lowercase();
    active == label || active.contains(&label) || label.contains(&active)
}

fn merge_reset(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn find_array<'a>(value: &'a Value, key: &str) -> Option<Vec<&'a Value>> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key).and_then(Value::as_array) {
                return Some(found.iter().collect());
            }
            for child in map.values() {
                if let Some(found) = find_array(child, key) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|item| find_array(item, key)),
        _ => None,
    }
}

fn get_str(value: &Value, path: &[&str]) -> Option<String> {
    get(value, path)
        .and_then(Value::as_str)
        .map(|text| text.to_string())
}

fn get_u64(value: &Value, path: &[&str]) -> Option<u64> {
    get(value, path).and_then(as_u64)
}

fn get_i64(value: &Value, path: &[&str]) -> Option<i64> {
    get(value, path).and_then(|item| {
        item.as_i64()
            .or_else(|| item.as_f64().map(|number| number as i64))
            .or_else(|| item.as_str().and_then(|text| text.parse::<i64>().ok()))
    })
}

fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|value| value.timestamp_millis())
}

/* ------------------------------------------------------------------ */
/* 测试                                                                */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_protobuf_duration() {
        assert_eq!(parse_duration_ms(&json!("1.23s")), Some(1230));
        assert_eq!(parse_duration_ms(&json!("0.8s")), Some(800));
        assert_eq!(parse_duration_ms(&json!("123ms")), Some(123));
        assert_eq!(parse_duration_ms(&json!("1m2.5s")), Some(62_500));
        assert_eq!(parse_duration_ms(&json!(850)), Some(850));
        assert_eq!(
            parse_duration_ms(&json!({"seconds": "1", "nanos": 230000000})),
            Some(1230)
        );
        assert_eq!(parse_duration_ms(&json!("")), None);
    }

    #[test]
    fn parses_trajectory_map_and_picks_running() {
        let value = json!({
            "trajectorySummaries": {
                "idle-1": {
                    "cascadeId": "idle-1",
                    "status": "CASCADE_RUN_STATUS_IDLE",
                    "stepCount": 3,
                    "lastModifiedTime": "2026-11-22T06:00:00Z"
                },
                "run-1": {
                    "trajectoryId": "internal-run-id",
                    "status": "CASCADE_RUN_STATUS_RUNNING",
                    "stepCount": 12,
                    "lastModifiedTime": "2026-11-22T05:00:00Z"
                }
            }
        });
        let items = parse_trajectories(&value);
        assert_eq!(items.len(), 2);
        let active = pick_active_trajectory(&items).unwrap();
        // map 的键才是后续 RPC 需要的 cascadeId，不能被内部 trajectoryId 覆盖。
        assert_eq!(active.id, "run-1");
        assert_eq!(active.step_count, 12);
        assert!(active.last_modified_ms > 1_700_000_000_000);
    }

    #[test]
    fn parses_step_states() {
        let streaming = json!({
            "steps": [
                {"type": "CASCADE_STEP_TYPE_PLANNER_RESPONSE", "status": "CASCADE_STEP_STATUS_RUNNING"}
            ]
        });
        assert_eq!(
            parse_step_state(&streaming, "CASCADE_RUN_STATUS_RUNNING").0,
            TelemetryStatus::Streaming
        );

        let waiting = json!({
            "steps": [
                {"type": "CASCADE_STEP_TYPE_RUN_COMMAND", "status": "CASCADE_STEP_STATUS_WAITING"}
            ]
        });
        assert_eq!(
            parse_step_state(&waiting, "CASCADE_RUN_STATUS_RUNNING"),
            (
                TelemetryStatus::WaitingApproval,
                Some("等待确认".to_string())
            )
        );

        let tool = json!({
            "steps": [
                {"type": "CASCADE_STEP_TYPE_RUN_COMMAND", "status": "CASCADE_STEP_STATUS_RUNNING"}
            ]
        });
        assert_eq!(
            parse_step_state(&tool, "CASCADE_RUN_STATUS_RUNNING"),
            (TelemetryStatus::ToolRunning, Some("执行命令".to_string()))
        );
    }

    #[test]
    fn parses_generator_metadata_and_context() {
        let value = json!({
            "generatorMetadata": [
                {
                    "chatModel": {
                        "modelDisplayName": "Gemini 3 Pro (High)",
                        "usage": {
                            "inputTokens": 120000,
                            "cacheReadTokens": 30000,
                            "outputTokens": 900,
                            "timeToFirstToken": "0.82s",
                            "streamingDuration": "12s"
                        }
                    }
                }
            ]
        });
        let sample = parse_generator_metadata(&value).unwrap();
        assert_eq!(sample.model_name.as_deref(), Some("Gemini 3 Pro (High)"));
        assert_eq!(sample.input_tokens, Some(120000));
        assert_eq!(sample.cache_read_tokens, Some(30000));
        assert_eq!(sample.ttft_ms, Some(820));
        assert_eq!(sample.streaming_duration_ms, Some(12_000));
        assert_eq!(context_limit_for("Gemini 3 Pro (High)"), Some(512_000));
        assert_eq!(context_limit_for("Claude Sonnet 4.5"), Some(200_000));
    }

    #[test]
    fn parses_user_status_pools() {
        let value = json!({
            "userStatus": {
                "cascadeModelConfigData": {
                    "clientModelConfigs": [
                        {
                            "label": "Gemini 3 Pro",
                            "quotaInfo": {"remainingFraction": 0.42, "resetTime": "2026-11-23T00:00:00Z"}
                        },
                        {
                            "label": "Claude Sonnet 4.5",
                            "quotaInfo": {"remainingFraction": 0.08, "resetTime": "2026-11-23T01:00:00Z"}
                        },
                        {
                            "label": "GPT-5",
                            "quotaInfo": {"remainingFraction": 1, "resetTime": "2026-11-23T02:00:00Z"}
                        }
                    ]
                }
            }
        });
        let quota = parse_user_status(&value, Some("Gemini 3 Pro"));
        assert_eq!(quota.pools.len(), 3);
        assert_eq!(quota.remaining_pct, Some(42.0));
        let active = quota.pools.iter().find(|pool| pool.is_active).unwrap();
        assert_eq!(active.id, "gemini");
        assert_eq!(quota.confidence, TelemetryConfidence::Exact);
    }

    #[test]
    fn maps_http_errors_to_contract_kinds() {
        assert_eq!(error_for_status(401).kind, TelemetryErrorKind::Auth);
        assert_eq!(error_for_status(403).kind, TelemetryErrorKind::Region);
        assert_eq!(error_for_status(429).kind, TelemetryErrorKind::RateLimit);
        assert_eq!(
            error_for_connect_code("unauthenticated").map(|err| err.kind),
            Some(TelemetryErrorKind::Auth)
        );
    }

    #[test]
    fn parses_real_shape_generator_metadata() {
        // 本机真实响应：generatorMetadata 数组元素本身就是 chatModel，
        // TTFT / streamingDuration 挂在该元素顶层。
        let value = json!({
            "generatorMetadata": [
                {
                    "model": "MODEL_PLACEHOLDER_M527",
                    "modelDisplayName": "Gemini 4 Hydrogen (Low)",
                    "timeToFirstToken": "0.94s",
                    "streamingDuration": "8.5s",
                    "usage": {
                        "inputTokens": 2444,
                        "cacheReadTokens": 18089,
                        "outputTokens": 514
                    }
                }
            ]
        });
        let sample = parse_generator_metadata(&value).unwrap();
        assert_eq!(
            sample.model_name.as_deref(),
            Some("Gemini 4 Hydrogen (Low)")
        );
        assert_eq!(sample.ttft_ms, Some(940));
        assert_eq!(sample.streaming_duration_ms, Some(8_500));
        assert_eq!(sample.input_tokens, Some(2444));
    }

    #[test]
    #[ignore = "needs a live local Antigravity language_server"]
    fn live_probe_antigravity() {
        let mut adapter = AntigravityAdapter::new();
        let summary = adapter
            .probe_summary()
            .expect("live Antigravity language_server should answer");
        println!("live probe: {summary}");
        assert!(summary.contains("status="));
        // 安全铁律：探针输出不得包含邮箱或 csrfToken。
        assert!(!summary.contains('@'));
        assert!(!summary.contains("csrf"));
    }

    #[test]
    fn parses_chunked_http_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let response = parse_http_response(raw).unwrap();
        assert_eq!(response.body, "{\"a\":1}");
    }

    #[test]
    fn recognizes_generating_step_and_estimates_live_tokens() {
        let steps = json!({
            "steps": [
                {
                    "type": "CASCADE_STEP_TYPE_PLANNER_RESPONSE",
                    "status": "CASCADE_STEP_STATUS_GENERATING",
                    "plannerResponse": {
                        "thinking": "Analyzing code...",
                        "response": "你好世界！Here is a 40-char ASCII response."
                    }
                }
            ]
        });
        let obs = observe_step_state(&steps, "CASCADE_RUN_STATUS_RUNNING", 4);
        assert_eq!(obs.status, TelemetryStatus::Streaming);
        assert_eq!(obs.streaming_step_key.as_deref(), Some("4:CASCADE_STEP_TYPE_PLANNER_RESPONSE"));
        assert!(obs.streaming_est_tokens.unwrap_or(0.0) > 10.0);
    }

    #[test]
    fn speed_estimator_tracks_live_step_growth_holds_brief_pause_and_ignores_stale_meta() {
        let mut estimator = SpeedEstimator::default();
        let prev_meta = GeneratorSample {
            model_name: Some("Gemini 4".to_string()),
            output_tokens: Some(600),
            streaming_duration_ms: Some(12_000),
            metadata_count: 2,
            ..GeneratorSample::default()
        };

        // 上一轮处于 Idle 时记录 pre_stream_meta_count = 2
        let idle_obs = StepObservation {
            status: TelemetryStatus::Idle,
            ..StepObservation::default()
        };
        let idle_speed = estimator.update(TelemetryStatus::Idle, &idle_obs, Some(&prev_meta), false, 1_000);
        assert_eq!(idle_speed.tokens_per_sec, Some(0.0));

        // 开始新流式吐字，但 generatorMetadata 仍停在旧的 2 条；不应把上一轮的 50 t/s 当成本轮实时速度
        let s1 = StepObservation {
            status: TelemetryStatus::Streaming,
            active_step_label: Some("生成中".to_string()),
            activity: AgentActivityKind::CodeStreaming,
            step_detail: None,
            streaming_step_key: Some("3:PLANNER".to_string()),
            streaming_exact_tokens: None,
            streaming_est_tokens: Some(20.0),
            streaming_thinking_only: false,
        };
        let sp1 = estimator.update(TelemetryStatus::Streaming, &s1, Some(&prev_meta), false, 2_000);
        assert_eq!(sp1.tokens_per_sec, None);

        // 300ms 后文本推进 +15 tokens -> 约 50.0 t/s
        let s2 = StepObservation {
            streaming_est_tokens: Some(35.0),
            ..s1.clone()
        };
        let sp2 = estimator.update(TelemetryStatus::Streaming, &s2, Some(&prev_meta), false, 2_300);
        assert_eq!(sp2.tokens_per_sec, Some(50.0));
        assert_eq!(sp2.confidence, TelemetryConfidence::Estimated);

        // 块间短停顿 600ms 不闪零
        let sp3 = estimator.update(TelemetryStatus::Streaming, &s2, Some(&prev_meta), false, 2_900);
        assert_eq!(sp3.tokens_per_sec, Some(50.0));

        // 退出流式后严格归零
        let sp_end = estimator.update(TelemetryStatus::Idle, &idle_obs, Some(&prev_meta), false, 3_200);
        assert_eq!(sp_end.tokens_per_sec, Some(0.0));
        assert_eq!(sp_end.confidence, TelemetryConfidence::Exact);
    }

    #[test]
    fn historical_failed_step_does_not_trigger_false_error() {
        // 第 0 步工具曾报错，但第 1 步正在生成中：绝不能误报 TelemetryStatus::Error！
        let steps = json!({
            "steps": [
                {"type": "CASCADE_STEP_TYPE_RUN_COMMAND", "status": "CASCADE_STEP_STATUS_ERROR"},
                {"type": "CASCADE_STEP_TYPE_PLANNER_RESPONSE", "status": "CASCADE_STEP_STATUS_RUNNING", "plannerResponse": {"thinking": "Analyzing failure"}}
            ]
        });
        let obs = observe_step_state(&steps, "CASCADE_RUN_STATUS_RUNNING", 0);
        assert_eq!(obs.status, TelemetryStatus::Streaming);
        assert_eq!(obs.activity, AgentActivityKind::DeepReasoning);

        // 第 0 步报错，第 1 步已 DONE，轨迹仍在 RUNNING（步间衔接）：也绝不能把历史第 0 步当当前异常
        let between_steps = json!({
            "steps": [
                {"type": "CASCADE_STEP_TYPE_RUN_COMMAND", "status": "CASCADE_STEP_STATUS_ERROR"},
                {"type": "CASCADE_STEP_TYPE_VIEW_FILE", "status": "CASCADE_STEP_STATUS_DONE"}
            ]
        });
        let obs2 = observe_step_state(&between_steps, "CASCADE_RUN_STATUS_RUNNING", 0);
        assert_eq!(obs2.status, TelemetryStatus::Streaming);
        assert_eq!(obs2.activity, AgentActivityKind::DeepReasoning);
    }

    #[test]
    fn active_trajectory_with_last_command_error_reports_terminal_error_not_connection_error() {
        let steps = json!({
            "steps": [
                {"type": "CASCADE_STEP_TYPE_RUN_COMMAND", "status": "CASCADE_STEP_STATUS_ERROR"}
            ]
        });
        let obs = observe_step_state(&steps, "CASCADE_RUN_STATUS_RUNNING", 0);
        assert_eq!(obs.status, TelemetryStatus::ToolRunning);
        assert_eq!(obs.activity, AgentActivityKind::TerminalError);
    }

    #[test]
    fn classifies_fine_grained_steps_and_counts_running_agents() {
        let thinking = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_PLANNER_RESPONSE", "status": "CASCADE_STEP_STATUS_RUNNING", "plannerResponse": {"thinking": "Let me think"}}]
        });
        assert_eq!(observe_step_state(&thinking, "RUNNING", 0).activity, AgentActivityKind::DeepReasoning);

        let pending_planner = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_PLANNER_RESPONSE", "status": "CASCADE_STEP_STATUS_PENDING"}]
        });
        let pending_obs = observe_step_state(&pending_planner, "RUNNING", 0);
        assert_eq!(pending_obs.activity, AgentActivityKind::DeepReasoning);
        assert_ne!(pending_obs.activity, AgentActivityKind::TtftWaiting);

        let generating_edit = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_CODE_ACTION", "status": "CASCADE_STEP_STATUS_GENERATING"}]
        });
        let generating_edit_obs = observe_step_state(&generating_edit, "RUNNING", 0);
        assert_eq!(generating_edit_obs.status, TelemetryStatus::ToolRunning);
        assert_eq!(generating_edit_obs.activity, AgentActivityKind::CodeDiffing);

        let browser = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_BROWSER_ACTION", "status": "CASCADE_STEP_STATUS_RUNNING"}]
        });
        assert_eq!(observe_step_state(&browser, "RUNNING", 0).activity, AgentActivityKind::BrowserRunning);

        let mcp = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_MCP_TOOL", "status": "CASCADE_STEP_STATUS_RUNNING", "mcp": {"serverName": "github"}}]
        });
        let mcp_obs = observe_step_state(&mcp, "RUNNING", 0);
        assert_eq!(mcp_obs.activity, AgentActivityKind::McpToolCalling);
        assert_eq!(mcp_obs.step_detail.as_deref(), Some("github"));

        let edit = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_CODE_ACTION", "status": "CASCADE_STEP_STATUS_RUNNING"}]
        });
        assert_eq!(observe_step_state(&edit, "RUNNING", 0).activity, AgentActivityKind::CodeDiffing);

        let grep = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_GREP_SEARCH", "status": "CASCADE_STEP_STATUS_RUNNING"}]
        });
        let grep_obs = observe_step_state(&grep, "RUNNING", 0);
        assert_eq!(grep_obs.activity, AgentActivityKind::ResearchLookup);
        assert_eq!(grep_obs.step_detail, None);

        let web_search = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_SEARCH_WEB", "status": "CASCADE_STEP_STATUS_RUNNING"}]
        });
        let web_obs = observe_step_state(&web_search, "RUNNING", 0);
        assert_eq!(web_obs.activity, AgentActivityKind::ResearchLookup);
        assert_eq!(web_obs.step_detail.as_deref(), Some("web_search"));

        let notify = json!({
            "steps": [{"type": "CASCADE_STEP_TYPE_NOTIFY_USER", "status": "CASCADE_STEP_STATUS_WAITING"}]
        });
        assert_eq!(observe_step_state(&notify, "WAITING", 0).activity, AgentActivityKind::ArtifactReviewReady);

        let traj = json!({
            "trajectorySummaries": {
                "a": {"status": "CASCADE_RUN_STATUS_RUNNING", "stepCount": 1},
                "b": {"status": "CASCADE_RUN_STATUS_WAITING_FOR_USER", "stepCount": 2},
                "c": {"status": "CASCADE_RUN_STATUS_IDLE", "stepCount": 3}
            }
        });
        let list = parse_trajectories(&traj);
        assert_eq!(count_running_trajectories(&list), 2);
    }

    #[test]
    fn pick_active_trajectory_prefers_waiting_then_newest_running() {
        let items = vec![
            TrajectorySummary {
                id: "old-run".to_string(),
                status: "CASCADE_RUN_STATUS_RUNNING".to_string(),
                step_count: 5,
                last_modified_ms: 1_000,
                workspace_name: None,
            },
            TrajectorySummary {
                id: "new-run".to_string(),
                status: "CASCADE_RUN_STATUS_RUNNING".to_string(),
                step_count: 8,
                last_modified_ms: 3_000,
                workspace_name: None,
            },
            TrajectorySummary {
                id: "idle".to_string(),
                status: "CASCADE_RUN_STATUS_IDLE".to_string(),
                step_count: 20,
                last_modified_ms: 5_000,
                workspace_name: None,
            },
        ];
        assert_eq!(
            pick_active_trajectory(&items).as_ref().map(|t| t.id.as_str()),
            Some("new-run")
        );

        let with_waiting = vec![
            items[1].clone(),
            TrajectorySummary {
                id: "wait".to_string(),
                status: "CASCADE_RUN_STATUS_WAITING_FOR_USER".to_string(),
                step_count: 4,
                last_modified_ms: 2_000,
                workspace_name: None,
            },
        ];
        assert_eq!(
            pick_active_trajectory(&with_waiting)
                .as_ref()
                .map(|t| t.id.as_str()),
            Some("wait")
        );
    }
}
