#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Deskpet V0 一键素材产线编排器。

Ricardo 只要双击 tools\\run_all.bat，本脚本负责：
  1. 环境自检（python / ffmpeg / ffprobe / numpy / Pillow），缺什么打印怎么装，不直接崩；
  2. 扫描 dsh\\ 下所有角色，算出「还剩多少段要编码」，已完成的整角色/整段自动跳过；
  3. 逐角色调用 build_assets.py，实时打印全库进度：第 N/剩余 段、当前角色、
     pass/warn/fail 累计、平均每段耗时与预计剩余时间；
  4. 全部输出同时写进 tools\\run_all.log（关掉窗口也能回查）；
  5. 跑完生成全库汇总报告 assets\\QA_SUMMARY.md + assets\\qa_summary.json
     （按角色分组 pass/warn/fail、体积、耗时、未通过明细）。

断点续跑：本脚本不删任何已产出片段；build_assets.py 见到
<clip>.webm + <clip>.meta.json + <clip>.mask.bin 三件套齐全就跳过。
中途关窗口或 Ctrl+C 都可以，重新双击即可接着跑。
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
import traceback

TOOLS = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(TOOLS)
DSH = os.path.join(ROOT, "dsh")
ASSETS = os.path.join(ROOT, "assets")
CHARS = os.path.join(ASSETS, "chars")
BUILD = os.path.join(TOOLS, "build_assets.py")
LOG_PATH = os.path.join(TOOLS, "run_all.log")
LOCK_PATH = os.path.join(TOOLS, "run_all.lock")
SUMMARY_MD = os.path.join(ASSETS, "QA_SUMMARY.md")
SUMMARY_JSON = os.path.join(ASSETS, "qa_summary.json")

STATUSES = ("pass", "warn", "fail")
_JOB = None  # 保持 Job Object 句柄存活，窗口被关时连带杀掉子进程


# --------------------------------------------------------------------------
# 输出与日志
# --------------------------------------------------------------------------
class Log:
    def __init__(self, path: str):
        self.path = path
        self.fh = None
        try:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            self.fh = open(path, "a", encoding="utf-8", buffering=1)
        except Exception:
            self.fh = None

    def line(self, text: str = "") -> None:
        try:
            print(text, flush=True)
        except Exception:
            pass
        if self.fh:
            try:
                self.fh.write(text + "\n")
            except Exception:
                pass

    def close(self) -> None:
        if self.fh:
            try:
                self.fh.close()
            except Exception:
                pass
            self.fh = None


def human_dur(sec: float) -> str:
    sec = int(round(max(0.0, sec)))
    if sec < 60:
        return "%d 秒" % sec
    m, s = divmod(sec, 60)
    if m < 60:
        return "%d 分 %d 秒" % (m, s)
    h, m = divmod(m, 60)
    return "%d 小时 %d 分" % (h, m)


def human_bytes(n: float) -> str:
    n = float(n or 0)
    if n >= 1024 ** 3:
        return "%.2f GB" % (n / 1024 ** 3)
    return "%.1f MB" % (n / 1024 ** 2)


def stamp() -> str:
    return time.strftime("%Y-%m-%d %H:%M:%S")


