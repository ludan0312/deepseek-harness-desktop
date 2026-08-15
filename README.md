# DeepSeekHarness Desktop

> DeepSeek Harness 的桌面包装器 —— 系统托盘、自动启动、零耦合升级

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Tauri](https://img.shields.io/badge/Built%20with-Tauri-FFC131?logo=tauri)](https://tauri.app)
[![Rust](https://img.shields.io/badge/Language-Rust-000000?logo=rust)](https://www.rust-lang.org)


---

## 功能特性

| 特性 | 说明 |
|------|------|
| 系统托盘常驻 | 启动后托盘图标常驻，DSH 在后台运行 |
| 单击显示/隐藏 | 左键单击托盘图标切换窗口显示状态 |
| 右键菜单 | 显示主界面 / 隐藏 / 重启 DSH / 更改路径 / 退出 |
| 拦截关闭按钮 | 点击窗口 X 最小化到托盘，不退出 |
| 自动检测启动 | 启动时自动检测 DSH 是否运行，未运行则自动启动 |
| 路径选择对话框 | 首次启动自动检测路径，找不到则弹窗选择 |
| 配置持久化 | 路径保存到 `%APPDATA%`，下次自动读取 |
| 完全独立 | 与 DSH 源码零耦合，不影响官方 OTA 升级 |

---

## 架构设计

```
+---------------------------------------------+
|  DeepSeekHarness Desktop (本包装器)          |
|  +-- 系统托盘图标 (Windows/macOS/Linux)       |
|  +-- 窗口管理 (显示/隐藏/拦截关闭)            |
|  +-- 进程管理 (启动/监控/重启 DSH)            |
|  +-- WebView (加载 http://127.0.0.1:3080)    |
+---------------------------------------------+
                    |
                    | 启动/监控
                    v
+---------------------------------------------+
|  DSH 后端 (独立进程)                         |
|  +-- Node.js 服务                             |
|  +-- 插件系统 (agent-teams, search-mcp 等)    |
|  +-- 官方 OTA 升级 (完全不受影响)             |
+---------------------------------------------+
```

**核心原则**：包装器只负责"窗口外壳"和"进程管理"，DSH 的所有业务逻辑、插件、升级完全独立。

---

## 前置条件

| 环境 | 版本要求 | 安装方式 |
|------|---------|---------|
| **Rust** | 1.70+ | [rustup.rs](https://rustup.rs/) |
| **Node.js** | 18+ | [nodejs.org](https://nodejs.org/) |
| **pnpm** | 8+ | `npm install -g pnpm` |
| **DSH 源码** | 已构建 | 见下方 |

### 1. 安装 Rust

```powershell
# Windows PowerShell
Invoke-WebRequest -Uri https://win.rustup.rs -OutFile rustup-init.exe
.\rustup-init.exe
# 选择默认安装 (1)，安装完成后重启终端
```

### 2. 安装 Node.js 和 pnpm

```powershell
# 从 https://nodejs.org/ 下载 LTS 版本安装
node --version  # v18+
npm --version

# 安装 pnpm
npm install -g pnpm
pnpm --version
```

### 3. 确保 DSH 已构建

```powershell
cd D:\DeepSeekHarness\deepseek-harness-master  # 你的实际路径
pnpm install
pnpm run build
pnpm dsh web  # 测试能正常启动
```

---

## 快速开始

### 1. 克隆项目

```powershell
git clone https://github.com/YOUR_USERNAME/deepseek-harness-desktop.git
cd deepseek-harness-desktop
```

### 2. 安装依赖

```powershell
pnpm install
```

### 3. 开发模式运行（带热重载）

```powershell
pnpm tauri:dev
```

这会：
1. 启动 Vite 开发服务器（端口 1420）
2. 编译 Rust 代码
3. 启动 Tauri 窗口
4. 自动检测并启动 DSH

### 4. 构建生产版本

```powershell
pnpm tauri:build
```

构建产物：
- **Windows**: `src-tauri/target/release/deepseek-harness.exe`
- **安装包**: `src-tauri/target/release/bundle/` (MSI, NSIS)

---

## 使用指南

### 首次启动

1. 双击 `deepseek-harness.exe`（或安装后的快捷方式）
2. 如果未找到 DSH 目录，自动弹出文件夹选择对话框
3. 选择 DSH 源码目录（包含 `package.json` 的文件夹）
4. 托盘出现图标，约 10-30 秒后自动加载 DSH Web UI

### 日常操作

| 操作 | 方式 |
|------|------|
| 显示主界面 | 左键单击托盘图标 |
| 隐藏到托盘 | 点击窗口 X 按钮，或托盘右键菜单"隐藏到托盘" |
| 重启 DSH 服务 | 托盘右键菜单 -> "重启 DSH 服务" |
| 更改 DSH 路径 | 托盘右键菜单 -> "更改 DSH 路径" |
| 完全退出 | 托盘右键菜单 -> "退出" |

---

## 配置说明

### 修改默认 DSH 路径（硬编码默认值）

编辑 `src-tauri/src/main.rs`：

```rust
const DEFAULT_DSH_DIR: &str = r"D:\DeepSeekHarness\deepseek-harness-master";
```

### 修改 DSH 端口

编辑 `src-tauri/src/main.rs`：

```rust
const DSH_URL: &str = "http://127.0.0.1:3080";
```

### 修改窗口大小

编辑 `src-tauri/tauri.conf.json`：

```json
"windows": [{
  "width": 1400,
  "height": 900,
  "minWidth": 800,
  "minHeight": 600
}]
```

### 配置文件位置

首次选择路径后，配置自动保存到：
```
%APPDATA%\dsh-tauri-wrapper\config.json
```

---

## 目录结构

```
deepseek-harness-desktop/
+-- src/                    # 前端代码（Vite 构建产物）
|   +-- (Vite 构建产物)
+-- src-tauri/              # Rust 后端代码
|   +-- src/
|   |   +-- main.rs         # 核心逻辑（托盘、窗口、进程管理）
|   +-- icons/              # 应用图标
|   +-- Cargo.toml          # Rust 依赖
|   +-- tauri.conf.json     # Tauri 配置
|   +-- build.rs            # 构建脚本
+-- index.html              # 前端入口（加载中页面）
+-- package.json            # Node 依赖
+-- vite.config.ts          # Vite 配置
+-- README.md               # 本文件
```

---

## 升级策略

### 升级 DSH（官方 OTA）

**完全不影响包装器！**

```powershell
cd D:\DeepSeekHarness\deepseek-harness-master
git pull
pnpm install
pnpm run build
# 重启包装器即可自动使用新版本 DSH
```

### 升级包装器本身

```powershell
cd deepseek-harness-desktop
git pull
pnpm install
pnpm tauri:build
```

---

## 技术栈

| 技术 | 用途 |
|------|------|
| **Tauri** | 跨平台桌面框架（Rust + WebView） |
| **Rust** | 后端逻辑（托盘、进程管理） |
| **Vite** | 前端构建工具 |
| **reqwest** | HTTP 客户端（检测 DSH 状态） |
| **tokio** | 异步运行时 |
| **rfd** | 原生文件选择对话框 |
| **dirs** | 跨平台配置目录 |

---

## 常见问题

### Q1: 启动时提示 "DSH 启动超时"

**原因**：DSH 路径配置错误，或 DSH 未构建。

**解决**：
1. 右键托盘菜单 -> "更改 DSH 路径" 重新选择
2. 确保 DSH 目录已执行 `pnpm install && pnpm run build`
3. 手动测试：`cd DSH_DIR && pnpm dsh web` 能否正常启动

### Q2: 托盘图标不显示

**原因**：Windows 图标缓存问题。

**解决**：
1. 确保 `src-tauri/icons/` 目录有图标文件
2. 重启 Windows 资源管理器：`taskkill /f /im explorer.exe && start explorer.exe`

### Q3: 点击关闭后窗口消失但进程还在

**这是正常设计！** 点击 X 是最小化到托盘，不是退出。要完全退出请用托盘右键菜单 -> "退出"。

### Q4: 如何更换应用图标？

1. 准备 1024x1024 的 PNG 图标
2. 安装 Tauri CLI：`npm install -g @tauri-apps/cli`
3. 生成图标：`tauri icon /path/to/source.png`
4. 重新构建

---

## 贡献指南

欢迎提交 Issue 和 PR！

1. Fork 本仓库
2. 创建你的特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 打开一个 Pull Request

---

## 许可证

[MIT](LICENSE) License -- 与 DSH 保持一致。

---

## 致谢

- [Tauri](https://tauri.app/) -- 跨平台桌面框架
- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) -- 核心 AI 平台

---

**文档版本**: 1.1.0
**最后更新**: 2026-08-15
**兼容 DSH 版本**: v0.1.0-rc.5+
