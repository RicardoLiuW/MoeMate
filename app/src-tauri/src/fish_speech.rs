use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::Emitter;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const DEFAULT_API_URL: &str = "http://127.0.0.1:8080";
pub const DEFAULT_DECODER_CONFIG: &str = "modded_dac_vq";
pub const DEFAULT_CHUNK_LENGTH: u32 = 200;
pub const DEFAULT_TOP_P: f32 = 0.8;
pub const DEFAULT_TEMPERATURE: f32 = 0.8;
pub const DEFAULT_REPETITION_PENALTY: f32 = 1.1;
pub const DEFAULT_MAX_NEW_TOKENS: u32 = 1024;
pub const DEFAULT_TIMEOUT_SECS: u64 = 45;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FishSpeechCharProfile {
    #[serde(default)]
    pub ref_audio_path: String,
    #[serde(default)]
    pub prompt_text: String,
    #[serde(default = "default_prompt_lang")]
    pub prompt_lang: String,
    #[serde(default = "default_text_lang")]
    pub text_lang: String,
    #[serde(default)]
    pub reference_id: String,
}

fn default_prompt_lang() -> String {
    "zh".to_string()
}

fn default_text_lang() -> String {
    "ja".to_string()
}

impl Default for FishSpeechCharProfile {
    fn default() -> Self {
        Self {
            ref_audio_path: String::new(),
            prompt_text: String::new(),
            prompt_lang: default_prompt_lang(),
            text_lang: default_text_lang(),
            reference_id: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FishSpeechConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_install_dir")]
    pub install_dir: String,
    #[serde(default = "default_api_url")]
    pub api_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub checkpoint_dir: String,
    #[serde(default)]
    pub decoder_checkpoint: String,
    #[serde(default = "default_decoder_config")]
    pub decoder_config: String,
    #[serde(default = "default_device")]
    pub device: String,
    #[serde(default = "default_half")]
    pub half: bool,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_chunk_length")]
    pub chunk_length: u32,
    #[serde(default = "default_top_p")]
    pub top_p: f32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_repetition_penalty")]
    pub repetition_penalty: f32,
    #[serde(default = "default_max_new_tokens")]
    pub max_new_tokens: u32,
    #[serde(default = "default_memory_cache")]
    pub use_memory_cache: String,
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
    pub char_profiles: BTreeMap<String, FishSpeechCharProfile>,
}

fn default_install_dir() -> String {
    default_install_path().to_string_lossy().to_string()
}

fn default_api_url() -> String {
    DEFAULT_API_URL.to_string()
}

fn default_decoder_config() -> String {
    DEFAULT_DECODER_CONFIG.to_string()
}

fn default_device() -> String {
    "auto".to_string()
}

fn default_half() -> bool {
    true
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

fn default_chunk_length() -> u32 {
    DEFAULT_CHUNK_LENGTH
}

fn default_top_p() -> f32 {
    DEFAULT_TOP_P
}

fn default_temperature() -> f32 {
    DEFAULT_TEMPERATURE
}

fn default_repetition_penalty() -> f32 {
    DEFAULT_REPETITION_PENALTY
}

fn default_max_new_tokens() -> u32 {
    DEFAULT_MAX_NEW_TOKENS
}

fn default_memory_cache() -> String {
    "on".to_string()
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

pub fn default_install_path() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.trim().is_empty() {
            return PathBuf::from(appdata)
                .join("Deskpet")
                .join("runtime_engines")
                .join("fish-speech");
        }
    }
    PathBuf::from("runtime_engines").join("fish-speech")
}

impl Default for FishSpeechConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            install_dir: default_install_dir(),
            api_url: default_api_url(),
            api_key: String::new(),
            checkpoint_dir: String::new(),
            decoder_checkpoint: String::new(),
            decoder_config: default_decoder_config(),
            device: default_device(),
            half: default_half(),
            timeout_secs: default_timeout_secs(),
            chunk_length: default_chunk_length(),
            top_p: default_top_p(),
            temperature: default_temperature(),
            repetition_penalty: default_repetition_penalty(),
            max_new_tokens: default_max_new_tokens(),
            use_memory_cache: default_memory_cache(),
            download_source: default_download_source(),
            gpu_backend: default_gpu_backend(),
            auto_start_service: false,
            auto_stop_on_exit: true,
            daily_dynamic_voice: true,
            char_profiles: BTreeMap::new(),
        }
    }
}

