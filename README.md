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
| 右键菜单 | 显示主界面 / 隐藏 / 关闭时最小化到托盘 / 重启 DSH / 更改路径 / 退出 |
| 关闭行为可切换 | 托盘菜单勾选「关闭时最小化到托盘」，随时在「托盘」与「退出」之间切换 |
| 拦截关闭按钮 | 点击窗口 X 不直接退出，按已保存的关闭行为处理 |
| 自动检测启动 | 启动时自动检测 DSH 是否运行，未运行则自动启动 |
| **token 认证适配** | 自动从 DSH 启动日志提取 `?token=xxx` 完整地址并导航（DSH 0.1.6+） |
| **外部实例自动接管** | 检测到 DSH 由外部实例运行且返回 401 时，自动结束外部实例并重启接管（v1.1） |
| 路径选择对话框 | 首次启动自动检测路径，找不到则弹窗选择 |
| 配置持久化 | 路径与关闭行为保存到 `%APPDATA%`，下次自动读取 |
| 完全独立 | 与 DSH 源码零耦合，不影响官方 OTA 升级 |

---

## 架构设计

```
+---------------------------------------------+
|  DeepSeekHarness Desktop (本包装器)          |
|  +-- 系统托盘图标 (Windows/macOS/Linux)       |
|  +-- 窗口管理 (显示/隐藏/拦截关闭)            |
|  +-- 进程管理 (启动/监控/重启 DSH)            |
|  +-- stdout/stderr 解析 (提取 ?token= 地址)   |
|  +-- WebView (唯一导航出口 load_dsh_ui)      |
+---------------------------------------------+
                    |
                    | 启动/监控 + 读取启动日志
                    v
+---------------------------------------------+
|  DSH 后端 (独立进程)                         |
|  +-- Node.js 服务                             |
|  +-- token 认证 (0.1.6+)                      |
|  +-- 插件系统 (agent-teams, search-mcp 等)    |
|  +-- 官方 OTA 升级 (完全不受影响)             |
+---------------------------------------------+
```

**核心原则**：包装器只负责"窗口外壳"和"进程管理"，DSH 的所有业务逻辑、插件、升级完全独立。

### 职责边界（重要）

| 关注点 | 归属 |
|--------|------|
| 窗口显示/隐藏/关闭拦截 | 外壳（`src-tauri/src/main.rs`） |
| 托盘菜单与勾选状态 | 外壳 |
| DSH 进程启停、输出采集 | 外壳 |
| **窗口导航（含 token 地址）** | **仅外壳** —— 前端 `index.html` 只保留加载动画，绝不改写 `window.location` |
| 业务逻辑、路由、插件 | DSH 本体，外壳不介入 |

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
cd D:\DeepSeekHarness\deepseek-harness   # 默认约定路径
pnpm install
pnpm run build
pnpm dsh web  # 测试能正常启动
```

> **DSH 0.1.6+ 提示**：`pnpm dsh web` 会打印一行带 token 的完整地址
> （形如 `http://127.0.0.1:3080/?token=xxxx`）。外壳会自动捕获这一行并直接用它导航，
> 无需手工复制。

---

## 快速开始

### 1. 克隆项目

