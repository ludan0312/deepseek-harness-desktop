#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! DeepSeekHarness Desktop —— DSH 的桌面外壳（wrapper）
//!
//! 职责边界（务必保持）：
//!   1. 窗口生命周期（显示 / 隐藏 / 拦截关闭按钮）；
//!   2. 系统托盘菜单；
//!   3. DSH 进程的启动、监控、重启与退出清理。
//!
//! 本文件不包含任何 DSH 业务逻辑，也不修改 DSH 源码目录，
//! 因此 DSH 官方 OTA 升级路径完全不受影响。

use tauri::{
    api::dialog::ask, CustomMenuItem, Manager, SystemTray, SystemTrayEvent, SystemTrayMenu,
    SystemTrayMenuItem, WindowEvent,
};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt as _;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 不带 token 的基础地址：仅用于「服务是否已起来」的探测。
///
/// 注意：DSH 0.1.6+ 对该地址返回 401 (`dsh web authentication required`)，
/// 因此它**不能**作为最终导航地址，只能作为探测与诊断用途。
const DSH_BASE_URL: &str = "http://127.0.0.1:3080";
const CHECK_INTERVAL_MS: u64 = 1000;
/// 等待服务就绪的最长秒数。取值偏大是刻意的：首次冷启动 / 正在导出的
/// DSH 可能要几分钟，宁可多等也不要误判失败。
const STARTUP_TIMEOUT_S: u64 = 180;
/// 服务端口就绪后，额外等待 DSH 打印带 token 地址的宽限秒数。
const TOKEN_GRACE_S: u64 = 5;
/// 进程意外退出时的自动重试次数。
const MAX_RESPAWN: u32 = 1;
/// `ds/log` 中最多保留的 DSH 输出行数（仅用于失败诊断）。
const OUTPUT_LINES_CAP: usize = 200;

/// 探测单个 pnpm 候选命令的超时时间。
///
/// 取 120 秒是因为 Corepack 首次解析/安装项目指定的 pnpm 版本可能较慢；
/// 探测只在整个进程生命周期内做一次，开销可以接受。
const PNPM_PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// 已解析出的 pnpm 命令缓存（进程内只探测一次）。
static PNPM_COMMAND: StdMutex<Option<String>> = StdMutex::new(None);

/// 默认 DSH 源码目录（配置文件中没有可用路径且用户取消选择时的最后兜底）。
const DEFAULT_DSH_DIR: &str = r"D:\DeepSeekHarness\deepseek-harness";

/// 关闭行为配置值：最小化到托盘。
const CLOSE_ACTION_TRAY: &str = "tray";
/// 关闭行为配置值：直接退出应用。
const CLOSE_ACTION_EXIT: &str = "exit";

/// 托盘菜单项 id。
const MENU_ID_SHOW: &str = "show";
const MENU_ID_HIDE: &str = "hide";
const MENU_ID_CLOSE_TO_TRAY: &str = "close_to_tray";
const MENU_ID_RESTART_DSH: &str = "restart_dsh";
const MENU_ID_CHANGE_DIR: &str = "change_dir";
const MENU_ID_QUIT: &str = "quit";

/// DSH 0.1.6 起 `dsh web` 默认启用 token 认证，启动日志会打印完整访问地址，
/// 形如 `http://127.0.0.1:3080/?token=xxxx`（也可能被终端着色或加引号包裹）。
///
/// 这里宽松匹配任意 host/port，只要求「带 token 参数的 http(s) 地址」；
/// token 本体先宽松吞掉可能的结尾标点，再由 `trim_end_matches` 去掉，
/// 否则 `...?token=abc.` 这类句末标点会被当成 token 的一部分。
const TOKEN_URL_PATTERN: &str = r#"(?i)https?://[^\s"'<>`]+[?&]token=[A-Za-z0-9._~+/=%-]+"#;

/// 托盘 tooltip（提取不到 token 时用来提示用户手动复制地址）。
const TOOLTIP_BASE: &str = "DeepSeek Harness Desktop";
const TOOLTIP_TOKEN_OK: &str = "DeepSeek Harness Desktop —— 已获取带 token 的访问地址";
const TOOLTIP_TOKEN_MISSING: &str =
    "DeepSeek Harness Desktop —— 未捕获到 token 地址，请从 DSH 控制台复制最新 URL";

/// 「外部实例 401 → 自动接管」是否已触发过（每次外壳运行最多一次，
/// 防止检测循环反复重启）。
static AUTH_RECOVERY_TRIGGERED: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct AppState {
    /// DSH 子进程句柄；None 表示当前没有由本外壳托管的进程。
    dsh_process: Arc<Mutex<Option<Child>>>,
    /// DSH 源码目录（启动 `pnpm dsh web` 的工作目录）。
    dsh_dir: String,
    /// 从 DSH stdout/stderr 中提取到的带 token 访问地址。
    ///
    /// 仅用于窗口导航，**不写入 config.json**：token 是一次性凭证，
    /// 持久化会让磁盘上残留失效/可复用的凭证。
    token_url: Arc<Mutex<Option<String>>>,
    /// DSH 最近若干行输出，仅在失败/超时时落盘用于诊断。
    recent_output: Arc<Mutex<Vec<String>>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct Config {
    dsh_dir: String,
    /// "tray" = 关闭时最小化到托盘；"exit" = 关闭时直接退出。
    ///
    /// 用 Option 保持与 1.0.x 旧配置文件的兼容：字段缺失 = 尚未询问过用户。
    close_action: Option<String>,
}

// ---------------------------------------------------------------------------
// 诊断日志
//
// GUI 程序（windows_subsystem = "windows"）没有控制台，println/eprintln 全部丢失，
// 出问题时用户完全看不到原因。这里把所有关键事件与 DSH 输出落到磁盘。
// ---------------------------------------------------------------------------

static LOG_INITIALIZED: AtomicBool = AtomicBool::new(false);
static LOG_LOCK: StdMutex<()> = StdMutex::new(());

/// 外壳自身的运行日志：`%APPDATA%\dsh-tauri-wrapper\wrapper.log`
fn get_wrapper_log_path() -> PathBuf {
    config_dir().join("wrapper.log")
}

/// DSH 子进程的输出日志：`%APPDATA%\dsh-tauri-wrapper\ds/log`
fn get_dsh_log_path() -> PathBuf {
    config_dir().join("ds").join("log")
}

fn config_dir() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("dsh-tauri-wrapper");
    path
}

fn ensure_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
}

fn timestamp() -> String {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("[{secs}]")
}

/// 追加一行到外壳日志（进程内首次调用时截断旧日志）。
fn log_line(msg: &str) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = get_wrapper_log_path();
    ensure_parent_dir(&path);

    let is_first = !LOG_INITIALIZED.swap(true, Ordering::SeqCst);
    let mut opts = OpenOptions::new();
    opts.create(true).write(true);
    if is_first {
        opts.truncate(true);
    } else {
        opts.append(true);
    }

    if let Ok(mut file) = opts.open(&path) {
        let _ = writeln!(file, "{} {}", timestamp(), msg);
    }
}

