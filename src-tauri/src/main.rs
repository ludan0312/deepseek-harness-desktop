#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::{api::dialog::ask, CustomMenuItem, Manager, SystemTray, SystemTrayEvent, SystemTrayMenu, SystemTrayMenuItem, WindowEvent};
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const DSH_URL: &str = "http://127.0.0.1:3080";
const CHECK_INTERVAL_MS: u64 = 1000;
const STARTUP_TIMEOUT_S: u64 = 60;
const DEFAULT_DSH_DIR: &str = r"D:\DeepSeekHarness\deepseek-harness-master";

#[derive(Clone)]
struct AppState {
    dsh_process: Arc<Mutex<Option<std::process::Child>>>,
    dsh_dir: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct Config {
    dsh_dir: String,
    close_action: Option<String>,
}

fn get_config_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("dsh-tauri-wrapper");
    path.push("config.json");
    path
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

fn ensure_dsh_dir() -> String {
    if let Some(config) = load_config() {
        if !config.dsh_dir.is_empty() && std::path::Path::new(&config.dsh_dir).exists() {
            return config.dsh_dir;
        }
    }

    if std::path::Path::new(DEFAULT_DSH_DIR).exists() {
        let config = Config {
            dsh_dir: DEFAULT_DSH_DIR.to_string(),
            ..Default::default()
        };
        let _ = save_config(&config);
        return DEFAULT_DSH_DIR.to_string();
    }

    let selected = rfd::FileDialog::new()
        .set_title("请选择 DSH 源码目录")
        .pick_folder();

    match selected {
        Some(path) => {
            let path_str = path.to_string_lossy().to_string();
            let config = Config {
                dsh_dir: path_str.clone(),
                ..Default::default()
            };
            let _ = save_config(&config);
            path_str
        }
        None => DEFAULT_DSH_DIR.to_string(),
    }
}

/// 杀掉进程及其子进程（Windows 用 taskkill /T 杀进程树）
fn kill_process_tree(child: &mut std::process::Child) {
    let pid = child.id();
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(&["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn main() {
    let dsh_dir = ensure_dsh_dir();

    let tray_menu = SystemTrayMenu::new()
        .add_item(CustomMenuItem::new("show", "显示主界面"))
        .add_item(CustomMenuItem::new("hide", "隐藏到托盘"))
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(CustomMenuItem::new("restart_dsh", "重启 DSH 服务"))
        .add_item(CustomMenuItem::new("change_dir", "更改 DSH 路径"))
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(CustomMenuItem::new("quit", "退出"));

    let system_tray = SystemTray::new().with_menu(tray_menu);

    tauri::Builder::default()
        .manage(AppState {
            dsh_process: Arc::new(Mutex::new(None)),
            dsh_dir: dsh_dir.clone(),
        })
        .setup(move |app| {
            let window = app.get_window("main").unwrap();
            let state: tauri::State<AppState> = app.state();
            let app_handle = app.app_handle();

            let state_clone = state.inner().clone();
            let app_handle_clone = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                check_and_start_dsh(state_clone, app_handle_clone).await;
            });

            let window_for_close = window.clone();
            let state_for_close = state.inner().clone();
            let app_handle_for_close = app_handle.clone();

            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();

                    let config = load_config();
                    let close_action = config.as_ref().and_then(|c| c.close_action.clone());

                    match close_action.as_deref() {
                        None => {
                            // 首次关闭：隐藏窗口并弹窗询问
                            let _ = window_for_close.hide();

                            let state = state_for_close.clone();
                            let app_handle = app_handle_for_close.clone();

                            ask(
                                Some(&window_for_close),
                                "关闭确认",
                                "请选择关闭行为：\n\n点击「是」→ 最小化到系统托盘（推荐，后台运行）\n点击「否」→ 直接退出应用",
                                move |result| {
                                    if result {
                                        // 选择托盘
                                        let mut cfg = load_config().unwrap_or_default();
                                        cfg.close_action = Some("tray".to_string());
                                        let _ = save_config(&cfg);
                                    } else {
                                        // 选择退出
                                        let mut cfg = load_config().unwrap_or_default();
                                        cfg.close_action = Some("exit".to_string());
                                        let _ = save_config(&cfg);

                                        // 杀掉 DSH 进程树
                                        tauri::async_runtime::block_on(async {
                                            let mut process = state.dsh_process.lock().await;
                                            if let Some(mut child) = process.take() {
                                                kill_process_tree(&mut child);
                                            }
                                        });

                                        app_handle.exit(0);
                                    }
                                }
                            );
                        }
                        Some("tray") => {
                            let _ = window_for_close.hide();
                        }
                        Some("exit") => {
                            let _ = window_for_close.hide();
                            tauri::async_runtime::block_on(async {
                                let mut process = state_for_close.dsh_process.lock().await;
                                if let Some(mut child) = process.take() {
                                    kill_process_tree(&mut child);
                                }
                            });
                            std::process::exit(0);
                        }
                        _ => {
                            let _ = window_for_close.hide();
                        }
                    }
                }
            });

