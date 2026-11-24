//! 本地控制面 HTTP 服务（仅监听 127.0.0.1，Bearer token 鉴权）。
//!
//! M3 将填充：
//! - `GET  /v1/health`
//! - `GET  /v1/stats`
//! - `GET  /v1/logs?since&level`（环形缓冲）
//! - `POST /v1/config/reload`
//! - `POST /v1/upstream/select`
//! - `POST /v1/exit-ip/test`
