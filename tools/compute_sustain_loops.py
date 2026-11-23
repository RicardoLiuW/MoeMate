#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Deskpet v2.2 离线中段无缝循环寻优脚本：
只读扫描 8 位角色的持续型动作（睡觉/工作/重连等），在 [24%, 76%] 中段自动寻找
400ms 过渡窗姿态差异最小且远离第 0 帧站立态的最佳 [startMs, endMs]，
写入各自 .meta.json 的可选字段 `sustainLoop`。
"""
from __future__ import annotations

import json
import os
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed

import numpy as np

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
CHARS_DIR = os.path.join(ROOT, "assets", "chars")
CHAR_IDS = ["claude", "deepseek", "gemini", "glm", "gpt", "grok", "kimi", "qwen"]

TARGET_NAMES = {
    "原地小憩沉眠",
    "沙发打瞌睡",
    "待机瞌睡点头",
    "困倦揉眼",
    "东张西望",
    "左右张望",
    "站着晃神打瞌睡",
    "焦虑踱步",
    "工作状态-清点归档",
    "轻快记录",
    "拨算盘珠",
    "写字哑剧",
    "翻阅书册",
    "收拾书桌 v2",
    "写代码",
    "工作状态-思考冒泡",
    "深度思考碎碎念",
    "深度思考碎碎念v4",
    "深度思考走神",
    "换装侦探服思考",
    "咬手指思索",
    "疑惑挠头",
    "书灵现世",
    "图书管理员嘘",
    "打字哑剧",
    "工作加班",
    "工作状态-忙碌点按",
    "理发师剪发",
    "碎碎念-对屏碎碎念",
    "工作状态-原地踱步张望",
    "工作状态-原地踱步张望 v2",
    "吃Token",
    "侧身举相机拍别人",
    "拍照取景",
    "星星精灵现世",
    "小蓝狐现世",
    "小金蝠现世",
    "小星灵现世",
    "召唤姿势",
    "施法手势",
    "打电话汇报",
    "招手过来",
    "挥手双手",
    "点击回应-元气挥手",
    "点击回应-开心跃动",
    "保安举金属探测仪",
    "警察开罚单",
    "慌张摆手",
    "合扇敲掌·等待不耐烦",
    "点击回应-傲娇生气",
    "工作状态-雀跃庆祝",
    "OK手势",
    "余额-分文不剩",
    "余额-袋空如洗",
    "沮丧垂头v2",
    "工作状态-垂头叹气冒汗",
    "工作状态-垂头叹气冒汗 v2",
    "被吓一跳",
    "惊吓后缩",
}

SAMPLE_FPS = 10
SW, SH = 160, 90
FRAME_BYTES = SW * SH * 4


def decode_features(webm_path: str) -> np.ndarray | None:
    cmd = [
        "ffmpeg",
        "-v",
        "error",
        "-c:v",
        "libvpx-vp9",
        "-i",
        webm_path,
        "-vf",
        f"fps={SAMPLE_FPS},scale={SW}:{SH}:flags=area",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "pipe:1",
    ]
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if proc.returncode != 0 or not proc.stdout:
        return None
    usable = (len(proc.stdout) // FRAME_BYTES) * FRAME_BYTES
    if usable == 0:
        return None
    arr = np.frombuffer(proc.stdout[:usable], dtype=np.uint8).reshape(-1, SH, SW, 4)
    rgba = arr.astype(np.float32) / 255.0
    alpha = rgba[..., 3:4]
    rgb_premul = rgba[..., :3] * alpha
    feat = np.concatenate([rgb_premul, alpha * 1.5], axis=-1)
    return feat.reshape(feat.shape[0], -1)


def find_best_sustain_loop(feat: np.ndarray, dur_ms: int, safe_ms: int) -> list[int] | None:
    n = feat.shape[0]
    if n < 12:
        return None

    min_start_ms = max(safe_ms + 800, min(2600, int(dur_ms * 0.24)))
    max_end_ms = dur_ms - min(2600, int(dur_ms * 0.24))
    min_span_ms = 3200 if dur_ms <= 12000 else 4500
    max_span_ms = min(max_end_ms - min_start_ms, 9500)
    if max_span_ms < min_span_ms:
        return None

    max_start_ms = min(max_end_ms - min_span_ms, int(dur_ms * 0.45))
    min_end_ms = max(min_start_ms + min_span_ms, int(dur_ms * 0.58))

    i_lo = max(3, min_start_ms // 100)
    i_hi = min(n - 5, max_start_ms // 100)
    j_lo = max(i_lo + 4, min_end_ms // 100)
    j_hi = min(n - 4, max_end_ms // 100)

    if i_lo > i_hi or j_lo > j_hi:
        return None

    safe_idx = min(n - 1, max(0, safe_ms // 100))
    stand_dist = np.mean(np.abs(feat - feat[safe_idx]), axis=1)
    peak_stand = float(np.max(stand_dist))
    apply_stand_penalty = peak_stand > 0.025

    i_cands = np.arange(i_lo, i_hi + 1)
    j_cands = np.arange(j_lo, j_hi + 1)

    # 构造 4 帧过渡窗特征矩阵: (len, 4 * D)
    win_i = np.concatenate([feat[i_cands - k] for k in (3, 2, 1, 0)], axis=1)
    win_j = np.concatenate([feat[j_cands - k] for k in (3, 2, 1, 0)], axis=1)

    # 向量化 L1 距离矩阵: (len(i_cands), len(j_cands))
    # 为控制峰值内存，按 i 分块计算
    best_score = float("inf")
    best_pair: tuple[int, int] | None = None
    min_span_frames = min_span_ms // 100
    max_span_frames = max_span_ms // 100

    for idx_i, i_val in enumerate(i_cands):
        span = j_cands - i_val
        valid = (span >= min_span_frames) & (span <= max_span_frames)
        if not np.any(valid):
            continue
        vj = j_cands[valid]
        vw = win_j[valid]
        win_err = np.mean(np.abs(vw - win_i[idx_i]), axis=1)

        penalty = np.zeros_like(win_err)
        if apply_stand_penalty:
            thresh = peak_stand * 0.28
            if stand_dist[i_val] < thresh:
                penalty += 0.06
            penalty += np.where(stand_dist[vj] < thresh, 0.06, 0.0)

        span_bonus = -0.0015 * ((vj - i_val) / 10.0)
        scores = win_err + penalty + span_bonus
        local_best = int(np.argmin(scores))
        if float(scores[local_best]) < best_score:
            best_score = float(scores[local_best])
            best_pair = (int(i_val), int(vj[local_best]))

    if best_pair is None:
        return None
    return [best_pair[0] * 100, best_pair[1] * 100]


def process_clip(task: tuple[str, str, str, str]) -> tuple[str, str, list[int] | None]:
    char_id, clip_id, meta_path, webm_path = task
    with open(meta_path, "r", encoding="utf-8") as f:
        meta = json.load(f)

    feat = decode_features(webm_path)
    if feat is None:
        return (char_id, clip_id, None)

    dur_ms = int(meta.get("durationMs") or (feat.shape[0] * 100))
    safe_ms = int(meta.get("safeStartMs") or 0)
    loop_win = find_best_sustain_loop(feat, dur_ms, safe_ms)
    if loop_win is None:
        return (char_id, clip_id, None)

    meta["sustainLoop"] = loop_win
    with open(meta_path, "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False, indent=2)
        f.write("\n")
    return (char_id, clip_id, loop_win)


def collect_tasks() -> list[tuple[str, str, str, str]]:
    tasks: list[tuple[str, str, str, str]] = []
    for char_id in CHAR_IDS:
        char_dir = os.path.join(CHARS_DIR, char_id)
        char_json_path = os.path.join(char_dir, "character.json")
        clips_dir = os.path.join(char_dir, "clips")
        if not os.path.isfile(char_json_path) or not os.path.isdir(clips_dir):
            continue

        with open(char_json_path, "r", encoding="utf-8") as f:
            char_data = json.load(f)

        bindings = char_data.get("bindings", {})
        candidates = set(TARGET_NAMES)
        for key in ("work", "sleep"):
            for cid in bindings.get(key, []):
                candidates.add(cid)

        for fname in sorted(os.listdir(clips_dir)):
            if not fname.endswith(".meta.json"):
                continue
            clip_id = fname[: -len(".meta.json")]
            meta_path = os.path.join(clips_dir, fname)
            webm_path = os.path.join(clips_dir, f"{clip_id}.webm")
            if not os.path.isfile(webm_path):
                continue
            try:
                with open(meta_path, "r", encoding="utf-8") as f:
                    meta = json.load(f)
            except Exception:
                continue
            if meta.get("qa", {}).get("status") == "fail":
                continue
            if int(meta.get("frames") or 0) < 150:
                continue
            if clip_id in candidates or meta.get("category") == "work":
                tasks.append((char_id, clip_id, meta_path, webm_path))
    return tasks


def main() -> None:
    tasks = collect_tasks()
    print(f"Collected {len(tasks)} candidate clips across {len(CHAR_IDS)} characters.")

    updated_counts: dict[str, int] = {c: 0 for c in CHAR_IDS}
    samples: dict[str, list[str]] = {c: [] for c in CHAR_IDS}
    focus = {"原地小憩沉眠", "写代码", "工作状态-思考冒泡", "工作状态-忙碌点按"}

    with ThreadPoolExecutor(max_workers=4) as pool:
        futures = [pool.submit(process_clip, t) for t in tasks]
        for fut in as_completed(futures):
            char_id, clip_id, win = fut.result()
            if win is not None:
                updated_counts[char_id] += 1
                if clip_id in focus:
                    samples[char_id].append(f"{clip_id}={win}")

    for char_id in CHAR_IDS:
        rep = ", ".join(sorted(samples[char_id])) or "-"
        print(f"{char_id}: updated={updated_counts[char_id]} | {rep}")


if __name__ == "__main__":
    main()
