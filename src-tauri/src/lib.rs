//! LocalProxy 菜单栏控制中心。
//!
//! - tray：图标 + 菜单（打开面板/设置/退出），左键切换 popover
//! - popover：状态、上游切换、运行中应用、快速操作
//! - settings：应用管理 / 上游 / 规则 / DNS / 日志 / 高级
//! - 启动时自动拉起本地核心（sidecar 子进程）
//!
//! 单实例：第二个实例获取不到配置文件目录下的 `app.lock` 时给出提示并退出，
//! 避免两个菜单栏图标、两套核心互相争抢端口与策略目录。

use tauri::Manager;

mod autostart;
mod clash;
mod clash_discovery;
mod commands;
mod control_client;
mod probe;
mod state;
#[cfg(test)]
mod test_util;
mod tray;

/// 启动 Tauri 应用。
pub fn run() {
    let instance_lock = match state::acquire_instance_lock() {
        Ok(lock) => lock,
        Err(reason) => {
            notify_already_running(&reason);
            return;
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(state::AppState::new(instance_lock))
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
            commands::update_app,
            commands::launch_app,
            commands::stop_app,
            commands::list_sessions,
            commands::set_app_enabled,
            commands::test_upstream,
            commands::find_mihomo,
            commands::probe_exit_ip,
            commands::list_recent_connections,
            commands::get_config,
            commands::save_config,
            commands::get_launch_at_login,
            commands::select_upstream,
            commands::get_clash_state,
            commands::select_clash_proxy,
            commands::discover_clash,
            commands::apply_clash_controller,
        ])
        .run(tauri::generate_context!())
        .expect("启动 LocalProxy 失败");
}

/// 已有实例时给出用户可见的提示。
///
/// 菜单栏应用没有控制台输出入口，macOS 走系统通知；其它平台至少留下 stderr。
fn notify_already_running(reason: &str) {
    tracing::warn!("{reason}");
    eprintln!("{reason}");
    let message = reason.replace('"', "'");
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{message}\" with title \"LocalProxy\" subtitle \"已在运行\""
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = message;
    }
}