impl FishSpeechConfig {
    /// Migrate legacy GPT-SoVITS paths and ports to Fish Speech defaults.
    pub fn migrate_legacy_defaults(&mut self) {
        let dir_lower = self.install_dir.to_ascii_lowercase();
        if self.install_dir.trim().is_empty() || dir_lower.contains("gpt-sovits") {
            self.install_dir = default_install_dir();
        }
        let api_trim = self.api_url.trim();
        if api_trim.is_empty()
            || api_trim == "http://127.0.0.1:9880"
            || api_trim == "http://localhost:9880"
        {
            self.api_url = DEFAULT_API_URL.to_string();
        }
        if self.decoder_config.trim().is_empty() {
            self.decoder_config = DEFAULT_DECODER_CONFIG.to_string();
        }
        if self.device.trim().is_empty() {
            self.device = "auto".to_string();
        }
        if self.timeout_secs == 0 {
            self.timeout_secs = DEFAULT_TIMEOUT_SECS;
        }
        if self.chunk_length == 0 {
            self.chunk_length = DEFAULT_CHUNK_LENGTH;
        }
        if self.max_new_tokens == 0 {
            self.max_new_tokens = DEFAULT_MAX_NEW_TOKENS;
        }
        if self.use_memory_cache.trim().is_empty() {
            self.use_memory_cache = "on".to_string();
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FishSpeechStatusPayload {
    pub state: String,
    pub label: String,
    pub installed: bool,
    pub python_ready: bool,
    pub model_ready: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub api_url: String,
    pub install_dir: String,
    pub checkpoint_dir: String,
    pub detected_checkpoints: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FishSpeechProgressPayload {
    pub active: bool,
    pub stage: String,
    pub message: String,
    pub percent: f32,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
    pub done: bool,
    pub error: Option<String>,
}

pub struct FishSpeechRuntime {
    child: Mutex<Option<Child>>,
    last_error: Mutex<Option<String>>,
    pub installing: AtomicBool,
    pub cancel_install: AtomicBool,
    pub last_used_ms: AtomicI64,
}

impl FishSpeechRuntime {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            last_error: Mutex::new(None),
            installing: AtomicBool::new(false),
            cancel_install: AtomicBool::new(false),
            last_used_ms: AtomicI64::new(0),
        }
    }

    pub fn touch_used(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.last_used_ms.store(now, Ordering::Relaxed);
    }

    pub fn is_running(&self) -> bool {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(None) => return true,
                Ok(Some(_)) | Err(_) => {
                    *guard = None;
                }
            }
        }
        false
    }

    pub fn current_pid(&self) -> Option<u32> {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(None) => return Some(child.id()),
                Ok(Some(_)) | Err(_) => {
                    *guard = None;
                }
            }
        }
        None
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_last_error(&self, err: Option<String>) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = err;
    }

    pub fn last_used(&self) -> i64 {
        self.last_used_ms.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut child) = guard.take() {
            let pid = child.id();
            let _ = child.kill();
            let _ = child.wait();
            #[cfg(target_os = "windows")]
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
            }
        }
    }
}

pub fn parse_full_url(raw: &str) -> Result<(bool, String, u16, String), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL 不能为空".to_string());
    }
    let (https, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        return Err("仅支持 http:// 或 https://".to_string());
    };
    let (host_port, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let default_port = if https { 443 } else { 8080 };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (
            h.trim().to_string(),
            p.trim().parse::<u16>().unwrap_or(default_port),
        ),
        None => (host_port.trim().to_string(), default_port),
    };
    if host.is_empty() {
        return Err("URL 缺少主机名".to_string());
    }
    Ok((https, host, port, path.to_string()))
}

