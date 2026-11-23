#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Deskpet V0 素材产线：绿幕抠像 -> VP9-alpha WebM + meta.json + mask.bin + QA 报告。

用法:
    python tools/build_assets.py --char claude
    python tools/build_assets.py --char claude --limit 3 --force
源目录 dsh/<char>/ 只读；产物写入 assets/chars/<char>/。
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import traceback
from concurrent.futures import ProcessPoolExecutor, as_completed

import numpy as np
from PIL import Image

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
DSH = os.path.join(ROOT, "dsh")
PETS = os.path.join(ROOT, "aboutme-pets", "pets")
ASSETS = os.path.join(ROOT, "assets")
CHARS = os.path.join(ASSETS, "chars")

# 契约 v1.1 §0：deepseek 角色的动作/音轨元数据在 dsh1 包，dsh 包只有旧动作表。
PACK_MAP = {"deepseek": "dsh1"}
# 帧数门禁（v1.1 §2.8）：150–1050 帧（5–35 秒）正常，超出判 fail。
FRAMES_MIN, FRAMES_MAX = 150, 1050

W, H, FPS = 1280, 720, 30
MW, MH = 160, 90
SW, SH = 320, 180
FRAME_BYTES = W * H * 4
# 每帧 160×90 单色掩码经 np.packbits 后固定 1800 字节。
MASK_BYTES = MW * MH // 8
# v1.3 safeStartMs：片头暗场淡入时，整帧/四角/外圈会被未抠净的绿幕雾污染。
# 只扫描前 30 帧（1 秒）；超出门禁的片段仍可正常起播，不会被裁掉大段内容。
SAFE_SCAN_FRAMES = 30
SAFE_CORNER = 8
SAFE_CORNER_MAX = 0.15
SAFE_EDGE_MAX = 0.15
SAFE_FRAME_MAX = 0.60
# 抠像参数可通过环境变量覆盖（worker 进程靠环境变量继承）：
#   DESKPET_KEY_SIMILARITY / DESKPET_KEY_BLEND
KEY_SIM = float(os.environ.get("DESKPET_KEY_SIMILARITY", "0.15"))
KEY_BLEND = float(os.environ.get("DESKPET_KEY_BLEND", "0.05"))
KEY = "colorkey=0x55AA51:%g:%g,despill=type=green:mix=0.5:expand=0" % (KEY_SIM, KEY_BLEND)
KEY_RGB = np.array([0x55, 0xAA, 0x51], dtype=np.float32)

CAT_MAP = {
    "待机": "idle", "小动作": "idle",
    "碎碎念": "chatter", "文字": "chatter",
    "移动": "move", "转向": "turn",
    "点击回应": "click", "拖拽": "drag",
    "情绪": "emotion", "情绪反应": "emotion", "余额": "balance",
    "吃喝": "eat", "吃什么": "eat",
    "玩耍": "play", "娱乐休闲": "play", "运动健身": "play",
    "工作状态": "work", "工作职业": "work",
    "日常起居": "daily", "起居": "daily",
    "时节": "seasonal", "手工节日": "festival",
    "礼仪": "etiquette", "礼仪致意": "etiquette",
    "才艺": "talent", "才艺表演": "talent",
    "魔法特效": "vfx", "特效": "vfx",
    "换装造型": "outfit",
    "模仿动物": "species", "龙族特性": "species",
    "未分类": "other",
}

TAG_RULES = [
    ("吃|喝|餐|咖啡|茶|火锅|月饼|汤圆|粽子|蛋糕|冰淇淋|零食|糖果|可可", "food"),
    ("睡|困|哈欠|赖床|眯|沉眠|小憩", "sleep"),
    ("醒|起床|刷牙|伸懒腰|洗漱|洗脸|晨间", "wake"),
    ("舞|唱|琴|笛|歌|演奏|画|手工|串珠|扎花", "music"),
    ("跑|走|跳|移动|飘|踏步", "move"),
    ("雨|雪|夏|秋|春|冬|蝉|扇|雪人|落叶", "season"),
    ("节|年|圣诞|中秋|端午|元宵|万圣|福|灯", "festive"),
    ("钱|余额|金币|穷|富|袋", "money"),
    ("代码|工作|加班|键盘|电脑|写|文件|报表", "work"),
    ("猫|狗|龙|狐|鸟|兔|动物", "animal"),
    ("害羞|脸红|生气|傲娇|委屈|开心|笑|哭|惊", "emotion"),
    ("挥手|招手|比心|飞吻|竖大拇指|击掌|鼓掌|OK手势", "gesture"),
    ("拳|踢|格挡|格斗|剑|盾|射箭|投篮|击球|劈", "martial"),
    ("深蹲|开合跳|弓步|拉伸|热身|平板支撑|仰卧起坐|举重|体前屈", "fitness"),
    ("待机|呼吸|张望|发呆|哈欠|瞌睡|伸懒腰|悬浮", "idle"),
    ("尾巴|龙角|龙威|龙吟|耳朵|呆毛|发丝|翅膀|鳍饰", "dragon"),
    ("魔法|特效|施法|召唤|变身|治愈|微光|护体", "magic"),
]

