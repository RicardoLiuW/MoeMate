# MoeMate App · Tauri 2 + Vite + TypeScript

MoeMate 的桌面端主工程（Rust 后端 + TypeScript / Canvas 2D 前端）。

## 目录结构

```text
app/
  index.html
  package.json
  tsconfig.json
  vite.config.ts
  src/
    main.ts                 主舞台入口与事件调度
    panelWindow.ts          独立四页签工作台入口
    settingsWindow.ts       720×560 五页签设置与资源下载中心
    styles.css              柔萌马卡龙手账风样式（零 backdrop-filter）
    assets/avatars/         8 角色 28×28 像素风头像
    shared/ipc.ts           IPC 契约（11 command / 5 核心 event）与动态 assetsRoot
    stage/                  Canvas 2D 双缓冲舞台播放与指针交互
    ui/                     灵动岛、工作台、气泡、右键菜单、像素头像与主题
  src-tauri/
    Cargo.toml              Rust 依赖（零额外第三方网络/压缩 crate）
    tauri.conf.json         Tauri 2 窗口与打包配置
    icons/                  Gemini 头像图标（ico + 32/128/256 png）
    src/                    Rust 核心后端模块
```

## 常用命令

```powershell
cd app
npm install
npm run build             # TypeScript 检查 + Vite 构建
npm run tauri dev         # 启动开发模式
npm run tauri build       # 编译正式安装包（.exe / .msi）
cargo test --manifest-path src-tauri/Cargo.toml --offline
```

## 契约位置

- 权威契约：`../docs/v1_contract.md`
- 前端封装：`src/shared/ipc.ts`
- Rust 实现：`src-tauri/src/lib.rs`
