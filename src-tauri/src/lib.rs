//! LocalProxy 菜单栏控制中心。
//!
//! M0：tray + 空窗口骨架。
//! M3：popover / 设置窗口 / 命令层 / 核心进程管理。

/// 启动 Tauri 应用。
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            setup_tray(&handle)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动 LocalProxy 失败");
}

/// 创建托盘图标（M0 骨架：仅图标 + 右键菜单）。
fn setup_tray(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};

    let menu = tauri::menu::MenuBuilder::new(app)
        .text("open-settings", "打开设置…")
        .text("quit", "退出 LocalProxy")
        .build()?;

    let _tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().cloned().unwrap())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("LocalProxy")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "quit" => {
                app.exit(0);
            }
            "open-settings" => {
                // M3：打开设置窗口
                tracing::info!("打开设置（M3 实现）");
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } = event
            {
                // M3：显示 popover 窗口
                let _ = tray;
                tracing::info!("左键点击托盘（M3 显示 popover）");
            }
        })
        .build(app)?;

    Ok(())
}
