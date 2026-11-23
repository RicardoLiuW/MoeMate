# Deskpet V1 接口契约（v1.2 · 2026-11-22 · Claude 终审修订）

> 本文是 DeepSeek（产线 + Rust 壳 + Telemetry 采集层）与 Gemini（舞台 + 相册 + Telemetry UI）并行开发的唯一交接物。字段名、IPC 通道名、事件名改动须经 Claude 审批。

## 0. 已锁定前提
- 栈：Tauri 2（Rust）+ WebView2 + Canvas 2D + SQLite（rusqlite），Windows 优先。
- 运行时规格：**1280×720 / 30fps 原画质**（Ricardo 定稿）。
- 源素材只读：`dsh\<folder>\<动作名>.webm`（1044 段，VP9 yuv420p 绿幕，无 alpha）；`aboutme-pets\pets\<pack>\actions.json` 只取 category / audio 字段（其中 `deepseek` 显式映射至 `dsh1` 包）。
- **运行时资产格式 = VP9 + alpha 的 WebM（yuva420p）**，不用动画 WebP。理由：WebView2 原生硬解，内存只占播放缓冲；离线抽帧/质检必须显式加 `-c:v libvpx-vp9`（ffmpeg 默认 VP9 解码器忽略 alpha）。全库实测约 6.85GB（清理 3 段超长废片并统一 claude 参数后约 5.9GB）。

## 1. 目录
```
Deskpet\
  app\            Tauri 工程（src-tauri\ Rust，src\ 前端）
  tools\          产线脚本（Python 3.12 + ffmpeg）
  assets\
    catalog.json
    chars\<charId>\character.json
    chars\<charId>\actions.index.json
    chars\<charId>\clips\<clipId>.webm      抠像后 VP9-alpha
    chars\<charId>\clips\<clipId>.meta.json
    chars\<charId>\clips\<clipId>.mask.bin   每帧 1/8 alpha 掩码（160×90×frames bit，每帧 1800B，行优先 MSB-first）
    chars\<charId>\audio\<clipId>.ogg        可选
  docs\
```
charId = dsh 下的文件夹名：`claude deepseek gemini glm gpt grok kimi qwen`（8 个角色）。
clipId = 源文件名去扩展名（UTF-8 原样，含空格与 v2 后缀）。

## 2. 产线（V0，DeepSeek）
1. 抠像：`colorkey=0x55AA51:0.15:0.05`（RGB 距离，0.22 实测会削薄白发/浅裙，已废弃），**禁止用 chromakey**。
2. 去溢色：`despill=type=green:mix=0.5:expand=0`。
3. 编码：`libvpx-vp9 -pix_fmt yuva420p -crf 30 -b:v 0 -auto-alt-ref 0`，不缩放、不裁边、不改帧。
4. 音轨：同包映射优先（`deepseek → dsh1`），**默认关闭跨包同名兜底**（避免串声线）；转 Opus .ogg，-16 LUFS；无音轨时 `audio: null`，运行时静默播放。
5. 分类与绑定：优先取对应包 `actions.json` 的 category；缺失时（如 `gpt` 新动作、`deepseek` v2/v3 重拍）必须按动作名关键词自动归类，严禁整角色退化为 `other`。`bindings` 生成必须过滤 `qa.status == "fail"`。
6. 包围盒：逐帧 alpha bbox，取**全片中位数**为 `bodyBox`（排除飘落道具），union 另存 `fullBox`。
7. 掩码：每帧缩到 160×90，alpha>32 记 1，按 `np.packbits`（行优先、MSB-first：`byte = frame*1800 + (y*160+x)//8`，`bit = 7 - ((y*160+x)%8)`）打包为 `mask.bin`。**Rust 与前端严禁硬编码 300 帧，必须以 `meta.frames` 为准。**
8. QA 硬门禁（不过 → `qa.status="fail"`，只进相册不进调度，且 `hit_test_mask` 拒绝加载 >2MB 掩码）：
   - 帧数：`decoded == source` 且 `150 ≤ frames ≤ 1050`（5–35 秒；>1050 帧如 11250 帧长片判 `fail`）；不足 300 或非 300 帧记 info/warn 不拦截正常 15s/30s 片段；
   - 绿边：不透明像素中 `G > max(R,B)+15` 比例 < 0.5%；
   - 抠穿：角色本体区域 alpha≥200 覆盖率与源片非绿像素比 ≥ 0.97。

