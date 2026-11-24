//! 本地开源语音引擎（GPT-SoVITS v2ProPlus / 自定义 API）管理与调用模块。
//!
//! 硬约束：
//! - 零外部 Cargo 依赖（纯 Rust MD5 + WinHTTP 流式断点续传 + Win32 原生文件/目录选择）；
//! - 主程序与发行包内严禁内置 PyTorch 或模型权重，所有运行态文件隔离在独立目录（默认 `./runtime_engines/gpt-sovits/`）；
//! - 不修改 11 个 Tauri command 契约，全部控制流通过 `settings_set` 与内部事件广播。

use std::collections::HashMap;
use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/* ------------------------------------------------------------------ */
/* 数据模型                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GptSovitsCharProfile {
    #[serde(default)]
    pub ref_audio_path: String,
    #[serde(default)]
    pub prompt_text: String,
    #[serde(default = "default_prompt_lang")]
    pub prompt_lang: String,
    #[serde(default = "default_text_lang")]
    pub text_lang: String,
    #[serde(default = "default_speed_factor")]
    pub speed_factor: f64,
    #[serde(default)]
    pub gpt_weights_path: String,
    #[serde(default)]
    pub sovits_weights_path: String,
}

fn default_prompt_lang() -> String {
    "zh".to_string()
}

fn default_text_lang() -> String {
    "auto".to_string()
}

fn default_speed_factor() -> f64 {
    1.0
}

impl Default for GptSovitsCharProfile {
    fn default() -> Self {
        Self {
            ref_audio_path: String::new(),
            prompt_text: String::new(),
            prompt_lang: default_prompt_lang(),
            text_lang: default_text_lang(),
            speed_factor: default_speed_factor(),
            gpt_weights_path: String::new(),
            sovits_weights_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GptSovitsConfig {
    #[serde(default)]
    pub install_dir: String,
    #[serde(default = "default_api_url")]
    pub api_url: String,
    #[serde(default = "default_download_source")]
    pub download_source: String,
    #[serde(default = "default_gpu_backend")]
    pub gpu_backend: String,
    #[serde(default)]
    pub auto_start_service: bool,
    #[serde(default = "default_true")]
    pub auto_stop_on_exit: bool,
    #[serde(default = "default_true")]
    pub daily_dynamic_voice: bool,
    #[serde(default)]
    pub char_profiles: HashMap<String, GptSovitsCharProfile>,
}

fn default_api_url() -> String {
    "http://127.0.0.1:9880".to_string()
}

fn default_download_source() -> String {
    "mirror".to_string()
}

fn default_gpu_backend() -> String {
    "nvidia".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for GptSovitsConfig {
    fn default() -> Self {
        Self {
            install_dir: String::new(),
            api_url: default_api_url(),
            download_source: default_download_source(),
            gpu_backend: default_gpu_backend(),
            auto_start_service: false,
            auto_stop_on_exit: true,
            daily_dynamic_voice: true,
            char_profiles: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GptSovitsStatusPayload {
    /// `"uninstalled" | "installing" | "ready" | "running"`
    pub state: String,
    pub label: String,
    pub install_dir: String,
    pub api_url: String,
    pub missing_files: Vec<String>,
    pub managed_pid: Option<u32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GptSovitsProgressPayload {
    pub active: bool,
    /// `"downloading" | "extracting" | "done" | "cancelled" | "error"`
    pub stage: String,
    pub current_file: String,
    pub file_index: usize,
    pub file_total: usize,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub percent: f64,
    pub speed_bps: u64,
    pub message: String,
}

#[derive(Debug)]
pub struct GptSovitsRuntime {
    pub process: Mutex<Option<Child>>,
    pub installing: AtomicBool,
    pub cancel_install: AtomicBool,
    pub last_gpt_weights: Mutex<Option<String>>,
    pub last_sovits_weights: Mutex<Option<String>>,
    pub last_error: Mutex<Option<String>>,
    pub last_used_ms: AtomicI64,
}

impl GptSovitsRuntime {
    pub fn mark_used(&self, now_ms: i64) {
        self.last_used_ms.store(now_ms, Ordering::Relaxed);
    }

    pub fn last_used(&self) -> i64 {
        self.last_used_ms.load(Ordering::Relaxed)
    }
}

impl Default for GptSovitsRuntime {
    fn default() -> Self {
        Self {
            process: Mutex::new(None),
            installing: AtomicBool::new(false),
            cancel_install: AtomicBool::new(false),
            last_gpt_weights: Mutex::new(None),
            last_sovits_weights: Mutex::new(None),
            last_error: Mutex::new(None),
            last_used_ms: AtomicI64::new(0),
        }
    }
}

/* ------------------------------------------------------------------ */
/* 纯 Rust RFC 1321 MD5（用于语音缓存键与旧缓存自动失效）                */
/* ------------------------------------------------------------------ */

pub fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for i in 0..16 {
            m[i] = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }

        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = Vec::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            digest.push(format!("{byte:02x}"));
        }
    }
    digest.concat()
}

/* ------------------------------------------------------------------ */
/* URL 解析、目录检测与健康探测                                         */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEndpoint {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub base_path: String,
}

pub fn parse_api_url(raw: &str) -> Result<ParsedEndpoint, String> {
    let trimmed = if raw.trim().is_empty() {
        "http://127.0.0.1:9880"
    } else {
        raw.trim().trim_end_matches('/')
    };
    let (https, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        (false, trimmed)
    };
    let (authority, base_path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], rest[idx..].trim_end_matches('/').to_string()),
        None => (rest, String::new()),
    };
    if authority.is_empty() {
        return Err("API 地址缺少主机名".to_string());
    }
    let (host, port) = if let Some(colon) = authority.rfind(':') {
        let h = &authority[..colon];
        let p_str = &authority[colon + 1..];
        if let Ok(p) = p_str.parse::<u16>() {
            (h.to_string(), p)
        } else {
            (authority.to_string(), if https { 443 } else { 9880 })
        }
    } else {
        (authority.to_string(), if https { 443 } else { 9880 })
    };
    Ok(ParsedEndpoint {
        https,
        host,
        port,
        base_path,
    })
}

