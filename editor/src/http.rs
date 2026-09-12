//! 轻量 HTTP 封装：统一处理代理、请求头与错误信息。
//!
//! 用 `minreq`（依赖面很小，HTTPS 走 rustls），Windows / Linux / macOS 均可直接编译。

use anyhow::{anyhow, Result};

const TIMEOUT_SECS: u64 = 45;

fn proxy_of(proxy: Option<&str>) -> Option<minreq::Proxy> {
    let url = proxy?.trim();
    if url.is_empty() {
        return None;
    }
    match minreq::Proxy::new(url) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            eprintln!("警告：代理地址无法解析（{url}）：{err}");
            None
        }
    }
}

fn describe(err: minreq::Error, url: &str) -> anyhow::Error {
    anyhow!("{err}：{url}")
}

/// 发起 GET；headers 统一用 `String` 以避免临时值借用问题。
pub fn get_text(url: &str, proxy: Option<&str>, headers: &[(String, String)]) -> Result<String> {
    let mut request = minreq::get(url)
        .with_timeout(TIMEOUT_SECS)
        .with_header("User-Agent", concat!("i18n-editor/", env!("CARGO_PKG_VERSION")));
    if let Some(proxy) = proxy_of(proxy) {
        request = request.with_proxy(proxy);
    }
    for (name, value) in headers {
        request = request.with_header(name, value);
    }
    let response = request.send().map_err(|err| describe(err, url))?;
    // 4xx/5xx 也可以读出正文，便于给出可读报错
    let status = response.status_code;
    let text = response
        .as_str()
        .map_err(|err| anyhow!("响应不是合法 UTF-8（{url}）：{err}"))?
        .to_string();
    if !(200..300).contains(&status) {
        let snippet: String = text.trim().chars().take(300).collect();
        return Err(anyhow!("HTTP {status}：{url}\n  {snippet}"));
    }
    Ok(text)
}

pub fn get_json(url: &str, proxy: Option<&str>, token: Option<&str>) -> Result<serde_json::Value> {
    let mut headers: Vec<(String, String)> = vec![(
        "Accept".into(),
        "application/vnd.github+json".into(),
    )];
    if let Some(token) = token {
        headers.push(("Authorization".into(), format!("token {token}")));
    }
    let text = get_text(url, proxy, &headers)?;
    serde_json::from_str(&text).map_err(|err| anyhow!("解析 JSON 失败（{url}）：{err}"))
}

/// 以 JSON 正文发起请求（GitHub Contents API 写入用，方法为 PUT）。
pub fn send_json(
    method: minreq::Method,
    url: &str,
    proxy: Option<&str>,
    token: &str,
    body: &str,
) -> Result<String> {
    let headers: Vec<(String, String)> = vec![
        ("Authorization".into(), format!("token {token}")),
        ("Accept".into(), "application/vnd.github+json".into()),
        ("Content-Type".into(), "application/json; charset=utf-8".into()),
    ];
    let mut request = minreq::Request::new(method, url)
        .with_timeout(TIMEOUT_SECS)
        .with_header("User-Agent", "i18n-editor")
        .with_body(body);
    if let Some(proxy) = proxy_of(proxy) {
        request = request.with_proxy(proxy);
    }
    for (name, value) in &headers {
        request = request.with_header(name, value);
    }
    let response = request.send().map_err(|err| describe(err, url))?;
    let status = response.status_code;
    let text = response
        .as_str()
        .map_err(|err| anyhow!("响应不是合法 UTF-8（{url}）：{err}"))?
        .to_string();
    if !(200..300).contains(&status) {
        let snippet: String = text.trim().chars().take(300).collect();
        return Err(anyhow!("HTTP {status}：{url}\n  {snippet}"));
    }
    Ok(text)
}

/// 供上层探测连通性用（`info` / 诊断）。
#[allow(dead_code)]
pub fn ping(url: &str, proxy: Option<&str>) -> Result<()> {
    get_text(url, proxy, &[]).map(|_| ())
}