## 3. meta.json
```json
{
  "schemaVersion": 1,
  "id": "被落叶淹没",
  "char": "claude",
  "category": "seasonal",
  "legacyCategory": "时节",
  "tags": ["autumn","once"],
  "frames": 300, "fps": 30, "durationMs": 10000,
  "size": [1280, 720],
  "bodyBox": [x, y, w, h],
  "fullBox": [x, y, w, h],
  "anchor": [0.5, 0.89],             // 图像归一化坐标（左上原点、y 向下）：x=bodyBox 中心/W，y=bodyBox 底边/H（脚底）
  "loop": false,                      // 首尾帧差 < 阈值才为 true
  "interruptAfterMs": 0,              // 吃喝/才艺类给 >0
  "mirrorable": true,                 // 含文字的段 false
  "audio": "audio/被落叶淹没.ogg" | null,
  "qa": { "status": "pass|warn|fail", "greenEdge": 0.0, "holeRatio": 0.0 },
  "hash": "sha256:..."
}
```
category 枚举（32 旧类归一）：`idle move turn click drag emotion chatter daily eat play talent work seasonal festival balance outfit vfx etiquette species other`。

## 4. character.json
```json
{
  "schemaVersion": 1, "id": "claude", "name": "…",
  "bindings": { "idle":[], "click":[], "drag":[], "moveLeft":[], "moveRight":[],
                "work":[], "eat":[], "sleep":[], "wake":[] },
  "schedule": { "wake":"08:30","lunch":"12:00","dinner":"18:30","sleep":"23:30" },
  "defaultScale": 0.5
}
```
bindings 由产线按 category+关键词自动生成，人工可改；每个键至少 1 段，缺失回退 `idle`。

## 5. IPC（Tauri command / event，`app\src\shared\ipc.ts` 与 Rust 同名）
| 名称 | 类型 | 载荷 → 返回 |
|---|---|---|
| `catalog_get` | cmd | → Catalog |
| `char_switch` | cmd | {charId} → CharState |
| `pet_state_get` | cmd | → {stats, pos, scale, pinned, muted} |
| `pet_interact` | cmd | {kind:"click"\|"dragStart"\|"dragEnd"\|"feed", x, y} |
| `action_play` | cmd | {clipId} （相册/指令） |
| `settings_set` | cmd | {key, value} |
| `chat_send` | cmd | {text} → {clipId, bubble} |
| `hit_test_mask` | cmd | {clipId} → mask.bin 字节（前端缓存） |
| `telemetry_get` | cmd | → UnifiedState（v1.2 遥测当前快照） |
| `telemetry_refresh` | cmd | → UnifiedState（v1.2 手动立即强刷一次额度与状态） |
| `telemetry_set_source` | cmd | {source} → UnifiedState（v1.2 切换遥测源，首版固定 `"antigravity"`） |
| `play` | event R←M | {clipId, startAtMs, mirror, audio} |
| `bubble` | event R←M | {text, ttlMs} |
| `stats` | event R←M | {satiety, energy, mood, affinity} |
| `telemetry` | event R←M | UnifiedState（v1.2 仅在状态/数值有变化时 Diff 推送） |
| `frame_hint` | event R→M | {clipId, frame} 每 100ms 上报，供命中测试 |

**调度与遥测采集全在 Rust**（WebView 被遮挡会节流）；前端只播放、上报帧号、渲染气泡与数据浮层。