```powershell
git clone https://github.com/ludan0312/deepseek-harness-desktop.git
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
4. 自动检测并启动 DSH，捕获 token 地址后载入主界面

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
| 切换关闭行为 | 托盘右键菜单 -> 勾选/取消「关闭时最小化到托盘」 |
| 重启 DSH 服务 | 托盘右键菜单 -> "重启 DSH 服务"（会重新提取 token 并刷新地址；若 3080 被外部实例占用会先结束该实例） |
| 更改 DSH 路径 | 托盘右键菜单 -> "更改 DSH 路径" |
| 完全退出 | 托盘右键菜单 -> "退出" |

### 关闭行为开关

首次关闭窗口时会弹一次确认框，选择结果会被记住。之后可随时在托盘右键菜单中
勾选或取消「关闭时最小化到托盘」：

- **勾选**（`close_action = "tray"`）：点击 X 隐藏窗口，DSH 继续在后台运行；
- **取消**（`close_action = "exit"`）：点击 X 结束 DSH 进程并退出应用。

也就是说，即使之前选择了"直接退出"，也能从这里恢复托盘行为。

---

## token 认证适配（DSH 0.1.6+）

DSH 0.1.6 起 `dsh web` 默认要求认证，用不带 token 的旧地址打开会提示
`authentication required`。以下结论来自阅读 DSH 源码
（`packages/client/connection/src/browser-auth.ts`）：

| 事实 | 源码依据 |
|------|---------|
| 访问地址 = `baseUrl` + `?token=<launchToken>` | `authenticatedUrl()` |
| `launchToken` 是**进程内随机值**，只存在于内存 | `PROCESS_LAUNCH_TOKENS` WeakMap，从不落盘 |
| 带正确 token 的 `GET /` → **303 跳转 `/`** 并下发认证 Cookie | `authorizeIndex()` |
| Cookie 默认有效期 **30 天**，签名密钥持久化在 `~/.dsh` | `cookieMaxAgeDays` 默认 30；`modifyRecord` 原子写入 |
| 无 token 但带有效 Cookie → 直接放行 | `authorizeIndex()` |
| 两者都没有 → 401 | `writeUnauthorized()` |

**因此 token 只需交换一次**：外壳捕获到启动日志里的完整 URL 并导航一次，
浏览器就会存下 30 天有效的 Cookie；此后即使 DSH 重启（`launchToken` 变化），
也能直接用裸地址访问。

外壳的处理方式：

1. **捕获**：启动/重启 DSH 时以 `Stdio::piped()` 采集子进程 stdout 与 stderr，
   逐行匹配形如 `dsh web: http://127.0.0.1:3080/?token=xxxx (LAN: ...)` 的地址
   （Windows 下 `CREATE_NO_WINDOW` 只隐藏控制台窗口，不影响管道读取）；
2. **导航**：优先使用捕获到的完整地址，由 Rust 侧 `load_dsh_ui()` 统一负责；
3. **外部实例**：外壳启动时若 3080 已被外部 DSH 实例占用，先用裸地址尝试导航
   （已有 Cookie 则直接进入）；若页面返回 401，则**自动结束外部实例、
   由外壳重新拉起 DSH 并捕获 token**（v1.1 起，无需手动干预）；
4. **重启**：托盘「重启 DSH 服务」会作废旧 token、重新捕获并刷新窗口地址；
   若端口被外部实例占用会先结束该实例再启动；
5. **不落盘**：token 只用于导航，**不写入 `config.json`**（它本身是内存值，也不该被持久化）。

### Windows 上的 Corepack 坑（重要）

若 `dsh web` 由 Corepack 代理启动，Corepack 在需要切换到项目 `packageManager`
指定的 pnpm 版本时会**交互式询问**。外壳以无控制台方式启动子进程（`CREATE_NO_WINDOW`
+ `stdin=null`），该询问永远无人应答，Corepack 便拒绝切换、退回自带版本，
进而触发 pnpm 版本守卫报错并以码 1 退出 —— 现象是「DSH 起不来且没有任何输出」。

外壳已为子进程设置 `COREPACK_ENABLE_DOWNLOAD_PROMPT=0` 规避此问题，
并在启动前用 `dsh web --help` 逐个探测可用的 pnpm 入口（不盲信 PATH）。

### 诊断日志

外壳以 GUI 方式运行（无控制台），因此所有关键事件都会写盘：

| 文件 | 内容 |
|------|------|
| `%APPDATA%\dsh-tauri-wrapper\wrapper.log` | 启动流程、pnpm 解析结果、端口占用清理、失败原因 |
| `%APPDATA%\dsh-tauri-wrapper\ds\log` | DSH 子进程的原始输出（stdout 与 stderr 合并） |

启动失败时窗口内会直接显示原因与这两个文件的路径，而不是跳转到浏览器的错误页。

---

## 配置说明

### 修改默认 DSH 路径（硬编码默认值）

编辑 `src-tauri/src/main.rs`：

```rust
const DEFAULT_DSH_DIR: &str = r"D:\DeepSeekHarness\deepseek-harness";
```

### 修改 DSH 端口

编辑 `src-tauri/src/main.rs`：

```rust
const DSH_BASE_URL: &str = "http://127.0.0.1:3080";
```

> 注意：改端口需要同步改 `kill_port_occupant()` 里的 `:3080` 端口标记。

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