# 关键词归类（契约 v1.1 §2.5）：actions.json 查不到的新动作（gpt 全部 193 段、
# deepseek 的 v2/v3 重拍）按动作名自动落到 20 个契约枚举，避免整角色退化为 other。
# 顺序即优先级：先匹配更具体的语义，再落到宽泛类别。
KEYWORD_RULES = [
    (r"拖拽|悬空反馈", "drag"),
    (r"点击回应|^点击|OK手势|竖大拇指|比心|飞吻|招手|挥手|击掌|鼓掌|点头肯定|摇头否定|坚决点头", "click"),
    (r"吃|喝|饮|餐|火锅|汤圆|粽子|月饼|零食|糖|蛋糕|咖啡|茶|饺子|拉面|面条|粥|西瓜|大闸蟹|蟹黄|蟹肉|吃蟹|年糕|青团|白饭|橘子|可可|冰淇淋|泡面|饼干|番茄酱|酱汁", "eat"),
    (r"魔法|特效|施法|召唤|变身|治愈之光|凭空|变鸽子|环绕|开花|龙吟|龙威|白龙|微光|发光|护体|盾牌", "vfx"),
    (r"燕式|鹤立|单腿|平衡", "balance"),
    (r"尾巴|龙角|龙鳞|耳朵|呆毛|头发|发丝|长发|裙摆|翅膀|鳍饰|兔子|小兔|小猫|猫咪|猫伸|猫洗|小狗|小鸟|小鹿|小熊|鱼儿|动物|龙族", "species"),
    (r"鞠躬|礼仪|屈膝|抚胸|合十|祈祷|谢幕|单膝跪|跪地", "etiquette"),
    (r"弹琴|弹奏|钢琴|小提琴|长笛|吹奏|笛子|演唱|唱歌|麦克风|打鼓|鼓节拍|节拍|节奏|响指|指挥|演奏|杂耍|抛接|魔术|戏法|空竹|快板|沙锤|木琴|芭蕾|街舞|踢踏|流星锤|舞狮|机械舞|宅舞|旋舞|绸带舞|舞", "talent"),
    (r"碎碎念|哑剧|叹气|嘀咕|计数", "chatter"),
    (r"工作|加班|代码|键盘|电脑|打字|报表|文件|会议|汇报|翻阅|书册|记录|拍照|取景", "work"),
    (r"待机|呼吸|张望|发呆|凝视|注视|微晃|悬停|深呼吸|伸懒腰|打哈欠|哈欠|瞌睡|困倦|装睡|蜷卧|静止|定住|悬浮|漂浮", "idle"),
    (r"害羞|委屈|嫉妒|害怕|发抖|尴尬|开心|快乐|惊讶|震惊|感动|得意|骄傲|沮丧|生气|鼓腮|焦虑|疑惑|挠头|无语|扶额|慌张|哭泣|大哭|大笑|吐舌|鬼脸|兴奋|狂喜|坚决|点头|摇头|温柔|好奇|偷看|咬手指|思索|受惊|惊吓|赌气|斜睨|赔笑|昂首|放松|无奈|呐喊|打气|抱臂|犹豫|退缩", "emotion"),
    (r"转体|转身|左转|右转|旋转|转圈|绕圈", "turn"),
    (r"踏步|行走|小跳|后退|螃蟹|横行|高抬腿|冲刺|走路|跑步|奔跑|迈步|走动", "move"),
    (r"蹦跳|踢腿|前踢|侧踢|回旋踢|上勾拳|直拳|连击|格挡|格斗|横扫|突刺|撩剑|投篮|挥拍|击球|挥棒|射箭|深蹲|开合跳|弓步|体前屈|拉伸|热身|绕环|甩臂|扭腰|骨盆|挺肚子|平板支撑|仰卧起坐|举重|下蹲|蹲起|下劈|劈|侧手翻|翻滚|打滚|闪避|抖|耸肩|踮脚|收势", "play"),
    (r"梳|洗|刷|穿针|缝|叠衣|折叠|擦|扫|拧|鞋带|拉链|撑伞|整理|收拾|收纳|铺床|熨|剪指甲|剪刘海|敷面膜|换灯泡|钉钉子", "daily"),
    (r"换装|试色|服装|衣服|围裙|浴袍", "outfit"),
    (r"玩|游戏|棋|陀螺|魔方|风筝|钓鱼|木马|积木|拼图|帐篷|气球|泡泡|玩具|自拍", "play"),
    (r"落叶|雪|雨|夏|秋|春|冬|阳光|风吹|扇|蝉|荷花|纳凉", "seasonal"),
    (r"节|新年|圣诞|中秋|端午|元宵|万圣|福|灯|红包|春联|礼花|烟花|彩旗|灯笼", "festival"),
]


def classify(name: str, legacy: str) -> str:
    """优先用 actions.json 的 category；查不到或为“未分类”时按关键词归类。"""
    mapped = CAT_MAP.get(legacy or "", "other")
    if mapped != "other":
        return mapped
    for pattern, category in KEYWORD_RULES:
        if re.search(pattern, name):
            return category
    # 兜底用 idle 而不是 other：未命中关键词的动作仍是可正常待机播放的素材，
    # other 会让它彻底掉出所有 binding。
    return "idle"

CHAR_NAMES = {
    "claude": "Claude", "deepseek": "DeepSeek", "gemini": "Gemini",
    "glm": "GLM", "gpt": "GPT", "grok": "Grok", "kimi": "Kimi", "qwen": "Qwen",
}

_ACTIONS_CACHE: dict[str, dict] = {}


def load_actions(char: str) -> list[dict]:
    """读角色对应包的 actions.json；deepseek 显式映射到 dsh1。"""
    pack = PACK_MAP.get(char, char)
    if pack not in _ACTIONS_CACHE:
        path = os.path.join(PETS, pack, "actions.json")
        items = []
        if os.path.isfile(path):
            with open(path, "r", encoding="utf-8") as fh:
                items = json.load(fh).get("animations", [])
        _ACTIONS_CACHE[pack] = items
    return _ACTIONS_CACHE[pack]


def action_record(char: str, clip_id: str) -> dict:
    """精确匹配优先；v2/v3 重拍按去掉后缀的基名继承元数据。"""
    recs = load_actions(char)
    for rec in recs:
        if rec.get("id") == clip_id:
            return rec
    base = re.sub(r"\s+v\d+$", "", clip_id, flags=re.IGNORECASE)
    if base != clip_id:
        for rec in recs:
            if rec.get("id") == base:
                hit = dict(rec)
                hit["_derivedFrom"] = base
                return hit
    return {}


def probe_frames(path: str) -> int:
    cmd = ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames",
           "-show_entries", "stream=nb_read_frames", "-of", "json", path]
    out = subprocess.run(cmd, capture_output=True, check=False)
    try:
        data = json.loads(out.stdout.decode("utf-8", "replace"))
        return int(data["streams"][0]["nb_read_frames"])
    except Exception:
        return 0


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(1024 * 1024), b""):
            h.update(block)
    return "sha256:" + h.hexdigest()


def make_tags(name: str, category: str) -> list[str]:
    tags = []
    for pattern, tag in TAG_RULES:
        if re.search(pattern, name) and tag not in tags:
            tags.append(tag)
    if category not in tags:
        tags.append(category)
    return tags


