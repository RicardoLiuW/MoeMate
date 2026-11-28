//! 通用 GPU 显存实时诊断模块（NVIDIA nvidia-smi + Windows AMD/Intel 性能计数器兜底 + Ollama/GPT-SoVITS 识别）。
//!
//! 硬约束：
//! - 零外部 Cargo 依赖；
//! - 仅在打开设置窗或手动刷新时按需探测，绝不后台高频轮询；
//! - Windows 子进程统一附带 `CREATE_NO_WINDOW`，绝不闪黑框。

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fish_speech::{check_api_health, FishSpeechConfig, FishSpeechRuntime};
use crate::stats::now_ms;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VramProcessInfo {
    pub pid: u32,
    pub name: String,
    pub used_mb: u64,
    /// `"gpu" | "ollama" | "moemate"`
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VramStatusPayload {
    pub available: bool,
    pub gpu_name: String,
    pub backend: String,
    pub used_mb: u64,
    pub total_mb: u64,
    pub used_pct: f64,
    pub moemate_fish_speech_running: bool,
    pub moemate_fish_speech_pid: Option<u32>,
    pub moemate_estimated_mb: u64,
    pub processes: Vec<VramProcessInfo>,
    pub advice: String,
    pub updated_at_ms: i64,
}

fn find_nvidia_smi() -> Option<String> {
    let candidates = [
        "nvidia-smi",
        r"C:\Windows\System32\nvidia-smi.exe",
        r"C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe",
    ];
    for c in candidates {
        if c == "nvidia-smi" || Path::new(c).is_file() {
            return Some(c.to_string());
        }
    }
    None
}

pub fn parse_nvidia_smi_gpu_csv(raw: &str) -> Option<(String, u64, u64)> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split(',').map(str::trim).collect();
        if parts.len() >= 3 {
            let name = parts[0].to_string();
            let used = parts[1].parse::<u64>().unwrap_or(0);
            let total = parts[2].parse::<u64>().unwrap_or(0);
            if total > 0 {
                return Some((name, used, total));
            }
        }
    }
    None
}

pub fn parse_nvidia_smi_procs_csv(raw: &str) -> Vec<VramProcessInfo> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split(',').map(str::trim).collect();
        if parts.len() >= 3 {
            let Ok(pid) = parts[0].parse::<u32>() else {
                continue;
            };
            let raw_name = parts[1];
            let short_name = Path::new(raw_name)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(raw_name)
                .to_string();
            let used_mb = parts[2].parse::<u64>().unwrap_or(0);
            if used_mb > 0 {
                out.push(VramProcessInfo {
                    pid,
                    name: short_name,
                    used_mb,
                    source: "gpu".to_string(),
                });
            }
        }
    }
    out.sort_by(|a, b| b.used_mb.cmp(&a.used_mb));
    out.truncate(8);
    out
}

