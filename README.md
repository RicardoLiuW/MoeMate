# MoeMate · 萌搭子 · 二次元 AI 编程桌宠（Tauri 2 + Antigravity IDE 遥测联动）

> 8 位 AI 角色（Claude / DeepSeek / Gemini / GLM / GPT / Grok / Kimi / Qwen）的透明桌面编程搭子，跑在 Windows 桌面最上层空白区；直连本机 Antigravity IDE 与本地/云端语音系统，集成「柔萌马卡龙手账风」UI、像素风角色头像、中字日音 Galgame 对话与零膨胀按需下载体系。

---

## 一、核心能力

- **常驻桌面与像素穿透**：透明无边框、始终置顶、像素级点击穿透（Rust 每 33ms `GetCursorPos` 查询 160×90 每帧掩码 + 3×3 邻域宽松命中），角色身体以外区域直达后方 IDE / 浏览器；`uiHitRects` 保证灵动岛按钮独立可点。
- **零延迟跟手拖拽与贴边「暗中观察」**：Win32 250Hz 非阻塞光标跟随 + 前端 `dragSlot` 预热槽；支持桌面左右边缘 62% 深藏身吸附、14° 歪头探望、悬停平滑探出、周期呼吸微动、贴边逆变换精准点击及多显示器坐标钳位。
- **8 位角色与 1044 段高清动作**：1280×720 / 30fps VP9-alpha 透明 WebM；支持全库 `safeStartMs` 跳过脏首帧与 164 段 `sustainLoop` 中段无缝交叉淡化循环；首次打包本体零内置素材，支持同仓库 Releases 按需勾选下载。
- **7 级优先级与三层呼吸调度器**：`DRAG(100)` > `CLICK(80)` > `COMMAND(60)` > `AGENT_ALERT(50)` > `SCHEDULE(40)` > `FOREGROUND_WORK(30)` > `IDLE(10)`，非工作态采用静息 55% / 日常玩乐干饭 30% / 才艺节日彩蛋 15% 三层呼吸调度 + 2h 去重加权；左键单击提供弹簧压扁回弹与五声音阶轻音效，3s 内连点 ≥5 次触发专属生气动画与语音。
- **柔萌马卡龙手账风 UI（v3.1.0）**：奶油手账 / 星糖暗夜双外观、8 角色马卡龙主题、8 角色像素风头像（`28×28` 清晰点阵）、全站零 `backdrop-filter`（防 Windows WebView2 透明窗口闪烁）；桌面与系统托盘采用 Gemini 头像图标。
  - **头顶双形态灵动岛**：完整态 + 极简胶囊，集成 `t/s` 速率脉冲、多模型池额度条、上下文一键续命、独立番茄钟胶囊与空素材引导按钮。
  - **四页签整合工作台**：集成「状态 / 伙伴 / 相册 / 闲聊」，支持动作搜索、随机播放与快捷词片，自由聊天基于 10 桶中/日关键词 + 颜文字情绪识别精确匹配动作。
  - **720×560 左右分栏设置中心**：覆盖「AI 对话模型 / 语音与显存管理 / 角色与语音下载 / 角色性格定制 / 系统与开发者」（支持 `Ctrl+S` 快捷保存）。
- **AI 对话、三档显存模式与 512 条双语离线语音**：
  - OpenAI 兼容接口（WinHTTP 原生客户端，支持 DeepSeek / Gemini / Qwen / GLM / Kimi / 硅基流动 / OpenAI / 自定义端点）。
  - Galgame「中文字幕 · 逐句完整日语配音」双语模式：支持 **Fish Speech（OpenAudio S1-mini）本地零样本克隆**（300 文件/150MB LRU `.ogg` 缓存）、**Azure AI Speech 旗舰女声**，以及 **8 角色 × 32 场景中/日双语离线语音包（0ms，共 512 条 `.ogg`）**，支持一键切换日配/中配（默认日配）。
  - **三档显存模式 + 实时显存诊断**：支持「零显存 / 生图优先」「均衡省显存（默认，闲置自动卸载）」「满血实时克隆」，内置 GPU / Ollama / Fish Speech 占用透明看板与一键释放显存。
- **IDE 事件语音与智能防打扰陪伴**：Antigravity IDE 状态跃迁沿语音（`work_start` / `done` / `approval` / `error` / `context_high`）、`quiet` / `balanced` / `lively` 三档自主陪伴节流阀（Win32 全屏/演示静默 + 键鼠高频输入避让 + AFK 静默）、`Alt + Space` 全局呼出快捷聊天、45 分钟智能久坐提醒、Windows 开机自启与 `127.0.0.1:19820` HTTP Hook。

---

## 二、开发与构建

### 1. 开发模式

```powershell
cd app
npm install
npm run tauri dev
```

### 2. 构建发布安装包

```powershell
cd app
npm run build
npm run tauri build
```

安装包生成于 `app/src-tauri/target/release/bundle/`（NSIS `.exe` 与 MSI `.msi`）。安装包本体**不捆绑** 6.88 GB 角色动画视频与 GPT-SoVITS 运行时。

### 3. 测试验证

```powershell
npm --prefix app run build
cargo test --manifest-path app/src-tauri/Cargo.toml --offline
```

---

## 三、GitHub 同仓库 Releases 分发结构