def qa_status(frames: int, green: float, hole: float, audio_warn: bool = False,
              body_box=None, mismatch: bool = False) -> str:
    """v1.1 §2.8 门禁：150–1050 帧正常；绿边 <0.5%；抠穿 ≥0.97。"""
    if (mismatch or frames < FRAMES_MIN or frames > FRAMES_MAX
            or green >= 0.005 or hole < 0.97):
        return "fail"
    warn = bool(audio_warn) or green >= 0.002 or hole < 0.99
    if body_box and (body_box[2] < 80 or body_box[3] < 80):
        warn = True
    return "warn" if warn else "pass"


def resolve_audio(char: str, clip_id: str, rec: dict) -> str | None:
    pack = PACK_MAP.get(char, char)
    rel = rec.get("audio")
    if rel:
        cand = os.path.join(PETS, pack, rel.replace("/", os.sep))
        if os.path.isfile(cand):
            return cand
    own = os.path.join(PETS, pack, "audio", clip_id + ".m4a")
    if os.path.isfile(own):
        return own
    # 跨包同名兜底只在不同角色共用同一套配音时才安全；默认关闭，--audio-scope any 打开。
    if os.environ.get("DESKPET_AUDIO_SCOPE", "own") == "any" and os.path.isdir(PETS):
        for pack in os.listdir(PETS):
            cand = os.path.join(PETS, pack, "audio", clip_id + ".m4a")
            if os.path.isfile(cand):
                return cand
    return None


def write_json(path: str, data) -> None:
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=2)
    os.replace(tmp, path)


def encode_clip(src: str, dst: str, threads: int = 4) -> float:
    t0 = time.time()
    cmd = ["ffmpeg", "-v", "error", "-y", "-i", src,
           "-vf", KEY + ",format=yuva420p",
           "-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-b:v", "0", "-crf", "30",
           "-row-mt", "1", "-threads", str(threads),
           "-deadline", "good", "-cpu-used", "2",
           "-auto-alt-ref", "0", "-an", dst]
    res = subprocess.run(cmd, capture_output=True, check=False)
    if res.returncode != 0:
        raise RuntimeError("ffmpeg encode failed: " + res.stderr.decode("utf-8", "replace")[-800:])
    return time.time() - t0


def read_exact(stream, n: int) -> bytes:
    parts = []
    got = 0
    while got < n:
        block = stream.read(n - got)
        if not block:
            break
        parts.append(block)
        got += len(block)
    return b"".join(parts)


def analyze_clip(keyed: str, src: str, expected: int) -> dict:
    """逐帧分析抠像产物：bbox、1/8 掩码、绿边率、抠穿率、循环判定。"""
    proc = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-c:v", "libvpx-vp9", "-i", keyed,
         "-f", "rawvideo", "-pix_fmt", "rgba", "-"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    boxes, masks = [], []
    union = np.zeros((MH, MW), dtype=bool)
    sampled_alpha: list[np.ndarray] = []
    green_hits = opaque_total = 0
    n = 0
    try:
        while True:
            buf = read_exact(proc.stdout, FRAME_BYTES)
            if len(buf) < FRAME_BYTES:
                break
            arr = np.frombuffer(buf, dtype=np.uint8).reshape(H, W, 4)
            alpha = arr[:, :, 3]
            solid = alpha >= 200
            if solid.any():
                rows = np.flatnonzero(solid.any(axis=1))
                cols = np.flatnonzero(solid.any(axis=0))
                boxes.append((int(cols[0]), int(rows[0]), int(cols[-1]), int(rows[-1])))
            small = np.asarray(Image.fromarray(alpha).resize((MW, MH), Image.BOX))
            m = small > 32
            masks.append(np.packbits(m))
            union |= m
            if n % 10 == 0:
                sampled_alpha.append(
                    np.asarray(Image.fromarray(alpha).resize((SW, SH), Image.BOX)).copy())
            if n % 5 == 0:
                rgb = arr[:, :, :3].astype(np.int16)
                op = alpha >= 128
                r, g, b = rgb[:, :, 0], rgb[:, :, 1], rgb[:, :, 2]
                over = op & (g > np.maximum(r, b) + 15)
                green_hits += int(np.count_nonzero(over))
                opaque_total += int(np.count_nonzero(op))
            n += 1
    finally:
        try:
            proc.stdout.close()
        except Exception:
            pass
        err = proc.stderr.read().decode("utf-8", "replace")
        proc.wait()
    if n == 0:
        raise RuntimeError("no frames decoded from " + keyed + " " + err[-500:])

    if boxes:
        med = np.median(np.array(boxes, dtype=np.float64), axis=0)
        x0, y0, x1, y1 = [int(round(v)) for v in med]
    else:
        x0, y0, x1, y1 = 0, 0, W - 1, H - 1
    x0 = max(0, min(x0, W - 1)); y0 = max(0, min(y0, H - 1))
    x1 = max(x0, min(x1, W - 1)); y1 = max(y0, min(y1, H - 1))
    body_box = [x0, y0, x1 - x0 + 1, y1 - y0 + 1]

    if union.any():
        rows = np.flatnonzero(union.any(axis=1))
        cols = np.flatnonzero(union.any(axis=0))
        full_box = [int(cols[0]) * 8, int(rows[0]) * 8,
                    int(cols[-1] - cols[0] + 1) * 8, int(rows[-1] - rows[0] + 1) * 8]
    else:
        full_box = body_box[:]
    return {"frames": n, "boxes": boxes, "masks": masks, "union": union,
            "sampled_alpha": sampled_alpha, "green_hits": green_hits,
            "opaque_total": opaque_total, "body_box": body_box, "full_box": full_box}