fn probe_nvidia_smi() -> Option<(String, u64, u64, Vec<VramProcessInfo>)> {
    let exe = find_nvidia_smi()?;
    let mut cmd = Command::new(&exe);
    cmd.args([
        "--query-gpu=name,memory.used,memory.total",
        "--format=csv,noheader,nounits",
    ]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let gpu_raw = String::from_utf8_lossy(&out.stdout);
    let (gpu_name, used_mb, total_mb) = parse_nvidia_smi_gpu_csv(&gpu_raw)?;

    let mut p_cmd = Command::new(&exe);
    p_cmd.args([
        "--query-compute-apps=pid,process_name,used_memory",
        "--format=csv,noheader,nounits",
    ]);
    #[cfg(windows)]
    p_cmd.creation_flags(CREATE_NO_WINDOW);
    let procs = match p_cmd.output() {
        Ok(po) if po.status.success() => {
            parse_nvidia_smi_procs_csv(&String::from_utf8_lossy(&po.stdout))
        }
        _ => Vec::new(),
    };
    Some((gpu_name, used_mb, total_mb, procs))
}

#[cfg(windows)]
fn probe_windows_gpu_fallback() -> Option<(String, u64, u64, Vec<VramProcessInfo>)> {
    let ps_script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$gpuName = 'Windows GPU'
$totalBytes = 0
Get-ItemProperty 'HKLM:\SYSTEM\ControlSet001\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\00*' | ForEach-Object {
  $desc = $_.DriverDesc
  $qw = $_.'HardwareInformation.qwMemorySize'
  if ($desc -and $desc -notmatch 'Todesk|Virtual|Basic Display|Mirror' -and $qw -is [int64] -and $qw -gt $totalBytes) {
    $totalBytes = $qw
    $gpuName = [string]$desc
  }
}
$usedBytes = 0
$adapters = (Get-Counter '\GPU Adapter Memory(*)\Dedicated Usage').CounterSamples
if ($adapters) {
  $usedBytes = ($adapters | Measure-Object -Property CookedValue -Maximum).Maximum
}
$procMap = @{}
$procs = (Get-Counter '\GPU Process Memory(*)\Dedicated Usage').CounterSamples
foreach ($s in $procs) {
  if ($s.InstanceName -match 'pid_(\d+)') {
    $p = [int]$matches[1]
    if (-not $procMap.ContainsKey($p)) { $procMap[$p] = 0 }
    $procMap[$p] += [double]$s.CookedValue
  }
}
$procList = @()
foreach ($k in $procMap.Keys) {
  $mb = [math]::Round($procMap[$k] / 1MB)
  if ($mb -ge 32) {
    $pn = (Get-Process -Id $k).ProcessName
    if (-not $pn) { $pn = "pid_$k" }
    $procList += [PSCustomObject]@{ pid = $k; name = $pn; usedMb = [int64]$mb }
  }
}
$procList = $procList | Sort-Object -Property usedMb -Descending | Select-Object -First 8
[PSCustomObject]@{
  gpuName = $gpuName
  totalMb = [math]::Round($totalBytes / 1MB)
  usedMb = [math]::Round($usedBytes / 1MB)
  processes = @($procList)
} | ConvertTo-Json -Compress
"#;

    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", ps_script]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let val: Value = serde_json::from_str(raw.trim()).ok()?;
    let gpu_name = val
        .get("gpuName")
        .and_then(Value::as_str)
        .unwrap_or("Windows GPU")
        .to_string();
    let total_mb = val.get("totalMb").and_then(Value::as_u64).unwrap_or(0);
    let used_mb = val.get("usedMb").and_then(Value::as_u64).unwrap_or(0);
    let mut procs = Vec::new();
    if let Some(arr) = val.get("processes").and_then(Value::as_array) {
        for item in arr {
            let pid = item.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("gpu-app")
                .to_string();
            let mb = item.get("usedMb").and_then(Value::as_u64).unwrap_or(0);
            if mb > 0 {
                procs.push(VramProcessInfo {
                    pid,
                    name,
                    used_mb: mb,
                    source: "gpu".to_string(),
                });
            }
        }
    }
    if total_mb == 0 && used_mb == 0 {
        return None;
    }
    Some((gpu_name, used_mb, total_mb, procs))
}

#[cfg(not(windows))]
fn probe_windows_gpu_fallback() -> Option<(String, u64, u64, Vec<VramProcessInfo>)> {
    None
}

pub fn parse_ollama_ps_models(raw: &str) -> Vec<VramProcessInfo> {
    let Ok(val) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(arr) = val.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for m in arr {
        let name = m
            .get("name")
            .or_else(|| m.get("model"))
            .and_then(Value::as_str)
            .unwrap_or("ollama-model");
        let size_vram = m.get("size_vram").and_then(Value::as_u64).unwrap_or(0);
        let used_mb = size_vram / (1024 * 1024);
        if used_mb > 0 {
            out.push(VramProcessInfo {
                pid: 0,
                name: format!("Ollama · {name}"),
                used_mb,
                source: "ollama".to_string(),
            });
        }
    }
    out
}

fn probe_ollama_running() -> Vec<VramProcessInfo> {
    #[cfg(windows)]
    {
        if let Ok((status, bytes)) =
            crate::ai_voice::winhttp_client::http_get_raw(false, "127.0.0.1", 11434, "/api/ps", 300)
        {
            if (200..300).contains(&status) {
                return parse_ollama_ps_models(&String::from_utf8_lossy(&bytes));
            }
        }
    }
    Vec::new()
}

pub fn build_vram_advice(
    available: bool,
    total_mb: u64,
    used_pct: f64,
    moemate_running: bool,
    ollama_present: bool,
) -> String {
    if !available {
        return "未能读取显卡硬件显存，建议 12G 及以下显卡使用「均衡省显存」或「零显存 / 生图优先」模式。".to_string();
    }
    if total_mb > 0 && total_mb <= 12_800 && used_pct >= 75.0 {
        if moemate_running {
            return "当前 12G 级显卡显存紧张，建议点击「一键释放语音显存」或切换「零显存 / 生图优先」模式，为本地大模型/出图腾出空间。".to_string();
        }
        if ollama_present {
            return "检测到本地大模型占用了大部分显存，MoeMate 已保持低显存占用，建议维持「零显存」或「均衡省显存」模式。".to_string();
        }
        return "当前显存占用较高，建议开启「零显存 / 生图优先」或「均衡省显存」模式避免与出图/大模型冲突。".to_string();
    }
    if moemate_running && used_pct >= 70.0 {
        return "Fish Speech 语音引擎正在占用显存；如要同时生图或跑 14B 模型，可随时点击「释放语音显存」。".to_string();
    }
    if moemate_running {
        return "Fish Speech 语音引擎运行中，当前显存余量充足。".to_string();
    }
    "MoeMate 当前未占用本地语音显存（0 MB），离线语音包可 0ms 秒播。".to_string()
}

pub fn inspect_vram_status(
    fs_cfg: &FishSpeechConfig,
    fs_rt: &FishSpeechRuntime,
) -> VramStatusPayload {
    let managed_pid = fs_rt.current_pid();
    let fs_running = fs_rt.is_running() || check_api_health(&fs_cfg.api_url).unwrap_or(false);

    let (available, gpu_name, backend, used_mb, total_mb, mut processes) =
        if let Some((name, used, total, procs)) = probe_nvidia_smi() {
            (true, name, "nvidia-smi".to_string(), used, total, procs)
        } else if let Some((name, used, total, procs)) = probe_windows_gpu_fallback() {
            (true, name, "windows-perf".to_string(), used, total, procs)
        } else {
            (
                false,
                "未检测到独立 GPU 计数器".to_string(),
                "unavailable".to_string(),
                0,
                0,
                Vec::new(),
            )
        };

    let mut moemate_estimated_mb = 0u64;
    if let Some(pid) = managed_pid {
        for p in &mut processes {
            if p.pid == pid {
                p.source = "moemate".to_string();
                p.name = format!("MoeMate Fish Speech ({})", p.name);
                moemate_estimated_mb = p.used_mb;
            }
        }
    }
    if fs_running && moemate_estimated_mb == 0 {
        moemate_estimated_mb = 3600;
    }

    let ollama_procs = probe_ollama_running();
    let ollama_present = !ollama_procs.is_empty();
    for op in ollama_procs {
        processes.push(op);
    }
    processes.sort_by(|a, b| b.used_mb.cmp(&a.used_mb));
    processes.truncate(8);

    let used_pct = if total_mb > 0 {
        ((used_mb as f64 / total_mb as f64) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let advice = build_vram_advice(available, total_mb, used_pct, fs_running, ollama_present);

    VramStatusPayload {
        available,
        gpu_name,
        backend,
        used_mb,
        total_mb,
        used_pct,
        moemate_fish_speech_running: fs_running,
        moemate_fish_speech_pid: managed_pid,
        moemate_estimated_mb,
        processes,
        advice,
        updated_at_ms: now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nvidia_smi_outputs() {
        let gpu = parse_nvidia_smi_gpu_csv("NVIDIA GeForce RTX 3060, 11820, 12288\n").unwrap();
        assert_eq!(gpu.0, "NVIDIA GeForce RTX 3060");
        assert_eq!(gpu.1, 11820);
        assert_eq!(gpu.2, 12288);

        let procs = parse_nvidia_smi_procs_csv(
            "1234, C:\\Users\\test\\ollama_llama_server.exe, 9600\n5678, python.exe, 2150\n",
        );
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].pid, 1234);
        assert_eq!(procs[0].name, "ollama_llama_server.exe");
        assert_eq!(procs[0].used_mb, 9600);
    }

    #[test]
    fn parse_ollama_ps_models_extracts_vram() {
        let raw = r#"{"models":[{"name":"qwen2.5:14b-q4_K_M","size_vram":10066329600}]}"#;
        let out = parse_ollama_ps_models(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "ollama");
        assert_eq!(out[0].used_mb, 9600);
        assert!(out[0].name.contains("qwen2.5:14b-q4_K_M"));
    }

    #[test]
    fn vram_advice_for_12gb_tight_gpu() {
        let adv = build_vram_advice(true, 12288, 92.0, true, false);
        assert!(adv.contains("12G"));
        assert!(adv.contains("零显存"));
    }
}