/// 追加一行到 DSH 输出日志（逐行写入，便于崩溃后仍能看到最后输出）。
fn append_dsh_log(line: &str) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = get_dsh_log_path();
    ensure_parent_dir(&path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{}", line);
    }
}

/// 把最近缓冲的 DSH 输出批量写入 `ds/log`（诊断失败原因时调用一次）。
fn flush_recent_output(state: &AppState) {
    let lines = match state.recent_output.try_lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return,
    };
    if lines.is_empty() {
        log_line("DSH 输出为空（子进程可能根本没有启动成功）");
        return;
    }
    let path = get_dsh_log_path();
    ensure_parent_dir(&path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "--- 最近 {} 行 DSH 输出 ---", lines.len());
        for line in &lines {
            let _ = writeln!(file, "{}", line);
        }
    }
    log_line(&format!("已导出最近 {} 行 DSH 输出到 ds/log", lines.len()));
}

/// 记录一条 DSH 输出：同时进入内存环形缓冲与 `ds/log` 文件。
async fn record_dsh_output(state: &AppState, line: &str) {
    {
        let mut buf = state.recent_output.lock().await;
        if buf.len() >= OUTPUT_LINES_CAP {
            buf.remove(0);
        }
        buf.push(line.to_string());
    }
    append_dsh_log(line);
}

// ---------------------------------------------------------------------------
// 配置读写
// ---------------------------------------------------------------------------

fn get_config_path() -> PathBuf {
    config_dir().join("config.json")
}

fn load_config() -> Option<Config> {
    let path = get_config_path();
    if path.exists() {
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    } else {
        None
    }
}

fn save_config(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let path = get_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(config)?;
    fs::write(path, content)?;
    Ok(())
}

/// 读取当前关闭行为，缺省视为「最小化到托盘」。
fn current_close_action() -> String {
    load_config()
        .and_then(|c| c.close_action)
        .unwrap_or_else(|| CLOSE_ACTION_TRAY.to_string())
}

/// 仅更新关闭行为，其余字段（尤其是 dsh_dir）原样保留。
fn update_close_action(action: &str) {
    let mut cfg = load_config().unwrap_or_default();
    cfg.close_action = Some(action.to_string());
    let _ = save_config(&cfg);
}

/// 仅更新 DSH 源码目录，**完整保留**已有设置（如 close_action）。
///
/// 修复 1.0.x 的缺陷：旧实现用 `Config { ..Default::default() }` 重建配置，
/// 会把用户在关闭确认框里做出的选择一并清空。
fn update_dsh_dir(dir: &str) {
    let mut cfg = load_config().unwrap_or_default();
    cfg.dsh_dir = dir.to_string();
    let _ = save_config(&cfg);
}

/// 解析 DSH 源码目录：配置文件 → 硬编码默认值 → 原生目录选择框。
fn ensure_dsh_dir() -> String {
    if let Some(config) = load_config() {
        if !config.dsh_dir.is_empty() && std::path::Path::new(&config.dsh_dir).exists() {
            return config.dsh_dir;
        }
    }

    if std::path::Path::new(DEFAULT_DSH_DIR).exists() {
        update_dsh_dir(DEFAULT_DSH_DIR);
        return DEFAULT_DSH_DIR.to_string();
    }

    let selected = rfd::FileDialog::new()
        .set_title("请选择 DSH 源码目录")
        .pick_folder();

    match selected {
        Some(path) => {
            let path_str = path.to_string_lossy().to_string();
            update_dsh_dir(&path_str);
            path_str
        }
        None => DEFAULT_DSH_DIR.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 进程与导航
// ---------------------------------------------------------------------------

/// 从一行日志里提取带 token 的访问地址。
///
/// 返回 owned String，避免把正则捕获借用传出函数体。
/// 结尾的句号、逗号、右括号等标点会被裁掉（日志里 token 后常直接跟标点）。
fn parse_token_url(line: &str) -> Option<String> {
    let re = regex::Regex::new(TOKEN_URL_PATTERN).ok()?;
    re.find(line)
        .map(|m| m.as_str().trim_end_matches(['.', ',', ';', ')', ']', '}']).to_string())
}

/// DSH 子进程是否仍在运行；返回 Some(状态) 表示已退出。
async fn process_exited(state: &AppState) -> Option<std::process::ExitStatus> {
    let mut guard = state.dsh_process.lock().await;
    match guard.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(Some(status)) => Some(status),
            // try_wait 自身出错时保守地当作仍在运行，避免误杀正常启动。
            _ => None,
        },
        // 句柄已经被取走（正在退出/重启），不视为「意外退出」。
        None => None,
    }
}

/// 读取当前应使用的导航地址；没有 token 时回退到基础地址。
async fn current_nav_url(state: &AppState) -> String {
    state
        .token_url
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| DSH_BASE_URL.to_string())
}

/// 更新托盘 tooltip，让用户在提取不到 token 时知道该从哪里复制地址。
fn refresh_tooltip(app_handle: &tauri::AppHandle, has_token: bool) {
    let text = if has_token {
        TOOLTIP_TOKEN_OK
    } else {
        TOOLTIP_TOKEN_MISSING
    };
    let _ = app_handle.tray_handle().set_tooltip(text);
}