pub fn parse_full_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let trimmed = url.trim();
    let (https, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        return Err("URL 必须以 http:// 或 https:// 开头".to_string());
    };
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = if let Some(colon) = authority.rfind(':') {
        if let Ok(p) = authority[colon + 1..].parse::<u16>() {
            (authority[..colon].to_string(), p)
        } else {
            (authority.to_string(), if https { 443 } else { 80 })
        }
    } else {
        (authority.to_string(), if https { 443 } else { 80 })
    };
    Ok((https, host, port, path.to_string()))
}

pub fn check_api_health(api_url: &str) -> bool {
    let Ok(ep) = parse_api_url(api_url) else {
        return false;
    };
    let addr_str = format!("{}:{}", ep.host, ep.port);
    let Ok(mut addrs) = addr_str.to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(320)).is_ok()
}

pub fn default_install_dir(base_root: &Path) -> PathBuf {
    base_root.join("runtime_engines").join("gpt-sovits")
}

pub fn resolve_install_dir(base_root: &Path, custom_dir: &str) -> PathBuf {
    let trimmed = custom_dir.trim();
    if trimmed.is_empty() {
        default_install_dir(base_root)
    } else {
        PathBuf::from(trimmed)
    }
}

pub fn find_python_exe(install_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        install_dir.join("runtime").join("python.exe"),
        install_dir.join("python.exe"),
        install_dir.join(".venv").join("Scripts").join("python.exe"),
        install_dir.join("venv").join("Scripts").join("python.exe"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn find_ffmpeg_exe(install_dir: &Path) -> Option<PathBuf> {
    let local = [
        install_dir.join("ffmpeg.exe"),
        install_dir.join("runtime").join("ffmpeg.exe"),
    ];
    for p in local {
        if p.is_file() {
            return Some(p);
        }
    }
    Some(PathBuf::from("ffmpeg"))
}

fn has_file_nonempty(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

fn has_weight_bin(dir: &Path) -> bool {
    has_file_nonempty(&dir.join("pytorch_model.bin"))
        || has_file_nonempty(&dir.join("model.safetensors"))
}

pub fn missing_v2proplus_files(install_dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    if !install_dir.join("api_v2.py").is_file() {
        missing.push("api_v2.py".to_string());
    }
    if find_python_exe(install_dir).is_none() {
        missing.push("runtime/python.exe".to_string());
    }

    let required_rel = [
        "GPT_SoVITS/pretrained_models/v2Pro/s2Gv2ProPlus.pth",
        "GPT_SoVITS/pretrained_models/s1v3.ckpt",
        "GPT_SoVITS/pretrained_models/sv/pretrained_eres2netv2w24s4ep4.ckpt",
        "GPT_SoVITS/pretrained_models/chinese-roberta-wwm-ext-large/config.json",
        "GPT_SoVITS/pretrained_models/chinese-roberta-wwm-ext-large/tokenizer.json",
        "GPT_SoVITS/pretrained_models/chinese-hubert-base/config.json",
        "GPT_SoVITS/pretrained_models/chinese-hubert-base/preprocessor_config.json",
    ];
    for rel in required_rel {
        if !has_file_nonempty(&install_dir.join(rel)) {
            missing.push(rel.to_string());
        }
    }
    let roberta_dir = install_dir.join("GPT_SoVITS/pretrained_models/chinese-roberta-wwm-ext-large");
    if !has_weight_bin(&roberta_dir) {
        missing.push("GPT_SoVITS/pretrained_models/chinese-roberta-wwm-ext-large/pytorch_model.bin".to_string());
    }
    let hubert_dir = install_dir.join("GPT_SoVITS/pretrained_models/chinese-hubert-base");
    if !has_weight_bin(&hubert_dir) {
        missing.push("GPT_SoVITS/pretrained_models/chinese-hubert-base/pytorch_model.bin".to_string());
    }
    missing
}

pub fn inspect_status(
    base_root: &Path,
    cfg: &GptSovitsConfig,
    runtime: &GptSovitsRuntime,
) -> GptSovitsStatusPayload {
    let install_dir = resolve_install_dir(base_root, &cfg.install_dir);
    let api_url = if cfg.api_url.trim().is_empty() {
        default_api_url()
    } else {
        cfg.api_url.trim().to_string()
    };
    let ep = parse_api_url(&api_url).ok();
    let host_port = ep
        .as_ref()
        .map(|e| format!("{}:{}", e.host, e.port))
        .unwrap_or_else(|| "127.0.0.1:9880".to_string());

    let managed_pid = runtime
        .process
        .lock()
        .ok()
        .and_then(|mut g| match g.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => Some(child.id()),
                _ => {
                    *g = None;
                    None
                }
            },
            None => None,
        });
    let error = runtime.last_error.lock().ok().and_then(|g| g.clone());

    if check_api_health(&api_url) {
        return GptSovitsStatusPayload {
            state: "running".to_string(),
            label: format!("运行中 {host_port}"),
            install_dir: install_dir.to_string_lossy().to_string(),
            api_url,
            missing_files: Vec::new(),
            managed_pid,
            error,
        };
    }

    if runtime.installing.load(Ordering::Relaxed) {
        return GptSovitsStatusPayload {
            state: "installing".to_string(),
            label: "下载解压中".to_string(),
            install_dir: install_dir.to_string_lossy().to_string(),
            api_url,
            missing_files: Vec::new(),
            managed_pid,
            error,
        };
    }

    let missing = missing_v2proplus_files(&install_dir);
    if missing.is_empty() {
        return GptSovitsStatusPayload {
            state: "ready".to_string(),
            label: "已就绪-未启动".to_string(),
            install_dir: install_dir.to_string_lossy().to_string(),
            api_url,
            missing_files: Vec::new(),
            managed_pid,
            error,
        };
    }

    GptSovitsStatusPayload {
        state: "uninstalled".to_string(),
        label: "未安装".to_string(),
        install_dir: install_dir.to_string_lossy().to_string(),
        api_url,
        missing_files: missing,
        managed_pid,
        error,
    }
}

/* ------------------------------------------------------------------ */
/* 一键异步下载安装（支持镜像/官方源切换、断点续传、解压、取消）           */
/* ------------------------------------------------------------------ */

struct DownloadTask {
    label: String,
    rel_dest: String,
    mirror_url: String,
    official_url: String,
    kind: DownloadKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DownloadKind {
    SourceZip,
    GpuRuntimeZip,
    DirectFile,
}

fn build_download_manifest(gpu_backend: &str, github_repo: &str, release_tag: &str) -> Vec<DownloadTask> {
    let is_amd = gpu_backend.trim().eq_ignore_ascii_case("amd");
    let runtime_asset = if is_amd {
        "gpt-sovits-amd-zluda.zip"
    } else {
        "gpt-sovits-nvidia-runtime.zip"
    };
    let runtime_label = if is_amd {
        "AMD 显卡 ZLUDA 实验运行时与补丁包"
    } else {
        "NVIDIA 显卡 CUDA 推理运行时包"
    };
    let rt_mirror = crate::assets_mgr::build_release_asset_url(
        github_repo,
        release_tag,
        runtime_asset,
        "mirror",
    );
    let rt_official = crate::assets_mgr::build_release_asset_url(
        github_repo,
        release_tag,
        runtime_asset,
        "official",
    );

    vec![
        DownloadTask {
            label: "GPT-SoVITS 推理源码包".to_string(),
            rel_dest: "_downloads/GPT-SoVITS-main.zip".to_string(),
            mirror_url: "https://ghfast.top/https://github.com/RVC-Boss/GPT-SoVITS/archive/refs/heads/main.zip".to_string(),
            official_url: "https://github.com/RVC-Boss/GPT-SoVITS/archive/refs/heads/main.zip".to_string(),
            kind: DownloadKind::SourceZip,
        },
        DownloadTask {
            label: runtime_label.to_string(),
            rel_dest: format!("_downloads/{runtime_asset}"),
            mirror_url: rt_mirror,
            official_url: rt_official,
            kind: DownloadKind::GpuRuntimeZip,
        },
        DownloadTask {
            label: "v2ProPlus SoVITS 权重 (s2Gv2ProPlus.pth)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/v2Pro/s2Gv2ProPlus.pth".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/v2Pro/s2Gv2ProPlus.pth".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/v2Pro/s2Gv2ProPlus.pth".to_string(),
            kind: DownloadKind::DirectFile,
        },
        DownloadTask {
            label: "v3/v2ProPlus GPT 权重 (s1v3.ckpt)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/s1v3.ckpt".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/s1v3.ckpt".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/s1v3.ckpt".to_string(),
            kind: DownloadKind::DirectFile,
        },
        DownloadTask {
            label: "说话人编码器 (eres2netv2)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/sv/pretrained_eres2netv2w24s4ep4.ckpt".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/sv/pretrained_eres2netv2w24s4ep4.ckpt".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/sv/pretrained_eres2netv2w24s4ep4.ckpt".to_string(),
            kind: DownloadKind::DirectFile,
        },
        DownloadTask {
            label: "RoBERTa 配置 (config.json)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/chinese-roberta-wwm-ext-large/config.json".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/chinese-roberta-wwm-ext-large/config.json".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/chinese-roberta-wwm-ext-large/config.json".to_string(),
            kind: DownloadKind::DirectFile,
        },
        DownloadTask {
            label: "RoBERTa 分词器 (tokenizer.json)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/chinese-roberta-wwm-ext-large/tokenizer.json".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/chinese-roberta-wwm-ext-large/tokenizer.json".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/chinese-roberta-wwm-ext-large/tokenizer.json".to_string(),
            kind: DownloadKind::DirectFile,
        },
        DownloadTask {
            label: "RoBERTa 特征权重 (pytorch_model.bin)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/chinese-roberta-wwm-ext-large/pytorch_model.bin".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/chinese-roberta-wwm-ext-large/pytorch_model.bin".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/chinese-roberta-wwm-ext-large/pytorch_model.bin".to_string(),
            kind: DownloadKind::DirectFile,
        },
        DownloadTask {
            label: "HuBERT 配置 (config.json)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/chinese-hubert-base/config.json".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/chinese-hubert-base/config.json".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/chinese-hubert-base/config.json".to_string(),
            kind: DownloadKind::DirectFile,
        },
        DownloadTask {
            label: "HuBERT 预处理器 (preprocessor_config.json)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/chinese-hubert-base/preprocessor_config.json".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/chinese-hubert-base/preprocessor_config.json".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/chinese-hubert-base/preprocessor_config.json".to_string(),
            kind: DownloadKind::DirectFile,
        },
        DownloadTask {
            label: "HuBERT 声学特征权重 (pytorch_model.bin)".to_string(),
            rel_dest: "GPT_SoVITS/pretrained_models/chinese-hubert-base/pytorch_model.bin".to_string(),
            mirror_url: "https://hf-mirror.com/lj1995/GPT-SoVITS/resolve/main/chinese-hubert-base/pytorch_model.bin".to_string(),
            official_url: "https://huggingface.co/lj1995/GPT-SoVITS/resolve/main/chinese-hubert-base/pytorch_model.bin".to_string(),
            kind: DownloadKind::DirectFile,
        },
    ]
}

fn extract_zip_archive(zip_path: &Path, dest_dir: &Path, strip_top: bool) -> Result<(), String> {
    fs::create_dir_all(dest_dir)
        .map_err(|e| format!("创建解压目录失败 {}: {e}", dest_dir.display()))?;

    let mut cmd = Command::new("tar");
    cmd.arg("-xf").arg(zip_path).arg("-C").arg(dest_dir);
    if strip_top {
        cmd.arg("--strip-components=1");
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd.output().map_err(|e| format!("调用系统 tar 解压失败: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(format!("解压 {} 失败: {}", zip_path.display(), stderr.trim()))
}

pub fn install_engine(
    base_root: &Path,
    install_dir_raw: &str,
    download_source: &str,
    gpu_backend: &str,
    github_repo: &str,
    release_tag: &str,
    runtime: Arc<GptSovitsRuntime>,
    mut emit_progress: impl FnMut(GptSovitsProgressPayload),
) -> Result<(), String> {
    if runtime
        .installing
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("已有安装任务正在进行中".to_string());
    }
    runtime.cancel_install.store(false, Ordering::SeqCst);
    if let Ok(mut err) = runtime.last_error.lock() {
        *err = None;
    }

    struct InstallGuard(Arc<GptSovitsRuntime>);
    impl Drop for InstallGuard {
        fn drop(&mut self) {
            self.0.installing.store(false, Ordering::SeqCst);
        }
    }
    let _guard = InstallGuard(runtime.clone());

    let install_dir = resolve_install_dir(base_root, install_dir_raw);
    fs::create_dir_all(&install_dir)
        .map_err(|e| format!("无法创建引擎目录 {}: {e}", install_dir.display()))?;

    let use_mirror = download_source.trim() != "official";
    let is_amd = gpu_backend.trim().eq_ignore_ascii_case("amd");
    let manifest = build_download_manifest(gpu_backend, github_repo, release_tag);
    let total_files = manifest.len();

    for (idx, task) in manifest.iter().enumerate() {
        if runtime.cancel_install.load(Ordering::Relaxed) {
            emit_progress(GptSovitsProgressPayload {
                active: false,
                stage: "cancelled".to_string(),
                current_file: task.label.clone(),
                file_index: idx + 1,
                file_total: total_files,
                downloaded_bytes: 0,
                total_bytes: 0,
                percent: 0.0,
                speed_bps: 0,
                message: "已取消下载，断点进度已保留".to_string(),
            });
            return Err("用户已取消安装".to_string());
        }

        let skip = match task.kind {
            DownloadKind::SourceZip => install_dir.join("api_v2.py").is_file(),
            DownloadKind::GpuRuntimeZip => {
                let has_py = find_python_exe(&install_dir).is_some();
                if is_amd {
                    has_py && has_zluda(&install_dir).is_some()
                } else {
                    has_py
                }
            }
            DownloadKind::DirectFile => {
                let target = install_dir.join(&task.rel_dest);
                has_file_nonempty(&target)
                    || (task.rel_dest.ends_with("pytorch_model.bin")
                        && target
                            .parent()
                            .map(|p| has_file_nonempty(&p.join("model.safetensors")))
                            .unwrap_or(false))
            }
        };

        if skip {
            continue;
        }

        let url = if use_mirror {
            &task.mirror_url
        } else {
            &task.official_url
        };
        let dest = install_dir.join(&task.rel_dest);
        #[cfg(windows)]
        {
            let (https, host, port, path) = parse_full_url(url)?;
            let label = task.label.clone();
            let res = crate::ai_voice::winhttp_client::download_file_resumable(
                https,
                &host,
                port,
                &path,
                &dest,
                &runtime.cancel_install,
                |downloaded, total, speed_bps| {
                    let pct = if total > 0 {
                        ((downloaded as f64) / (total as f64) * 100.0).clamp(0.0, 100.0)
                    } else {
                        0.0
                    };
                    emit_progress(GptSovitsProgressPayload {
                        active: true,
                        stage: "downloading".to_string(),
                        current_file: label.clone(),
                        file_index: idx + 1,
                        file_total: total_files,
                        downloaded_bytes: downloaded,
                        total_bytes: total,
                        percent: pct,
                        speed_bps,
                        message: format!("正在下载 ({}/{})：{}", idx + 1, total_files, label),
                    });
                },
            );

            if let Err(err) = res {
                let cancelled = runtime.cancel_install.load(Ordering::Relaxed);
                if let Ok(mut last) = runtime.last_error.lock() {
                    *last = if cancelled { None } else { Some(err.clone()) };
                }
                emit_progress(GptSovitsProgressPayload {
                    active: false,
                    stage: if cancelled { "cancelled" } else { "error" }.to_string(),
                    current_file: label,
                    file_index: idx + 1,
                    file_total: total_files,
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    percent: 0.0,
                    speed_bps: 0,
                    message: err.clone(),
                });
                return Err(err);
            }
        }

        #[cfg(not(windows))]
        {
            let _ = (url, dest);
            return Err("当前仅支持 Windows 平台下载安装".to_string());
        }

        match task.kind {
            DownloadKind::SourceZip => {
                emit_progress(GptSovitsProgressPayload {
                    active: true,
                    stage: "extracting".to_string(),
                    current_file: task.label.clone(),
                    file_index: idx + 1,
                    file_total: total_files,
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    percent: 100.0,
                    speed_bps: 0,
                    message: "正在解压 GPT-SoVITS 推理脚本…".to_string(),
                });
                extract_zip_archive(&dest, &install_dir, true)?;
                let _ = fs::remove_file(&dest);
            }
            DownloadKind::GpuRuntimeZip => {
                emit_progress(GptSovitsProgressPayload {
                    active: true,
                    stage: "extracting".to_string(),
                    current_file: task.label.clone(),
                    file_index: idx + 1,
                    file_total: total_files,
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    percent: 100.0,
                    speed_bps: 0,
                    message: format!("正在解压 {}…", task.label),
                });
                extract_zip_archive(&dest, &install_dir, false)?;
                let pth = install_dir.join("runtime").join("python310._pth");
                if let Ok(raw) = fs::read_to_string(&pth) {
                    let patched = raw.replace("#import site", "import site");
                    let _ = fs::write(&pth, patched);
                }
                let _ = fs::remove_file(&dest);
            }
            DownloadKind::DirectFile => {}
        }
    }

    let _ = fs::remove_dir_all(install_dir.join("_downloads"));
    emit_progress(GptSovitsProgressPayload {
        active: false,
        stage: "done".to_string(),
        current_file: String::new(),
        file_index: total_files,
        file_total: total_files,
        downloaded_bytes: 0,
        total_bytes: 0,
        percent: 100.0,
        speed_bps: 0,
        message: "GPT-SoVITS v2ProPlus 核心模型与运行时已就绪".to_string(),
    });
    Ok(())
}

/* ------------------------------------------------------------------ */
/* 服务进程生命周期管理                                                */
/* ------------------------------------------------------------------ */

fn has_nvidia_gpu() -> bool {
    let mut cmd = Command::new("nvidia-smi");
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn probe_torch_cuda(python_exe: &Path, install_dir: &Path) -> bool {
    let mut cmd = Command::new(python_exe);
    cmd.current_dir(install_dir)
        .arg("-c")
        .arg("import torch; print('1' if torch.cuda.is_available() else '0')")
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
        .unwrap_or(false)
}

fn has_zluda(install_dir: &Path) -> Option<PathBuf> {
    let exe = install_dir.join("zluda").join("zluda.exe");
    if exe.is_file() {
        Some(exe)
    } else {
        None
    }
}

fn find_rocm_bin() -> Option<PathBuf> {
    if let Ok(hip) = std::env::var("HIP_PATH") {
        let bin = PathBuf::from(hip.trim()).join("bin");
        if bin.is_dir() {
            return Some(bin);
        }
    }
    let fallback = PathBuf::from(r"C:\Program Files\AMD\ROCm\5.7\bin");
    if fallback.is_dir() {
        Some(fallback)
    } else {
        None
    }
}

fn ensure_v2proplus_infer_config(
    install_dir: &Path,
    python_exe: &Path,
    use_zluda: bool,
) -> Option<String> {
    let s2 = install_dir.join("GPT_SoVITS/pretrained_models/v2Pro/s2Gv2ProPlus.pth");
    let s1 = install_dir.join("GPT_SoVITS/pretrained_models/s1v3.ckpt");
    if !has_file_nonempty(&s2) || !has_file_nonempty(&s1) {
        return None;
    }
    let (device, is_half) = if use_zluda {
        // ZLUDA（AMD GPU）直接启用 cuda 并强制 FP32（is_half: false），跳过额外子进程探测
        ("cuda", "false")
    } else {
        let has_cuda = probe_torch_cuda(python_exe, install_dir);
        let has_nv = has_cuda && has_nvidia_gpu();
        (if has_cuda { "cuda" } else { "cpu" }, if has_nv { "true" } else { "false" })
    };

    let rel_cfg = "GPT_SoVITS/configs/deskpet_v2proplus.yaml";
    let cfg_path = install_dir.join(rel_cfg);
    if let Some(parent) = cfg_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let yaml = format!(
        "custom:\n\
         \x20 bert_base_path: GPT_SoVITS/pretrained_models/chinese-roberta-wwm-ext-large\n\
         \x20 cnhuhbert_base_path: GPT_SoVITS/pretrained_models/chinese-hubert-base\n\
         \x20 device: {device}\n\
         \x20 is_half: {is_half}\n\
         \x20 t2s_weights_path: GPT_SoVITS/pretrained_models/s1v3.ckpt\n\
         \x20 version: v2ProPlus\n\
         \x20 vits_weights_path: GPT_SoVITS/pretrained_models/v2Pro/s2Gv2ProPlus.pth\n"
    );
    if fs::write(&cfg_path, yaml).is_ok() {
        Some(rel_cfg.to_string())
    } else {
        None
    }
}

pub fn start_service(
    base_root: &Path,
    cfg: &GptSovitsConfig,
    runtime: &GptSovitsRuntime,
) -> Result<(), String> {
    let api_url = if cfg.api_url.trim().is_empty() {
        default_api_url()
    } else {
        cfg.api_url.trim().to_string()
    };
    if check_api_health(&api_url) {
        if let Ok(mut err) = runtime.last_error.lock() {
            *err = None;
        }
        return Ok(());
    }

    let install_dir = resolve_install_dir(base_root, &cfg.install_dir);
    let api_script = install_dir.join("api_v2.py");
    if !api_script.is_file() {
        return Err("未找到 api_v2.py，请先安装或关联本地 GPT-SoVITS 目录".to_string());
    }
    let python_exe = find_python_exe(&install_dir)
        .ok_or_else(|| "未找到 Python 运行时，请先安装或补全环境".to_string())?;
    let ep = parse_api_url(&api_url)?;
    let zluda_exe = has_zluda(&install_dir);
    let infer_cfg = ensure_v2proplus_infer_config(&install_dir, &python_exe, zluda_exe.is_some());

    let log_path = install_dir.join("deskpet_gpt_sovits.log");
    let stdout_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| format!("打开服务日志文件失败: {e}"))?;
    let stderr_file = stdout_file
        .try_clone()
        .map_err(|e| format!("克隆日志句柄失败: {e}"))?;

    let mut cmd = if let Some(ref z_exe) = zluda_exe {
        let mut c = Command::new(z_exe);
        c.arg("--").arg(&python_exe);
        c
    } else {
        Command::new(&python_exe)
    };
    cmd.current_dir(&install_dir)
        .arg("api_v2.py")
        .arg("-a")
        .arg(&ep.host)
        .arg("-p")
        .arg(ep.port.to_string());
    if let Some(ref cfg_rel) = infer_cfg {
        cmd.arg("-c").arg(cfg_rel);
    }
    cmd.env("PYTHONIOENCODING", "utf-8");

    if zluda_exe.is_some() {
        let mut extra_paths = vec![install_dir.join("zluda")];
        if let Some(rocm_bin) = find_rocm_bin() {
            extra_paths.push(rocm_bin);
        }
        let orig_path = std::env::var_os("PATH").unwrap_or_default();
        let mut merged = std::ffi::OsString::new();
        for p in &extra_paths {
            merged.push(p.as_os_str());
            merged.push(";");
        }
        merged.push(orig_path);
        cmd.env("PATH", merged);

        let zluda_cache = install_dir.join("zluda_cache");
        let _ = fs::create_dir_all(&zluda_cache);
        cmd.env("ZLUDA_CACHE_DIR", zluda_cache);
        cmd.env("DISABLE_ADDMM_CUDA_LT", "1");
        cmd.env("CUDAGraph", "0");
        cmd.env("HF_ENDPOINT", "https://hf-mirror.com");
    }

    cmd.stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd
        .spawn()
        .map_err(|e| format!("启动 GPT-SoVITS 服务失败: {e}"))?;

    if let Ok(mut guard) = runtime.process.lock() {
        if let Some(mut old) = guard.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
        *guard = Some(child);
    }

    // 轮询端口就绪（最多 45 秒，兼容 CPU / ZLUDA 冷启动），若子进程提前退出则直接提取日志报错
    for _ in 0..90 {
        if check_api_health(&api_url) {
            if let Ok(mut err) = runtime.last_error.lock() {
                *err = None;
            }
            return Ok(());
        }
        let exited = runtime
            .process
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|c| c.try_wait().ok().flatten()));
        if let Some(status) = exited {
            let tail = fs::read_to_string(&log_path)
                .ok()
                .and_then(|s| s.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
                .unwrap_or_else(|| format!("进程退出 ({status})"));
            let msg = format!("GPT-SoVITS 启动异常退出：{tail}");
            if let Ok(mut err) = runtime.last_error.lock() {
                *err = Some(msg.clone());
            }
            return Err(msg);
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let msg = "GPT-SoVITS 启动超时（>45s），请查看引擎日志".to_string();
    if let Ok(mut err) = runtime.last_error.lock() {
        *err = Some(msg.clone());
    }
    Err(msg)
}

pub fn stop_service(api_url: &str, runtime: &GptSovitsRuntime) {
    if let Ok(ep) = parse_api_url(api_url) {
        #[cfg(windows)]
        {
            let path = format!("{}/control?command=exit", ep.base_path);
            let _ = crate::ai_voice::winhttp_client::http_get_raw(
                ep.https,
                &ep.host,
                ep.port,
                &path,
                1_500,
            );
        }
    }

    if let Ok(mut guard) = runtime.process.lock() {
        if let Some(mut child) = guard.take() {
            for _ in 0..10 {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(120));
            }
            #[cfg(windows)]
            {
                let mut tk = Command::new("taskkill");
                tk.args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW);
                let _ = tk.status();
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    if let Ok(mut g) = runtime.last_gpt_weights.lock() {
        *g = None;
    }
    if let Ok(mut g) = runtime.last_sovits_weights.lock() {
        *g = None;
    }
}

/* ------------------------------------------------------------------ */
/* 角色权重热切换、参考音频兼容转码、MD5 缓存与 /tts 合成                  */
/* ------------------------------------------------------------------ */

fn url_encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 2);
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

pub fn hot_swap_weights_if_needed(
    cfg: &GptSovitsConfig,
    char_id: &str,
    runtime: &GptSovitsRuntime,
) -> Result<(), String> {
    let Some(profile) = cfg.char_profiles.get(char_id) else {
        return Ok(());
    };
    let gpt = profile.gpt_weights_path.trim();
    let sovits = profile.sovits_weights_path.trim();
    if gpt.is_empty() && sovits.is_empty() {
        return Ok(());
    }
    if !check_api_health(&cfg.api_url) {
        return Ok(());
    }
    let ep = parse_api_url(&cfg.api_url)?;

    if !gpt.is_empty() && Path::new(gpt).is_file() {
        let needs_swap = runtime
            .last_gpt_weights
            .lock()
            .map(|g| g.as_deref() != Some(gpt))
            .unwrap_or(true);
        if needs_swap {
            #[cfg(windows)]
            {
                let path = format!(
                    "{}/set_gpt_weights?weights_path={}",
                    ep.base_path,
                    url_encode_component(gpt)
                );
                let (status, _) = crate::ai_voice::winhttp_client::http_get_raw(
                    ep.https, &ep.host, ep.port, &path, 15_000,
                )?;
                if status >= 200 && status < 300 {
                    if let Ok(mut g) = runtime.last_gpt_weights.lock() {
                        *g = Some(gpt.to_string());
                    }
                }
            }
        }
    }

    if !sovits.is_empty() && Path::new(sovits).is_file() {
        let needs_swap = runtime
            .last_sovits_weights
            .lock()
            .map(|g| g.as_deref() != Some(sovits))
            .unwrap_or(true);
        if needs_swap {
            #[cfg(windows)]
            {
                let path = format!(
                    "{}/set_sovits_weights?weights_path={}",
                    ep.base_path,
                    url_encode_component(sovits)
                );
                let (status, _) = crate::ai_voice::winhttp_client::http_get_raw(
                    ep.https, &ep.host, ep.port, &path, 15_000,
                )?;
                if status >= 200 && status < 300 {
                    if let Ok(mut g) = runtime.last_sovits_weights.lock() {
                        *g = Some(sovits.to_string());
                    }
                }
            }
        }
    }

    Ok(())
}

fn file_stamp(path: &Path) -> String {
    let Ok(meta) = fs::metadata(path) else {
        return "0:0".to_string();
    };
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{len}:{mtime}")
}

fn prepare_compatible_ref_audio(
    install_dir: &Path,
    cache_dir: &Path,
    raw_ref_path: &str,
) -> Result<String, String> {
    let src = Path::new(raw_ref_path.trim());
    if !src.is_file() {
        return Err("参考音频文件不存在，请在设置中重新选择".to_string());
    }
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "wav" {
        return Ok(src.to_string_lossy().to_string());
    }

    let refs_dir = cache_dir.join("refs");
    let _ = fs::create_dir_all(&refs_dir);
    let key = md5_hex(format!("{}|{}", raw_ref_path, file_stamp(src)).as_bytes());
    let out_wav = refs_dir.join(format!("{key}.wav"));
    if has_file_nonempty(&out_wav) {
        return Ok(out_wav.to_string_lossy().to_string());
    }

    if let Some(ffmpeg) = find_ffmpeg_exe(install_dir) {
        let mut cmd = Command::new(ffmpeg);
        cmd.arg("-y")
            .arg("-i")
            .arg(src)
            .arg("-ar")
            .arg("32000")
            .arg("-ac")
            .arg("1")
            .arg(&out_wav);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        if let Ok(out) = cmd.output() {
            if out.status.success() && has_file_nonempty(&out_wav) {
                return Ok(out_wav.to_string_lossy().to_string());
            }
        }
    }

    Ok(src.to_string_lossy().to_string())
}

pub fn normalize_cache_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_end_matches(|c: char| {
            matches!(
                c,
                '~' | '～' | '!' | '！' | '。' | '.' | '…' | ',' | '，' | '♪' | '✨'
            )
        })
        .trim()
        .to_string()
}

pub fn build_cache_key(char_id: &str, text: &str, profile: &GptSovitsCharProfile) -> String {
    let norm = normalize_cache_text(text);
    let ref_stamp = file_stamp(Path::new(profile.ref_audio_path.trim()));
    let speed = profile.speed_factor.clamp(0.8, 1.3);
    let raw = format!(
        "v4|{char_id}|{norm}|{}|{ref_stamp}|{}|{}|{}|{speed:.2}|{}|{}",
        profile.ref_audio_path.trim(),
        profile.prompt_text.trim(),
        profile.prompt_lang.trim(),
        profile.text_lang.trim(),
        profile.gpt_weights_path.trim(),
        profile.sovits_weights_path.trim()
    );
    md5_hex(raw.as_bytes())
}

fn build_legacy_cache_key_v3(char_id: &str, text: &str, profile: &GptSovitsCharProfile) -> String {
    let ref_stamp = file_stamp(Path::new(profile.ref_audio_path.trim()));
    let speed = profile.speed_factor.clamp(0.8, 1.3);
    let raw = format!(
        "v3|{char_id}|{}|{}|{ref_stamp}|{}|{}|{}|{speed:.2}|{}|{}",
        text.trim(),
        profile.ref_audio_path.trim(),
        profile.prompt_text.trim(),
        profile.prompt_lang.trim(),
        profile.text_lang.trim(),
        profile.gpt_weights_path.trim(),
        profile.sovits_weights_path.trim()
    );
    md5_hex(raw.as_bytes())
}

pub fn lookup_cached_wav(
    cache_dir: &Path,
    cfg: &GptSovitsConfig,
    char_id: &str,
    text: &str,
) -> Option<Vec<u8>> {
    let clean = text.trim();
    if clean.is_empty() {
        return None;
    }
    let profile = cfg.char_profiles.get(char_id)?;
    for key in [
        build_cache_key(char_id, clean, profile),
        build_legacy_cache_key_v3(char_id, clean, profile),
    ] {
        let p = cache_dir.join(format!("{key}.wav"));
        if has_file_nonempty(&p) {
            if let Ok(bytes) = fs::read(&p) {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

pub fn prune_voice_cache_if_needed(cache_dir: &Path, max_files: usize, max_bytes: u64) {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    let mut items: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total_bytes: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let len = meta.len();
            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            total_bytes = total_bytes.saturating_add(len);
            items.push((path, len, mtime));
        }
    }
    if items.len() <= max_files && total_bytes <= max_bytes {
        return;
    }
    items.sort_by_key(|(_, _, mtime)| *mtime);
    let mut count = items.len();
    for (path, len, _) in items {
        if count <= max_files && total_bytes <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            count = count.saturating_sub(1);
            total_bytes = total_bytes.saturating_sub(len);
        }
    }
}

fn contains_japanese_kana(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            c,
            '\u{3041}'..='\u{3096}'
                | '\u{3099}'..='\u{309F}'
                | '\u{30A1}'..='\u{30FA}'
                | '\u{30FC}'..='\u{30FF}'
                | '\u{31F0}'..='\u{31FF}'
                | '\u{FF66}'..='\u{FF9F}'
        )
    })
}

fn contains_cjk_hanzi(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}'))
}

fn resolve_effective_text_lang(configured: &str, text: &str) -> &'static str {
    let has_kana = contains_japanese_kana(text);
    let has_hanzi = contains_cjk_hanzi(text);
    if has_kana {
        match configured.trim() {
            "ja" => "ja",
            "all_ja" => "all_ja",
            _ => "auto",
        }
    } else if has_hanzi {
        match configured.trim() {
            "all_zh" => "all_zh",
            _ => "zh",
        }
    } else {
        match configured.trim() {
            "en" => "en",
            _ => "auto",
        }
    }
}