# --------------------------------------------------------------------------
# Windows：窗口关闭时连带结束 ffmpeg 子进程，避免留下孤儿进程
# --------------------------------------------------------------------------
def install_kill_on_close_job():
    global _JOB
    if os.name != "nt":
        return None
    try:
        import ctypes
        from ctypes import wintypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)

        class IO_COUNTERS(ctypes.Structure):
            _fields_ = [
                ("ReadOperationCount", ctypes.c_ulonglong),
                ("WriteOperationCount", ctypes.c_ulonglong),
                ("OtherOperationCount", ctypes.c_ulonglong),
                ("ReadTransferCount", ctypes.c_ulonglong),
                ("WriteTransferCount", ctypes.c_ulonglong),
                ("OtherTransferCount", ctypes.c_ulonglong),
            ]

        class JOBOBJECT_BASIC_LIMIT_INFORMATION(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_longlong),
                ("PerJobUserTimeLimit", ctypes.c_longlong),
                ("LimitFlags", wintypes.DWORD),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class JOBOBJECT_EXTENDED_LIMIT_INFORMATION(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", JOBOBJECT_BASIC_LIMIT_INFORMATION),
                ("IoInfo", IO_COUNTERS),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        k32.CreateJobObjectW.restype = wintypes.HANDLE
        k32.CreateJobObjectW.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p]
        job = k32.CreateJobObjectW(None, None)
        if not job:
            return None

        info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION()
        info.BasicLimitInformation.LimitFlags = 0x2000  # KILL_ON_JOB_CLOSE
        k32.SetInformationJobObject.argtypes = [
            wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
        if not k32.SetInformationJobObject(job, 9, ctypes.byref(info), ctypes.sizeof(info)):
            return None

        k32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        if not k32.AssignProcessToJobObject(job, k32.GetCurrentProcess()):
            return None
        _JOB = job
        return job
    except Exception:
        return None


def set_console_title(text: str) -> None:
    if os.name != "nt":
        return
    try:
        import ctypes
        ctypes.windll.kernel32.SetConsoleTitleW(text)
    except Exception:
        pass


def pid_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    if os.name != "nt":
        try:
            os.kill(pid, 0)
            return True
        except OSError:
            return False
    try:
        import ctypes
        from ctypes import wintypes
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.OpenProcess.restype = wintypes.HANDLE
        h = k32.OpenProcess(0x1000, False, int(pid))  # PROCESS_QUERY_LIMITED_INFORMATION
        if not h:
            return False
        code = wintypes.DWORD()
        ok = k32.GetExitCodeProcess(h, ctypes.byref(code))
        k32.CloseHandle(h)
        return bool(ok) and code.value == 259  # STILL_ACTIVE
    except Exception:
        return False


# --------------------------------------------------------------------------
# 环境自检
# --------------------------------------------------------------------------
def check_environment(log: Log) -> bool:
    problems = []
    log.line("---- 环境自检 ----")

    if sys.version_info < (3, 9):
        problems.append("Python 版本过低（当前 %d.%d），需要 3.9 以上：https://www.python.org/downloads/"
                        % sys.version_info[:2])
    else:
        log.line("  Python %d.%d.%d  OK" % sys.version_info[:3])

    exe_dir = os.path.dirname(sys.executable)
    for exe, hint in (
        ("ffmpeg", "装 ffmpeg 后把 bin 目录加进 PATH（winget install Gyan.FFmpeg，或从 https://www.gyan.dev/ffmpeg/builds/ 下 essentials 包解压）"),
        ("ffprobe", "ffprobe 和 ffmpeg 在同一个包里，一起加进 PATH"),
    ):
        path = shutil.which(exe) or shutil.which(exe, path=exe_dir)
        if path:
            log.line("  %-8s OK  %s" % (exe, path))
        else:
            problems.append("找不到 %s：%s" % (exe, hint))

    for mod, pip_name in (("numpy", "numpy"), ("PIL", "pillow")):
        try:
            __import__(mod)
            log.line("  %-8s OK" % mod)
        except Exception:
            problems.append("缺少 Python 包 %s：在本窗口执行  python -m pip install %s"
                            % (pip_name, pip_name))

    if not os.path.isfile(BUILD):
        problems.append("找不到产线脚本：%s" % BUILD)
    if not os.path.isdir(DSH):
        problems.append("找不到原片目录：%s" % DSH)

    if problems:
        log.line("")
        log.line("环境不满足，先按下面逐条处理，再重新双击 run_all.bat：")
        for i, p in enumerate(problems, 1):
            log.line("  %d) %s" % (i, p))
        log.line("")
        return False
    log.line("  全部就绪。")
    return True


# --------------------------------------------------------------------------
# 素材扫描
# --------------------------------------------------------------------------
def source_clips(char: str) -> list[str]:
    d = os.path.join(DSH, char)
    if not os.path.isdir(d):
        return []
    return sorted(fn for fn in os.listdir(d)
                  if fn.lower().endswith(".webm") and os.path.isfile(os.path.join(d, fn)))


def clip_paths(char: str, clip_id: str):
    base = os.path.join(CHARS, char, "clips", clip_id)
    return base + ".webm", base + ".meta.json", base + ".mask.bin"


def scan_char(char: str) -> dict:
    """统计某角色已完成（三件套齐全）的片段与 QA 分布。"""
    out = {"done": 0, "counts": {s: 0 for s in STATUSES}, "bytes": 0, "bad": []}
    clip_dir = os.path.join(CHARS, char, "clips")
    if not os.path.isdir(clip_dir):
        return out
    for fn in os.listdir(clip_dir):
        if not fn.endswith(".meta.json"):
            continue
        clip_id = fn[: -len(".meta.json")]
        webm, meta_path, mask = clip_paths(char, clip_id)
        if not (os.path.isfile(webm) and os.path.isfile(mask)):
            continue
        try:
            with open(meta_path, "r", encoding="utf-8") as fh:
                meta = json.load(fh)
        except Exception:
            continue
        status = str(meta.get("qa", {}).get("status", "fail"))
        if status not in out["counts"]:
            status = "fail"
        out["counts"][status] += 1
        out["done"] += 1
        try:
            out["bytes"] += os.path.getsize(webm)
        except Exception:
            pass
        if status != "pass":
            out["bad"].append((clip_id, status, meta.get("qa", {})))
    return out


def discover_chars() -> list[str]:
    if not os.path.isdir(DSH):
        return []
    chars = []
    for name in sorted(os.listdir(DSH)):
        if name.startswith(".") or name.startswith("_"):
            continue
        if os.path.isdir(os.path.join(DSH, name)) and source_clips(name):
            chars.append(name)
    return chars


def clean_stale_tmp(char: str, log: Log) -> int:
    """清掉上次被打断时残留的半成品临时文件（不影响已完成的段）。"""
    clip_dir = os.path.join(CHARS, char, "clips")
    if not os.path.isdir(clip_dir):
        return 0
    n = 0
    for fn in os.listdir(clip_dir):
        if fn.endswith(".tmp.webm") or fn.endswith(".tmp"):
            try:
                os.remove(os.path.join(clip_dir, fn))
                n += 1
            except Exception:
                pass
    if n:
        log.line("    清理上次中断残留的临时文件 %d 个" % n)
    return n


# --------------------------------------------------------------------------
# 单角色构建
# --------------------------------------------------------------------------
def run_char(char: str, opts, ctx: dict, log: Log) -> dict:
    src = source_clips(char)
    limit = opts.limit if opts.limit and opts.limit > 0 else 0
    planned = min(len(src), limit) if limit else len(src)

    clean_stale_tmp(char, log)

    cmd = [sys.executable, "-X", "utf8", BUILD, "--char", char,
           "--workers", str(opts.workers), "--threads", str(opts.threads),
           "--progress-json", "--audio-scope", "own",
           "--key-similarity", "%g" % opts.key_similarity,
           "--key-blend", "%g" % opts.key_blend]
    if opts.force:
        cmd.append("--force")
    if limit:
        cmd += ["--limit", str(limit)]
    if opts.samples:
        cmd += ["--samples", str(opts.samples)]

    env = dict(os.environ)
    env["PYTHONUTF8"] = "1"
    env["PYTHONIOENCODING"] = "utf-8"
    env["DESKPET_AUDIO_SCOPE"] = "own"

    t0 = time.time()
    log.line("  $ %s" % " ".join(["build_assets.py"] + cmd[4:]))
    proc = subprocess.Popen(
        cmd, cwd=ROOT, env=env,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, encoding="utf-8", errors="replace", bufsize=1)

    try:
        assert proc.stdout is not None
        for raw in proc.stdout:
            line = raw.rstrip("\r\n")
            if not line:
                continue
            if line.startswith("PROGRESS "):
                try:
                    ev = json.loads(line[9:])
                except Exception:
                    continue
                on_event(char, ev, ctx, log)
            elif line.startswith("["):
                # build_assets 自己的逐段进度行（clipId 经过 ASCII 转义，难读），
                # 全库进度统一由 on_event 打印。
                continue
            else:
                log.line("    " + line)
        rc = proc.wait()
    except BaseException:
        try:
            proc.kill()
        except Exception:
            pass
        raise

    elapsed = time.time() - t0
    info = scan_char(char)

    report_elapsed = None
    report_path = os.path.join(CHARS, char, "qa_report.json")
    if os.path.isfile(report_path):
        try:
            with open(report_path, "r", encoding="utf-8") as fh:
                report_elapsed = json.load(fh).get("elapsedSec")
        except Exception:
            pass

    ok = (rc == 0)
    log.line("  -> %s 完成：本次计划 %d 段，累计已完成 %d/%d 段，pass %d / warn %d / fail %d，%s，耗时 %s" % (
        char, planned or len(src), info["done"], len(src),
        info["counts"]["pass"], info["counts"]["warn"], info["counts"]["fail"],
        human_bytes(info["bytes"]),
        human_dur(report_elapsed if report_elapsed is not None else elapsed)))
    if not ok:
        log.line("  !! %s 退出码 %d，该角色可能有片段没跑完，重跑会自动续" % (char, rc))

    return {"char": char, "src": len(src), "planned": planned or len(src),
            "done": info["done"], "counts": dict(info["counts"]),
            "bytes": info["bytes"], "elapsed": report_elapsed if report_elapsed is not None else elapsed,
            "rc": rc}


def on_event(char: str, ev: dict, ctx: dict, log: Log) -> None:
    if ev.get("cached"):
        return
    status = str(ev.get("status", "fail"))
    if status not in STATUSES:
        status = "fail"

    ctx["run_done"] += 1
    ctx["counts"][status] = ctx["counts"].get(status, 0) + 1
    ctx["run_bytes"] += int(ev.get("bytes") or 0)
    ctx["run_seconds"] += float(ev.get("seconds") or 0)
    ctx["last_event_at"] = time.time()

    total = max(1, ctx["run_total"])
    done = ctx["run_done"]
    pct = int(round(done * 100.0 / total))
    tag = {"pass": "PASS", "warn": "WARN", "fail": "FAIL"}[status]
    clip_id = str(ev.get("id") or "")
    msg = "[%d/%d %3d%%] %-8s %-4s %6.1fs  %s" % (
        done, total, pct, char, tag, float(ev.get("seconds") or 0), clip_id)
    if ev.get("note"):
        msg += "  （%s）" % ev["note"]
    if ev.get("error"):
        msg += "  [转换异常，详见 tools/run_all.log]"
    log.line(msg)

    # 按墙钟时间算，不再累加各 worker 的耗时（否则并行 3 段会显示成 3 倍慢）。
    elapsed_wall = max(0.001, time.time() - ctx["started_at"])
    avg = elapsed_wall / max(1, done)
    left = total - done
    if left and (done % 25 == 0):
        log.line("          平均 %.1f 秒/段，预计剩余 %s" % (avg, human_dur(avg * left)))
    set_console_title("Deskpet 产线 %d/%d (%d%%) · %s" % (done, total, pct, char))


# --------------------------------------------------------------------------
# 全库汇总报告
# --------------------------------------------------------------------------
def write_summary(opts, log: Log, chars: list[str], per_char: list[dict],
                  started_at: float, ctx: dict) -> dict:
    rows = []
    totals = {s: 0 for s in STATUSES}
    total_bytes = 0
    total_clips = 0
    total_src = 0
    bad_rows = []

    done_map = {r["char"]: r for r in per_char}
    for char in chars:
        info = scan_char(char)
        src_n = len(source_clips(char))
        rec = done_map.get(char)
        rows.append({
            "char": char,
            "src": src_n,
            "done": info["done"],
            "counts": info["counts"],
            "bytes": info["bytes"],
            "elapsed": rec["elapsed"] if rec and rec.get("elapsed") is not None else None,
        })
        total_clips += info["done"]
        total_src += src_n
        total_bytes += info["bytes"]
        for s in STATUSES:
            totals[s] += info["counts"][s]
        for clip_id, status, qa in info["bad"]:
            bad_rows.append((char, clip_id, status, qa))

    bad_rows.sort(key=lambda r: (0 if r[2] == "fail" else 1, r[0], r[1]))
    wall = time.time() - started_at
    now = stamp()

    run_line = ("- 本次任务：--meta-only 元数据重建（分类/音轨/QA/索引），未重编码任何视频"
                if ctx.get("meta_only") else
                "- 本次新处理：%d 段（编码 + 分析），命中缓存 %d 段" % (
                    ctx["run_done"], max(0, ctx.get("seen", 0) - ctx["run_done"])))
    lines = [
        "# Deskpet 全库素材 QA 汇总",
        "",
        "- 生成时间：%s" % now,
        "- 本次运行：%s 起，用时 %s" % (time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(started_at)),
                                       human_dur(wall)),
        run_line,
        "- 全库：%d 个角色 / 源片 %d 段 / 已产出 %d 段" % (len(chars), total_src, total_clips),
        "- 通过情况：pass %d / warn %d / fail %d" % (totals["pass"], totals["warn"], totals["fail"]),
        "- 产出体积：%s（均段 %s）" % (
            human_bytes(total_bytes),
            human_bytes(total_bytes / total_clips) if total_clips else "0"),
        "",
        "## 按角色",
        "",
        "| 角色 | 源片 | 已完成 | pass | warn | fail | 体积 | 本次耗时 |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for r in rows:
        lines.append("| %s | %d | %d | %d | %d | %d | %s | %s |" % (
            r["char"], r["src"], r["done"], r["counts"]["pass"], r["counts"]["warn"],
            r["counts"]["fail"], human_bytes(r["bytes"]),
            human_dur(r["elapsed"]) if (r["elapsed"] is not None and not ctx.get("meta_only")) else "—"))

    lines += ["", "## 未通过明细（warn / fail）", ""]
    if bad_rows:
        lines.append("| 角色 | clipId | 状态 | greenEdge | holeRatio | 备注 |")
        lines.append("|---|---|---|---|---|---|")
        for char, clip_id, status, qa in bad_rows[:300]:
            note = ""
            try:
                with open(os.path.join(CHARS, char, "clips", clip_id + ".meta.json"), "r",
                          encoding="utf-8") as fh:
                    meta = json.load(fh)
                frames = int(meta.get("frames") or 0)
                if frames and (frames < 150 or frames > 1050):
                    note = "超长/超短片%d帧" % frames
                elif float(meta.get("qa", {}).get("holeRatio") or 1) < 0.99:
                    note = "半透明道具"
            except Exception:
                pass
            lines.append("| %s | %s | %s | %s | %s | %s |" % (
                char, clip_id, status, qa.get("greenEdge", ""), qa.get("holeRatio", ""), note))
    else:
        lines.append("全部片段通过门禁。")

    lines += ["", "## 说明", "",
              "- 门禁（v1.1）：150 ≤ 帧数 ≤ 1050；绿边率 < 0.5%；抠穿率 ≥ 0.97；450/900 帧正常通过。",
              "- `fail` 的片段仍会产出资产，但 `qa.status=fail`，只进相册、不进自动调度。",
              "- 每角色明细：`assets/chars/<角色>/QA_REPORT.md`。",
              ""]

    try:
        with open(SUMMARY_MD, "w", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
    except Exception as exc:
        log.line("  !! 写汇总 Markdown 失败：%s" % exc)

    data = {
        "generatedAt": now,
        "runStartedAt": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(started_at)),
        "runWallSec": int(round(wall)),
        "runNewClips": ctx["run_done"],
        "runCachedClips": max(0, ctx.get("seen", 0) - ctx["run_done"]),
        "chars": rows,
        "totals": {"clips": total_clips, "src": total_src, "bytes": total_bytes, **totals},
        "problems": [{"char": c, "id": i, "status": s, "qa": q} for c, i, s, q in bad_rows],
    }
    try:
        tmp = SUMMARY_JSON + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(data, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, SUMMARY_JSON)
    except Exception as exc:
        log.line("  !! 写汇总 JSON 失败：%s" % exc)

    return {"rows": rows, "totals": totals, "bytes": total_bytes,
            "clips": total_clips, "src": total_src, "wall": wall, "bad": bad_rows}


# --------------------------------------------------------------------------
# 主流程
# --------------------------------------------------------------------------
def acquire_lock(log: Log) -> bool:
    if os.path.isfile(LOCK_PATH):
        try:
            with open(LOCK_PATH, "r", encoding="utf-8") as fh:
                data = json.load(fh)
            pid = int(data.get("pid") or 0)
        except Exception:
            pid = 0
        if pid and pid_alive(pid):
            log.line("已经有一个产线在跑（PID %d）。先关掉那个窗口，或等它跑完再启动。" % pid)
            return False
        try:
            os.remove(LOCK_PATH)
        except Exception:
            pass
    try:
        with open(LOCK_PATH, "w", encoding="utf-8") as fh:
            json.dump({"pid": os.getpid(), "startedAt": stamp()}, fh, ensure_ascii=False)
    except Exception:
        pass
    return True


def release_lock() -> None:
    try:
        if os.path.isfile(LOCK_PATH):
            os.remove(LOCK_PATH)
    except Exception:
        pass


def build_plan(opts, log: Log) -> list[dict]:
    chars = discover_chars()
    if opts.chars:
        wanted = [c.strip() for c in opts.chars.split(",") if c.strip()]
        unknown = [c for c in wanted if c not in chars]
        if unknown:
            log.line("!! 这些角色在 dsh\\ 下不存在或是空的：%s" % ", ".join(unknown))
        chars = [c for c in wanted if c in chars]
    if not chars:
        log.line("!! dsh\\ 下没有可用角色。")
        return []

    plan = []
    for char in chars:
        src = source_clips(char)
        info = scan_char(char)
        limit = opts.limit if opts.limit and opts.limit > 0 else 0
        planned = min(len(src), limit) if limit else len(src)
        remaining = planned if opts.force else max(0, planned - min(info["done"], planned))
        plan.append({"char": char, "src": len(src), "done": info["done"],
                     "planned": planned, "remaining": remaining,
                     "counts": info["counts"], "bytes": info["bytes"]})

    plan.sort(key=lambda p: (-p["remaining"], p["char"]))
    return plan


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Deskpet 一键素材产线")
    ap.add_argument("--workers", type=int, default=3, help="并行片段数（默认 3）")
    ap.add_argument("--threads", type=int, default=4, help="每段 ffmpeg 线程数（默认 4）")
    ap.add_argument("--force", action="store_true", help="忽略缓存，全部重新编码")
    ap.add_argument("--limit", type=int, default=0, help="每个角色只跑前 N 段（调试用）")
    ap.add_argument("--chars", default="", help="只跑这些角色，逗号分隔")
    ap.add_argument("--samples", type=int, default=0, help="每角色跑完抽 N 段生成对比图")
    ap.add_argument("--key-similarity", type=float, default=0.15,
                    help="抠像相似度；默认 0.15（契约写的 0.22 实测会把角色削成半透明）")
    ap.add_argument("--key-blend", type=float, default=0.05,
                    help="抠像过渡带；默认 0.05（配 0.22 时契约用 0.08）")
    ap.add_argument("--dry-run", action="store_true", help="只体检和算计划，不动手")
    ap.add_argument("--meta-only", action="store_true",
                    help="只重建分类/标签/音轨/QA/索引并刷新汇总报告，不重编码任何视频")
    opts = ap.parse_args(argv)

    install_kill_on_close_job()
    log = Log(LOG_PATH)
    started_at = time.time()

    log.line("")
    log.line("=" * 72)
    log.line("Deskpet 素材产线  %s" % stamp())
    log.line("项目根：%s" % ROOT)
    log.line("=" * 72)

    if not check_environment(log):
        log.close()
        return 1

    if not acquire_lock(log):
        log.close()
        return 3

    per_char: list[dict] = []
    ctx = {"run_total": 0, "run_done": 0, "run_bytes": 0, "run_seconds": 0.0,
           "counts": {s: 0 for s in STATUSES}, "seen": 0,
           "started_at": time.time()}
    interrupted = False
    try:
        if opts.meta_only:
            chars = discover_chars()
            if opts.chars:
                wanted = [c.strip() for c in opts.chars.split(",") if c.strip()]
                chars = [c for c in chars if c in wanted]
            if not chars:
                log.line("!! dsh\\ 下没有可用角色。")
                return 2
            log.line("")
            log.line("---- 元数据重建（--meta-only，不重编码任何视频）----")
            ctx["meta_only"] = True
            rc_all = 0
            for char in chars:
                cmd = [sys.executable, "-X", "utf8", BUILD, "--meta-only", "--char", char]
                env = dict(os.environ, PYTHONUTF8="1", PYTHONIOENCODING="utf-8")
                t0 = time.time()
                res = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True,
                                     text=True, encoding="utf-8", errors="replace")
                for line in (res.stdout or "").splitlines():
                    if line.strip():
                        log.line("    " + line)
                if res.returncode != 0:
                    rc_all = res.returncode
                    log.line("    !! %s 元数据重建失败：" % char)
                    for line in (res.stderr or "").splitlines()[-10:]:
                        log.line("    " + line)
                info = scan_char(char)
                per_char.append({
                    "char": char, "src": len(source_clips(char)), "planned": 0,
                    "done": info["done"], "counts": dict(info["counts"]),
                    "bytes": info["bytes"], "elapsed": time.time() - t0,
                    "rc": res.returncode})
            summary = write_summary(opts, log, chars, per_char, started_at, ctx)
            print_final(log, summary, started_at)
            return rc_all

        plan = build_plan(opts, log)
        if not plan:
            log.close()
            return 2

        todo = [p for p in plan if p["remaining"] > 0]
        skipped = [p for p in plan if p["remaining"] <= 0]
        ctx["run_total"] = sum(p["remaining"] for p in todo)

        log.line("")
        log.line("---- 计划 ----")
        for p in plan:
            if p["remaining"] > 0:
                log.line("  %-9s 源 %4d 段，已完成 %4d，本次待处理 %4d" % (
                    p["char"], p["src"], p["done"], p["remaining"]))
            else:
                log.line("  %-9s 源 %4d 段，已完成 %4d，无需处理，跳过" % (
                    p["char"], p["src"], p["done"]))
        log.line("  合计：%d 个角色，本次待编码 %d 段" % (len(todo), ctx["run_total"]))
        if skipped:
            log.line("  已完成并跳过的角色：%s" % ", ".join(p["char"] for p in skipped))
        log.line("  参数：并行 %d 段 × 每段 %d 线程；断点续跑已开启%s" % (
            opts.workers, opts.threads, "；--force 全量重跑" if opts.force else ""))
        log.line("  抠像：colorkey 0x55AA51 相似度 %g / 过渡带 %g%s" % (
            opts.key_similarity, opts.key_blend,
            "（v1.1 定案 0.15/0.05；见 tools\\README.md）"
            if (abs(opts.key_similarity - 0.15) > 1e-9) else "（v1.1 定案值）"))

        total_bytes_all = sum(scan_char(c)["bytes"] for c in [p["char"] for p in plan])
        log.line("  当前已有产物：%s" % human_bytes(total_bytes_all))

        if opts.dry_run:
            log.line("")
            log.line("--dry-run：只体检和算计划，没有开始编码。")
            summary = write_summary(opts, log, [p["char"] for p in plan], [], started_at, ctx)
            log.line("  汇总报告：%s" % SUMMARY_MD)
            return 0

        if not todo:
            log.line("")
            log.line("全部角色都已完成，无需编码。下面是现有产物的汇总。")
            summary = write_summary(opts, log, [p["char"] for p in plan], [], started_at, ctx)
            print_final(log, summary, started_at)
            return 0

        log.line("")
        log.line("---- 开始编码 ----")
        set_console_title("Deskpet 产线启动中")

        for idx, p in enumerate(todo, 1):
            char = p["char"]
            log.line("")
            log.line("== [%d/%d] %s  源 %d 段，本次待处理 %d 段 ==" % (
                idx, len(todo), char, p["src"], p["remaining"]))
            rec = run_char(char, opts, ctx, log)
            per_char.append(rec)

        summary = write_summary(opts, log, [p["char"] for p in plan], per_char,
                                started_at, ctx)
        print_final(log, summary, started_at)
        set_console_title("Deskpet 产线完成")
        return 0 if all(r["rc"] == 0 for r in per_char) else 4

    except KeyboardInterrupt:
        interrupted = True
        log.line("")
        log.line("!! 收到中断信号，已停止。已完成的片段不会丢，重新双击 run_all.bat 就从断点接着跑。")
        try:
            summary = write_summary(opts, log, [p["char"] for p in build_plan(opts, log)],
                                    per_char, started_at, ctx)
            print_final(log, summary, started_at, interrupted=True)
        except Exception:
            log.line(traceback.format_exc())
        return 130
    except Exception:
        log.line("")
        log.line("!! 产线异常终止：")
        log.line(traceback.format_exc())
        return 1
    finally:
        release_lock()
        log.close()


def print_final(log: Log, summary: dict, started_at: float, interrupted: bool = False) -> None:
    totals = summary["totals"]
    log.line("")
    log.line("=" * 72)
    log.line("%s：%d 个角色 / 源片 %d 段 / 已产出 %d 段" % (
        "已中断" if interrupted else "产线完成",
        len(summary["rows"]), summary["src"], summary["clips"]))
    log.line("pass %d / warn %d / fail %d ｜ %s ｜ 本次用时 %s" % (
        totals["pass"], totals["warn"], totals["fail"],
        human_bytes(summary["bytes"]), human_dur(summary["wall"])))
    if summary["bad"]:
        log.line("未通过 %d 段，明细见汇总报告。" % len(summary["bad"]))
    log.line("汇总报告：%s" % SUMMARY_MD)
    log.line("每角色报告：assets\\chars\\<角色>\\QA_REPORT.md")
    log.line("运行日志：%s" % LOG_PATH)
    log.line("=" * 72)


if __name__ == "__main__":
    sys.exit(main())