fn find_python_in_install(install_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        install_dir.join(".venv").join("Scripts").join("python.exe"),
        install_dir.join(".venv").join("bin").join("python"),
        install_dir.join("python.exe"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn checkpoint_priority_names() -> [&'static str; 3] {
    ["openaudio-s1-mini", "s2-pro", "fish-speech-1.5"]
}

fn is_valid_checkpoint_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let has_model = dir.join("model.pth").is_file()
        || dir.join("model.safetensors").is_file()
        || dir.join("config.json").is_file();
    let has_codec = dir.join("codec.pth").is_file()
        || dir.join("firefly-gan-vq-fsq-8x1024-21hz-generator.pth").is_file();
    has_model && has_codec
}

fn resolve_decoder_in_dir(ckpt_dir: &Path) -> Option<(PathBuf, String)> {
    let codec = ckpt_dir.join("codec.pth");
    if codec.is_file() {
        return Some((codec, DEFAULT_DECODER_CONFIG.to_string()));
    }
    let legacy = ckpt_dir.join("firefly-gan-vq-fsq-8x1024-21hz-generator.pth");
    if legacy.is_file() {
        return Some((legacy, "firefly_gan_vq".to_string()));
    }
    None
}

pub fn list_detected_checkpoints(install_dir: &Path) -> Vec<String> {
    let ckpts_root = install_dir.join("checkpoints");
    let mut found = Vec::new();
    for name in checkpoint_priority_names() {
        let dir = ckpts_root.join(name);
        if is_valid_checkpoint_dir(&dir) {
            found.push(name.to_string());
        }
    }
    if let Ok(entries) = fs::read_dir(&ckpts_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if !found.iter().any(|x| x == name) && is_valid_checkpoint_dir(&path) {
                    found.push(name.to_string());
                }
            }
        }
    }
    found
}

pub fn detect_checkpoint(install_dir: &Path) -> Option<(PathBuf, PathBuf, String)> {
    let ckpts_root = install_dir.join("checkpoints");
    for name in checkpoint_priority_names() {
        let dir = ckpts_root.join(name);
        if is_valid_checkpoint_dir(&dir) {
            if let Some((dec, cfg)) = resolve_decoder_in_dir(&dir) {
                return Some((dir, dec, cfg));
            }
        }
    }
    if let Ok(entries) = fs::read_dir(&ckpts_root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if is_valid_checkpoint_dir(&dir) {
                if let Some((dec, cfg)) = resolve_decoder_in_dir(&dir) {
                    return Some((dir, dec, cfg));
                }
            }
        }
    }
    None
}

pub fn check_api_health(api_url: &str) -> Result<bool, String> {
    let (_, host, port, _) = parse_full_url(api_url)?;
    let addr = format!("{host}:{port}");
    let mut stream = match TcpStream::connect_timeout(
        &addr.parse().map_err(|e| format!("无效地址 {addr}: {e}"))?,
        Duration::from_secs(2),
    ) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let req = format!("GET /v1/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return Ok(false);
    }
    let mut buf = [0u8; 256];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => {
            let head = String::from_utf8_lossy(&buf[..n]);
            Ok(head.contains("200 OK"))
        }
        _ => Ok(false),
    }
}

pub fn inspect_status(
    cfg: &FishSpeechConfig,
    rt: &FishSpeechRuntime,
) -> FishSpeechStatusPayload {
    let install_dir = PathBuf::from(&cfg.install_dir);
    let installed = install_dir.join("tools").join("api_server.py").is_file();
    let python_ready = find_python_in_install(&install_dir).is_some();
    let detected = list_detected_checkpoints(&install_dir);
    let custom_ckpt = PathBuf::from(&cfg.checkpoint_dir);
    let model_ready = if !cfg.checkpoint_dir.trim().is_empty() && is_valid_checkpoint_dir(&custom_ckpt) {
        true
    } else {
        !detected.is_empty()
    };
    let running = rt.is_running() || check_api_health(&cfg.api_url).unwrap_or(false);
    let active_ckpt = if !cfg.checkpoint_dir.trim().is_empty() {
        cfg.checkpoint_dir.clone()
    } else if let Some((dir, _, _)) = detect_checkpoint(&install_dir) {
        dir.to_string_lossy().to_string()
    } else {
        String::new()
    };

    let (state, label) = if rt.installing.load(Ordering::Relaxed) {
        ("installing".to_string(), "安装部署中".to_string())
    } else if running {
        ("running".to_string(), "运行中".to_string())
    } else if installed && python_ready && model_ready {
        ("ready".to_string(), "已就绪（未启动）".to_string())
    } else {
        ("uninstalled".to_string(), "未安装".to_string())
    };

    FishSpeechStatusPayload {
        state,
        label,
        installed,
        python_ready,
        model_ready,
        running,
        pid: rt.current_pid(),
        api_url: cfg.api_url.clone(),
        install_dir: cfg.install_dir.clone(),
        checkpoint_dir: active_ckpt,
        detected_checkpoints: detected,
        error: rt.last_error(),
    }
}