/// 杀掉进程及其子进程（Windows 用 taskkill /T 杀整棵进程树）。
///
/// `child.wait()` 是阻塞调用，统一放到独立线程里执行，避免在 tokio
/// 工作线程上阻塞（本函数在持有 dsh_process 锁时被调用，更不能在异步上下文里做阻塞等待）。
fn kill_process_tree(mut child: Child) {
    // tokio 的 Child::id() 返回 Option<u32>，进程已被回收时拿不到 pid。
    let pid = child.id();
    #[cfg(target_os = "windows")]
    {
        if let Some(pid) = pid {
            let _ = std::process::Command::new("taskkill")
                .args(&["/T", "/F", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = child.start_kill();
    }

    // 等待回收，防止留下僵尸进程；阻塞操作交给专用线程。
    let _ = std::thread::Builder::new()
        .name("dsh-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
}

/// 按需结束旧的 DSH 进程（先把进程句柄从锁里取出来，再在锁外等待）。
async fn kill_existing_process(state: &AppState) {
    let old = {
        let mut guard = state.dsh_process.lock().await;
        guard.take()
    };
    if let Some(child) = old {
        kill_process_tree(child);
    }
}

/// 结束占用 DSH 端口的外部进程（非本外壳 spawn 的实例）。
///
/// 场景：外壳启动时发现 3080 已被外部实例占用、或托盘「重启 DSH 服务」
/// 时旧实例不是本外壳的子进程——这些实例不在 `dsh_process` 句柄里，
/// 必须通过端口反查 PID 才能结束，否则新 DSH 会因 EADDRINUSE 秒退。
#[cfg(target_os = "windows")]
fn kill_port_occupant() {
    let port_tag = ":3080";
    let output = match std::process::Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log_line(&format!("netstat 查询端口占用失败: {e}"));
            return;
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut pids: Vec<u32> = Vec::new();
    for line in text.lines() {
        // 形如: TCP    127.0.0.1:3080    0.0.0.0:0    LISTENING    37316
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 5
            && cols[1].ends_with(port_tag)
            && cols[3] == "LISTENING"
        {
            if let Ok(pid) = cols[4].parse::<u32>() {
                // pid 0/4 是系统空闲/System 进程，绝不能碰。
                if pid > 4 && !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
        }
    }
    for pid in pids {
        log_line(&format!("结束占用 3080 的外部进程 pid={pid}"));
        match std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(mut child) => {
                // 等待 taskkill 结果：失败（典型为"拒绝访问"——外部实例以更高
                // 权限运行）必须落日志，否则端口释放超时后无从判断原因。
                let _ = std::thread::spawn(move || match child.wait() {
                    Ok(status) if status.success() => {
                        log_line(&format!("taskkill pid={pid} 成功"));
                    }
                    Ok(status) => {
                        log_line(&format!(
                            "taskkill pid={pid} 退出码={status}（可能被拒绝访问：外部实例权限高于外壳）"
                        ));
                    }
                    Err(e) => log_line(&format!("taskkill pid={pid} 等待失败: {e}")),
                });
            }
            Err(e) => log_line(&format!("taskkill pid={pid} 启动失败: {e}")),
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn kill_port_occupant() {
    // 非 Windows 平台暂不实现端口反查；外部实例场景请手动结束后再启动外壳。
    log_line("非 Windows 平台未实现端口占用清理，如启动失败请手动结束占用 3080 的进程");
}

/// 3080 是否有进程在监听（原始 TCP 连接探测）。
///
/// 用途是判断"端口是否被占用"，不是"服务是否就绪"——HTTP 探测在目标进程
/// 濒死时结果飘忽（半死 socket 可能接受连接又无法完成响应），且耗时不可控；
/// TCP connect 由内核直接裁决，毫秒级、语义准确。
fn is_port_listening() -> bool {
    std::net::TcpStream::connect(("127.0.0.1", 3080)).is_ok()
}

/// 等待 DSH 端口释放（taskkill 是异步的，端口不会立刻空出来）。
/// 返回 true 表示端口已空；超时仍被占用返回 false，调用方应中止启动。
///
/// 循环内一旦探测到释放立即信任并返回，不做二次复核——复核用的是同一个
/// 探测，在进程濒死窗口内前后结果可能相反，只会把"已释放"误判成"超时"。
async fn wait_port_free(max_secs: u64) -> bool {
    let mut waited: u64 = 0;
    while waited < max_secs {
        if !is_port_listening() {
            log_line("3080 端口已释放");
            return true;
        }
        sleep(Duration::from_millis(CHECK_INTERVAL_MS)).await;
        waited += 1;
    }
    log_line(&format!("等待 3080 端口释放超时（>{max_secs}s）"));
    false
}

/// 后台逐行读取 DSH 输出（stdout 与 stderr 共用），捕获带 token 的访问地址。
///
/// 说明：Windows 下 `CREATE_NO_WINDOW` 只影响控制台窗口的创建，
/// **不影响**管道读取；关键是启动时用 `Stdio::piped()`
/// 而不是 `Stdio::inherit()`，否则输出会被父进程直接吞掉、无法捕获。
/// stderr 也一并捕获：部分 CLI 会把访问地址打到 stderr，
/// 只接 stdout 会漏掉 token。
fn spawn_output_token_reader<R>(
    stream: R,
    state: AppState,
    app_handle: tauri::AppHandle,
    source: &'static str,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    // 原始输出全部落盘/入缓冲，便于事后诊断启动失败。
                    record_dsh_output(&state, &line).await;

                    if let Some(url) = parse_token_url(&line) {
                        {
                            let mut guard = state.token_url.lock().await;
                            *guard = Some(url.clone());
                        }
                        log_line(&format!("已捕获带 token 的访问地址（来自 {source}）"));
                        refresh_tooltip(&app_handle, true);
                    }
                }
                // 管道 EOF：该输出流已关闭（进程退出或关闭了对应流）。
                Ok(None) => break,
                Err(e) => {
                    log_line(&format!("读取 DSH {source} 失败: {e}"));
                    break;
                }
            }
        }
    });
}

/// 探测单个候选 pnpm 是否真的能跑通 `dsh web`。
///
/// 只测 `pnpm --version` 是不够的：那只证明 pnpm 本身能启动，
/// 并不能证明 `dsh web` 子命令可用（Corepack 的版本校验、项目脚本解析、
/// 以及 tsx 运行时都可能在更后面才失败）。
/// 因此这里用 `dsh web --help` 做端到端探测——它会在真正拉起服务之前退出。
async fn probe_pnpm(candidate: &str, dsh_dir: &str) -> bool {
    let mut cmd = Command::new(candidate);
    cmd.arg("dsh")
        .arg("web")
        .arg("--help")
        // 必须在 DSH 目录里探测：`dsh` 是该目录下的 workspace 命令，
        // 在 wrapper 自己的目录里跑会因找不到命令而秒退（假阴性）。
        .current_dir(dsh_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    apply_process_env(&mut cmd);

    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    // stdout/stderr 均为 null：只关心退出码，不需要输出，
    // 同时也避免管道缓冲区被写满而阻塞子进程（探测命令理论上也会长期运行）。
    match cmd.spawn() {
        Ok(mut child) => match tokio::time::timeout(PNPM_PROBE_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => status.success(),
            _ => false,
        },
        Err(_) => false,
    }
}

/// 为 DSH 子进程注入稳定的运行环境。
///
/// 关键是 `COREPACK_ENABLE_DOWNLOAD_PROMPT=0`：
/// Corepack 需要切换到项目 `packageManager` 指定的 pnpm 版本时，默认会**交互式询问**；
/// 而本外壳以无控制台方式（CREATE_NO_WINDOW + stdin=null）启动子进程，
/// 这个询问永远得不到回答，Corepack 便会拒绝切换、退回自带版本，
/// 进而触发 pnpm 的版本守卫报错并以码 1 退出——现象就是「DSH 起不来且没有任何输出」。
/// 关闭下载提示后，Corepack 直接使用其缓存中正确的 pnpm 版本。
///
/// 注意：**不要**覆盖 `COREPACK_HOME`。`C:\Program Files\nodejs\node_modules\corepack`
/// 通常不可写，把它当作 COREPACK_HOME 会让 Corepack 在 mkdir 时直接 EPERM 失败。
/// 使用 Corepack 自己的默认缓存目录即可。
fn apply_process_env(cmd: &mut Command) {
    cmd.env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0");
}

/// pnpm 探测结果的磁盘缓存路径。
///
/// 为什么需要：探测一个候选要实际跑一遍 `pnpm dsh web --help`，
/// 等于把 tsx 冷启动整个 CLI 再执行一次（数秒）。每次外壳启动都探测纯属浪费，
/// 因此把首个成功结果持久化，后续启动直接读文件。
fn pnpm_command_cache_path() -> PathBuf {
    config_dir().join("pnpm-command.txt")
}

/// 解析出一个真正可用的 pnpm 命令。
///
/// 候选顺序：npm 全局安装的真实 pnpm 优先，其后才是裸 `pnpm.cmd`（PATH 解析）。
/// 之所以不直接信任 PATH：某些环境下 PATH 上的 pnpm 是损坏的 corepack 垫片
/// （指向不存在的 corepack/dist/pnpm.js），会瞬时失败且没有任何输出。
async fn resolve_pnpm_command(dsh_dir: &str) -> String {
    if let Ok(cached) = PNPM_COMMAND.lock() {
        if let Some(known) = cached.as_ref() {
            return known.clone();
        }
    }

    // 磁盘缓存命中且候选仍然可用（文件路径需存在）时，跳过探测。
    if let Ok(content) = fs::read_to_string(pnpm_command_cache_path()) {
        let cached = content.trim().to_string();
        let looks_usable = !cached.is_empty()
            && (!cached.contains(std::path::MAIN_SEPARATOR)
                || std::path::Path::new(&cached).exists());
        if looks_usable {
            log_line(&format!("使用缓存的 pnpm: {cached}"));
            if let Ok(mut guard) = PNPM_COMMAND.lock() {
                *guard = Some(cached.clone());
            }
            return cached;
        }
    }

    let mut candidates: Vec<String> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            candidates.push(format!(r"{appdata}\npm\pnpm.cmd"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(format!(r"{pf}\nodejs\pnpm.CMD"));
        }
        candidates.push("pnpm.cmd".to_string());
    }
    #[cfg(not(target_os = "windows"))]
    {
        candidates.push("pnpm".to_string());
    }

    for candidate in &candidates {
        if probe_pnpm(candidate, dsh_dir).await {
            log_line(&format!("使用 pnpm: {candidate}"));
            if let Ok(mut cached) = PNPM_COMMAND.lock() {
                *cached = Some(candidate.clone());
            }
            // 持久化探测结果，下次启动直接读缓存（探测本身要冷启动一遍 tsx，很贵）。
            let _ = fs::write(pnpm_command_cache_path(), candidate.as_bytes());
            return candidate.clone();
        }
        log_line(&format!("pnpm 候选不可用，跳过: {candidate}"));
    }

    // 全都探测失败：仍返回默认名字，让后续启动失败走统一的错误提示与日志。
    log_line("未找到可用的 pnpm，将尝试默认命令名");
    let fallback = default_pnpm_name().to_string();
    if let Ok(mut cached) = PNPM_COMMAND.lock() {
        *cached = Some(fallback.clone());
    }
    fallback
}

fn default_pnpm_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "pnpm.cmd"
    } else {
        "pnpm"
    }
}

/// 启动 DSH：先清理旧进程（并作废旧 token），再拉起新进程并开始捕获输出。
/// 返回是否成功 spawn（不代表服务已经就绪）。
async fn start_dsh_process(state: &AppState, app_handle: &tauri::AppHandle) -> bool {
    kill_existing_process(state).await;

    // 旧 token 随旧进程一起作废，避免重启后仍用失效地址导航。
    {
        let mut guard = state.token_url.lock().await;
        *guard = None;
    }

    // 端口若仍被占用，说明是「外部实例」（非本外壳 spawn，句柄不在
    // dsh_process 里，kill_existing_process 杀不到）。必须先结束占用者，
    // 否则新 DSH 会因 EADDRINUSE 秒退。
    //
    // 触发场景：a) 外壳启动时检测到外部实例且页面返回 401，自动接管重启；
    //           b) 托盘「重启 DSH 服务」时 DSH 由外部启动。
    if is_port_listening() {
        log_line("3080 端口被外部实例占用，结束占用进程后重试");
        kill_port_occupant();
        // 端口释放实测可能需要 15 秒以上（进程树逐个退出 + socket 收尾）。
        if !wait_port_free(30).await {
            // 端口未能释放（外部实例权限更高、杀不掉）：中止启动。
            // 绝不能硬闯——新 DSH 会因 EADDRINUSE 秒退，外壳还会把死进程的
            // token 拿去导航，用户只会看到连环 401。
            log_line("3080 端口释放失败，放弃本次启动");
            return false;
        }
    }

    let pnpm = resolve_pnpm_command(&state.dsh_dir).await;
    let mut cmd = Command::new(&pnpm);
    cmd.arg("dsh")
        .arg("web")
        // --no-open：阻止 DSH 自行拉起系统默认浏览器（界面由本外壳的窗口承载）。
        // 该标志不影响 printUrl，带 token 的地址仍会照常打印到 stdout 供我们捕获。
        .arg("--no-open")
        .current_dir(&state.dsh_dir)
        .stdin(Stdio::null())
        // stdout/stderr 必须是 piped：既捕获启动日志中的 token 地址，
        // 又避免继承父进程控制台。
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // 注入 Corepack 环境（见 apply_process_env 的说明，这是启动成功的关键）。
    apply_process_env(&mut cmd);

    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    match cmd.spawn() {
        Ok(mut child) => {
            let pid = child.id().unwrap_or(0);
            if let Some(stdout) = child.stdout.take() {
                spawn_output_token_reader(stdout, state.clone(), app_handle.clone(), "stdout");
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_output_token_reader(stderr, state.clone(), app_handle.clone(), "stderr");
            }
            let mut guard = state.dsh_process.lock().await;
            *guard = Some(child);
            log_line(&format!(
                "已启动 DSH: {pnpm} dsh web --no-open（pid={pid}, cwd={}）",
                state.dsh_dir
            ));
            true
        }
        Err(e) => {
            log_line(&format!("启动 DSH 失败: {e}"));
            false
        }
    }
}

/// 探测 DSH HTTP 服务是否已经响应。
///
/// 只要拿到任何 HTTP 响应（含 401 "authentication required"）就说明服务已起来；
/// 用 `is_success()` 会在启用认证时误判为未就绪。
///
/// 必须带超时：目标进程濒死/刚被 taskkill 时，TCP 连接可能长时间不返回，
/// reqwest 默认无超时会把这个探测卡死数秒到数十秒（实测把 15 秒的端口
/// 释放等待拖成了 66 秒）。
async fn is_dsh_running() -> bool {
    tokio::time::timeout(Duration::from_secs(3), reqwest::get(DSH_BASE_URL))
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
}

/// 是否已经捕获到带 token 的访问地址。
async fn has_token(state: &AppState) -> bool {
    state.token_url.lock().await.is_some()
}

/// 唯一的窗口导航出口：外壳负责所有导航，前端 index.html 不再改写 location。
async fn load_dsh_ui(app_handle: &tauri::AppHandle, state: &AppState) {
    let url = current_nav_url(state).await;
    let window = app_handle.get_window("main").unwrap();
    log_line(&format!("导航到: {url}"));
    let _ = window.eval(&format!("window.location.href = '{}'", url));
    let _ = window.show();
    let _ = window.set_focus();
}

/// 用无凭证的 HTTP 请求判断 DSH 是否开启了 token 认证。
///
/// 为什么不用 WebView 页面检测（eval + document.title 标记）：
/// 实测 Tauri 1.x 的 `Window::title()` 返回的是窗口配置标题，
/// 不跟踪 document.title，标记数据根本读不回来。
/// 而 reqwest 直连检测完全不依赖 WebView 状态（页面是否加载、是否导航成功
/// 都不影响判定），是唯一稳定的通道。
///
/// 必须带浏览器导航头：实测 DSH 按 `Accept` 区分请求类型——
/// `Accept: */*`（reqwest 默认值）会被当成 API 请求放行（返回非 401），
/// 只有 `Accept: text/html...` 才走文档认证拦截。不带头会漏判。
/// 另加正文兜底：状态非 401 但 Content-Type 为 text/plain 且正文包含
/// "authentication required" 时同样视为需要认证（防御 DSH 行为变化）。
///
/// 已知取舍：若 WebView 里已存有效的 30 天认证 Cookie，裸导航其实能进，
/// 但 reqwest 无 Cookie 仍会看到 401，此时会"误重启"一次外部实例。
/// 该场景仅在外壳曾成功登录过、之后又出现外部实例时发生，代价可接受。
/// 无凭证认证探测的三态结果。
enum AuthProbe {
    /// 明确返回 401（或 401 文案）→ 需要 token 认证。
    Required,
    /// 拿到正常响应且不是 401 → 未启用认证。
    NotRequired,
    /// 超时 / 连接被拒 / 读失败 → 无法判定。
    ///
    /// 典型场景：「僵尸」外部实例——父进程已死、stdout 管道断裂，
    /// TCP 握手由内核受理（端口显示占用），但 HTTP 请求永远无响应。
    /// 绝不能把这种状态误读成"未要求认证"而让着它，否则用户只能看到
    /// 转圈/拒绝连接页面。
    Inconclusive,
}

async fn probe_auth_requirement() -> AuthProbe {
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return AuthProbe::Inconclusive,
    };
    let resp = client
        .get(DSH_BASE_URL)
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        )
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Sec-Fetch-User", "?1")
        .header("Upgrade-Insecure-Requests", "1")
        .send()
        .await;

    match resp {
        Ok(r) => {
            if r.status() == reqwest::StatusCode::UNAUTHORIZED {
                return AuthProbe::Required;
            }
            let is_plain = r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|ct| ct.starts_with("text/plain"))
                .unwrap_or(false);
            if is_plain {
                return match r.text().await {
                    Ok(body) if body.contains("authentication required") => AuthProbe::Required,
                    Ok(_) => AuthProbe::NotRequired,
                    Err(_) => AuthProbe::Inconclusive,
                };
            }
            AuthProbe::NotRequired
        }
        Err(_) => AuthProbe::Inconclusive,
    }
}