## 6. 状态事件与优先级
`DRAG(100) > CLICK(80) > COMMAND(60) > AGENT_ALERT(50) > SCHEDULE(40) > FOREGROUND_WORK(30) > IDLE(10)`
事件：`APP_READY USER_CLICK USER_DRAG_START USER_DRAG_END NEED_HUNGRY NEED_SLEEPY SCHEDULE_TICK WORK_APP_FOCUS WORK_APP_BLUR CHAT_REPLY CLIP_ENDED AGENT_STREAMING AGENT_TOOL_RUNNING AGENT_WAITING_APPROVAL AGENT_ALERT_TRIGGERED`。
高优先级可在 `interruptAfterMs` 之后打断；切段 150ms 透明度交叉；IDLE 选段：加权随机，2 小时内播过的权重 ×0.1。
- **Agent 联动规则（v1.2）**：
  - `streaming` / `tool_running` 激活 `FOREGROUND_WORK(30)`（优先播 `work` 动作池，不强拉视频倍速防鬼畜；前端可在静音时微调 `0.95x–1.2x` 并在迷你徽章表现实时脉冲）。
  - `waiting_approval`、上下文占用 `>80%`、当前池剩余额度 `<10%`、账号/区域/限流异常（401/403/429）触发 `AGENT_ALERT(50)` + 提醒气泡，同类告警强制 **60s 冷却防刷屏**（不打断 `DRAG`/`CLICK`）。
数值：饱食 −4/h、精力 −3.5/h（睡 +7/h）、心情 −1.2/h、亲密度只增（互动 +1）；离线结算上限 12h。

## 7. 像素穿透（硬条款）
- 窗口 transparent + decorations=false + alwaysOnTop + skipTaskbar。
- 默认 `set_ignore_cursor_events(true)`。Tauri 无 Electron 的 forward 参数，故 **Rust 每 33ms 用 `GetCursorPos` 轮询**，换算到窗口内坐标，查 当前 clipId × 前端上报帧号 对应的 mask 位；命中 → `set_ignore_cursor_events(false)`，离开 300ms → 恢复 true。拖拽期间（`dragging`）与面板打开期间（`uiInteractive`）强制 false。
- **UI 常驻热区 `uiHitRects`（v1.2 新增）**：前端通过 `settings_set("uiHitRects", UiHitRect[])` 上报常驻悬浮控件（如迷你状态徽章）的窗口归一化矩形 `[{x, y, w, h}]`（0..1，仅存内存不落库）。33ms 穿透轮询时，光标落入任意 `uiHitRect` 视同命中前景，立即 `set_ignore_cursor_events(false)`，离开后同 300ms 延迟恢复穿透，确保迷你徽章可悬停/点击而空白区域仍完全穿透。
- 镜像播放时掩码 x 轴翻转（`uiHitRects` 不随角色镜像翻转）。

## 8. 验收（V1）
常驻 8h 内存不随已播段数增长；切段 ≤400ms 且无上段音频残留；透明处点击穿透到后方窗口；前台为 IDE 时工作段占比明显上升。

## 9. Agent 遥测架构与 UnifiedState 契约（v1.2 · Antigravity IDE 首版）

### 9.1 三层解耦（采集在 Rust，展现零耦合）
```
[ AntigravityAdapter ] ──┐
                         ├──▶ Rust TelemetryManager ──(emit "telemetry")──▶ [ 桌宠表现层 UI ]
[ 未来其他工具 Adapter ] ──┘      (trait TelemetryAdapter + UnifiedState)     (徽章/面板/预警复用)
```
Rust 端定义统一 trait `TelemetryAdapter`，后续接入 Claude Code / Cursor / Codex / Ollama 仅需新增 Adapter 实现并注册，前端无需改动。

