# 素材产线（tools）

绿幕原片 `dsh\<char>\*.webm` → 运行时资产。源目录只读，产物全部写到 `assets\chars\<char>\`。

---

## 一、一键跑全库（Ricardo 用这个）

**双击 `tools\run_all.bat` 就行，不用开命令行、不用带参数。**

它会做这些事：

1. 体检 Python / ffmpeg / ffprobe / numpy / Pillow，缺什么当场打印怎么装，不会一闪而过；
2. 扫描 `dsh\` 下所有角色，算出还剩多少段，已经完成的**整角色或整段自动跳过**
   （判定标准：`<段>.webm` + `<段>.meta.json` + `<段>.mask.bin` 三件套齐全）；
3. 逐角色调用 `build_assets.py`，实时打印全库进度：

   ```
   [412/878  47%] gemini  PASS   6.3s  余额-袋空如洗
            平均 6.1 秒/段，预计剩余 47 分 12 秒
   ```

4. 全部输出同时写进 `tools\run_all.log`，窗口关了也能回查；
5. 跑完生成全库汇总：`assets\QA_SUMMARY.md` 和 `assets\qa_summary.json`
   （按角色分组 pass/warn/fail、体积、耗时、未通过明细）。

**断了怎么办**：直接重新双击。已完成的段不会重编码，从断点接着跑。
关窗口时 run_all 用 Windows Job Object 把 ffmpeg 子进程一起结束，不会留下孤儿进程占 CPU。

命令行等价写法（排查或只想跑一部分时用）：

```powershell
python -X utf8 tools\run_all.py --dry-run                # 只体检 + 算计划，不动手
python -X utf8 tools\run_all.py                          # 跑剩下所有角色
python -X utf8 tools\run_all.py --chars claude --force   # 只重跑某角色全部
python -X utf8 tools\run_all.py --limit 3                # 每个角色只跑前 3 段（调试）
python -X utf8 tools\run_all.py --meta-only              # 只重建全库元数据 + 刷新汇总（不重编码）
```

---

## 二、抠像参数（v1.1 定案 0.15 / 0.05）

v1.1 契约已把抠像参数定为 `colorkey=0x55AA51:0.15:0.05`。早期 0.22 实测偏激进：

- deepseek「中国功夫」有 **28.7%** 的像素 alpha 落在 30–200（角色被削成半透明），叠浅色壁纸发灰；
- 0.15 后同一段半透明像素降到 **3.2%**，绿边率仍为 **0**，叠深底和纯白都实心；
- gemini「优雅猫耳娘舞」单段体积从 12.9MB 降到 **6.3MB**。

`run_all.bat` 与 `build_assets.py` 现在都默认 **0.15 / 0.05**，与 `docs\v1_contract.md` v1.1 一致。
历史产物里 claude 166 段仍是 0.22 参数（可选 `run_all.bat --chars claude --force` 统一，约 22 分钟，不阻塞 V1）。

---

## 三、单独跑一个角色（build_assets.py）

```powershell
python -X utf8 tools\build_assets.py --char claude                # 增量（已产出的跳过）
python -X utf8 tools\build_assets.py --char claude --force        # 全量重跑
python -X utf8 tools\build_assets.py --char claude --limit 3      # 只跑前 3 段，调试
python -X utf8 tools\build_assets.py --char claude --workers 3 --threads 4 --samples 5
python -X utf8 tools\build_assets.py --char claude --audio-only   # 只重挂音轨
python -X utf8 tools\build_assets.py --meta-only                  # 全库 8 角色只重建元数据
python -X utf8 tools\build_assets.py --meta-only --char deepseek  # 只重建某个角色
```

- `--workers`：并行片段数（本机 12 线程，默认 3×4 线程跑满）。
- `--samples N`：跑完后抽 N 段生成「深色 + 白底」对比图到 `qa_samples\`。
- `--key-similarity` / `--key-blend`：抠像参数，默认 **0.15 / 0.05**（v1.1 定案值）。
- `--meta-only`：只重算分类/标签/音轨/QA 状态 + `character.json` / `actions.index.json` / `catalog.json`，
  **不动 `.webm` / `.mask.bin`，不重跑像素分析，不重编码任何视频**。改了 `actions.json`、包映射或门禁后用它刷新。
- 中途断掉直接重跑同一条命令，已完成的片段会跳过。

## 每段的处理

1. 抠像：`colorkey=0x55AA51:<sim>:<blend>,despill=type=green:mix=0.5:expand=0`，**不用 chromakey**（白裙白发会被抠穿）。
2. 编码：`libvpx-vp9 -pix_fmt yuva420p -crf 30 -b:v 0 -auto-alt-ref 0`，不缩放、不裁边、不改帧。
3. 分析：逐帧 alpha 包围盒，取中位数作 `bodyBox`（排除飘落道具），union 作 `fullBox`。
4. 掩码：每帧缩到 160×90，alpha>32 记 1，按位打包成 `mask.bin`（每帧 1800 字节）。
5. 音轨：`actions.json` 的 audio 优先 → 同包 `audio\<id>.m4a`（默认不跨包，理由见下），转 Opus 48k / -16 LUFS。
6. 生成 `meta.json`、`actions.index.json`、`character.json`、`catalog.json`、`QA_REPORT.md`。

## 已知坑

- **ffmpeg 默认 VP9 解码器不读 alpha**。分析与样张必须加 `-c:v libvpx-vp9`，否则整帧 alpha 全是 255，`bodyBox` 会变成整张 1280×720。ffprobe 同样显示 `yuv420p`，但 `TAG:ALPHA_MODE=1` 说明 alpha 已在流里，Chromium / WebView2 能正常播。
- **音轨默认只用本包**（`--audio-scope own`，`deepseek → dsh1`）。跨包同名 `m4a` 兜底默认关闭：实测 glm / grok / kimi 的同名文件字节不同（不同角色的配音），给 claude 挂 glm 的音频等于换错声线。要旧行为时加 `--audio-scope any`。
- 单段 300 帧、1280×720、crf 30、0.15 抠像下全库 1044 段约 6.85GB（均段 6.7MB），体积以 `QA_SUMMARY.md` 为准。
- `mask.bin` 每段 540KB（300×1800），全库约 560MB。
- 帧数按源片实际产出，不补齐、不抽帧：claude「吃糖葫芦」298 帧、「吃青团」235 帧；deepseek / gpt 有一批 450 帧、少量 900 帧。按 v1.1 门禁 150–1050 帧都正常通过；只有 deepseek 3 段 11250 帧超长片判 `fail`。
- **包映射**：`deepseek` 的 category/audio 来自 `aboutme-pets\pets\dsh1\`（不是 `pets\deepseek\`，那里没有包）。`actions.json` 查不到的新动作（gpt 全部 193 段、deepseek v2/v3 重拍）按动作名关键词自动归类；未命中兜底 `idle`，不再整角色退化成 `other`。
- `--audio-only` 可以在不重编码画面的前提下按新规则重挂音轨。
- `--progress-json` 每完成一段多打一行 `PROGRESS {...}`，是 `run_all.py` 用来汇总全库进度的接口，人看不用管。

## v1.1 元数据重建结果（2026-11-22）

- 全库 1044 段：**pass 1038 / warn 3 / fail 3**，零视频重编码（`--meta-only`）。
- deepseek（dsh1）与 gpt 的 `category="other"` 从 171 / 193 降到 **0**；7 项核心 bindings 恢复为真实动作池，不再是字母前 3 段。
- deepseek 3 段 11250 帧超长片（叠衣服 / 挤酱汁 / 视频）标 `fail`，未进入任何 binding；dsh1 的 `工作加班`、`熬夜刷手机` 两条音轨已挂上。
- claude 从 5 条 warn 降到 3 条（298 / 235 帧不再误报，只剩放孔明灯 / 放河灯 / 被落叶淹没 3 段半透明道具）。
- 复跑命令：`python -X utf8 tools\run_all.py --meta-only`。

## 实测（2026-11-19，claude 166 段，0.22 抠像）

- 3 worker × 4 线程，全量 22.6 分钟；增量重跑（全部命中缓存）约 5 秒。
- 结果：pass 161 / warn 5 / fail 0；warn 的原因是 2 段源片不足 300 帧（按 v1.1 的 150–1050 门禁已不再拦截）+ 3 段半透明道具（孔明灯、河灯、落叶）抠穿率 0.985–0.989。
- 产出：166 段 WebM + 166 份 meta + 166 份 mask + 8 条音轨 + `actions.index.json` / `character.json` / `catalog.json` / `QA_REPORT.md`。

## QA 门禁

| 项 | 门禁 | 不通过 |
|---|---|---|
| 帧数 | 解码帧数 = 源片帧数，且 150 ≤ frames ≤ 1050（5–35 秒） | <150、>1050 或转换丢帧 fail |
| 绿边率 | 不透明像素中 `G > max(R,B)+15` 比例 < 0.5% | fail |
| 抠穿率 | `alpha>=200` 覆盖 / 源片明显非绿像素 ≥ 0.97 | fail |
| 循环 | 首尾掩码 IoU ≥ 0.98 记为 loop | 只影响 meta |

`fail` 的片段仍产出资产，但 `qa.status="fail"`，只进相册、不进自动调度；生成 `character.json` 的 bindings 时严格过滤 `fail`。

---

## 四、汇总报告怎么读

`assets\QA_SUMMARY.md` 是全库视角：每个角色一行（源片数 / 已完成 / pass / warn / fail / 体积 / 本次耗时），
下面是所有 warn / fail 的明细表。单个角色的详细报告在 `assets\chars\<角色>\QA_REPORT.md`。