/// 外壳启动时检测到「外部 DSH 实例」后的兜底流程。
///
/// 背景（读 DSH 源码 `browser-auth.ts` 得出，非猜测）：
///   launchToken 只存在于 DSH 进程内存，外部实例的 token 外壳永远拿不到；
///   但带正确 token 访问一次会换取 30 天认证 Cookie，之后裸地址即可放行。
/// 因此策略是「先礼后兵」：
///   1. 立即用裸地址导航——若 WebView 里已有 30 天认证 Cookie 则直接进入；
///   2. 同时用 reqwest 无凭证探测裸地址（见 bare_url_requires_auth）：
///      返回 401 说明无凭证路径被堵，自动重启接管
///      （start_dsh_process 会结束外部实例、自己拉起并捕获 token）。
fn schedule_external_instance_auth_check(state: AppState, app_handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // 三次机会：立即 / +6s / +14s。无法判定累计两次 → 按无主实例接管。
        let mut inconclusive_streak: u8 = 0;
        for extra_wait in [0u64, 6u64, 8u64] {
            sleep(Duration::from_secs(extra_wait)).await;

            if AUTH_RECOVERY_TRIGGERED.load(Ordering::SeqCst) {
                return;
            }
            if !is_port_listening() {
                // 让着的外部实例已消失（可能是被安装器/双击产生的瞬时进程）：
                // 不再傻等，改为自己拉起 DSH。
                log_line("接管检查: 外部实例已消失，改为自行启动 DSH");
                check_and_start_dsh(state, app_handle, false).await;
                return;
            }
            match probe_auth_requirement().await {
                AuthProbe::Required => {
                    AUTH_RECOVERY_TRIGGERED.store(true, Ordering::SeqCst);
                    log_line("外部 DSH 实例要求 token 认证（裸地址 401），自动重启接管");
                    show_startup_error(
                        &app_handle,
                        "检测到 DSH 正在由外部实例运行，且需要 token 认证。<br>\
                         正在自动重启 DSH 服务以完成登录（外部实例将被结束）…",
                    );
                    restart_dsh_service(state, app_handle).await;
                    return;
                }
                AuthProbe::NotRequired => {
                    log_line("接管检查: 服务确认未要求认证，保持让着外部实例");
                    return;
                }
                AuthProbe::Inconclusive => {
                    inconclusive_streak += 1;
                    log_line(&format!(
                        "接管检查: 探测无响应（连续 {inconclusive_streak}/2），疑似无主实例"
                    ));
                    if inconclusive_streak >= 2 {
                        // 端口占着但连续两次探测均无响应：典型的僵尸外部实例，
                        // 继续让着它用户只能看到转圈/拒绝连接，不如接管重启。
                        AUTH_RECOVERY_TRIGGERED.store(true, Ordering::SeqCst);
                        log_line("接管检查: 按无主实例接管重启");
                        show_startup_error(
                            &app_handle,
                            "检测到 3080 上的实例持续无响应（疑似残留进程），<br>\
                             正在自动重启 DSH 服务…",
                        );
                        restart_dsh_service(state, app_handle).await;
                        return;
                    }
                }
            }
        }
    });
}