### 9.2 UnifiedState（Rust ↔ TS 严格同构，camelCase）
```ts
type TelemetryStatus = "offline" | "idle" | "streaming" | "tool_running" | "waiting_approval" | "error";
type TelemetryConfidence = "exact" | "estimated" | "unavailable";

interface UnifiedState {
  source: string;                         // 首版 "antigravity"
  availableSources: string[];             // ["antigravity"]，未来自动扩展
  status: TelemetryStatus;
  statusSinceMs: number;                  // 当前状态起始毫秒时间戳（前端本地跳秒算耗时）
  activeStepLabel: string | null;         // 例如 "执行命令" / "搜索网页" / "等待确认"
  capabilities: {
    hasQuota: boolean;
    hasMultiPool: boolean;
    hasRealtimeSpeed: boolean;
    hasTtft: boolean;
    hasContext: boolean;
    hasApproval: boolean;
  };
  model: {
    name: string | null;                  // 当前激活模型展示名
    contextLimit: number | null;          // 上下文上限（如 200000 / 1000000）
  };
  quota: {
    remainingPct: number | null;          // 0..100
    resetAtMs: number | null;             // 下次重置毫秒时间戳（前端每秒本地递减）
    pools: Array<{
      id: string;                         // 模型/池标识
      label: string;                      // 展示名（如 "Claude" / "Gemini"）
      remainingPct: number;               // 0..100
      resetAtMs: number | null;
      isActive: boolean;
    }>;
    confidence: TelemetryConfidence;
  };
  speed: {
    tokensPerSec: number | null;          // 非 streaming 时必须归零或 null
    ttftMs: number | null;                // 首字延迟毫秒（无数据为 null，严禁伪造）
    confidence: TelemetryConfidence;
  };
  context: {
    usedTokens: number | null;
    limitTokens: number | null;
    usedPct: number | null;               // 0..100
    deltaTokens: number | null;           // 本步新增消耗 Token
    confidence: TelemetryConfidence;
  };
  error: {
    code: number | null;                  // 401 / 403 / 429 等
    kind: "auth" | "region" | "rate_limit" | "network" | "unknown";
    message: string;
  } | null;
  alerts: {
    contextHigh: boolean;                 // usedPct >= 80
    quotaLow: boolean;                    // remainingPct <= 10
    waitingApproval: boolean;
    hasError: boolean;
  };
  updatedAtMs: number;
  stale: boolean;                         // 超过阈值未刷新成功时为 true
}
```

### 9.3 AntigravityAdapter 真实本地对接规范（已本机实测验证）
- **进程与端口自动发现 + 静默重连**：
  - 扫描 `%USERPROFILE%\.gemini\antigravity\daemon\hub_*.json`（按修改时间倒序，解析 `{ pid, httpPort, httpsPort, lsVersion, csrfToken }`）。
  - 通过 Win32 `OpenProcess` 验证 `pid` 存活；受限权限下 `OpenProcess` 拒绝时以 `127.0.0.1:{httpPort}` TCP 探活兜底；若端口变动或进程重启，下一次探测自动切换新 `hub_*.json`；无存活进程则置 `status = "offline"`。
  - **安全铁律**：`csrfToken` 仅存内存用于本机 `127.0.0.1:{httpPort}` 请求头 `X-Codeium-Csrf-Token`，严禁写入日志、SQLite 或发往前端；`GetUserStatus` 响应含 `email`，适配器仅解析 `clientModelConfigs`，绝不留存或外传。
