//! 仅为代理子进程清理继承的代理配置，保留父进程和其他应用的环境。

pub fn is_proxy_var(key: &str) -> bool {
    [
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "ftp_proxy",
        "no_proxy",
        "socks_proxy",
    ]
    .iter()
    .any(|candidate| key.eq_ignore_ascii_case(candidate))
}

pub fn remove_from(command: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(is_proxy_var) {
            command.env_remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_environment_matching_is_case_insensitive_and_scoped() {
        assert!(is_proxy_var("Http_Proxy"));
        assert!(is_proxy_var("No_PrOxY"));
        assert!(is_proxy_var("SOCKS_PROXY"));
        assert!(!is_proxy_var("PROXY_TOKEN"));
        assert!(!is_proxy_var("PATH"));
    }
}