/// 启动失败时在窗口内显示中文说明与日志位置。
///
/// 关键点：**绝不**把窗口导航到 `DSH_BASE_URL`——DSH 0.1.6+ 对该地址返回 401，
/// 用户只会看到浏览器的 "无法访问此页面 / 拒绝连接" 或 "authentication required"，
/// 完全不知道发生了什么。停在加载页并显示原因，比跳转到失败页面有用得多。
fn show_startup_error(app_handle: &tauri::AppHandle, message: &str) {
    let window = match app_handle.get_window("main") {
        Some(w) => w,
        None => return,
    };
    let _ = window.show();
    let _ = window.set_focus();

    // 路径里的反斜杠在 JS 字符串字面量中会被当转义符吞掉，需先转义。
    let log_path = get_wrapper_log_path().display().to_string().replace('\\', "\\\\");
    let dsh_log_path = get_dsh_log_path().display().to_string().replace('\\', "\\\\");
    let script = format!(
        r#"(() => {{
  document.title = 'DSH 启动失败 — DeepSeekHarness';
  const old = document.getElementById('dsh-startup-error');
  if (old) {{ old.remove(); }}
  const box = document.createElement('div');
  box.id = 'dsh-startup-error';
  box.style.cssText = 'position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);max-width:640px;width:88%;background:#1b1b33;border:1px solid #3a3a5c;border-radius:12px;padding:24px 28px;color:#e0e0e0;font-family:system-ui,sans-serif;font-size:13px;line-height:1.8;box-shadow:0 12px 40px rgba(0,0,0,.5);z-index:99999;';
  box.innerHTML = '<div style="font-size:17px;font-weight:600;color:#ff8a8a;margin-bottom:12px;">DSH 启动失败</div>'
    + '<div style="margin-bottom:12px;">{message}</div>'
    + '<div style="color:#9a9ab0;">排查日志：</div>'
    + '<div style="font-family:Consolas,monospace;font-size:12px;background:#0f0f23;border-radius:6px;padding:8px 10px;margin:6px 0 12px;word-break:break-all;user-select:text;">{log}</div>'
    + '<div style="font-family:Consolas,monospace;font-size:12px;background:#0f0f23;border-radius:6px;padding:8px 10px;margin:6px 0 12px;word-break:break-all;user-select:text;">{dshlog}</div>'
    + '<div style="color:#9a9ab0;">可尝试：托盘右键「重启 DSH 服务」（会重新捕获访问地址）。</div>';
  document.body.appendChild(box);
}})();"#,
        message = message,
        log = log_path,
        dshlog = dsh_log_path
    );
    let _ = window.eval(&script);
}

