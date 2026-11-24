//! LocalProxy 菜单栏控制中心。
//!
//! - tray：图标 + 菜单（打开面板/设置/退出），左键切换 popover
//! - popover：状态、上游切换、运行中应用、快速操作
//! - settings：应用管理 / 上游 / 规则 / DNS / 日志 / 高级
//! - 启动时自动拉起本地核心（sidecar 子进程）

use tauri::Manager;

mod commands;
mod state;
mod tray;

/// 启动 Tauri 应用。
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(state::AppState::new())
        .setup(|app| {
            let handle = app.handle().clone();
            tray::setup(&handle)?;

            // macOS：隐藏 dock 图标（纯菜单栏应用）
            #[cfg(target_os = "macos")]
            {
                let _ = handle.set_dock_visibility(false);
            }

            // 自动启动本地核心
            let app_state = app.state::<state::AppState>();
            if let Err(e) = state::spawn_core(&app_state) {
                tracing::warn!("自动启动核心失败: {e}");
            }

            // 向前端广播窗口标签
            tray::announce_window_label(&handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::start_core,
            commands::stop_core,
            commands::list_apps,
            commands::add_app,
            commands::remove_app,
            commands::launch_app,
            commands::stop_app,
            commands::list_sessions,
            commands::get_config,
            commands::save_config,
            commands::select_upstream,
        ])
        .run(tauri::generate_context!())
        .expect("启动 LocalProxy 失败");
}