def sample_source(src: str) -> list[np.ndarray]:
    """源片每 10 帧取一帧，缩到 320x180，返回「明显非绿」掩码，用于检测抠穿。"""
    cmd = ["ffmpeg", "-v", "error", "-i", src,
           "-vf", "select=not(mod(n\\,10)),scale=320:180:flags=area",
           "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]
    res = subprocess.run(cmd, capture_output=True, check=False)
    fb = SW * SH * 3
    data = res.stdout
    out = []
    for i in range(len(data) // fb):
        rgb = np.frombuffer(data[i * fb:(i + 1) * fb], dtype=np.uint8)
        rgb = rgb.reshape(SH, SW, 3).astype(np.float32)
        dist = np.sqrt(((rgb - KEY_RGB) ** 2).sum(axis=2))
        out.append(dist > 120)
    return out


def convert_audio(src_audio: str, dst: str) -> bool:
    cmd = ["ffmpeg", "-v", "error", "-y", "-i", src_audio,
           "-af", "loudnorm=I=-16:TP=-1.5:LRA=11",
           "-c:a", "libopus", "-b:a", "64k", "-vbr", "on", dst]
    res = subprocess.run(cmd, capture_output=True, check=False)
    return res.returncode == 0 and os.path.isfile(dst)


def mask_frame(packed) -> np.ndarray:
    """把 analyze_clip 里 np.packbits 过的 160×90 掩码还原成布尔矩阵。"""
    if isinstance(packed, (bytes, bytearray, memoryview)):
        arr = np.frombuffer(packed, dtype=np.uint8)
    else:
        arr = np.asarray(packed, dtype=np.uint8)
    bits = np.unpackbits(arr)[:MW * MH]
    return bits.reshape(MH, MW).astype(bool)


def frame_is_clean(mask: np.ndarray) -> bool:
    """片头干净帧判据：整帧不透明率、四角 8×8、外圈 2px 都不能被暗场雾污染。"""
    if float(mask.mean()) > SAFE_FRAME_MAX:
        return False
    corners = (mask[:SAFE_CORNER, :SAFE_CORNER],
               mask[:SAFE_CORNER, -SAFE_CORNER:],
               mask[-SAFE_CORNER:, :SAFE_CORNER],
               mask[-SAFE_CORNER:, -SAFE_CORNER:])
    if max(float(c.mean()) for c in corners) > SAFE_CORNER_MAX:
        return False
    edge = np.concatenate((mask[:2, :].ravel(), mask[-2:, :].ravel(),
                           mask[:, :2].ravel(), mask[:, -2:].ravel()))
    return float(edge.mean()) <= SAFE_EDGE_MAX


def compute_safe_start_ms(masks, frames: int) -> int:
    """按片头掩码算安全起播偏移（ms）。

    第 0 帧就干净 → 0；否则取第一个干净帧 k 再多跳 1 帧，规避过渡帧残留。
    前 30 帧全脏 → 跳到第 30 帧（绝不越过片段末尾）。
    """
    if frames <= 0 or not masks:
        return 0
    limit = min(len(masks), frames, SAFE_SCAN_FRAMES)
    first_clean = None
    for k in range(limit):
        if frame_is_clean(mask_frame(masks[k])):
            first_clean = k
            break
    if first_clean is None:
        safe_frame = min(SAFE_SCAN_FRAMES, frames - 1)
    elif first_clean == 0:
        return 0
    else:
        safe_frame = min(first_clean + 1, frames - 1)
    return int(round(safe_frame * 1000.0 / FPS))


def safe_start_from_mask(mask_path: str, frames: int) -> int:
    """缓存命中 / --meta-only 时，从已有 .mask.bin 补算 safeStartMs。"""
    try:
        with open(mask_path, "rb") as fh:
            raw = fh.read(MASK_BYTES * SAFE_SCAN_FRAMES)
        got = len(raw) // MASK_BYTES
        count = min(got, frames if frames > 0 else got)
        if count <= 0:
            return 0
        masks = [raw[i * MASK_BYTES:(i + 1) * MASK_BYTES] for i in range(count)]
        return compute_safe_start_ms(masks, frames if frames > 0 else count)
    except Exception:
        return 0


def process_one(args: tuple) -> dict:
    char, src_path, force, threads = args
    clip_id = os.path.splitext(os.path.basename(src_path))[0]
    out_dir = os.path.join(CHARS, char)
    clip_dir = os.path.join(out_dir, "clips")
    audio_dir = os.path.join(out_dir, "audio")
    os.makedirs(clip_dir, exist_ok=True)
    os.makedirs(audio_dir, exist_ok=True)
    webm = os.path.join(clip_dir, clip_id + ".webm")
    meta_path = os.path.join(clip_dir, clip_id + ".meta.json")
    mask_path = os.path.join(clip_dir, clip_id + ".mask.bin")

    if not force and os.path.isfile(meta_path) and os.path.isfile(webm) and os.path.isfile(mask_path):
        try:
            with open(meta_path, "r", encoding="utf-8") as fh:
                cached = json.load(fh)
            frames_c = int(cached.get("frames") or 0)
            green_c = float(cached.get("qa", {}).get("greenEdge") or 0.0)
            hole_raw = cached.get("qa", {}).get("holeRatio")
            hole_c = float(hole_raw) if hole_raw is not None else 1.0
            status_c = qa_status(frames_c, green_c, hole_c)
            dirty_c = False
            if cached.get("qa", {}).get("status") != status_c:
                cached.setdefault("qa", {})["status"] = status_c
                dirty_c = True
            safe_cached = cached.get("safeStartMs")
            if not isinstance(safe_cached, int) or isinstance(safe_cached, bool):
                safe_cached = safe_start_from_mask(mask_path, frames_c)
                cached["safeStartMs"] = safe_cached
                dirty_c = True
            if dirty_c:
                write_json(meta_path, cached)
            notes_c = []
            if frames_c and (frames_c < FRAMES_MIN or frames_c > FRAMES_MAX):
                notes_c.append("超出帧数门禁")
            elif frames_c and frames_c not in (300, 450, 900):
                notes_c.append("源片%d帧" % frames_c)
            if hole_c < 0.99:
                notes_c.append("半透明道具")
            note = "、".join(notes_c)
            return {"id": clip_id, "status": status_c,
                    "cached": True, "seconds": 0.0, "note": note,
                    "greenEdge": cached.get("qa", {}).get("greenEdge"),
                    "holeRatio": cached.get("qa", {}).get("holeRatio"),
                    "frames": cached.get("frames"), "category": cached.get("category"),
                    "loop": cached.get("loop"), "audio": bool(cached.get("audio")),
                    "safeStartMs": safe_cached,
                    "bytes": os.path.getsize(webm)}
        except Exception:
            pass

    t0 = time.time()
    expected = probe_frames(src_path)
    tmp_webm = webm + ".tmp.webm"
    encode_sec = encode_clip(src_path, tmp_webm, threads)
    os.replace(tmp_webm, webm)

    stats = analyze_clip(webm, src_path, expected)
    frames = stats["frames"]
    body_box = stats["body_box"]
    full_box = stats["full_box"]

    masks = stats["masks"]
    # v1.3：按本次新编码的掩码重算起播点，--force 覆写也不会丢。
    safe_start_ms = compute_safe_start_ms(masks, frames)
    first = np.unpackbits(masks[0])[:MW * MH]
    last = np.unpackbits(masks[-1])[:MW * MH]
    inter = int(np.count_nonzero(first & last))
    uni = int(np.count_nonzero(first | last))
    iou = (inter / uni) if uni else 1.0
    loop = iou >= 0.98

    src_masks = sample_source(src_path)
    keyed_masks = stats["sampled_alpha"]
    bx0 = max(0, int(body_box[0] * SW / W)); by0 = max(0, int(body_box[1] * SH / H))
    bx1 = min(SW, int((body_box[0] + body_box[2]) * SW / W) + 1)
    by1 = min(SH, int((body_box[1] + body_box[3]) * SH / H) + 1)
    keyed_cov = src_cov = 0
    for km, sm in zip(keyed_masks, src_masks):
        keyed_cov += int(np.count_nonzero(km[by0:by1, bx0:bx1] >= 200))
        src_cov += int(np.count_nonzero(sm[by0:by1, bx0:bx1]))
    hole = min(1.0, keyed_cov / src_cov) if src_cov else 1.0
    green_ratio = (stats["green_hits"] / stats["opaque_total"]) if stats["opaque_total"] else 0.0

    rec = action_record(char, clip_id)
    legacy = str(rec.get("category") or "未分类")
    category = classify(clip_id, legacy)
    name = str(rec.get("name") or clip_id)

    audio_rel = None
    audio_warn = False
    audio_src = resolve_audio(char, clip_id, rec)
    if audio_src:
        ogg = os.path.join(audio_dir, clip_id + ".ogg")
        if convert_audio(audio_src, ogg):
            audio_rel = "audio/" + clip_id + ".ogg"
        else:
            audio_warn = True

    mirrorable = not any(k in name for k in ("文字", "写福", "福字", "春联"))
    duration_ms = int(round(frames * 1000 / FPS))
    interrupt = int(round(duration_ms * 0.3 / 100) * 100) if category in ("eat", "talent") else 0

    status = qa_status(frames, green_ratio, hole, audio_warn, body_box,
                       mismatch=(frames != expected))
    notes = []
    if frames != expected:
        notes.append("解码%d帧≠源片%d帧" % (frames, expected))
    if frames < FRAMES_MIN or frames > FRAMES_MAX:
        notes.append("超出150–1050帧门禁")
    elif expected not in (300, 450, 900):
        notes.append("源片%d帧" % expected)
    if hole < 0.99:
        notes.append("半透明道具")

    tags = make_tags(name, category)
    tags.append("loop" if loop else "once")

    with open(mask_path + ".tmp", "wb") as fh:
        for m in masks:
            fh.write(m.tobytes())
    os.replace(mask_path + ".tmp", mask_path)

    meta = {
        "schemaVersion": 1,
        "id": clip_id,
        "char": char,
        "category": category,
        "legacyCategory": legacy,
        "tags": tags,
        "frames": frames,
        "fps": FPS,
        "durationMs": duration_ms,
        "size": [W, H],
        "bodyBox": body_box,
        "fullBox": full_box,
        "anchor": [round((body_box[0] + body_box[2] / 2.0) / W, 4),
                   round((body_box[1] + body_box[3]) / H, 4)],
        "loop": loop,
        "interruptAfterMs": interrupt,
        "mirrorable": mirrorable,
        "audio": audio_rel,
        "qa": {"status": status, "greenEdge": round(green_ratio, 5),
               "holeRatio": round(hole, 4)},
        "hash": sha256_file(webm),
        # v1.3：片头安全起播点（ms），0 表示第 0 帧就干净。
        "safeStartMs": safe_start_ms,
    }
    write_json(meta_path, meta)
    return {"id": clip_id, "name": name, "status": status, "cached": False,
            "seconds": round(time.time() - t0, 1), "encodeSeconds": round(encode_sec, 1),
            "bytes": os.path.getsize(webm), "loop": loop, "category": category,
            "frames": frames, "note": "、".join(notes), "safeStartMs": safe_start_ms,
            "greenEdge": round(green_ratio, 5), "holeRatio": round(hole, 4),
            "bodyBox": body_box, "audio": bool(audio_rel)}


def build_char_files(char: str, max_items: int | None = None) -> dict:
    out_dir = os.path.join(CHARS, char)
    clip_dir = os.path.join(out_dir, "clips")
    metas = []
    if os.path.isdir(clip_dir):
        for fn in sorted(os.listdir(clip_dir)):
            if fn.endswith(".meta.json"):
                with open(os.path.join(clip_dir, fn), "r", encoding="utf-8") as fh:
                    metas.append(json.load(fh))
    metas.sort(key=lambda m: m["id"])
    for m in metas:
        new_tags = make_tags(m["id"], m["category"])
        new_tags.append("loop" if m["loop"] else "once")
        if m.get("tags") != new_tags:
            m["tags"] = new_tags
            write_json(os.path.join(clip_dir, m["id"] + ".meta.json"), m)
    # 契约 v1.1 §2.5：qa.status=fail 的片段只进相册，绝不进入任何 binding。
    usable = [m for m in metas if m.get("qa", {}).get("status") != "fail"]

    def pick(pred, limit=8):
        hits = [m["id"] for m in usable if pred(m)]
        return hits[:limit]

    idle_ids = pick(lambda m: m["category"] == "idle" and "sleep" not in m["tags"])
    if not idle_ids:
        # 像 deepseek(dsh1) 这种包没有“待机”类：用低强度动作名兜底，不能退化成字母前 3 段。
        idle_ids = pick(lambda m: re.search(
            r"待机|呼吸|打瞌睡|晃神|发呆|张望|伸懒腰|哈欠|摸鱼|抖腿|咬指甲|东张西望|小憩|休息|梳|整理|收拾|凝视|眨眼",
            m["id"]))
    if not idle_ids:
        idle_ids = [m["id"] for m in usable[:3]] or [m["id"] for m in metas[:3]]
    # 没有 click/move 类的角色包（如 deepseek/dsh1）用语义最近的池子兜底，
    # 避免 click/drag/move 全部退化成同一组 idle。
    click_ids = (pick(lambda m: m["category"] == "click")
                 or pick(lambda m: m["category"] == "emotion") or idle_ids)
    move_ids = (pick(lambda m: m["category"] == "move")
                or pick(lambda m: re.search(r"走|跑|跳|飘|踏步|移动|前行|后退", m["id"]))
                or idle_ids)
    bindings = {
        "idle": idle_ids,
        "click": click_ids,
        "drag": pick(lambda m: m["category"] == "drag") or idle_ids,
        "moveLeft": move_ids,
        "moveRight": move_ids,
        "work": pick(lambda m: m["category"] == "work") or idle_ids,
        "eat": pick(lambda m: m["category"] == "eat") or idle_ids,
        "sleep": pick(lambda m: "sleep" in m["tags"]) or idle_ids,
        "wake": pick(lambda m: "wake" in m["tags"]) or idle_ids,
    }
    character = {
        "schemaVersion": 1,
        "id": char,
        "name": CHAR_NAMES.get(char, char),
        "bindings": bindings,
        "schedule": {"wake": "08:30", "lunch": "12:00", "dinner": "18:30", "sleep": "23:30"},
        "defaultScale": 0.5,
    }
    write_json(os.path.join(out_dir, "character.json"), character)

    index = [{
        "id": m["id"], "category": m["category"], "legacyCategory": m["legacyCategory"],
        "tags": m["tags"], "frames": m["frames"], "durationMs": m["durationMs"],
        "bodyBox": m["bodyBox"], "anchor": m["anchor"], "loop": m["loop"],
        "audio": m["audio"], "qaStatus": m["qa"]["status"],
    } for m in metas]
    write_json(os.path.join(out_dir, "actions.index.json"), index)

    catalog_path = os.path.join(ASSETS, "catalog.json")
    catalog = {"schemaVersion": 1, "chars": []}
    if os.path.isfile(catalog_path):
        try:
            with open(catalog_path, "r", encoding="utf-8") as fh:
                catalog = json.load(fh)
        except Exception:
            pass
    chars = [c for c in catalog.get("chars", []) if c.get("id") != char]
    chars.append({"id": char, "name": CHAR_NAMES.get(char, char),
                  "clips": len(metas), "path": "chars/" + char})
    chars.sort(key=lambda c: c["id"])
    catalog = {"schemaVersion": 1, "chars": chars,
               "generatedAt": time.strftime("%Y-%m-%dT%H:%M:%S")}
    write_json(catalog_path, catalog)
    return {"character": character, "clips": len(metas), "index": index}


def safe_process(args: tuple) -> dict:
    try:
        return process_one(args)
    except Exception:
        clip = os.path.splitext(os.path.basename(args[1]))[0]
        return {"id": clip, "status": "fail", "cached": False, "seconds": 0,
                "error": traceback.format_exc()[-1200:]}


def make_samples(char: str, count: int, at_sec: int = 5) -> list[str]:
    clip_dir = os.path.join(CHARS, char, "clips")
    if not os.path.isdir(clip_dir):
        return []
    names = sorted(fn[:-5] for fn in os.listdir(clip_dir)
                   if fn.endswith(".webm") and not fn.endswith(".tmp.webm"))
    if not names:
        return []
    sample_dir = os.path.join(CHARS, char, "qa_samples")
    os.makedirs(sample_dir, exist_ok=True)
    picks = [names[round(i * (len(names) - 1) / max(1, count - 1))] for i in range(count)]
    made = []
    for name in picks:
        webm = os.path.join(clip_dir, name + ".webm")
        png = os.path.join(sample_dir, name + ".png")
        cmd = ["ffmpeg", "-v", "error", "-y", "-c:v", "libvpx-vp9",
               "-ss", str(at_sec), "-i", webm,
               "-f", "lavfi", "-i", "color=0x2b2f3a:s=1280x720",
               "-f", "lavfi", "-i", "color=white:s=1280x720",
               "-filter_complex",
               "[0:v]format=rgba,split[a][b];[1:v][a]overlay[d];[2:v][b]overlay[w];[d][w]hstack",
               "-frames:v", "1", png]
        subprocess.run(cmd, capture_output=True, check=False)
        if os.path.isfile(png):
            made.append(png)
    return made


def audio_only(char: str) -> int:
    """只按当前音频规则重挂音轨，不重新编码画面。"""
    clip_dir = os.path.join(CHARS, char, "clips")
    audio_dir = os.path.join(CHARS, char, "audio")
    os.makedirs(audio_dir, exist_ok=True)
    changed = 0
    for fn in sorted(os.listdir(clip_dir)):
        if not fn.endswith(".meta.json"):
            continue
        meta_path = os.path.join(clip_dir, fn)
        with open(meta_path, "r", encoding="utf-8") as fh:
            m = json.load(fh)
        src = resolve_audio(char, m["id"], action_record(char, m["id"]))
        ogg = os.path.join(audio_dir, m["id"] + ".ogg")
        if src:
            if not os.path.isfile(ogg):
                convert_audio(src, ogg)
            new_audio = ("audio/" + m["id"] + ".ogg") if os.path.isfile(ogg) else None
        else:
            if os.path.isfile(ogg):
                os.remove(ogg)
            new_audio = None
        if m.get("audio") != new_audio:
            m["audio"] = new_audio
            write_json(meta_path, m)
            changed += 1
    info = build_char_files(char)
    with_audio = sum(1 for x in info["index"] if x["audio"])
    print("AUDIO_ONLY char=%s clips=%d changed=%d withAudio=%d" % (
        char, len(info["index"]), changed, with_audio), flush=True)
    return 0


def list_src_chars() -> list[str]:
    if not os.path.isdir(DSH):
        return []
    chars = []
    for name in sorted(os.listdir(DSH)):
        d = os.path.join(DSH, name)
        if os.path.isdir(d) and any(f.lower().endswith(".webm") for f in os.listdir(d)):
            chars.append(name)
    return chars


def meta_only(args) -> int:
    """只重算分类/标签/音轨/QA/索引，不碰 .webm/.mask.bin，不重跑像素分析。"""
    chars = [args.char] if args.char else list_src_chars()
    if not chars:
        print("META_ONLY 未找到任何角色目录", flush=True)
        return 2
    grand = {"pass": 0, "warn": 0, "fail": 0}
    for char in chars:
        clip_dir = os.path.join(CHARS, char, "clips")
        audio_dir = os.path.join(CHARS, char, "audio")
        if not os.path.isdir(clip_dir):
            print("META_ONLY_SKIP char=%s（没有产物目录）" % char, flush=True)
            continue
        os.makedirs(audio_dir, exist_ok=True)
        counts = {"pass": 0, "warn": 0, "fail": 0}
        cats: dict[str, int] = {}
        bad_rows = []
        changed = 0
        audio_added = 0
        for fn in sorted(os.listdir(clip_dir)):
            if not fn.endswith(".meta.json"):
                continue
            meta_path = os.path.join(clip_dir, fn)
            with open(meta_path, "r", encoding="utf-8") as fh:
                m = json.load(fh)
            before = (m.get("category"), m.get("legacyCategory"), m.get("tags"),
                      m.get("audio"), m.get("qa", {}).get("status"),
                      m.get("safeStartMs"))
            rec = action_record(char, m["id"])
            legacy = str(rec.get("category") or m.get("legacyCategory") or "未分类")
            category = classify(m["id"], legacy)
            tags = make_tags(m["id"], category)
            tags.append("loop" if m.get("loop") else "once")
            m["legacyCategory"] = legacy
            m["category"] = category
            m["tags"] = tags

            # 音轨：有源就补挂，已有 .ogg 不重复转换，没有源也不删旧文件。
            src = resolve_audio(char, m["id"], rec)
            if src:
                ogg = os.path.join(audio_dir, m["id"] + ".ogg")
                if not os.path.isfile(ogg):
                    if convert_audio(src, ogg):
                        audio_added += 1
                if os.path.isfile(ogg):
                    m["audio"] = "audio/" + m["id"] + ".ogg"

            # v1.3 起 safeStartMs 由产线写入；--meta-only 只补缺失，绝不覆盖已有值。
            if not isinstance(m.get("safeStartMs"), int) or isinstance(m.get("safeStartMs"), bool):
                m["safeStartMs"] = safe_start_from_mask(
                    os.path.join(clip_dir, m["id"] + ".mask.bin"),
                    int(m.get("frames") or 0))

            qa = m.setdefault("qa", {})
            hole_raw = qa.get("holeRatio")
            status = qa_status(int(m.get("frames") or 0),
                               float(qa.get("greenEdge") or 0.0),
                               float(hole_raw) if hole_raw is not None else 1.0,
                               body_box=m.get("bodyBox"))
            qa["status"] = status
            counts[status] += 1
            cats[category] = cats.get(category, 0) + 1
            if status != "pass":
                bad_rows.append((m["id"], status, qa.get("greenEdge"),
                                 qa.get("holeRatio"), int(m.get("frames") or 0)))
            after = (m.get("category"), m.get("legacyCategory"), m.get("tags"),
                     m.get("audio"), qa.get("status"), m.get("safeStartMs"))
            if before != after:
                changed += 1
                write_json(meta_path, m)
        info = build_char_files(char)
        rep = ["# 素材产线 QA 报告 · %s" % char, "",
               "- 生成时间：%s（--meta-only 元数据重建）" % time.strftime("%Y-%m-%dT%H:%M:%S"),
               "- 总数：%d，pass %d / warn %d / fail %d" % (
                   sum(counts.values()), counts["pass"], counts["warn"], counts["fail"]),
               "- 门禁：150–1050 帧；绿边率<0.5%；抠穿率≥0.97（v1.1）", ""]
        if bad_rows:
            rep += ["## 未通过明细", "",
                    "| clipId | status | greenEdge | holeRatio | 备注 |",
                    "|---|---|---|---|---|"]
            for cid, st, ge, hr, fr in bad_rows:
                note = ("超长/超短片%d帧" % fr if (fr < 150 or fr > 1050)
                        else ("半透明道具" if hr is not None and float(hr) < 0.99 else ""))
                rep.append("| %s | %s | %s | %s | %s |" % (cid, st, ge, hr, note))
        else:
            rep.append("全部片段通过硬门禁。")
        with open(os.path.join(CHARS, char, "QA_REPORT.md"), "w", encoding="utf-8") as fh:
            fh.write("\n".join(rep) + "\n")
        with_audio = sum(1 for x in info["index"] if x["audio"])
        for k in grand:
            grand[k] += counts[k]
        print("META_ONLY char=%-8s clips=%3d pass=%3d warn=%2d fail=%d other=%d "
              "audio=%3d changed=%3d" % (
                  char, len(info["index"]), counts["pass"], counts["warn"], counts["fail"],
                  cats.get("other", 0), with_audio, changed), flush=True)
    print("META_ONLY_DONE pass=%d warn=%d fail=%d" % (
        grand["pass"], grand["warn"], grand["fail"]), flush=True)
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--char", default="", help="角色名；--meta-only 时省略表示全库 8 角色")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--samples", type=int, default=0)
    ap.add_argument("--only", default="", help="逗号分隔的 clipId，调试用")
    ap.add_argument("--audio-only", action="store_true", help="只重挂音轨，不重编码")
    ap.add_argument("--meta-only", action="store_true",
                    help="只重建分类/标签/音轨/QA/索引，不重编码视频、不重跑像素分析")
    ap.add_argument("--audio-scope", choices=["own", "any"], default="own")
    ap.add_argument("--progress-json", action="store_true",
                    help="每完成一段额外打印一行机器可读的 PROGRESS {...}，供 run_all.py 汇总全库进度")
    ap.add_argument("--key-similarity", type=float, default=0.15,
                    help="抠像相似度（v1.1 定案 0.15；旧值 0.22 会削薄白发/浅裙）")
    ap.add_argument("--key-blend", type=float, default=0.05,
                    help="抠像过渡带（v1.1 定案 0.05）")
    args = ap.parse_args(argv)
    if args.meta_only:
        os.environ["DESKPET_AUDIO_SCOPE"] = args.audio_scope
        return meta_only(args)
    if not args.char:
        ap.error("--char 在重编码模式下必填；只重建元数据请用 --meta-only")
    os.environ["DESKPET_AUDIO_SCOPE"] = args.audio_scope
    # 抠像参数写进环境变量，worker 进程（Windows 上为 spawn）才会继承到同样的值
    os.environ["DESKPET_KEY_SIMILARITY"] = "%g" % args.key_similarity
    os.environ["DESKPET_KEY_BLEND"] = "%g" % args.key_blend
    global KEY, KEY_SIM, KEY_BLEND
    KEY_SIM, KEY_BLEND = args.key_similarity, args.key_blend
    KEY = "colorkey=0x55AA51:%g:%g,despill=type=green:mix=0.5:expand=0" % (KEY_SIM, KEY_BLEND)
    if args.audio_only:
        return audio_only(args.char)

    src_dir = os.path.join(DSH, args.char)
    if not os.path.isdir(src_dir):
        print("MISSING_SRC_DIR " + args.char)
        return 2
    files = sorted(os.path.join(src_dir, fn) for fn in os.listdir(src_dir)
                   if fn.lower().endswith(".webm"))
    if args.only:
        wanted = set(x.strip() for x in args.only.split(",") if x.strip())
        files = [f for f in files if os.path.splitext(os.path.basename(f))[0] in wanted]
    if args.limit:
        files = files[:args.limit]

    out_dir = os.path.join(CHARS, args.char)
    os.makedirs(os.path.join(out_dir, "clips"), exist_ok=True)
    log_path = os.path.join(out_dir, "pipeline.log")
    total = len(files)
    t0 = time.time()
    results = []
    print("BUILD char=%s clips=%d workers=%d" % (args.char, total, args.workers), flush=True)
    with open(log_path, "a", encoding="utf-8") as logf:
        logf.write("\n=== %s build %s clips=%d ===\n" % (
            time.strftime("%Y-%m-%d %H:%M:%S"), args.char, total))
        tasks = [(args.char, f, args.force, args.threads) for f in files]
        with ProcessPoolExecutor(max_workers=args.workers) as pool:
            futures = [pool.submit(safe_process, t) for t in tasks]
            for done, fut in enumerate(as_completed(futures), 1):
                res = fut.result()
                results.append(res)
                if args.progress_json:
                    print("PROGRESS " + json.dumps({
                        "id": res.get("id"), "status": res.get("status", "fail"),
                        "cached": bool(res.get("cached")),
                        "seconds": res.get("seconds", 0),
                        "bytes": int(res.get("bytes") or 0),
                        "note": res.get("note") or "",
                        "error": bool(res.get("error")),
                    }, ensure_ascii=False), flush=True)
                mark = "ERR" if res.get("error") else res.get("status", "?")
                msg = "[%d/%d] %-5s %5.1fs %s" % (
                    done, total, mark, res.get("seconds", 0),
                    res.get("id", "").encode("unicode_escape").decode("ascii"))
                print(msg, flush=True)
                logf.write(msg + "\n")
                if res.get("error"):
                    logf.write(res["error"] + "\n")
                logf.flush()

    results.sort(key=lambda r: r["id"])
    counts = {"pass": 0, "warn": 0, "fail": 0}
    for r in results:
        counts[r.get("status", "fail")] = counts.get(r.get("status", "fail"), 0) + 1
    elapsed = round(time.time() - t0, 1)
    info = build_char_files(args.char)
    total_bytes = sum(int(r.get("bytes") or 0) for r in results)
    report = {
        "char": args.char,
        "generatedAt": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "total": len(results),
        "counts": counts,
        "elapsedSec": elapsed,
        "totalBytes": total_bytes,
        "character": info["character"],
        "items": results,
    }
    write_json(os.path.join(out_dir, "qa_report.json"), report)

    stats_path = os.path.join(out_dir, "build_stats.json")
    if elapsed >= 60:
        write_json(stats_path, {"elapsedSec": int(round(elapsed)),
                                "finishedAt": report["generatedAt"],
                                "clips": len(results)})
    prev = None
    if os.path.isfile(stats_path):
        try:
            with open(stats_path, "r", encoding="utf-8") as fh:
                prev = json.load(fh)
        except Exception:
            prev = None

    bad = [r for r in results if r.get("status") != "pass"]
    lines = ["# 素材产线 QA 报告 · %s" % args.char, "",
             "- 生成时间：%s" % report["generatedAt"],
             "- 总数：%d，pass %d / warn %d / fail %d" % (
                 len(results), counts["pass"], counts["warn"], counts["fail"]),
             "- 本次运行耗时：%.0f 秒（%.1f 分钟；全部命中缓存时为 0）" % (elapsed, elapsed / 60.0)]
    if prev:
        lines.append("- 上次全量构建：%d 秒（%.1f 分钟，%s）" % (
            prev.get("elapsedSec", 0), prev.get("elapsedSec", 0) / 60.0,
            prev.get("finishedAt", "")))
    lines += ["- 产出体积：%.1f MB，均段 %.2f MB" % (
        total_bytes / 1048576.0, total_bytes / 1048576.0 / max(1, len(results))),
        "- 门禁：150–1050 帧；绿边率<0.5%；抠穿率≥0.97（v1.1）", ""]
    if bad:
        lines.append("## 未通过明细")
        lines.append("| clipId | status | greenEdge | holeRatio | 备注 |")
        lines.append("|---|---|---|---|---|")
        for r in bad[:80]:
            note = r.get("note") or ("转换异常" if r.get("error") else "")
            lines.append("| %s | %s | %s | %s | %s |" % (
                r.get("id"), r.get("status"), r.get("greenEdge", ""),
                r.get("holeRatio", ""), note))
    else:
        lines.append("全部片段通过硬门禁。")
    with open(os.path.join(out_dir, "QA_REPORT.md"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")

    print("DONE total=%d pass=%d warn=%d fail=%d elapsed=%.0fs bytes=%.1fMB" % (
        len(results), counts["pass"], counts["warn"], counts["fail"],
        elapsed, total_bytes / 1048576.0), flush=True)

    if args.samples:
        made = make_samples(args.char, args.samples)
        print("SAMPLES %d" % len(made), flush=True)
        for p in made:
            print("SAMPLE " + os.path.basename(p), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