/// 等待 DSH 服务就绪的最终结果。
enum DshOutcome {
    /// 服务已响应；是否拿到了带 token 的地址。
    Ready { has_token: bool },
    /// 服务始终没有响应（超时）。
    Timeout,
    /// 子进程在服务就绪前就退出了（附退出码描述）。
    ProcessDied(String),
}

/// 等待 DSH 服务就绪。
///
/// 服务就绪后还会额外给 token 一点宽限期：HTTP 端口先于 DSH 打印启动日志恢复响应，
/// 若立即导航就会用上不带 token 的兜底地址、直接撞上 401。
/// 轮询期间同时检测子进程是否已经退出，避免白等到超时。
async fn wait_for_dsh_ready(state: &AppState) -> DshOutcome {
    let mut elapsed: u64 = 0;
    while elapsed < STARTUP_TIMEOUT_S {
        sleep(Duration::from_millis(CHECK_INTERVAL_MS)).await;
        elapsed += 1;

        if is_dsh_running().await {
            let mut grace = 0;
            while grace < TOKEN_GRACE_S && !has_token(state).await {
                // 宽限期内进程若退出，说明拿不到 token 了，不必再等。
                if let Some(status) = process_exited(state).await {
                    return DshOutcome::ProcessDied(status.to_string());
                }
                sleep(Duration::from_millis(CHECK_INTERVAL_MS)).await;
                grace += 1;
            }
            return DshOutcome::Ready {
                has_token: has_token(state).await,
            };
        }

        if let Some(status) = process_exited(state).await {
            return DshOutcome::ProcessDied(status.to_string());
        }

        if elapsed % 5 == 0 {
            log_line(&format!(
                "等待 DSH 启动... ({elapsed}/{STARTUP_TIMEOUT_S}s)"
            ));
        }
    }

    DshOutcome::Timeout
}

/// 确保 DSH 可用并把窗口导航到可访问的地址。
///
/// `force_restart`：
///   - false（启动时）：若已有实例在跑，先尝试直接导航（有 30 天 Cookie 即可进入），
///     页面若返回 401 则由 `schedule_external_instance_auth_check` 自动重启接管；
///   - true（托盘「重启 DSH 服务」）：先杀掉现有实例再重新拉起，以便重新捕获 token。
///
/// 背景（读 DSH 源码得出，非猜测）：
///   - `authenticatedUrl()` = baseUrl + `?token=<本进程随机 launchToken>`；
///   - 该 token 只存在于进程内存（`PROCESS_LAUNCH_TOKENS` WeakMap），**不落盘**；
///   - `authorizeIndex()`：带正确 token 的 `GET /` → 303 跳 `/` 并下发 30 天 Cookie；
///     无 token 但带有效 Cookie → 直接放行；两者皆无 → 401。
///   因此「捕获 token → 导航一次」即可换取长期 Cookie，之后裸地址也能访问。
async fn check_and_start_dsh(state: AppState, app_handle: tauri::AppHandle, force_restart: bool) {
    if !force_restart && is_dsh_running().await {
        log_line("检测到 DSH 已在运行（非本外壳启动），先尝试直接导航（若已有 30 天认证 Cookie 即可进入）");
        refresh_tooltip(&app_handle, false);
        load_dsh_ui(&app_handle, &state).await;
        schedule_external_instance_auth_check(state, app_handle);
        return;
    }

    log_line(if force_restart {
        "托盘触发：重启 DSH 服务"
    } else {
        "未检测到运行中的 DSH，准备启动"
    });

    let mut attempts: u32 = 0;
    loop {
        if !start_dsh_process(&state, &app_handle).await {
            show_startup_error(
                &app_handle,
                "DSH 启动失败：3080 端口被外部实例占用且无法结束。<br>\
                 若该实例是以<b>管理员身份</b>启动的，请先用管理员权限结束它，\
                 或以管理员身份运行本外壳后再试托盘「重启 DSH 服务」。",
            );
            refresh_tooltip(&app_handle, false);
            return;
        }

        match wait_for_dsh_ready(&state).await {
            DshOutcome::Ready { has_token } => {
                if has_token {
                    log_line("DSH 已就绪，使用捕获到的 token 地址导航");
                } else {
                    // 端口通了但没抓到 token：DSH 0.1.6+ 会返回 401。
                    log_line("DSH 端口已就绪，但未捕获到 token 地址（该版本可能需要 token）");
                    flush_recent_output(&state);
                }
                refresh_tooltip(&app_handle, has_token);
                load_dsh_ui(&app_handle, &state).await;
                return;
            }
            DshOutcome::ProcessDied(status) => {
                log_line(&format!("DSH 子进程在服务就绪前退出（{status}）"));
                flush_recent_output(&state);

                if attempts < MAX_RESPAWN {
                    attempts += 1;
                    log_line(&format!("尝试自动重启 DSH（第 {attempts} 次重试）"));
                    continue;
                }

                show_startup_error(
                    &app_handle,
                    &format!(
                        "DSH 进程启动后立即退出（{status}），已重试 {attempts} 次仍未成功。<br>\
                         常见原因：DSH 目录未执行 <code>pnpm install</code>，\
                         或 <code>pnpm dsh web</code> 无法正常运行。"
                    ),
                );
                refresh_tooltip(&app_handle, false);
                return;
            }
            DshOutcome::Timeout => {
                log_line(&format!("DSH 启动超时（>{STARTUP_TIMEOUT_S}s）"));
                flush_recent_output(&state);
                show_startup_error(
                    &app_handle,
                    &format!(
                        "等待 {STARTUP_TIMEOUT_S} 秒后 DSH 服务仍未响应。<br>\
                         首次冷启动或正在重建时可能较慢，可先手动在 DSH 目录执行 \
                         <code>pnpm dsh web</code> 确认能否正常启动，再回到托盘菜单重试。"
                    ),
                );
                refresh_tooltip(&app_handle, false);
                return;
            }
        }
    }
}

