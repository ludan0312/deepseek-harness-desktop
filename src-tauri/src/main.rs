#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::{CustomMenuItem, Manager, SystemTray, SystemTrayEvent, SystemTrayMenu, SystemTrayMenuItem, WindowEvent};
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// Windows 专用：隐藏子进程窗口
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// ============================================
// 配置区
// ============================================
const DSH_URL: &str = "http://127.0.0.1:3080";
const CHECK_INTERVAL_MS: u64 = 1000;
const STARTUP_TIMEOUT_S: u64 = 60;
const DEFAULT_DSH_DIR: &str = r"D:\DeepSeekHarness\deepseek-harness-master";

#[derive(Clone)]
struct AppState {
    dsh_process: Arc<Mutex<Option<std::process::Child>>>,
    dsh_dir: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Config {
    dsh_dir: String,
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
        if std::path::Path::new(&config.dsh_dir).exists() {
            return config.dsh_dir;
        }
    }

    if std::path::Path::new(DEFAULT_DSH_DIR).exists() {
        let config = Config { dsh_dir: DEFAULT_DSH_DIR.to_string() };
        let _ = save_config(&config);
        return DEFAULT_DSH_DIR.to_string();
    }

    let selected = rfd::FileDialog::new()
        .set_title("请选择 DSH 源码目录")
        .pick_folder();

    match selected {
        Some(path) => {
            let path_str = path.to_string_lossy().to_string();
            let config = Config { dsh_dir: path_str.clone() };
            let _ = save_config(&config);
            path_str
        }
        None => DEFAULT_DSH_DIR.to_string(),
    }
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
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    window_for_close.hide().unwrap();
                    api.prevent_close();
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
                                    let config = Config { dsh_dir: path_str.clone() };
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
                                    let _ = child.kill();
                                    let _ = child.wait();
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
        let _ = old.kill();
        let _ = old.wait();
    }

    let mut cmd = Command::new("pnpm.cmd");
    cmd.arg("dsh")
        .arg("web")
        .current_dir(&state.dsh_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Windows: 隐藏 pnpm.cmd 弹出的命令行窗口
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
}

async fn load_dsh_ui(app_handle: &tauri::AppHandle) {
    let window = app_handle.get_window("main").unwrap();
    let _ = window.eval(&format!("window.location.href = '{}'", DSH_URL));
    window.show().unwrap();
    window.set_focus().unwrap();
}