```json
{
  "dsh_dir": "D:\\DeepSeekHarness\\deepseek-harness",
  "close_action": "tray"
}
```

> `close_action` 取值 `"tray"` 或 `"exit"`；token 地址不在其中（见上文"不落盘"）。

---

## 目录结构

```
deepseek-harness-desktop/
+-- index.html              # 前端入口（仅加载动画，不做探测/跳转）
+-- package.json            # Node 依赖与脚本
+-- vite.config.ts          # Vite 配置
+-- README.md               # 本文件
+-- src-tauri/              # Rust 后端代码
    +-- src/
    |   +-- main.rs         # 核心逻辑（托盘、窗口、进程管理、token 捕获、401 接管）
    +-- icons/              # 应用图标
    +-- Cargo.toml          # Rust 依赖
    +-- tauri.conf.json     # Tauri 配置
    +-- build.rs            # 构建脚本
```

---

## 升级策略

### 升级 DSH（官方 OTA）

**完全不影响包装器！**

```powershell
cd D:\DeepSeekHarness\deepseek-harness
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
| **reqwest** | HTTP 客户端（检测 DSH 状态，仅用 async API） |
| **tokio** | 异步运行时 + 子进程输出逐行读取 |
| **regex** | 从启动日志提取带 token 的访问地址 |
| **rfd** | 原生文件选择对话框 |
| **dirs** | 跨平台配置目录 |

---

## 常见问题

### Q1: 启动时提示 "DSH 启动超时"

**原因**：DSH 路径配置错误、DSH 未构建，或 pnpm/Corepack 无法启动 `dsh web`。

**解决**：
1. 先看窗口内提示的日志路径，`wrapper.log` 与 `ds/log` 会直接写明失败原因；
2. 右键托盘菜单 -> "更改 DSH 路径" 重新选择；
3. 确保 DSH 目录已执行 `pnpm install && pnpm run build`；
4. 手动测试：`cd DSH_DIR && pnpm dsh web` 能否正常启动。

### Q2: 窗口显示 "authentication required"

**原因**：DSH 0.1.6+ 启用了 token 认证，而本次没有可用凭证——
常见于 DSH 在包装器启动前就已由其它方式运行（其进程内 token 无法被外部获取），
且 WebView 中还没有此前交换得到的认证 Cookie。

**解决**（v1.1 起全自动，正常情况下无需手动操作）：
1. 外壳检测到外部实例时会先用裸地址尝试导航；若页面是 401，
   **会在数秒内自动结束外部实例并重启 DSH**，重启后自动捕获带 token 的地址完成登录；
2. 若自动接管失败（如端口清理超时），托盘右键菜单 -> "重启 DSH 服务" 手动触发同一流程；
3. 正常情况下只需成功进入一次：认证 Cookie 有效期 30 天，之后即使 DSH 重启也能直接进入；
4. 若仍失败，手动执行 `pnpm dsh web`，从控制台复制完整 URL 后在系统浏览器中使用；
5. 悬停托盘图标可看到 tooltip，确认当前是否已捕获到 token 地址。

### Q3: 托盘图标不显示

**原因**：Windows 图标缓存问题。

**解决**：
1. 确保 `src-tauri/icons/` 目录有图标文件
2. 重启 Windows 资源管理器：`taskkill /f /im explorer.exe && start explorer.exe`

### Q4: 点击关闭后窗口消失但进程还在

**这是正常设计！** 当前关闭行为为"最小化到托盘"时，点击 X 只是隐藏窗口。
想改成点 X 直接退出，请在托盘右键菜单取消勾选「关闭时最小化到托盘」；
要立即退出请用托盘右键菜单 -> "退出"。

### Q5: 如何更换应用图标？

1. 准备 1024x1024 的 PNG 图标
2. 安装 Tauri CLI：`npm install -g @tauri-apps/cli`
3. 生成图标：`tauri icon /path/to/source.png`
4. 重新构建

> 若需为 macOS 打包，请补齐 `src-tauri/icons/icon.icns` 后将其加回
> `tauri.conf.json` 的 `bundle.icon` 列表（Windows-only 仓库默认未附带该文件）。

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
**最后更新**: 2026-09-21
**兼容 DSH 版本**: v0.1.5-rc.1+（含 0.1.6+ token 认证）