/// 托盘「重启 DSH 服务」：杀掉现有实例、重新捕获 token、刷新窗口地址。
///
/// 复用 `check_and_start_dsh` 的完整流程，避免两处逻辑漂移。
async fn restart_dsh_service(state: AppState, app_handle: tauri::AppHandle) {
    check_and_start_dsh(state, app_handle, true).await;
}

/// 同步托盘「关闭时最小化到托盘」勾选状态。
fn set_close_to_tray_checked(app_handle: &tauri::AppHandle, checked: bool) {
    if let Some(item) = app_handle
        .tray_handle()
        .try_get_item(MENU_ID_CLOSE_TO_TRAY)
    {
        let _ = item.set_selected(checked);
    }
}

/// 退出应用：杀掉 DSH 进程树后结束进程。
async fn quit_app(state: AppState, app_handle: tauri::AppHandle) {
    kill_existing_process(&state).await;
    app_handle.exit(0);
}

// ---------------------------------------------------------------------------
// 单实例守卫
//
// 多个外壳实例会共享同一份 config/wrapper.log，并互相残杀对方托管的 DSH
// （占 3080、互相 taskkill），因此同一机器只允许一个外壳进程。
//
// 实现：占用一个本机回环固定端口作为互斥锁——绑定成功即获得实例权，
// 进程退出时操作系统自动释放，无锁文件残留、无 pid 复用风险、零依赖。
// ---------------------------------------------------------------------------

/// 互斥锁端口（任取一个冷门端口，只绑 127.0.0.1，不对外服务）。
const SINGLE_INSTANCE_PORT: u16 = 45119;

static INSTANCE_LISTENER: StdMutex<Option<std::net::TcpListener>> = StdMutex::new(None);

