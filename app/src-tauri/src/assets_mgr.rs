//! 角色动画资产与日语语音素材包（GitHub 同仓库 Releases 按需下载管理器）。
//!
//! 约束：
//! - 零外部 Cargo 依赖（WinHTTP 断点续传 + 系统 `tar -xf` 解压）；
//! - 本体安装包不内置 6.88GB 角色动画视频，用户按需勾选下载到可写目录；
//! - 兼容两种常见 ZIP 目录层级（直接包含 `clips/` 或包一层 `<charId>/`），解压自动规范化落盘。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const DEFAULT_GITHUB_REPO: &str = "RicardoLiuW/MoeMate";
pub const DEFAULT_RELEASE_TAG: &str = "latest";
pub const VOICE_PACK_ARCHIVE: &str = "ja-voices-pack.zip";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetsDownloadConfig {
    #[serde(default = "default_github_repo")]
    pub github_repo: String,
    #[serde(default = "default_release_tag")]
    pub release_tag: String,
    #[serde(default = "default_download_source")]
    pub download_source: String,
}

fn default_github_repo() -> String {
    DEFAULT_GITHUB_REPO.to_string()
}

fn default_release_tag() -> String {
    DEFAULT_RELEASE_TAG.to_string()
}

fn default_download_source() -> String {
    "mirror".to_string()
}

