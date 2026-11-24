# MoeMate 变更日志

按时间倒序。版本号对齐 `docs/v1_contract.md` 的契约版本（11 个 Tauri commands + 5 个核心事件不变）。

---

## v3.0.1 · 2026-11-24 · 启动可见性修复、显存治理、512 条双语语音、智能陪伴与贴边「暗中观察」

### 新增与升级
- **三档显存模式与实时显存诊断（`vram.rs` / `gpt_sovits.rs` / `settingsWindow.ts`）**：
  - 新增 **零显存 / 生图优先（`zero_vram`）**、**均衡省显存（`balanced`，默认）** 与 **满血实时克隆（`full_realtime`）** 三档一键切换，支持右键菜单快捷切换与闲置自动卸载 GPT-SoVITS 释放显存。
  - 新增显存占用实时诊断看板（零新增 Crate，按需探测 GPU 总显存、Ollama / GPT-SoVITS 进程占用并给出调度建议，支持一键释放 MoeMate 语音显存）。
  - GPT-SoVITS 本地语音缓存升级为 Opus `.ogg` + 语义归一化键 + **300 文件 / 150 MB LRU** 自动清理。
- **8 角色 × 32 场景中/日双语离线语音包（共 512 条 `.ogg`）**：
  - 扩充至 **32 场景/角色**（覆盖基础互动、连点生气 `angry_1/2`、作息、IDE 事件 `work_start_2`/`done_2`/`pomodoro_done`/`context_high`、自主轻陪伴 `ambient_1~4` 及情绪兜底 `emo_*`）。
  - 完善 `tools/voice_scripts.json` 与 `tools/generate_voice_pack.py`，完成全部 **256 条日语（GPT-SoVITS）+ 256 条中文（Azure HD 女声，其中 `qwen` 迁移至 `zh-CN-Xiaoxiao:DragonHDFlashLatestNeural`）** 离线合成并打入 `ja-voices-pack.zip`。
  - 新增内置语音语种切换（`builtinVoiceLang`，默认 `"ja"`，可在设置窗与右键菜单一键切换日配/中配）。
- **点击触感反馈升级与 3s 连点生气（`player.ts` / `interaction.ts` / `scheduler.rs`）**：
  - 新增脚底锚点阻尼弹簧压扁回弹形变与纯 Web Audio API 五声音阶轻点击音效（遵循静音与 `clickSfxEnabled` 开关）。
  - 取消普通单击切动画，改为 3s 滑动窗口 ≥ 5 次连点触发各角色专属生气动画（`resolve_angry_clip`）+ `angry_1`/`angry_2` 交替语音与 14s 冷却（贴边单击弹出不计入生气）。
- **非工作态三层呼吸动画调度（`telemetry/mod.rs` / `scheduler.rs` / `player.ts`）**：
  - 解除 `ProcessOffline` 的 `is_held()` 永久睡眠锁，IDE 未启动时恢复正常日常待机轮播；修复 `zzz` 粒子仅在当前动作确为睡眠片段时生成。
  - 重构非工作态三层选段池（静息 55% / 日常玩乐干饭 30% / 才艺节日彩蛋 15%），结合早午晚深夜时段权重、节庆加权与连续同层/同分类防重。
- **回复关键词细粒度情绪抓取（`ai_voice.rs` / `scheduler.rs`）**：
  - 新增 10 桶中/日关键词 + 颜文字/Emoji 情绪识别引擎（`detect_reply_emotion`）与跨 `category` 8 角色真实库存映射（`resolve_clip_for_emotion`），解决聊天回复动作情绪反转问题。
- **IDE 事件语音与智能防打扰自主陪伴（`companion.rs` / `foreground.rs`）**：
  - 新增 `IdeVoiceTracker`，接入 `work_start`/`done`/`approval`/`error`/`context_high` 状态跃迁沿播报与独立冷却。
  - 零新增 Crate 接入 Win32 `SHQueryUserNotificationState`（全屏/演示/勿扰静默）与 `GetLastInputInfo`（键鼠空闲探测），实现 `CompanionVoiceThrottle`（`quiet` / `balanced` / `lively` 三档、全局冷却预算、高频输入避让、AFK 静默、Agent 忙碌顺延与深夜陪伴）。