            Ok(())
        })
        .system_tray(system_tray)
        .on_system_tray_event(|app, event| {
            let window = app.get_window("main").unwrap();

            match event {
                SystemTrayEvent::LeftClick { .. } => {
                    if window.is_visible().unwrap() {
                        window.hide().unwrap();
                    } else {
                        window.show().unwrap();
                        window.set_focus().unwrap();
                    }
                }
                SystemTrayEvent::MenuItemClick { id, .. } => {
                    match id.as_str() {
                        "show" => {
                            window.show().unwrap();
                            window.set_focus().unwrap();
                        }
                        "hide" => {
                            window.hide().unwrap();
                        }
                        "restart_dsh" => {
                            let state: tauri::State<AppState> = app.state();
                            let state_clone = state.inner().clone();
                            let app_handle_clone = app.app_handle().clone();
                            tauri::async_runtime::spawn(async move {
                                restart_dsh_service(state_clone, app_handle_clone).await;
                            });
                        }
                        "change_dir" => {
                            let app_handle = app.app_handle().clone();
                            tauri::async_runtime::spawn(async move {
                                let selected = rfd::FileDialog::new()
                                    .set_title("重新选择 DSH 源码目录")
                                    .pick_folder();
                                if let Some(path) = selected {
                                    let path_str = path.to_string_lossy().to_string();
                                    let config = Config {
                                        dsh_dir: path_str.clone(),
                                        ..Default::default()
                                    };
                                    let _ = save_config(&config);
                                    let _ = app_handle.restart();
                                }
                            });
                        }
                        "quit" => {
                            let state: tauri::State<AppState> = app.state();
                            tauri::async_runtime::block_on(async {
                                let mut process = state.dsh_process.lock().await;
                                if let Some(mut child) = process.take() {
                                    kill_process_tree(&mut child);
                                }
                            });
                            std::process::exit(0);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

async fn is_dsh_running() -> bool {
    match reqwest::get(DSH_URL).await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn start_dsh_process(state: &AppState) {
    let mut process = state.dsh_process.lock().await;

    if let Some(mut old) = process.take() {
        kill_process_tree(&mut old);
    }

    let mut cmd = Command::new("pnpm.cmd");
    cmd.arg("dsh")
        .arg("web")
        .current_dir(&state.dsh_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    match cmd.spawn() {
        Ok(c) => {
            *process = Some(c);
        }
        Err(e) => {
            eprintln!("[DSH Wrapper] 启动 DSH 失败: {}", e);
        }
    }
}

async fn check_and_start_dsh(state: AppState, app_handle: tauri::AppHandle) {
    if is_dsh_running().await {
        load_dsh_ui(&app_handle).await;
        return;
    }

    start_dsh_process(&state).await;

    let mut attempts = 0;
    let max_attempts = STARTUP_TIMEOUT_S;

    while attempts < max_attempts {
        sleep(Duration::from_millis(CHECK_INTERVAL_MS)).await;

        if is_dsh_running().await {
            load_dsh_ui(&app_handle).await;
            return;
        }

        attempts += 1;
        if attempts % 5 == 0 {
            println!("[DSH Wrapper] 等待 DSH 启动... ({}/{})", attempts, max_attempts);
        }
    }

    eprintln!("[DSH Wrapper] DSH 启动超时！");
}

async fn restart_dsh_service(state: AppState, app_handle: tauri::AppHandle) {
    start_dsh_process(&state).await;

    let mut attempts = 0;
    while attempts < STARTUP_TIMEOUT_S {
        sleep(Duration::from_millis(CHECK_INTERVAL_MS)).await;
        if is_dsh_running().await {
            let window = app_handle.get_window("main").unwrap();
            let _ = window.eval("window.location.reload()");
            return;
        }
        attempts += 1;
    }
    eprintln!("[DSH Wrapper] DSH 重启超时！");
}

async fn load_dsh_ui(app_handle: &tauri::AppHandle) {
    let window = app_handle.get_window("main").unwrap();
    let _ = window.eval(&format!("window.location.href = '{}'", DSH_URL));
    window.show().unwrap();
    window.set_focus().unwrap();
}