pub fn synthesize_gpt_sovits_wav(
    base_root: &Path,
    cache_dir: &Path,
    cfg: &GptSovitsConfig,
    char_id: &str,
    text: &str,
    runtime: &GptSovitsRuntime,
) -> Result<Vec<u8>, String> {
    let clean = text.trim();
    if clean.is_empty() {
        return Err("朗读文本为空".to_string());
    }
    let profile = cfg
        .char_profiles
        .get(char_id)
        .ok_or_else(|| "当前角色未设置克隆参考音频".to_string())?;
    if profile.ref_audio_path.trim().is_empty() || !Path::new(profile.ref_audio_path.trim()).is_file() {
        return Err("当前角色未设置克隆参考音频".to_string());
    }

    let _ = fs::create_dir_all(cache_dir);
    if let Some(cached) = lookup_cached_wav(cache_dir, cfg, char_id, clean) {
        return Ok(cached);
    }
    let cache_key = build_cache_key(char_id, clean, profile);
    let cache_file = cache_dir.join(format!("{cache_key}.wav"));

    if !check_api_health(&cfg.api_url) {
        if cfg.auto_start_service {
            start_service(base_root, cfg, runtime)?;
        } else {
            return Err("本地克隆语音引擎未启动".to_string());
        }
    }

    runtime.mark_used(crate::stats::now_ms());
    let _ = hot_swap_weights_if_needed(cfg, char_id, runtime);

    let install_dir = resolve_install_dir(base_root, &cfg.install_dir);
    let ref_audio_path =
        prepare_compatible_ref_audio(&install_dir, cache_dir, &profile.ref_audio_path)?;

    let ep = parse_api_url(&cfg.api_url)?;
    let effective_text_lang = resolve_effective_text_lang(&profile.text_lang, clean);
    let body = json!({
        "text": clean,
        "text_lang": effective_text_lang,
        "ref_audio_path": ref_audio_path,
        "prompt_text": profile.prompt_text.trim(),
        "prompt_lang": if profile.prompt_lang.trim().is_empty() { "zh" } else { profile.prompt_lang.trim() },
        "text_split_method": "cut5",
        "split_bucket": false,
        "fragment_interval": 0.15,
        "top_k": 10,
        "top_p": 0.8,
        "temperature": 0.65,
        "repetition_penalty": 1.35,
        "speed_factor": profile.speed_factor.clamp(0.8, 1.3),
        "media_type": "wav",
        "streaming_mode": false,
    });
    let body_bytes = serde_json::to_vec(&body).map_err(|e| format!("序列化请求失败: {e}"))?;

    #[cfg(windows)]
    {
        let tts_path = format!("{}/tts", ep.base_path);
        let (status, wav) = crate::ai_voice::winhttp_client::http_request_raw(
            "POST",
            ep.https,
            &ep.host,
            ep.port,
            &tts_path,
            "Content-Type: application/json\r\n",
            &body_bytes,
            60_000,
            16 * 1024 * 1024,
        )?;
        if status < 200 || status >= 300 || wav.is_empty() {
            return Err(format!("本地推理接口返回 HTTP {status}"));
        }
        let _ = fs::write(&cache_file, &wav);
        prune_voice_cache_if_needed(cache_dir, 300, 150 * 1024 * 1024);
        runtime.mark_used(crate::stats::now_ms());
        Ok(wav)
    }

    #[cfg(not(windows))]
    {
        let _ = (ep, body_bytes);
        Err("当前仅支持 Windows 本地推理请求".to_string())
    }
}

