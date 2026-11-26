//! 探针专用的极简 HTTP/1.1 客户端。
//!
//! 只用于对 `127.0.0.1:<port>` 发 `GET /json/version`、`GET /json/list` 这类
//! DevTools HTTP 端点。**不支持 https**，也不需要——CDP 的 HTTP 发现端点永远是明文。
//!
//! 为什么不用 reqwest：见 Cargo.toml 注释（rustls→ring 需要 C 工具链）。

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

/// POST 用的返回体（协议重放实验需要）
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 按 Content-Type 猜是否文本
    pub fn is_text(&self) -> bool {
        match self.header("content-type") {
            Some(ct) => {
                let ct = ct.to_ascii_lowercase();
                ct.contains("text/")
                    || ct.contains("json")
                    || ct.contains("xml")
                    || ct.contains("javascript")
                    || ct.contains("html")
            }
            None => true,
        }
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

async fn read_response(stream: &mut TcpStream, limit: usize) -> Result<HttpResponse> {
    let mut raw: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];

    // 先读到 header 结束
    let head_end = loop {
        if let Some(idx) = find_subslice(&raw, b"\r\n\r\n") {
            break idx + 4;
        }
        let n = stream.read(&mut chunk).await.context("读取响应失败")?;
        if n == 0 {
            bail!("连接在响应头结束前关闭");
        }
        raw.extend_from_slice(&chunk[..n]);
        if raw.len() > 1 << 20 {
            bail!("响应头超过 1MB，判定异常");
        }
    };

    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("无法解析状态行: {status_line:?}"))?;

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    let mut body = raw[head_end..].to_vec();
    let content_len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok());
    let chunked = headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked"));

    if let Some(cl) = content_len {
        let want = cl.min(limit);
        while body.len() < want {
            let n = stream.read(&mut chunk).await.context("读取响应体失败")?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(want);
    } else if chunked {
        // 只做最朴素的 chunked 解码，够用即可
        let mut decoded = Vec::new();
        let mut cursor = 0usize;
        loop {
            if decoded.len() >= limit {
                break;
            }
            let Some(line_end) = find_subslice(&body[cursor..], b"\r\n") else {
                let n = stream.read(&mut chunk).await.context("读取 chunk 头失败")?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
                continue;
            };
            let line_end = cursor + line_end;
            let size_str = String::from_utf8_lossy(&body[cursor..line_end]).to_string();
            let size_str = size_str.split(';').next().unwrap_or("").trim().to_string();
            let Ok(size) = usize::from_str_radix(&size_str, 16) else {
                break;
            };
            if size == 0 {
                break;
            }
            while body.len() < line_end + 2 + size + 2 {
                let n = stream.read(&mut chunk).await.context("读取 chunk 体失败")?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            decoded.extend_from_slice(&body[line_end + 2..(line_end + 2 + size).min(body.len())]);
            cursor = line_end + 2 + size + 2;
            if cursor >= body.len() {
                break;
            }
        }
        body = decoded;
    } else {
        // 无长度信息：读到对端关闭为止（上限 limit）
        while body.len() < limit {
            let n = stream.read(&mut chunk).await.context("读取响应体失败")?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(limit);
    }

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 对 127.0.0.1:port 发请求。
pub async fn request(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout_ms: u64,
) -> Result<HttpResponse> {
    let fut = async {
        let mut stream = TcpStream::connect((host, port))
            .await
            .with_context(|| format!("连接 {host}:{port} 失败"))?;
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n");
        for (k, v) in headers {
            head.push_str(k);
            head.push_str(": ");
            head.push_str(v);
            head.push_str("\r\n");
        }
        if let Some(b) = body {
            head.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await.context("写请求头失败")?;
        if let Some(b) = body {
            stream.write_all(b).await.context("写请求体失败")?;
        }
        stream.flush().await.ok();
        read_response(&mut stream, 8 << 20).await
    };

    match timeout(Duration::from_millis(timeout_ms), fut).await {
        Ok(r) => r,
        Err(_) => bail!("请求 {path} 超时（{timeout_ms}ms）"),
    }
}

pub async fn get_text(port: u16, path: &str, timeout_ms: u64) -> Result<String> {
    let r = request("127.0.0.1", port, "GET", path, &[], None, timeout_ms).await?;
    if r.status >= 400 {
        bail!("GET {path} 返回 {}", r.status);
    }
    Ok(r.body_text())
}

#[cfg(test)]
mod tests {
    use super::find_subslice;

    #[test]
    fn finds_header_terminator() {
        let buf = b"HTTP/1.1 200 OK\r\nA: b\r\n\r\nbody";
        assert_eq!(find_subslice(buf, b"\r\n\r\n"), Some(21));
    }

    #[test]
    fn missing_returns_none() {
        assert_eq!(find_subslice(b"abc", b"\r\n\r\n"), None);
    }
}
