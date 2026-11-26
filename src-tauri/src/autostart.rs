//! 开机自启动（macOS LaunchAgent / Windows 注册表 Run）。
//!
//! 不依赖第三方插件：直接写平台标准位置，行为透明可控。

#[cfg(any(target_os = "macos", target_os = "windows", test))]
use std::path::PathBuf;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::Command;

#[cfg(any(target_os = "macos", test))]
const LABEL: &str = "com.localproxy.app";

/// autostart 错误。
#[derive(Debug)]
#[allow(dead_code)] // Command variant 仅 Windows 分支使用
pub enum AutoStartError {
    Io(String),
    Command(String),
}

impl std::fmt::Display for AutoStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AutoStartError::Io(msg) => write!(f, "IO 失败: {msg}"),
            AutoStartError::Command(msg) => write!(f, "命令失败: {msg}"),
        }
    }
}

impl std::error::Error for AutoStartError {}

/// 当前 App 可执行路径。
#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn app_executable() -> Result<PathBuf, AutoStartError> {
    std::env::current_exe().map_err(|e| AutoStartError::Io(e.to_string()))
}

/// 启用开机自启。
pub fn enable() -> Result<(), AutoStartError> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let exe = app_executable()?;
    #[cfg(target_os = "macos")]
    {
        let plist_path = launch_agent_plist_path()?;
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
</dict>
</plist>"#,
            exe.display()
        );
        std::fs::write(&plist_path, plist).map_err(|e| AutoStartError::Io(e.to_string()))?;
        // 立即加载（已加载时 launchctl 报错，忽略）
        let _ = Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&plist_path)
            .output();
    }
    #[cfg(target_os = "windows")]
    {
        // HKCU\...\Run 写入当前用户启动项（无需管理员）
        let output = Command::new("reg")
            .args([
                "add",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "LocalProxy",
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{}\"", exe.display()),
                "/f",
            ])
            .output()
            .map_err(|e| AutoStartError::Command(e.to_string()))?;
        if !output.status.success() {
            return Err(AutoStartError::Command(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
    }
    Ok(())
}

/// 禁用开机自启。
pub fn disable() -> Result<(), AutoStartError> {
    #[cfg(target_os = "macos")]
    {
        let plist_path = launch_agent_plist_path()?;
        let _ = Command::new("launchctl")
            .args(["unload"])
            .arg(&plist_path)
            .output();
        if plist_path.exists() {
            std::fs::remove_file(&plist_path).map_err(|e| AutoStartError::Io(e.to_string()))?;
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("reg")
            .args([
                "delete",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "LocalProxy",
                "/f",
            ])
            .output();
    }
    Ok(())
}

/// 当前是否已启用。
pub fn is_enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        launch_agent_plist_path()
            .map(|p| p.exists())
            .unwrap_or(false)
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("reg")
            .args([
                "query",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "LocalProxy",
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
fn launch_agent_plist_path() -> Result<PathBuf, AutoStartError> {
    let home =
        std::env::var_os("HOME").ok_or_else(|| AutoStartError::Io("HOME 未设置".to_string()))?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_is_stable() {
        assert_eq!(LABEL, "com.localproxy.app");
    }

    #[test]
    fn app_executable_resolves() {
        let exe = app_executable().unwrap();
        assert!(exe.is_absolute());
        assert!(exe.exists());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn enable_disable_roundtrip() {
        // 注意：本测试会真实写 LaunchAgents；仅在非 CI 环境手动验证
        if std::env::var_os("CI").is_some() {
            return;
        }
        assert!(!is_enabled());
        enable().unwrap();
        assert!(is_enabled());
        disable().unwrap();
        assert!(!is_enabled());
    }
}