程序首次运行若本地无角色动画，会在灵动岛、右键菜单、系统托盘与设置中心「角色与语音下载」提供一键下载入口（支持 GitHub 官方直连与 `ghfast.top` 国内加速，以及断点续传）。

在同一个 GitHub Release 下按以下文件名上传附件即可被客户端自动识别：

| 类别 | 文件名 | 说明 |
|---|---|---|
| 本体安装程序 | `MoeMate_*_x64-setup.exe` / `.msi` | 轻量本体，不含角色视频与语音模型 |
| 角色动画包（8 个） | `char-claude.zip`、`char-deepseek.zip`、`char-gemini.zip`、`char-glm.zip`、`char-gpt.zip`、`char-grok.zip`、`char-kimi.zip`、`char-qwen.zip` | 单包 548 MB–1.62 GB（均低于 GitHub Releases 2 GB 单文件上限），支持单角色按需勾选下载 |
| 日语语音素材包 | `ja-voices-pack.zip` | 8 角色 × 32 场景中/日双语离线语音包（共 512 条 `.ogg`）与 `ref_voices/*.wav` 参考音色（约 20.3 MB） |
| GPT-SoVITS 运行时 | `gpt-sovits-nvidia-runtime.zip` / `gpt-sovits-amd-zluda.zip` | 分别对应 NVIDIA CUDA（推荐）与 AMD ZLUDA 实验运行时 |

> **默认分发仓库**：`RicardoLiuW/MoeMate`（已在 [assets_mgr.rs](file:///c:/Users/Qiu/Desktop/Deskpet/app/src-tauri/src/assets_mgr.rs#L22) 默认配置为 `https://github.com/RicardoLiuW/MoeMate`，用户亦可在「设置 → 角色与语音下载」中自行覆盖）。

---

## 四、目录清点

```text
Deskpet/
  app/
    index.html                 入口页面（MoeMate）
    package.json               moemate-app 前端工程配置
    src/
      main.ts                  主舞台入口、空素材容错与事件调度
      panelWindow.ts           独立工作台窗口入口
      settingsWindow.ts        720×560 五页签设置与资源下载中心
      styles.css               柔萌马卡龙手账风样式（零 backdrop-filter）
      assets/avatars/*.png     8 角色 28×28 像素风头像
      shared/ipc.ts            11 command / 5 核心 event 契约与动态 assetsRoot
      stage/                   Canvas 2D 舞台、双缓冲与拖拽预热播放器、指针交互
      ui/                      灵动岛、工作台、右键菜单、气泡、像素头像与主题
    src-tauri/
      Cargo.toml               Rust 后端依赖声明（离线可编）
      tauri.conf.json          窗口参数、NSIS/MSI 打包配置（不捆绑动画/引擎）
      icons/                   Gemini 头像桌面与系统托盘图标
      src/
        lib.rs                 Tauri 入口、托盘、开机自启、资源目录路由
        assets_mgr.rs          同仓库 Releases 角色动画与日语语音下载管理器
        fish_speech.rs         Fish Speech（OpenAudio S1-mini）本地零样本语音引擎管理
        ai_voice.rs            OpenAI 对话、逐句日语配音翻译与三模语音调度
        scheduler.rs           7 级动作调度器、三层呼吸调度、连点生气与情绪映射
        passthrough.rs         33ms 像素级穿透、贴边歪头逆变换与 250Hz 非阻塞拖拽
        companion.rs           IDE 状态跃迁语音与智能防打扰自主陪伴节流阀
        vram.rs                三档显存模式策略与 GPU/进程显存诊断
        telemetry/             Antigravity IDE 遥测适配器
        db.rs / state.rs / model.rs / mask.rs / stats.rs / window_layout.rs / foreground.rs / http_hook.rs
  assets/
    catalog.json               角色目录元数据
    chars/                     （git 忽略，打包不内置）8 角色动画与本地语音资产
  runtime_engines/             （git 忽略，打包不内置）本地 GPT-SoVITS + ZLUDA 环境
  tools/                       离线素材/语音生成辅助脚本
  docs/v1_contract.md          IPC 契约权威文档
```

---

## 五、素材来源与开源协议声明

### 1. 代码开源协议
- 本项目源代码（`app/` 与 `tools/`）采用 **[MIT License](./LICENSE)** 开源，欢迎学习、修改与二次开发。

### 2. 角色动画素材来源声明
- 本项目使用的 8 位角色透明动作素材（`char-*.zip` 独立分发包及由此派生的小尺寸像素头像）来自开源项目：
  - **[hunmer/aboutme-pets](https://github.com/hunmer/aboutme-pets/tree/main)**
  - 上游原始素材归属：[PC2005-cloud/dsh-pet](https://github.com/PC2005-cloud/dsh-pet) 与基础立绘 [lokicorvus/crosspet](https://github.com/lokicorvus/crosspet)。
- **授权限制**：遵循上游 `aboutme-pets` 仓库许可要求，角色动画与立绘衍生素材**禁止用于任何商业用途**；基于本素材进行公开二次创作或分发时，**必须保留并署名上述上游仓库来源**。

### 3. 第三方语音与推理组件
- 本地语音克隆引擎调用 **[RVC-Boss/GPT-SoVITS](https://github.com/RVC-Boss/GPT-SoVITS)**（MIT 协议）及对应开源预训练权重，以及 AMD Windows 运行时兼容层 ZLUDA；上述外部运行时均与本软件本体完全隔离，由用户按需自行拉取使用。