- **本地 ConnectRPC 接口**（`POST http://127.0.0.1:{httpPort}/exa.language_server_pb.LanguageServerService/<Method>`，头 `Content-Type: application/json`、`Connect-Protocol-Version: 1`、`X-Codeium-Csrf-Token: <token>`）：
  1. `GetUserStatus`（body `{"metadata":{"ideName":"antigravity","extensionName":"antigravity","locale":"en"}}`）：
     - 解析 `userStatus.cascadeModelConfigData.clientModelConfigs[]` → 按 pool / model 聚合 `quotaInfo.remainingFraction`（×100 为百分比）与 RFC3339 `resetTime`（转 `resetAtMs`），输出当前激活池与备选模型池余量概览。
     - 捕获 HTTP/Connect 401（`unauthenticated`）、403（`permission_denied` / 区域限制）、429（`resource_exhausted`）映射至 `UnifiedState.error`。
  2. `GetAllCascadeTrajectories`（body `{}`）：
     - `trajectorySummaries` 实测为 **map（键即 `cascadeId`）**，值里的 `trajectoryId` 不可当 `cascadeId` 用。挑选最新活跃会话（优先 `CASCADE_RUN_STATUS_RUNNING` / `WAITING_FOR_USER`，否则取 `lastModifiedTime` 最新）。
     - 若无活跃运行态 → `idle`；若状态变化（从运行切回 `idle`），立即触发一次 `GetUserStatus` 同步额度。
  3. `GetCascadeTrajectorySteps`（body `{"cascadeId": "<id>", "startIndex": max(0, stepCount-4), "endIndex": stepCount}`）：
     - 依据末尾 step 的 `type` 与 `status` 精确区分：`WAITING` → `waiting_approval`；`PLANNER_RESPONSE` 运行/生成中 → `streaming`；`RUN_COMMAND` / `SEARCH_WEB` / `BROWSER_*` / 其他工具运行中 → `tool_running`（映射中文 `activeStepLabel`）；`ERROR` → `error`。
  4. `GetCascadeTrajectoryGeneratorMetadata`（body `{"cascadeId": "<id>", "generatorMetadataOffset": 0}`）：
     - `generatorMetadata` 实测为数组，**元素本身即 `chatModel`**（无外层包裹，兼容可选 `chatModel` 壳）；`timeToFirstToken` 与 `streamingDuration` 位于元素顶层而非 `usage` 内。取末尾元素解析 `modelDisplayName`/`model`（及内置上下文上限表：Gemini 1M、Claude 200K、GPT/默认 128K–200K）；`usage.inputTokens` + `cacheReadTokens` 计算上下文占用，与前一步 `usage` 差值计算 `deltaTokens`；`timeToFirstToken`（Protobuf duration 如 `"1.23s"`）解析为精确 `ttftMs`；`usage.outputTokens` / `streamingDuration` 计算精确 `tokensPerSec`（流式进行中尚未封口时用采样窗口新增估算并标 `confidence: "estimated"`，非 `streaming` 状态归 `0` / `null`）。

### 9.4 分级刷新与防自伤策略
- **实时速度采样**：仅 `status == "streaming"` 时 400ms 高频采样；离开 `streaming` 立即停采并将 `speed.tokensPerSec` 归零。
- **工作状态与确认检测**：`1s` 轻量检测（`GetAllCascadeTrajectories` + 仅活跃时查尾步）；状态与字段未变化时**不 emit `telemetry`**（Diff 推送省唤醒）。
- **额度与重置倒计时**：`45s` 低频轮询 + 每次 Turn 结束即时同步 + 手动 `telemetry_refresh`；遇到 **429 限流时自动指数退避至 `120s–300s`**，绝不自伤。

### 9.5 前端双形态与 P0 快捷操作（Gemini）
- **迷你状态徽章**：常驻角色下方/侧边紧凑药丸（展示状态色点 + `t/s` 或步骤耗时 + 额度%/上下文% 缩略，流式期呼吸脉冲；触发预警时变色高亮），通过 `ResizeObserver` / 布局变化自动上报 `uiHitRects`。
- **展开数据浮窗**：悬停或点击迷你徽章展开毛玻璃完整面板（数据源标签、当前模型、额度 + 本地跳秒倒计时 + 多池余量条、实时速度 + TTFT、上下文进度条 + 单步 Delta、401/403/429 异常横幅、手动刷新按钮），展开期间锁 `uiInteractive=true`。
- **一键续命提示词（提进 P0）**：点击上下文进度条（或旁边的“续命”按钮），自动复制结构化“总结当前任务进度以便开新 Task”提示词到剪贴板，并弹出确认气泡。
