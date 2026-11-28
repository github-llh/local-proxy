//! 测试辅助：读取手写 HTTP 请求，供 clash / discovery 的 mock 服务端共用。

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

/// 一次收到的 HTTP 请求。
pub struct TestRequest {
    pub line: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl TestRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn authorization(&self) -> String {
        self.header("authorization").unwrap_or_default().to_string()
    }
}

/// 读取请求行、头与 `Content-Length` 指定的 body。
pub async fn read_request<S>(stream: &mut BufReader<S>) -> TestRequest
where
    S: AsyncRead + Unpin,
{
    let mut line = String::new();
    stream.read_line(&mut line).await.unwrap();

    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        stream.read_line(&mut header).await.unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if length > 0 {
        stream.read_exact(&mut body).await.unwrap();
    }

    TestRequest {
        line: line.trim().to_string(),
        headers,
        body,
    }
}
