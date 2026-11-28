#!/usr/bin/env python3
"""
为 MoeMate 8 位角色批量生成本地双语情绪语音包（voices/zh/*.ogg + voices/ja/*.ogg）：
- 中文语音（--lang zh）：保留并迁移现有 18 条 Azure HD 中文 .ogg，增量补齐至 32 条/角色；
- 日语语音（--lang ja）：调用本地 Fish Speech v2ProPlus API（默认 http://127.0.0.1:8080）+
  ffmpeg loudnorm（-16 LUFS）编码为 24kHz 单声道 Opus .ogg（32 条/角色）。

安全原则：
- API Key 仅通过 `--key-file`、环境变量 `AZURE_SPEECH_KEY` 或 `--from-db`（本机 SQLite）读取，绝不写入代码、manifest 或日志。
"""

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from xml.sax.saxutils import escape


def default_assets_chars_dir(repo_root: Path) -> Path:
    dev_dir = repo_root / "assets" / "chars"
    if dev_dir.is_dir():
        return dev_dir
    appdata = os.environ.get("APPDATA", "")
    if appdata:
        return Path(appdata) / "Deskpet" / "assets" / "chars"
    return dev_dir


def load_db_chat_config() -> dict:
    appdata = os.environ.get("APPDATA", "")
    if not appdata:
        return {}
    db_path = Path(appdata) / "Deskpet" / "deskpet.db"
    if not db_path.is_file():
        return {}
    try:
        conn = sqlite3.connect(str(db_path))
        row = conn.execute("SELECT value FROM settings WHERE key='chatAiConfig'").fetchone()
        conn.close()
        if row and row[0]:
            return json.loads(row[0])
    except Exception:
        pass
    return {}


def load_api_key(key_file: str | None, db_cfg: dict) -> str:
    env_key = os.environ.get("AZURE_SPEECH_KEY", "").strip()
    if env_key:
        return env_key
    if key_file:
        raw = Path(key_file).read_text(encoding="utf-8-sig")
        tokens = re.findall(r"[A-Za-z0-9]{24,128}", raw)
        if tokens:
            return tokens[0]
    db_key = str(db_cfg.get("azureSpeechKey", "")).strip()
    if db_key:
        return db_key
    raise SystemExit("错误：未找到 Azure Speech Key，请通过 --key-file、AZURE_SPEECH_KEY 或 --from-db 提供。")


def build_ssml(voice: str, style: str, rate: str, pitch: str, text: str) -> bytes:
    safe_text = escape(text)
    if style:
        inner = (
            f'<mstts:express-as style="{escape(style)}">'
            f'<prosody rate="{escape(rate)}" pitch="{escape(pitch)}">{safe_text}</prosody>'
            f"</mstts:express-as>"
        )
    else:
        inner = f'<prosody rate="{escape(rate)}" pitch="{escape(pitch)}">{safe_text}</prosody>'

    ssml = (
        '<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" '
        'xmlns:mstts="https://www.w3.org/2001/mstts" xml:lang="zh-CN">'
        f'<voice name="{escape(voice)}">{inner}</voice>'
        "</speak>"
    )
    return ssml.encode("utf-8")