- **贴边「暗中观察」方案 A（零新素材，`window_layout.rs` / `passthrough.rs` / `player.ts`）**：
  - 增大贴边深藏身比例（`TUCK_FRACTION = 0.62`），结合 `bodyBox` 头部与肩部枢轴实现 14° 歪头探望、悬停平滑探出（+3.5° 与内移）及周期呼吸微动，工作台自动避让贴边探头区。
  - `passthrough.rs` 同步实现贴边歪头/探出刚体逆变换命中测试与 `pet_edge_hover` 悬停同步，确保贴边态稳定可点且空白区保持穿透。

### 修复
- **启动首帧隐身竞态修复（`player.ts` / `main.ts` / `state.rs` / `lib.rs`）**：
  - 新增 1200ms 首帧看门狗、`video.play()` 自动重试、`guardSafeStart` 450ms 安全放行、`assetsRootReady` 2500ms 兜底、`switchCharacter` 防重入及 `frame_hint` 已上报时跳过冗余补播。
- **左键单击事件被捕获阶段清空修复（`interaction.ts`）**：
  - 将 `window` 的 `pointerup` 兜底监听由捕获阶段改为冒泡阶段，修复单击被 `pointerId === null` 静默吞掉的问题。
- **清理编译器警告**：移除 `state.rs` 中已废弃的 `last_click_voice_ms` 与 `try_claim_click_voice`，实现 Rust 编译 0 警告。

---

## v3.0 · 2026-11-23 · MoeMate 品牌重塑、马卡龙手账 UI 与分发打包体系

### 新增与重设计
- **品牌与视觉重构（「柔萌马卡龙手账风」）**：
  - 项目正式更名为 **MoeMate（萌搭子）**，更新托盘/窗口标题/开机自启项。
  - 桌面图标与系统托盘图标重绘为 **Gemini 角色头像**（`icon.ico` + `32/128/256px` PNG）。
  - 全站切换奶油手账 / 星糖暗夜双外观与 8 角色马卡龙主题，继续严格保持 **0 条 `backdrop-filter`**。
  - 为 8 位角色制作 `28×28` 像素风头像（`app/src/assets/avatars/*.png`），应用于伙伴选择页、当前角色头卡、聊天气泡与设置/下载卡片；彻底移除界面上的声优名字展示。
  - **720×560 分栏设置中心**：重构为 5 页签（AI 对话模型 / 语音与声优克隆 / 角色与语音下载 / 角色性格定制 / 系统与开发者），支持 `Ctrl+S` 保存。
  - **灵动岛与工作台升级**：灵动岛新增独立番茄钟胶囊与 `Alt+Space` 快捷闲聊入口；动作相册新增「随机」试播；右键菜单紧凑防溢出。
- **角色动画与日语语音素材零捆绑分发体系（`assets_mgr.rs`）**：
  - 移除 `ipc.ts` 与 `tauri.conf.json` 中的本机硬编码路径，发布态运行期资产与引擎统一落盘到可写的 `%APPDATA%\Deskpet\` 目录，并通过 `asset_protocol_scope` 动态放行。
  - 支持 GitHub 同仓库 Releases 按需下载 8 个角色动画压缩包（`char-*.zip`）与独立日语语音素材包（`ja-voices-pack.zip`），内置 WinHTTP 断点续传与 `ghfast.top` / 官方直连切换。
  - 在系统托盘、主舞台右键菜单、工作台伙伴页与设置中心全部打通「角色与语音下载」入口，并在零角色启动时由灵动岛显示一键下载引导。
- **GPT-SoVITS 显卡分流**：
  - 安装选项拆分为 **NVIDIA 显卡（推荐 · 原生 CUDA）** 与 **AMD 显卡（实验性 · ZLUDA 转译）**，并在选择 AMD 时显示醒目的适配性风险提示。

### 修复
- **回复文字语音不读完（漏读后半句）**：
  - 优化 `ai_voice.rs` 提示词要求大模型将前面全部中文分句完整逐句译为日语，严禁省略后半句，并兼容全角冒号 `[日语：...]` / `[动作：...]` 标签及提升 `max_tokens`。
  - 将 `gpt_sovits.rs` 的 `text_split_method` 从 `cut3` 调整为 `cut5`（按中英文标点分句合成），并升级缓存键前缀为 `v3|` 自动失效旧残缺缓存。
- **清理无关大目录**：清理 `aboutme-pets/` 与 `dsh/` 冗余原片目录（释放约 8.27 GB），并将 `assets/chars/` 加入 `.gitignore`。

---