impl Default for AssetsDownloadConfig {
    fn default() -> Self {
        Self {
            github_repo: default_github_repo(),
            release_tag: default_release_tag(),
            download_source: default_download_source(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCharPackStatus {
    pub id: String,
    pub name: String,
    pub archive_name: String,
    pub approx_size_mb: u32,
    pub installed: bool,
    pub clip_count: u32,
    pub voice_ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetsStatusPayload {
    pub assets_root: String,
    pub github_repo: String,
    pub release_tag: String,
    pub download_source: String,
    pub installing: bool,
    pub voice_pack_archive: String,
    pub voice_pack_installed: bool,
    pub chars: Vec<AssetCharPackStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetsProgressPayload {
    pub active: bool,
    pub target: String,
    pub stage: String,
    pub label: String,
    pub item_index: usize,
    pub item_total: usize,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub percent: f64,
    pub speed_bps: u64,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct AssetsRuntime {
    pub installing: AtomicBool,
    pub cancel_install: AtomicBool,
}

struct CharPackDef {
    id: &'static str,
    name: &'static str,
    archive_name: &'static str,
    approx_size_mb: u32,
}

const CHAR_PACKS: &[CharPackDef] = &[
    CharPackDef {
        id: "claude",
        name: "Claude",
        archive_name: "char-claude.zip",
        approx_size_mb: 771,
    },
    CharPackDef {
        id: "deepseek",
        name: "DeepSeek",
        archive_name: "char-deepseek.zip",
        approx_size_mb: 1622,
    },
    CharPackDef {
        id: "gemini",
        name: "Gemini",
        archive_name: "char-gemini.zip",
        approx_size_mb: 580,
    },
    CharPackDef {
        id: "kimi",
        name: "Kimi",
        archive_name: "char-kimi.zip",
        approx_size_mb: 548,
    },
    CharPackDef {
        id: "grok",
        name: "Grok",
        archive_name: "char-grok.zip",
        approx_size_mb: 756,
    },
    CharPackDef {
        id: "qwen",
        name: "Qwen",
        archive_name: "char-qwen.zip",
        approx_size_mb: 645,
    },
    CharPackDef {
        id: "gpt",
        name: "GPT",
        archive_name: "char-gpt.zip",
        approx_size_mb: 1069,
    },
    CharPackDef {
        id: "glm",
        name: "GLM",
        archive_name: "char-glm.zip",
        approx_size_mb: 881,
    },
];

pub fn normalize_github_repo(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let stripped = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .unwrap_or(trimmed);
    let mut parts = stripped.split('/').filter(|s| !s.is_empty());
    match (parts.next(), parts.next()) {
        (Some(owner), Some(repo)) => format!("{owner}/{repo}"),
        _ => DEFAULT_GITHUB_REPO.to_string(),
    }
}

pub fn build_release_asset_url(
    github_repo: &str,
    release_tag: &str,
    asset_name: &str,
    download_source: &str,
) -> String {
    let repo = normalize_github_repo(github_repo);
    let tag = if release_tag.trim().is_empty() {
        DEFAULT_RELEASE_TAG
    } else {
        release_tag.trim()
    };
    let official = if tag.eq_ignore_ascii_case("latest") {
        format!("https://github.com/{repo}/releases/latest/download/{asset_name}")
    } else {
        format!("https://github.com/{repo}/releases/download/{tag}/{asset_name}")
    };
    if download_source.trim() == "official" {
        official
    } else {
        format!("https://ghfast.top/{official}")
    }
}

fn count_webm_clips(char_dir: &Path) -> u32 {
    fs::read_dir(char_dir.join("clips"))
        .map(|items| {
            items
                .flatten()
                .filter(|item| item.file_name().to_string_lossy().ends_with(".webm"))
                .count() as u32
        })
        .unwrap_or(0)
}

fn has_voice_manifest(char_dir: &Path) -> bool {
    char_dir.join("voices").join("manifest.json").is_file()
}

pub fn inspect_assets_status(
    assets_root: &Path,
    cfg: &AssetsDownloadConfig,
    runtime: &AssetsRuntime,
) -> AssetsStatusPayload {
    let chars_root = assets_root.join("chars");
    let mut chars = Vec::with_capacity(CHAR_PACKS.len());
    let mut voice_count = 0usize;

    for def in CHAR_PACKS {
        let dir = chars_root.join(def.id);
        let clip_count = count_webm_clips(&dir);
        let installed = dir.join("character.json").is_file() && clip_count > 0;
        let voice_ready = has_voice_manifest(&dir);
        if voice_ready {
            voice_count += 1;
        }
        chars.push(AssetCharPackStatus {
            id: def.id.to_string(),
            name: def.name.to_string(),
            archive_name: def.archive_name.to_string(),
            approx_size_mb: def.approx_size_mb,
            installed,
            clip_count,
            voice_ready,
        });
    }

    AssetsStatusPayload {
        assets_root: assets_root.to_string_lossy().to_string(),
        github_repo: normalize_github_repo(&cfg.github_repo),
        release_tag: if cfg.release_tag.trim().is_empty() {
            DEFAULT_RELEASE_TAG.to_string()
        } else {
            cfg.release_tag.trim().to_string()
        },
        download_source: if cfg.download_source.trim() == "official" {
            "official".to_string()
        } else {
            "mirror".to_string()
        },
        installing: runtime.installing.load(Ordering::Relaxed),
        voice_pack_archive: VOICE_PACK_ARCHIVE.to_string(),
        voice_pack_installed: voice_count == CHAR_PACKS.len(),
        chars,
    }
}

fn extract_zip_to(zip_path: &Path, dest_dir: &Path) -> Result<(), String> {
    let _ = fs::remove_dir_all(dest_dir);
    fs::create_dir_all(dest_dir)
        .map_err(|e| format!("创建临时解压目录失败 {}: {e}", dest_dir.display()))?;

    let mut cmd = Command::new("tar");
    cmd.arg("-xf").arg(zip_path).arg("-C").arg(dest_dir);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd.output().map_err(|e| format!("调用 tar 解压失败: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(format!("解压 {} 失败: {}", zip_path.display(), stderr.trim()))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|e| format!("创建目录失败 {}: {e}", dst.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("读取目录失败 {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("读取目录项失败: {e}"))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .map_err(|e| format!("复制文件失败 {} -> {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn locate_char_payload_root(stage_dir: &Path, char_id: &str) -> Option<PathBuf> {
    if stage_dir.join("character.json").is_file() || stage_dir.join("clips").is_dir() {
        return Some(stage_dir.to_path_buf());
    }
    let direct = stage_dir.join(char_id);
    if direct.join("character.json").is_file() || direct.join("clips").is_dir() {
        return Some(direct);
    }
    let nested = stage_dir.join("chars").join(char_id);
    if nested.join("character.json").is_file() || nested.join("clips").is_dir() {
        return Some(nested);
    }
    if let Ok(entries) = fs::read_dir(stage_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && (p.join("character.json").is_file() || p.join("clips").is_dir()) {
                return Some(p);
            }
        }
    }
    None
}

fn install_voice_pack_from_stage(
    stage_dir: &Path,
    assets_root: &Path,
    ref_voices_dir: &Path,
) -> Result<(), String> {
    let chars_src = if stage_dir.join("chars").is_dir() {
        stage_dir.join("chars")
    } else {
        stage_dir.to_path_buf()
    };

    let chars_dst = assets_root.join("chars");
    let mut synced_any = false;
    if let Ok(entries) = fs::read_dir(&chars_src) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "ref_voices" {
                copy_dir_recursive(&p, ref_voices_dir)?;
                synced_any = true;
                continue;
            }
            if p.join("voices").is_dir() {
                copy_dir_recursive(&p.join("voices"), &chars_dst.join(&name).join("voices"))?;
                synced_any = true;
            } else if CHAR_PACKS.iter().any(|c| c.id == name) {
                copy_dir_recursive(&p, &chars_dst.join(&name).join("voices"))?;
                synced_any = true;
            }
        }
    }

    if stage_dir.join("ref_voices").is_dir() {
        copy_dir_recursive(&stage_dir.join("ref_voices"), ref_voices_dir)?;
        synced_any = true;
    }

    if !synced_any {
        return Err("语音压缩包内未识别到有效的角色 voices 目录".to_string());
    }
    Ok(())
}

pub fn install_asset_targets(
    assets_root: &Path,
    ref_voices_dir: &Path,
    cfg: &AssetsDownloadConfig,
    targets: &[String],
    runtime: Arc<AssetsRuntime>,
    mut emit_progress: impl FnMut(AssetsProgressPayload),
) -> Result<Vec<String>, String> {
    if runtime
        .installing
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("已有资源下载任务正在进行中".to_string());
    }
    runtime.cancel_install.store(false, Ordering::SeqCst);

    struct InstallGuard(Arc<AssetsRuntime>);
    impl Drop for InstallGuard {
        fn drop(&mut self) {
            self.0.installing.store(false, Ordering::SeqCst);
        }
    }
    let _guard = InstallGuard(runtime.clone());

    let mut resolved: Vec<(&'static str, String, &'static str)> = Vec::new();
    for t in targets {
        let key = t.trim();
        if key == "voices" || key == "ja_voices" {
            resolved.push((
                "voices",
                VOICE_PACK_ARCHIVE.to_string(),
                "8 角色日语语音素材包",
            ));
        } else if let Some(def) = CHAR_PACKS.iter().find(|c| c.id == key) {
            resolved.push((def.id, def.archive_name.to_string(), def.name));
        }
    }
    if resolved.is_empty() {
        return Err("未指定有效的下载目标".to_string());
    }

    let dl_dir = assets_root.join("_downloads");
    fs::create_dir_all(&dl_dir)
        .map_err(|e| format!("无法创建下载目录 {}: {e}", dl_dir.display()))?;

    let total = resolved.len();
    let mut installed_chars = Vec::new();

    for (idx, (target_id, archive_name, display_name)) in resolved.iter().enumerate() {
        if runtime.cancel_install.load(Ordering::Relaxed) {
            emit_progress(AssetsProgressPayload {
                active: false,
                target: (*target_id).to_string(),
                stage: "cancelled".to_string(),
                label: (*display_name).to_string(),
                item_index: idx + 1,
                item_total: total,
                downloaded_bytes: 0,
                total_bytes: 0,
                percent: 0.0,
                speed_bps: 0,
                message: "已取消下载，断点进度已保留".to_string(),
            });
            return Err("用户已取消下载".to_string());
        }

        let url = build_release_asset_url(
            &cfg.github_repo,
            &cfg.release_tag,
            archive_name,
            &cfg.download_source,
        );
        let zip_path = dl_dir.join(archive_name);

        #[cfg(windows)]
        {
            let (https, host, port, path) = crate::gpt_sovits::parse_full_url(&url)?;
            let label = format!("{}（{}）", display_name, archive_name);
            let res = crate::ai_voice::winhttp_client::download_file_resumable(
                https,
                &host,
                port,
                &path,
                &zip_path,
                &runtime.cancel_install,
                |downloaded, total_bytes, speed_bps| {
                    let pct = if total_bytes > 0 {
                        ((downloaded as f64) / (total_bytes as f64) * 100.0).clamp(0.0, 100.0)
                    } else {
                        0.0
                    };
                    emit_progress(AssetsProgressPayload {
                        active: true,
                        target: (*target_id).to_string(),
                        stage: "downloading".to_string(),
                        label: label.clone(),
                        item_index: idx + 1,
                        item_total: total,
                        downloaded_bytes: downloaded,
                        total_bytes,
                        percent: pct,
                        speed_bps,
                        message: format!("正在下载 ({}/{}): {}", idx + 1, total, label),
                    });
                },
            );
            if let Err(err) = res {
                let cancelled = runtime.cancel_install.load(Ordering::Relaxed);
                emit_progress(AssetsProgressPayload {
                    active: false,
                    target: (*target_id).to_string(),
                    stage: if cancelled { "cancelled" } else { "error" }.to_string(),
                    label,
                    item_index: idx + 1,
                    item_total: total,
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
            let _ = (url, zip_path);
            return Err("当前仅支持 Windows 平台下载安装".to_string());
        }

        emit_progress(AssetsProgressPayload {
            active: true,
            target: (*target_id).to_string(),
            stage: "extracting".to_string(),
            label: (*display_name).to_string(),
            item_index: idx + 1,
            item_total: total,
            downloaded_bytes: 0,
            total_bytes: 0,
            percent: 100.0,
            speed_bps: 0,
            message: format!("正在解压安装：{}…", display_name),
        });

        let stage_dir = dl_dir.join(format!("_stage_{target_id}"));
        extract_zip_to(&zip_path, &stage_dir)?;

        if *target_id == "voices" {
            install_voice_pack_from_stage(&stage_dir, assets_root, ref_voices_dir)?;
        } else {
            let src_char = locate_char_payload_root(&stage_dir, target_id)
                .ok_or_else(|| format!("压缩包 {archive_name} 内未找到 {target_id} 角色动画文件"))?;
            let dst_char = assets_root.join("chars").join(target_id);
            copy_dir_recursive(&src_char, &dst_char)?;
            installed_chars.push((*target_id).to_string());
        }

        let _ = fs::remove_dir_all(&stage_dir);
        let _ = fs::remove_file(&zip_path);
    }

    let _ = fs::remove_dir_all(&dl_dir);
    emit_progress(AssetsProgressPayload {
        active: false,
        target: targets.join(","),
        stage: "done".to_string(),
        label: String::new(),
        item_index: total,
        item_total: total,
        downloaded_bytes: 0,
        total_bytes: 0,
        percent: 100.0,
        speed_bps: 0,
        message: "角色与语音资源包已下载并生效！".to_string(),
    });
    Ok(installed_chars)
}

pub fn delete_char_assets(assets_root: &Path, char_id: &str) -> Result<(), String> {
    if !CHAR_PACKS.iter().any(|c| c.id == char_id) {
        return Err("无效的角色 ID".to_string());
    }
    let dir = assets_root.join("chars").join(char_id);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("清理角色目录失败: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_url_builds_latest_and_tag_variants() {
        assert_eq!(
            build_release_asset_url("https://github.com/alice/MoeMate.git", "latest", "char-gemini.zip", "official"),
            "https://github.com/alice/MoeMate/releases/latest/download/char-gemini.zip"
        );
        assert_eq!(
            build_release_asset_url("alice/MoeMate", "v1.0.0", "ja-voices-pack.zip", "mirror"),
            "https://ghfast.top/https://github.com/alice/MoeMate/releases/download/v1.0.0/ja-voices-pack.zip"
        );
    }
}
