#!/usr/bin/env python3
"""
使用 Azure AI Speech REST API 为 Deskpet 8 位角色批量生成本地情绪语音包（OGG Opus）。

安全原则：
- API Key 仅通过 `--key-file` 或环境变量 `AZURE_SPEECH_KEY` 读取，绝不写入代码、manifest 或日志。
"""

import argparse
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from xml.sax.saxutils import escape


def load_api_key(key_file: str | None) -> str:
    env_key = os.environ.get("AZURE_SPEECH_KEY", "").strip()
    if env_key:
        return env_key
    if key_file:
        raw = Path(key_file).read_text(encoding="utf-8-sig")
        tokens = re.findall(r"[A-Za-z0-9]{24,128}", raw)
        if tokens:
            return tokens[0]
    raise SystemExit("错误：未找到 Azure Speech Key，请通过 --key-file 或 AZURE_SPEECH_KEY 提供。")


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


def synthesize_ogg(region: str, key: str, ssml: bytes, retries: int = 3) -> bytes:
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


def main() -> int:
    parser = argparse.ArgumentParser(description="生成 Deskpet 8 角色本地 Azure AI Speech 语音包")
    parser.add_argument("--region", default="eastasia", help="Azure Speech 区域，默认 eastasia")
    parser.add_argument("--key-file", default=None, help="包含 Azure Speech Key 的文本文件路径")
    parser.add_argument("--chars", default="", help="可选，逗号分隔的角色 ID 过滤")
    parser.add_argument("--force", action="store_true", help="强制重新生成全部语音")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    scripts_path = repo_root / "tools" / "voice_scripts.json"
    assets_chars = repo_root / "assets" / "chars"

    spec = json.loads(scripts_path.read_text(encoding="utf-8"))
    chars_spec: dict = spec["characters"]
    filter_set = {c.strip() for c in args.chars.split(",") if c.strip()}

    key = load_api_key(args.key_file)
    total_ok = 0
    total_skip = 0

    for char_id, cfg in chars_spec.items():
        if filter_set and char_id not in filter_set:
            continue
        voices_dir = assets_chars / char_id / "voices"
        voices_dir.mkdir(parents=True, exist_ok=True)
        manifest_path = voices_dir / "manifest.json"
        old_manifest = {}
        if manifest_path.is_file() and not args.force:
            try:
                old_manifest = json.loads(manifest_path.read_text(encoding="utf-8")).get("scenes", {})
            except Exception:
                old_manifest = {}

        azure_voice = cfg["azureVoice"]
        rate = cfg.get("rate", "+0%")
        pitch = cfg.get("pitch", "+0Hz")
        new_scenes: dict = {}

        for scene_key, item in cfg["scenes"].items():
            text = item["text"]
            style = item.get("style", "")
            sig = hashlib.sha256(f"{azure_voice}|{style}|{rate}|{pitch}|{text}".encode("utf-8")).hexdigest()[:16]
            out_file = f"{scene_key}.ogg"
            out_path = voices_dir / out_file

            prev = old_manifest.get(scene_key)
            if (
                not args.force
                and isinstance(prev, dict)
                and prev.get("hash") == sig
                and out_path.is_file()
                and out_path.stat().st_size > 100
            ):
                new_scenes[scene_key] = prev
                total_skip += 1
                continue

            ssml = build_ssml(azure_voice, style, rate, pitch, text)
            audio_bytes = synthesize_ogg(args.region, key, ssml)
            out_path.write_bytes(audio_bytes)
            new_scenes[scene_key] = {
                "file": out_file,
                "text": text,
                "style": style,
                "hash": sig,
            }
            total_ok += 1
            print(f"[OK] {char_id}/{scene_key}.ogg ({style}, {len(audio_bytes)}B)")
            time.sleep(0.08)

        manifest_obj = {
            "schemaVersion": 1,
            "charId": char_id,
            "voice": azure_voice,
            "scenes": new_scenes,
        }
        manifest_path.write_text(
            json.dumps(manifest_obj, ensure_ascii=False, indent=2),
            encoding="utf-8",
        )

    print(f"完成：新生成 {total_ok} 条，增量跳过 {total_skip} 条。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