fn emit_progress(
    app: &tauri::AppHandle,
    stage: &str,
    message: &str,
    percent: f32,
    done: bool,
    error: Option<String>,
) {
    let _ = app.emit(
        "pet_fish_speech_progress",
        FishSpeechProgressPayload {
            active: !done,
            stage: stage.to_string(),
            message: message.to_string(),
            percent,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed_bps: 0,
            done,
            error,
        },
    );
}

fn run_cmd_checked(cmd: &mut Command, label: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd
        .output()
        .map_err(|e| format!("{label} 启动失败: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        return Err(format!("{label} 失败 ({:?}): {msg}", out.status.code()));
    }
    Ok(())
}

pub fn install_engine(
    app: &tauri::AppHandle,
    cfg: &FishSpeechConfig,
    rt: &FishSpeechRuntime,
) -> Result<FishSpeechStatusPayload, String> {
    rt.set_last_error(None);
    let install_dir = PathBuf::from(&cfg.install_dir);
    if let Some(parent) = install_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }

    emit_progress(app, "clone", "正在准备 Fish Speech 源码...", 10.0, false, None);
    if !install_dir.join("tools").join("api_server.py").is_file() {
        if install_dir.exists() && !install_dir.join(".git").exists() {
            let _ = fs::remove_dir_all(&install_dir);
        }
        run_cmd_checked(
            Command::new("git").args([
                "clone",
                "--depth",
                "1",
                "https://github.com/fishaudio/fish-speech.git",
                &install_dir.to_string_lossy(),
            ]),
            "git clone fish-speech",
        )?;
    }

    emit_progress(app, "venv", "正在检查 Python 虚拟环境...", 30.0, false, None);
    if find_python_in_install(&install_dir).is_none() {
        run_cmd_checked(
            Command::new("python").args([
                "-m",
                "venv",
                &install_dir.join(".venv").to_string_lossy(),
            ]),
            "创建 .venv",
        )?;
    }

    let py = find_python_in_install(&install_dir)
        .ok_or_else(|| "未能找到 Fish Speech .venv Python".to_string())?;

    emit_progress(app, "deps", "正在安装 Fish Speech Python 依赖...", 55.0, false, None);
    run_cmd_checked(
        Command::new(&py)
            .current_dir(&install_dir)
            .args(["-m", "pip", "install", "--no-deps", "-e", "."]),
        "pip install -e .",
    )?;

    if detect_checkpoint(&install_dir).is_none() {
        emit_progress(
            app,
            "weights",
            "正在下载 OpenAudio S1-mini 权重...",
            75.0,
            false,
            None,
        );
        let ckpt_dir = install_dir.join("checkpoints").join("openaudio-s1-mini");
        fs::create_dir_all(&ckpt_dir).map_err(|e| format!("创建权重目录失败: {e}"))?;
        let script = r#"
import pathlib, sys, urllib.request
dst_dir = pathlib.Path(sys.argv[1])
files = ['config.json', 'configuration.json', 'special_tokens.json', 'tokenizer.tiktoken', 'model.pth', 'codec.pth']
for name in files:
    dst = dst_dir / name
    if dst.is_file() and dst.stat().st_size > 50:
        continue
    url = f'https://www.modelscope.cn/models/fishaudio/openaudio-s1-mini/resolve/master/{name}'
    tmp = dst_dir / (name + '.part')
    req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
    with urllib.request.urlopen(req, timeout=300) as r, open(tmp, 'wb') as f:
        while True:
            buf = r.read(1024 * 1024)
            if not buf: break
            f.write(buf)
    tmp.replace(dst)
"#;
        run_cmd_checked(
            Command::new(&py).args(["-c", script, &ckpt_dir.to_string_lossy()]),
            "下载 OpenAudio S1-mini",
        )?;
    }

    emit_progress(app, "done", "Fish Speech 部署完成", 100.0, true, None);
    Ok(inspect_status(cfg, rt))
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
        return Some(fallback);
    }
    None
}