def synthesize_azure_ogg(region: str, key: str, ssml: bytes, retries: int = 3) -> bytes:
    url = f"https://{region}.tts.speech.microsoft.com/cognitiveservices/v1"
    for attempt in range(1, retries + 1):
        req = urllib.request.Request(
            url,
            data=ssml,
            headers={
                "Ocp-Apim-Subscription-Key": key,
                "Content-Type": "application/ssml+xml",
                "X-Microsoft-OutputFormat": "ogg-24khz-16bit-mono-opus",
                "User-Agent": "Deskpet-VoicePack-Generator",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=25) as resp:
                data = resp.read()
                if not data.startswith(b"OggS"):
                    raise RuntimeError(f"返回音频非 OggS 格式 ({len(data)} bytes)")
                return data
        except urllib.error.HTTPError as exc:
            if exc.code == 429 and attempt < retries:
                time.sleep(1.5 * attempt)
                continue
            body = exc.read().decode("utf-8", errors="ignore")[:160]
            raise RuntimeError(f"HTTP {exc.code} {exc.reason}: {body}") from exc
        except Exception:
            if attempt < retries:
                time.sleep(1.0 * attempt)
                continue
            raise
    raise RuntimeError("重试耗尽")


def synthesize_fish_speech_ogg(
    api_url: str,
    ja_text: str,
    ref_audio_path: str,
    prompt_text: str,
    out_ogg: Path,
) -> None:
    endpoint = f"{api_url.rstrip('/')}/v1/tts"
    ref_b64 = base64.b64encode(Path(ref_audio_path).read_bytes()).decode("ascii")
    payload = json.dumps(
        {
            "text": ja_text,
            "format": "wav",
            "references": [{"audio": ref_b64, "text": prompt_text}],
            "chunk_length": 200,
            "normalize": True,
            "streaming": False,
            "use_memory_cache": "on",
            "max_new_tokens": 1024,
            "top_p": 0.8,
            "repetition_penalty": 1.1,
            "temperature": 0.8,
        }
    ).encode("utf-8")
    req = urllib.request.Request(
        endpoint,
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    host = (urllib.parse.urlparse(endpoint).hostname or "").lower()
    if host in ("127.0.0.1", "localhost", "::1"):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        resp_ctx = opener.open(req, timeout=120)
    else:
        resp_ctx = urllib.request.urlopen(req, timeout=120)
    with resp_ctx as resp:
        wav_bytes = resp.read()
    if len(wav_bytes) < 512 or not wav_bytes.startswith(b"RIFF"):
        raise RuntimeError(f"Fish Speech 返回无效 WAV 音频 ({len(wav_bytes)} bytes)")

    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tf:
        tf.write(wav_bytes)
        tmp_wav = Path(tf.name)
    try:
        subprocess.run(
            [
                "ffmpeg",
                "-y",
                "-v",
                "error",
                "-i",
                str(tmp_wav),
                "-af",
                "highpass=f=80,loudnorm=I=-16:TP=-1.5:LRA=7",
                "-c:a",
                "libopus",
                "-b:a",
                "32k",
                "-ar",
                "24000",
                "-ac",
                "1",
                str(out_ogg),
            ],
            check=True,
        )
    finally:
        tmp_wav.unlink(missing_ok=True)


def run_zh(
    chars_spec: dict,
    assets_chars: Path,
    filter_set: set[str],
    region: str,
    key: str,
    force: bool,
    dry_run: bool,
) -> tuple[int, int]:
    total_ok = 0
    total_skip = 0
    for char_id, cfg in chars_spec.items():
        if filter_set and char_id not in filter_set:
            continue
        legacy_dir = assets_chars / char_id / "voices"
        zh_dir = legacy_dir / "zh"
        if not dry_run:
            zh_dir.mkdir(parents=True, exist_ok=True)

        old_zh_manifest = {}
        if (zh_dir / "manifest.json").is_file() and not force:
            try:
                old_zh_manifest = json.loads((zh_dir / "manifest.json").read_text(encoding="utf-8")).get("scenes", {})
            except Exception:
                old_zh_manifest = {}

        old_legacy_manifest = {}
        if (legacy_dir / "manifest.json").is_file() and not force:
            try:
                old_legacy_manifest = json.loads((legacy_dir / "manifest.json").read_text(encoding="utf-8")).get("scenes", {})
            except Exception:
                old_legacy_manifest = {}

        azure_voice = cfg["azureVoice"]
        rate = cfg.get("rate", "+0%")
        pitch = cfg.get("pitch", "+0Hz")
        new_scenes: dict = {}

        for scene_key, item in cfg["scenes"].items():
            text = item["text"]
            style = item.get("style", "")
            sig = hashlib.sha256(f"{azure_voice}|{style}|{rate}|{pitch}|{text}".encode("utf-8")).hexdigest()[:16]
            out_file = f"{scene_key}.ogg"
            out_path = zh_dir / out_file

            prev = old_zh_manifest.get(scene_key)
            if (
                not force
                and isinstance(prev, dict)
                and prev.get("hash") == sig
                and out_path.is_file()
                and out_path.stat().st_size > 100
            ):
                new_scenes[scene_key] = prev
                total_skip += 1
                continue

            # 迁移旧根目录 voices/<scene>.ogg（避免重复计费）
            leg = old_legacy_manifest.get(scene_key)
            leg_path = legacy_dir / out_file
            if (
                not force
                and isinstance(leg, dict)
                and leg.get("hash") == sig
                and leg_path.is_file()
                and leg_path.stat().st_size > 100
            ):
                if not dry_run:
                    shutil.copy2(leg_path, out_path)
                new_scenes[scene_key] = leg
                total_skip += 1
                continue

            if dry_run:
                total_ok += 1
                continue

            ssml = build_ssml(azure_voice, style, rate, pitch, text)
            audio_bytes = synthesize_azure_ogg(region, key, ssml)
            out_path.write_bytes(audio_bytes)
            new_scenes[scene_key] = {
                "file": out_file,
                "text": text,
                "style": style,
                "hash": sig,
            }
            total_ok += 1
            print(f"[ZH OK] {char_id}/zh/{out_file} ({style}, {len(audio_bytes)}B)")
            time.sleep(0.08)

        if not dry_run:
            manifest_obj = {
                "schemaVersion": 2,
                "charId": char_id,
                "lang": "zh",
                "voice": azure_voice,
                "scenes": new_scenes,
            }
            (zh_dir / "manifest.json").write_text(
                json.dumps(manifest_obj, ensure_ascii=False, indent=2),
                encoding="utf-8",
            )
    return total_ok, total_skip


def run_ja(
    chars_spec: dict,
    assets_chars: Path,
    filter_set: set[str],
    db_cfg: dict,
    gpt_url: str,
    force: bool,
    dry_run: bool,
    model_tag: str = "",
) -> tuple[int, int]:
    total_ok = 0
    total_skip = 0
    gs_profiles = (db_cfg.get("fishSpeech") or db_cfg.get("gptSovits") or {}).get("charProfiles", {})
    appdata = os.environ.get("APPDATA", "")

    for char_id, cfg in chars_spec.items():
        if filter_set and char_id not in filter_set:
            continue
        ja_dir = assets_chars / char_id / "voices" / "ja"
        if not dry_run:
            ja_dir.mkdir(parents=True, exist_ok=True)

        old_manifest = {}
        if (ja_dir / "manifest.json").is_file() and not force:
            try:
                old_manifest = json.loads((ja_dir / "manifest.json").read_text(encoding="utf-8")).get("scenes", {})
            except Exception:
                old_manifest = {}

        prof = gs_profiles.get(char_id, {})
        ref_audio = str(prof.get("refAudioPath", "")).strip()
        if not ref_audio and appdata:
            cand = Path(appdata) / "Deskpet" / "ref_voices" / f"{char_id}.wav"
            if cand.is_file():
                ref_audio = str(cand)
        prompt_text = str(prof.get("promptText", "")).strip()
        prompt_lang = str(prof.get("promptLang", "zh")).strip() or "zh"
        speed_factor = float(prof.get("speedFactor", 1.0) or 1.0)

        new_scenes: dict = {}
        for scene_key, item in cfg["scenes"].items():
            zh_text = item["text"]
            ja_text = item.get("ja", "").strip()
            if not ja_text:
                raise RuntimeError(f"缺少日语台词: {char_id}/{scene_key}")
            sig_prefix = f"fish_speech|{model_tag}|ja" if model_tag else "fish_speech|ja"
            sig = hashlib.sha256(f"{sig_prefix}|{ref_audio}|{prompt_text}|{ja_text}".encode("utf-8")).hexdigest()[:16]
            out_file = f"{scene_key}.ogg"
            out_path = ja_dir / out_file

            prev = old_manifest.get(scene_key)
            if (
                not force
                and isinstance(prev, dict)
                and prev.get("hash") == sig
                and out_path.is_file()
                and out_path.stat().st_size > 100
            ):
                new_scenes[scene_key] = prev
                total_skip += 1
                continue

            if dry_run:
                total_ok += 1
                continue

            if not ref_audio or not Path(ref_audio).is_file():
                raise SystemExit(f"错误：角色 {char_id} 缺少参考音频 ref_voices/{char_id}.wav")

            synthesize_fish_speech_ogg(
                gpt_url, ja_text, ref_audio, prompt_text, out_path
            )
            new_scenes[scene_key] = {
                "file": out_file,
                "text": zh_text,
                "zhText": zh_text,
                "jaText": ja_text,
                "hash": sig,
            }
            total_ok += 1
            print(f"[JA OK] {char_id}/ja/{out_file} ({out_path.stat().st_size}B)")

        if not dry_run:
            manifest_obj = {
                "schemaVersion": 2,
                "charId": char_id,
                "lang": "ja",
                "scenes": new_scenes,
            }
            if model_tag:
                manifest_obj["model"] = model_tag
            (ja_dir / "manifest.json").write_text(
                json.dumps(manifest_obj, ensure_ascii=False, indent=2),
                encoding="utf-8",
            )
    return total_ok, total_skip


def main() -> int:
    parser = argparse.ArgumentParser(description="生成 Deskpet 8 角色本地双语离线语音包 (32 条/角色)")
    parser.add_argument("--lang", choices=["zh", "ja", "both"], default="both", help="生成语种")
    parser.add_argument("--region", default="eastasia", help="Azure Speech 区域，默认 eastasia")
    parser.add_argument("--key-file", default=None, help="包含 Azure Speech Key 的文本文件路径")
    parser.add_argument("--from-db", action="store_true", help="从本机 %%APPDATA%%\\Deskpet\\deskpet.db 读取配置")
    parser.add_argument("--fish-url", "--gpt-url", dest="gpt_url", default="http://127.0.0.1:8080", help="Fish Speech API 地址")
    parser.add_argument("--model-tag", default="", help="可选模型标识，用于区分预生成语音包模型版本如 s2-pro")
    parser.add_argument("--assets-dir", default="", help="覆盖输出 chars 目录路径")
    parser.add_argument("--chars", default="", help="可选，逗号分隔的角色 ID 过滤")
    parser.add_argument("--force", action="store_true", help="强制重新生成全部语音")
    parser.add_argument("--dry-run", action="store_true", help="仅校验台词与统计待生成数量，不发请求")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    scripts_path = repo_root / "tools" / "voice_scripts.json"
    assets_chars = Path(args.assets_dir) if args.assets_dir else default_assets_chars_dir(repo_root)

    spec = json.loads(scripts_path.read_text(encoding="utf-8"))
    chars_spec: dict = spec["characters"]
    filter_set = {c.strip() for c in args.chars.split(",") if c.strip()}
    db_cfg = load_db_chat_config() if args.from_db else {}

    if args.lang in ("zh", "both"):
        key = "" if args.dry_run else load_api_key(args.key_file, db_cfg)
        zh_ok, zh_skip = run_zh(chars_spec, assets_chars, filter_set, args.region, key, args.force, args.dry_run)
        print(f"[ZH] 待生成/新生成 {zh_ok} 条，迁移/跳过 {zh_skip} 条。")

    if args.lang in ("ja", "both"):
        ja_ok, ja_skip = run_ja(chars_spec, assets_chars, filter_set, db_cfg, args.gpt_url, args.force, args.dry_run, args.model_tag)
        print(f"[JA] 待生成/新生成 {ja_ok} 条，跳过 {ja_skip} 条。")

    return 0


if __name__ == "__main__":
    sys.exit(main())
