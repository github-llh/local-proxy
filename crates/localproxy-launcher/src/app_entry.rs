//! 被代理应用条目模型。
//!
//! 用户登记的应用；launcher 据此以代理模式启动。
//! M2 将补充平台解析（macOS `.app` 主二进制 / Windows `.exe`）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 一个被代理应用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    /// 稳定 ID（UUID）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 应用路径（macOS `.app` bundle / Windows `.exe`）
    pub path: String,
    /// 是否启用（未启用不出现在启动列表）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 额外启动参数
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// 备注
    #[serde(default)]
    pub note: String,
    /// 最近一次代理启动的 Unix 时间戳（秒；用于「最近使用」排序）
    #[serde(default)]
    pub last_launched_at: Option<u64>,
}

fn default_true() -> bool {
    true
}

impl AppEntry {
    /// 由路径构造（名称取文件名去扩展名）。
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return None;
        }
        let name = app_display_name(path);
        Some(Self {
            id: new_id(),
            name,
            path: path.to_string_lossy().to_string(),
            enabled: true,
            extra_args: Vec::new(),
            note: String::new(),
            last_launched_at: None,
        })
    }

    /// 应用路径（PathBuf）。
    pub fn path_buf(&self) -> PathBuf {
        PathBuf::from(&self.path)
    }

    /// 是否为 macOS `.app` bundle。
    pub fn is_macos_bundle(&self) -> bool {
        self.path_buf()
            .extension()
            .map(|e| e.eq_ignore_ascii_case("app"))
            .unwrap_or(false)
    }
}

/// 显示名称：`.app` 去扩展名；其它取文件名。
pub fn app_display_name(path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    if path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("app"))
        .unwrap_or(false)
    {
        // 去掉 .app 后缀
        file_name
            .strip_suffix(".app")
            .or_else(|| file_name.strip_suffix(".APP"))
            .unwrap_or(&file_name)
            .to_string()
    } else {
        file_name
    }
}

/// 生成简短唯一 ID（时间戳 + 随机，无第三方依赖）。
pub fn new_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // 混入一个栈地址做轻量随机化（仅用于 ID，不用于安全场景）
    let entropy = &nanos as *const u128 as usize;
    format!("{:x}{:x}", nanos, entropy & 0xFFFFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_for_app_bundle() {
        assert_eq!(
            app_display_name(Path::new("/Applications/Google Chrome.app")),
            "Google Chrome"
        );
    }

    #[test]
    fn display_name_for_plain_binary() {
        assert_eq!(app_display_name(Path::new("/usr/bin/curl")), "curl");
    }

    #[test]
    fn is_macos_bundle_detection() {
        let entry = AppEntry {
            id: "x".into(),
            name: "Chrome".into(),
            path: "/Applications/Google Chrome.app".into(),
            enabled: true,
            extra_args: vec![],
            note: String::new(),
            last_launched_at: None,
        };
        assert!(entry.is_macos_bundle());
        assert_eq!(
            entry.path_buf(),
            PathBuf::from("/Applications/Google Chrome.app")
        );
    }

    #[test]
    fn id_is_unique_enough() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(!a.is_empty());
    }
}