fn ensure_zluda_torch_dlls(install_dir: &Path) {
    let zluda_dir = install_dir.join("zluda");
    let torch_lib = install_dir
        .join(".venv")
        .join("Lib")
        .join("site-packages")
        .join("torch")
        .join("lib");
    if !zluda_dir.is_dir() || !torch_lib.is_dir() {
        return;
    }
    let pairs = [
        ("cublas.dll", "cublas64_11.dll"),
        ("cufft.dll", "cufft64_10.dll"),
        ("cufftw.dll", "cufftw64_10.dll"),
        ("cusparse.dll", "cusparse64_11.dll"),
        ("nvrtc.dll", "nvrtc64_112_0.dll"),
    ];
    for (src_name, dst_name) in pairs {
        let src = zluda_dir.join(src_name);
        let dst = torch_lib.join(dst_name);
        if src.is_file() && dst.is_file() {
            if let Ok(meta) = fs::metadata(&dst) {
                if meta.len() > 1_000_000 {
                    let _ = fs::copy(&src, &dst);
                }
            }
        }
    }
}

pub fn start_service(
    _app: &tauri::AppHandle,
    cfg: &FishSpeechConfig,
    rt: &FishSpeechRuntime,
) -> Result<FishSpeechStatusPayload, String> {
    rt.set_last_error(None);
    if check_api_health(&cfg.api_url).unwrap_or(false) {
        return Ok(inspect_status(cfg, rt));
    }

    let install_dir = PathBuf::from(&cfg.install_dir);
    let api_script = install_dir.join("tools").join("api_server.py");
    if !api_script.is_file() {
        return Err(format!("未找到 Fish Speech api_server.py: {}", api_script.display()));
    }
    let py = find_python_in_install(&install_dir)
        .ok_or_else(|| "未找到 Fish Speech Python 虚拟环境 (.venv)".to_string())?;

    let (ckpt_dir, dec_path, dec_cfg) = if !cfg.checkpoint_dir.trim().is_empty() {
        let dir = PathBuf::from(&cfg.checkpoint_dir);
        let dec = if !cfg.decoder_checkpoint.trim().is_empty() {
            PathBuf::from(&cfg.decoder_checkpoint)
        } else {
            resolve_decoder_in_dir(&dir)
                .map(|(p, _)| p)
                .ok_or_else(|| "未找到解码器权重 codec.pth".to_string())?
        };
        let dcfg = if cfg.decoder_config.trim().is_empty() {
            DEFAULT_DECODER_CONFIG.to_string()
        } else {
            cfg.decoder_config.clone()
        };
        (dir, dec, dcfg)
    } else {
        detect_checkpoint(&install_dir)
            .ok_or_else(|| "未检测到可用 Fish Speech 模型权重，请先下载 OpenAudio S1-mini".to_string())?
    };

    let (_, host, port, _) = parse_full_url(&cfg.api_url)?;
    let listen = format!("{host}:{port}");

    let device = match cfg.device.trim() {
        "" | "auto" => {
            if install_dir.join("zluda").join("zluda.exe").is_file() {
                "cuda"
            } else {
                "cuda"
            }
        }
        other => other,
    };

    let zluda_exe = install_dir.join("zluda").join("zluda.exe");
    let use_zluda = zluda_exe.is_file() && device == "cuda";
    let mut cmd = if use_zluda {
        let mut c = Command::new(&zluda_exe);
        c.arg("--").arg(&py);
        c
    } else {
        Command::new(&py)
    };

    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.env("MIOPEN_FIND_MODE", "FAST");
    if use_zluda {
        ensure_zluda_torch_dlls(&install_dir);
        let mut extra_paths = vec![install_dir.join("zluda")];
        if let Some(rocm_bin) = find_rocm_bin() {
            extra_paths.push(rocm_bin);
        }
        let orig_path = std::env::var_os("PATH").unwrap_or_default();
        if let Ok(merged) = std::env::join_paths(
            extra_paths
                .into_iter()
                .chain(std::env::split_paths(&orig_path)),
        ) {
            cmd.env("PATH", merged);
        }
        let zluda_cache = install_dir.join("zluda_cache");
        let _ = fs::create_dir_all(&zluda_cache);
        cmd.env("ZLUDA_CACHE_DIR", zluda_cache);
        cmd.env("DISABLE_ADDMM_CUDA_LT", "1");
        cmd.env("CUDAGraph", "0");
    }

    let log_path = install_dir.join("deskpet_fish_speech.log");
    let (stdout_cfg, stderr_cfg) = match fs::File::create(&log_path) {
        Ok(f) => {
            let f2 = f.try_clone().ok();
            (
                Stdio::from(f),
                f2.map(Stdio::from).unwrap_or_else(Stdio::null),
            )
        }
        Err(_) => (Stdio::null(), Stdio::null()),
    };

    cmd.current_dir(&install_dir)
        .arg(&api_script)
        .args([
            "--listen",
            &listen,
            "--llama-checkpoint-path",
            &ckpt_dir.to_string_lossy(),
            "--decoder-checkpoint-path",
            &dec_path.to_string_lossy(),
            "--decoder-config-name",
            &dec_cfg,
            "--device",
            device,
        ])
        .stdout(stdout_cfg)
        .stderr(stderr_cfg);

    if cfg.half {
        cmd.arg("--half");
    }
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd.spawn().map_err(|e| {
        let msg = format!("启动 Fish Speech 服务失败: {e}");
        rt.set_last_error(Some(msg.clone()));
        msg
    })?;

    {
        let mut guard = rt.child.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(child);
    }

    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if !rt.is_running() {
            let msg = "Fish Speech 进程意外退出，请检查显卡驱动或端口占用".to_string();
            rt.set_last_error(Some(msg.clone()));
            return Err(msg);
        }
        if check_api_health(&cfg.api_url).unwrap_or(false) {
            return Ok(inspect_status(cfg, rt));
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let msg = "等待 Fish Speech /v1/health 就绪超时 (60s)".to_string();
    rt.set_last_error(Some(msg.clone()));
    Err(msg)
}

pub fn stop_service(
    cfg: &FishSpeechConfig,
    rt: &FishSpeechRuntime,
) -> Result<FishSpeechStatusPayload, String> {
    rt.stop();
    Ok(inspect_status(cfg, rt))
}

pub fn build_tts_request_json(
    cfg: &FishSpeechConfig,
    text: &str,
    profile: Option<&FishSpeechCharProfile>,
) -> Result<serde_json::Value, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("TTS 文本为空".to_string());
    }

    let mut references = Vec::new();
    let mut reference_id: Option<String> = None;

    if let Some(p) = profile {
        let ref_id = p.reference_id.trim();
        if !ref_id.is_empty() {
            reference_id = Some(ref_id.to_string());
        } else {
            let audio_path = p.ref_audio_path.trim();
            if !audio_path.is_empty() {
                let bytes = fs::read(audio_path)
                    .map_err(|e| format!("读取参考音频失败 ({audio_path}): {e}"))?;
                let b64 = crate::ai_voice::base64_encode(&bytes);
                references.push(serde_json::json!({
                    "audio": b64,
                    "text": p.prompt_text.trim(),
                }));
            }
        }
    }

    Ok(serde_json::json!({
        "text": trimmed,
        "format": "wav",
        "references": references,
        "reference_id": reference_id,
        "chunk_length": cfg.chunk_length.max(100),
        "normalize": true,
        "streaming": false,
        "use_memory_cache": if cfg.use_memory_cache.trim().is_empty() { "on" } else { cfg.use_memory_cache.trim() },
        "max_new_tokens": cfg.max_new_tokens.max(128),
        "top_p": cfg.top_p,
        "repetition_penalty": cfg.repetition_penalty,
        "temperature": cfg.temperature,
    }))
}