fn acquire_single_instance() -> bool {
    match std::net::TcpListener::bind(("127.0.0.1", SINGLE_INSTANCE_PORT)) {
        Ok(listener) => {
            if let Ok(mut guard) = INSTANCE_LISTENER.lock() {
                *guard = Some(listener);
            }
            true
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

fn main() {
    if !acquire_single_instance() {
        // 已有实例：用原生对话框提示后退出（GUI 程序无控制台，静默退出会让
        // 用户误以为点了没反应）。
        let _ = rfd::MessageDialog::new()
            .set_title("DeepSeekHarness Desktop")
            .set_level(rfd::MessageLevel::Warning)
            .set_description(
                "已有一个外壳实例在运行（见系统托盘）。\n请先托盘右键「退出」再启动新的。",
            )
            .show();
        std::process::exit(1);
    }

    let dsh_dir = ensure_dsh_dir();

    let tray_menu = SystemTrayMenu::new()
        .add_item(CustomMenuItem::new(MENU_ID_SHOW, "显示主界面"))
        .add_item(CustomMenuItem::new(MENU_ID_HIDE, "隐藏到托盘"))
        .add_native_item(SystemTrayMenuItem::Separator)
        // 两态开关：勾选 = 关闭窗口时最小化到托盘，取消勾选 = 关闭窗口时退出。
        // 修复 1.0.x 的缺陷：close_action 一旦写入就没有入口改回来。
        .add_item(CustomMenuItem::new(
            MENU_ID_CLOSE_TO_TRAY,
            "关闭时最小化到托盘",
        ))
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(CustomMenuItem::new(MENU_ID_RESTART_DSH, "重启 DSH 服务"))
        .add_item(CustomMenuItem::new(MENU_ID_CHANGE_DIR, "更改 DSH 路径"))
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(CustomMenuItem::new(MENU_ID_QUIT, "退出"));

    let system_tray = SystemTray::new()
        .with_menu(tray_menu)
        .with_tooltip(TOOLTIP_BASE);

    tauri::Builder::default()
        .manage(AppState {
            dsh_process: Arc::new(Mutex::new(None)),
            dsh_dir: dsh_dir.clone(),
            token_url: Arc::new(Mutex::new(None)),
            recent_output: Arc::new(Mutex::new(Vec::new())),
        })
        .setup(move |app| {
            let window = app.get_window("main").unwrap();
            let state: tauri::State<AppState> = app.state();
            let app_handle = app.app_handle();

            // 让托盘勾选状态与已持久化的配置保持一致。
            set_close_to_tray_checked(&app_handle, current_close_action() != CLOSE_ACTION_EXIT);
            let state_boot = state.inner().clone();
            let app_handle_boot = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                check_and_start_dsh(state_boot, app_handle_boot, false).await;
            });

            let window_for_close = window.clone();
            let state_for_close = state.inner().clone();
            let app_handle_for_close = app_handle.clone();

            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    // 关闭按钮永不直接销毁窗口，一律由 close_action 决定行为。
                    api.prevent_close();

                    match current_close_action().as_str() {
                        CLOSE_ACTION_EXIT => {
                            let _ = window_for_close.hide();
                            let state = state_for_close.clone();
                            let app_handle = app_handle_for_close.clone();
                            tauri::async_runtime::spawn(async move {
                                quit_app(state, app_handle).await;
                            });
                        }
                        // "tray" 以及任何未知取值：最小化到托盘（最安全的默认行为）。
                        _ => {
                            let _ = window_for_close.hide();
                        }
                    }
                }
            });

            // 首次启动且从未询问过关闭行为时，弹一次确认框。
            if load_config().and_then(|c| c.close_action).is_none() {
                let state_for_ask = state.inner().clone();
                let app_handle_for_ask = app_handle.clone();
                let window_for_ask = window.clone();

                ask(
                    Some(&window_for_ask),
                    "关闭确认",
                    "请选择关闭行为：\n\n点击「是」→ 最小化到系统托盘（推荐，后台运行）\n点击「否」→ 直接退出应用\n\n（之后可在托盘右键菜单「关闭时最小化到托盘」中随时修改）",
                    move |to_tray| {
                        if to_tray {
                            update_close_action(CLOSE_ACTION_TRAY);
                            set_close_to_tray_checked(&app_handle_for_ask, true);
                        } else {
                            update_close_action(CLOSE_ACTION_EXIT);
                            set_close_to_tray_checked(&app_handle_for_ask, false);
                            let state = state_for_ask.clone();
                            let app_handle = app_handle_for_ask.clone();
                            tauri::async_runtime::spawn(async move {
                                quit_app(state, app_handle).await;
                            });
                        }
                    },
                );
            }

            Ok(())
        })
        .system_tray(system_tray)
        .on_system_tray_event(|app, event| {
            let window = app.get_window("main").unwrap();

            match event {
                SystemTrayEvent::LeftClick { .. } => {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
                    MENU_ID_SHOW => {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                    MENU_ID_HIDE => {
                        let _ = window.hide();
                    }
                    MENU_ID_CLOSE_TO_TRAY => {
                        // 当前是「退出」则切回托盘，否则切到「退出」。
                        let enable_tray = current_close_action() == CLOSE_ACTION_EXIT;
                        update_close_action(if enable_tray {
                            CLOSE_ACTION_TRAY
                        } else {
                            CLOSE_ACTION_EXIT
                        });
                        set_close_to_tray_checked(&app.app_handle(), enable_tray);
                    }
                    MENU_ID_RESTART_DSH => {
                        let state: tauri::State<AppState> = app.state();
                        let state_clone = state.inner().clone();
                        let app_handle_clone = app.app_handle().clone();
                        tauri::async_runtime::spawn(async move {
                            restart_dsh_service(state_clone, app_handle_clone).await;
                        });
                    }
                    MENU_ID_CHANGE_DIR => {
                        let app_handle = app.app_handle().clone();
                        tauri::async_runtime::spawn(async move {
                            let selected = rfd::FileDialog::new()
                                .set_title("重新选择 DSH 源码目录")
                                .pick_folder();
                            if let Some(path) = selected {
                                let path_str = path.to_string_lossy().to_string();
                                // 只改路径，保留既有的关闭行为设置。
                                update_dsh_dir(&path_str);
                                let _ = app_handle.restart();
                            }
                        });
                    }
                    MENU_ID_QUIT => {
                        let state: tauri::State<AppState> = app.state();
                        let state_clone = state.inner().clone();
                        let app_handle_clone = app.app_handle().clone();
                        tauri::async_runtime::spawn(async move {
                            quit_app(state_clone, app_handle_clone).await;
                        });
                    }
                    _ => {}
                },
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 单元测试：重点是 token 地址提取（DSH 0.1.6+ 认证适配的核心逻辑）。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_token_url() {
        let line = "  DSH web ready at http://127.0.0.1:3080/?token=abc123DEF_-";
        assert_eq!(
            parse_token_url(line).as_deref(),
            Some("http://127.0.0.1:3080/?token=abc123DEF_-")
        );
    }

    #[test]
    fn trims_trailing_punctuation_after_token() {
        // DSH 常把地址写在句末，句号不属于 token。
        let line = "打开 http://127.0.0.1:3080/?token=abc.def123.";
        assert_eq!(
            parse_token_url(line).as_deref(),
            Some("http://127.0.0.1:3080/?token=abc.def123")
        );
    }

    #[test]
    fn extracts_token_url_ignoring_ansi_and_quotes() {
        // 终端着色 + 引号包裹时也要能提取，且不能把 ANSI 转义符带进 URL。
        let line = "\u{1b}[32mOpen \u{1b}[0m\"http://127.0.0.1:3080/?token=xyz789\u{1b}[0m\"";
        assert_eq!(
            parse_token_url(line).as_deref(),
            Some("http://127.0.0.1:3080/?token=xyz789")
        );
    }

    #[test]
    fn extracts_token_url_with_extra_query_param() {
        let line = "http://localhost:3080/?foo=1&token=tok_ABCdef";
        assert_eq!(
            parse_token_url(line).as_deref(),
            Some("http://localhost:3080/?foo=1&token=tok_ABCdef")
        );
    }

    #[test]
    fn extracts_real_dsh_018_log_line() {
        // 取自 DSH 真实输出格式（packages/bundle/web-app: console.log(`dsh web: ${authenticatedUrl}${lanUrl...}`)）：
        // 必须取到 127.0.0.1 那个地址，且不能把后面的 " (LAN: ..." 吞进来。
        let line = "dsh web: http://127.0.0.1:3080/?token=test-token (LAN: http://192.168.1.5:3080/?token=test-token)";
        assert_eq!(
            parse_token_url(line).as_deref(),
            Some("http://127.0.0.1:3080/?token=test-token")
        );
    }

    #[test]
    fn extracts_real_dsh_log_line_without_lan() {
        let line = "dsh web: http://127.0.0.1:3080/?token=test-token";
        assert_eq!(
            parse_token_url(line).as_deref(),
            Some("http://127.0.0.1:3080/?token=test-token")
        );
    }

    #[test]
    fn ignores_line_without_token() {
        for line in [
            "DSH web listening on http://127.0.0.1:3080",
            "authentication required",
            "http://127.0.0.1:3080/?token=",
            "",
        ] {
            assert_eq!(parse_token_url(line), None, "不应匹配: {line}");
        }
    }

    #[test]
    fn close_action_defaults_to_tray() {
        // 未配置时最安全的默认行为是最小化到托盘。
        assert_ne!(CLOSE_ACTION_TRAY, CLOSE_ACTION_EXIT);
    }

    /// 验证「子进程 stdout 是管道时，日志能被及时读到」。
    ///
    /// 这是 token 捕获链路的前提：如果 Node 在管道模式下缓冲输出，
    /// 我们就只能在 DSH 退出时才拿到 URL，启动阶段永远等不到 token。
    #[test]
    fn child_stdout_on_a_pipe_is_readable_while_running() {
        tauri::async_runtime::block_on(async {
            let mut child = Command::new(if cfg!(target_os = "windows") {
                "node.exe"
            } else {
                "node"
            })
            .arg("-e")
            .arg("console.log('http://127.0.0.1:3080/?token=pipeTest'); setTimeout(()=>{}, 30000);")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn node for the pipe test");

            let stdout = child.stdout.take().expect("stdout was not piped");
            let mut lines = BufReader::new(stdout).lines();

            // 必须在子进程仍然存活时读到这一行（5 秒上限）。
            let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
                .await
                .expect("timed out: child stdout appeared to be buffered")
                .expect("read failed")
                .expect("stdout closed early");

            assert_eq!(
                parse_token_url(&line).as_deref(),
                Some("http://127.0.0.1:3080/?token=pipeTest")
            );
        });
    }
}
