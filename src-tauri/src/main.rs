// LocalProxy 菜单栏应用入口。

// 防止 main 函数被当作未使用（Tauri 桌面端入口）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    localproxy_app_lib::run()
}