fn decode_chunked_body(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < raw.len() {
        let line_end = raw[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "分块响应格式无效".to_string())?;
        let size_str = std::str::from_utf8(&raw[pos..pos + line_end])
            .map_err(|_| "分块大小非 UTF-8".to_string())?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|e| format!("解析分块大小失败 ({size_str}): {e}"))?;
        pos += line_end + 2;
        if size == 0 {
            break;
        }
        if pos + size > raw.len() {
            return Err("分块响应数据不完整".to_string());
        }
        out.extend_from_slice(&raw[pos..pos + size]);
        pos += size + 2;
    }
    Ok(out)
}

pub fn synthesize_fish_speech_wav(
    cfg: &FishSpeechConfig,
    text: &str,
    profile: Option<&FishSpeechCharProfile>,
) -> Result<Vec<u8>, String> {
    let (_, host, port, _) = parse_full_url(&cfg.api_url)?;
    let addr = format!("{host}:{port}");
    let payload = build_tts_request_json(cfg, text, profile)?;
    let body_bytes = serde_json::to_vec(&payload).map_err(|e| format!("序列化 TTS 请求失败: {e}"))?;
    let timeout = Duration::from_secs(cfg.timeout_secs.max(5));

    let mut stream = TcpStream::connect_timeout(
        &addr.parse().map_err(|e| format!("无效地址 {addr}: {e}"))?,
        Duration::from_secs(5),
    )
    .map_err(|e| format!("连接 Fish Speech ({addr}) 失败: {e}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    let auth_header = if !cfg.api_key.trim().is_empty() {
        format!("Authorization: Bearer {}\r\n", cfg.api_key.trim())
    } else {
        String::new()
    };
    let req = format!(
        "POST /v1/tts HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{auth_header}Connection: close\r\n\r\n",
        body_bytes.len()
    );
    stream
        .write_all(req.as_bytes())
        .and_then(|_| stream.write_all(&body_bytes))
        .map_err(|e| format!("发送 Fish Speech /v1/tts 请求失败: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("读取 Fish Speech 响应失败: {e}"))?;
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "Fish Speech 返回无效 HTTP 响应".to_string())?;
    let headers = String::from_utf8_lossy(&raw[..header_end]);
    let status_line = headers.lines().next().unwrap_or("");
    if !status_line.contains("200") {
        let err_body = String::from_utf8_lossy(&raw[header_end + 4..]);
        return Err(format!("Fish Speech {status_line}: {err_body}"));
    }
    let body_raw = &raw[header_end + 4..];
    let body = if headers.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        decode_chunked_body(body_raw)?
    } else {
        body_raw.to_vec()
    };
    if body.len() <= 44 || !body.starts_with(b"RIFF") {
        return Err(format!("Fish Speech 返回无效 WAV 音频 ({} bytes)", body.len()));
    }
    Ok(body)
}

pub fn pick_file_or_dir(kind: &str) -> Option<String> {
    let script = match kind {
        "dir" => "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; if($d.ShowDialog() -eq 'OK'){ $d.SelectedPath }",
        _ => "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; if($d.ShowDialog() -eq 'OK'){ $d.FileName }",
    };
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-Command", script]);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_legacy_defaults_updates_gpt_sovits_values() {
        let mut cfg = FishSpeechConfig {
            install_dir: r"C:\Users\Qiu\AppData\Roaming\Deskpet\runtime_engines\gpt-sovits".into(),
            api_url: "http://127.0.0.1:9880".into(),
            ..Default::default()
        };
        cfg.migrate_legacy_defaults();
        assert!(cfg.install_dir.to_ascii_lowercase().contains("fish-speech"));
        assert_eq!(cfg.api_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn build_tts_request_json_includes_reference_id() {
        let cfg = FishSpeechConfig::default();
        let profile = FishSpeechCharProfile {
            reference_id: "char_neko".into(),
            ..Default::default()
        };
        let body = build_tts_request_json(&cfg, "こんにちは", Some(&profile)).unwrap();
        assert_eq!(body["text"], "こんにちは");
        assert_eq!(body["format"], "wav");
        assert_eq!(body["reference_id"], "char_neko");
    }
}