/* ------------------------------------------------------------------ */
/* Windows 原生文件 / 目录选择器（零外部 Cargo 依赖）                    */
/* ------------------------------------------------------------------ */

pub fn pick_file_or_dir(kind: &str) -> Option<String> {
    #[cfg(windows)]
    {
        win_dialog::pick(kind)
    }
    #[cfg(not(windows))]
    {
        let _ = kind;
        None
    }
}

#[cfg(windows)]
mod win_dialog {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
    const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
    const OFN_NOCHANGEDIR: u32 = 0x0000_0008;

    const BIF_RETURNONLYFSDIRS: u32 = 0x0000_0001;
    const BIF_EDITBOX: u32 = 0x0000_0010;
    const BIF_NEWDIALOGSTYLE: u32 = 0x0000_0040;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;

    #[repr(C)]
    struct OpenFileNameW {
        l_struct_size: u32,
        hwnd_owner: *mut c_void,
        h_instance: *mut c_void,
        lpstr_filter: *const u16,
        lpstr_custom_filter: *mut u16,
        n_max_cust_filter: u32,
        n_filter_index: u32,
        lpstr_file: *mut u16,
        n_max_file: u32,
        lpstr_file_title: *mut u16,
        n_max_file_title: u32,
        lpstr_initial_dir: *const u16,
        lpstr_title: *const u16,
        flags: u32,
        n_file_offset: u16,
        n_file_extension: u16,
        lpstr_def_ext: *const u16,
        l_cust_data: isize,
        lpfn_hook: *const c_void,
        lp_template_name: *const u16,
        pv_reserved: *mut c_void,
        dw_reserved: u32,
        flags_ex: u32,
    }

    #[repr(C)]
    struct BrowseInfoW {
        hwnd_owner: *mut c_void,
        pidl_root: *const c_void,
        psz_display_name: *mut u16,
        lpsz_title: *const u16,
        ul_flags: u32,
        lpfn: *const c_void,
        l_param: isize,
        i_image: i32,
    }

    #[link(name = "comdlg32")]
    extern "system" {
        fn GetOpenFileNameW(lpofn: *mut OpenFileNameW) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHBrowseForFolderW(lpbi: *mut BrowseInfoW) -> *mut c_void;
        fn SHGetPathFromIDListW(pidl: *const c_void, psz_path: *mut u16) -> i32;
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv_reserved: *mut c_void, dw_coinit: u32) -> i32;
        fn CoTaskMemFree(pv: *mut c_void);
    }

    fn to_wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn wide_buf_to_string(buf: &[u16]) -> Option<String> {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if len == 0 {
            None
        } else {
            Some(String::from_utf16_lossy(&buf[..len]))
        }
    }

    pub fn pick(kind: &str) -> Option<String> {
        unsafe {
            if kind == "dir" {
                let _ = CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED);
                let title = to_wide_null("选择 GPT-SoVITS 本地目录");
                let mut display = [0u16; 260];
                let mut bi = BrowseInfoW {
                    hwnd_owner: null_mut(),
                    pidl_root: null(),
                    psz_display_name: display.as_mut_ptr(),
                    lpsz_title: title.as_ptr(),
                    ul_flags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX,
                    lpfn: null(),
                    l_param: 0,
                    i_image: 0,
                };
                let pidl = SHBrowseForFolderW(&mut bi);
                if pidl.is_null() {
                    return None;
                }
                let mut path_buf = [0u16; 1024];
                let ok = SHGetPathFromIDListW(pidl, path_buf.as_mut_ptr());
                CoTaskMemFree(pidl);
                if ok == 0 {
                    return None;
                }
                return wide_buf_to_string(&path_buf);
            }

            let (title_s, filter_s): (&str, &str) = match kind {
                "ckpt" => (
                    "选择 GPT 权重 (.ckpt)",
                    "GPT 权重 (*.ckpt)\0*.ckpt\0所有文件 (*.*)\0*.*\0\0",
                ),
                "pth" => (
                    "选择 SoVITS 权重 (.pth)",
                    "SoVITS 权重 (*.pth)\0*.pth\0所有文件 (*.*)\0*.*\0\0",
                ),
                _ => (
                    "选择参考音频 (.wav / .ogg / .mp3 / .flac)",
                    "音频文件 (*.wav;*.ogg;*.mp3;*.flac)\0*.wav;*.ogg;*.mp3;*.flac\0所有文件 (*.*)\0*.*\0\0",
                ),
            };
            let title_w = to_wide_null(title_s);
            let filter_w: Vec<u16> = filter_s.encode_utf16().collect();
            let mut file_buf = [0u16; 2048];
            let mut ofn = OpenFileNameW {
                l_struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
                hwnd_owner: null_mut(),
                h_instance: null_mut(),
                lpstr_filter: filter_w.as_ptr(),
                lpstr_custom_filter: null_mut(),
                n_max_cust_filter: 0,
                n_filter_index: 1,
                lpstr_file: file_buf.as_mut_ptr(),
                n_max_file: file_buf.len() as u32,
                lpstr_file_title: null_mut(),
                n_max_file_title: 0,
                lpstr_initial_dir: null(),
                lpstr_title: title_w.as_ptr(),
                flags: OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR,
                n_file_offset: 0,
                n_file_extension: 0,
                lpstr_def_ext: null(),
                l_cust_data: 0,
                lpfn_hook: null(),
                lp_template_name: null(),
                pv_reserved: null_mut(),
                dw_reserved: 0,
                flags_ex: 0,
            };
            if GetOpenFileNameW(&mut ofn) == 0 {
                None
            } else {
                wide_buf_to_string(&file_buf)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
    }

    #[test]
    fn cache_key_changes_when_profile_changes() {
        let mut p1 = GptSovitsCharProfile {
            ref_audio_path: "C:/voices/ref1.wav".to_string(),
            prompt_text: "你好世界".to_string(),
            prompt_lang: "zh".to_string(),
            text_lang: "auto".to_string(),
            speed_factor: 1.0,
            gpt_weights_path: String::new(),
            sovits_weights_path: String::new(),
        };
        let k1 = build_cache_key("claude", "今天写代码吗？", &p1);
        p1.speed_factor = 1.15;
        let k2 = build_cache_key("claude", "今天写代码吗？", &p1);
        assert_ne!(k1, k2);
    }

    #[test]
    fn parse_api_url_handles_defaults_and_custom_port() {
        let ep = parse_api_url("http://127.0.0.1:9880/").unwrap();
        assert!(!ep.https);
        assert_eq!(ep.host, "127.0.0.1");
        assert_eq!(ep.port, 9880);
        assert_eq!(ep.base_path, "");

        let custom = parse_api_url("https://voice.local:8443/v2").unwrap();
        assert!(custom.https);
        assert_eq!(custom.host, "voice.local");
        assert_eq!(custom.port, 8443);
        assert_eq!(custom.base_path, "/v2");
    }

    #[test]
    fn normalize_cache_text_strips_trailing_punctuation() {
        assert_eq!(normalize_cache_text("你好呀～～！ "), "你好呀");
        assert_eq!(normalize_cache_text("  早安   世界。  "), "早安 世界");
    }
}
