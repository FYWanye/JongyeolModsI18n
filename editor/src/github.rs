//! GitHub Contents API：读写仓库里的汉化表。
//!
//! 只用 GET / PUT 两个接口，避免引入完整 SDK；令牌从配置或 GITHUB_TOKEN 读取。

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};

use crate::config::{self, State};
use crate::http;

/// 仓库里的一个文件（含版本指纹 sha，更新时必须回传）。
pub struct RemoteFile {
    pub text: String,
    pub sha: Option<String>,
}

fn contents_url(repo: &str, branch: &str, path: &str) -> String {
    format!("https://api.github.com/repos/{repo}/contents/{path}?ref={branch}")
}

/// 读取文件；404（文件还不存在）返回 None 而不是报错。
pub fn read_file(state: &State, path: &str) -> Result<Option<RemoteFile>> {
    let repo = state.repo();
    let branch = state.branch();
    let url = contents_url(&repo, &branch, path);
    let token = config::github_token(state);
    let value = match http::get_json(&url, state.proxy.as_deref(), token.as_deref()) {
        Ok(value) => value,
        Err(err) => {
            let text = err.to_string();
            if text.contains("HTTP 404") {
                return Ok(None);
            }
            return Err(err);
        }
    };
    let encoded = value
        .get("content")
        .and_then(|c| c.as_str())
        .context("响应里没有 content 字段")?;
    let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = BASE64.decode(cleaned).context("解码 base64 失败")?;
    let text = String::from_utf8(bytes).context("文件不是合法 UTF-8")?;
    let sha = value.get("sha").and_then(|s| s.as_str()).map(|s| s.to_string());
    Ok(Some(RemoteFile { text, sha }))
}

/// 写入文件（自动带上已有 sha 以更新，没有则新建）。
pub fn write_file(state: &State, path: &str, text: &str, message: &str) -> Result<()> {
    let token = config::github_token(state)
        .context("没有配置 GitHub 令牌：请先执行 `i18n-editor auth <token>`，或设置环境变量 GITHUB_TOKEN")?;
    let repo = state.repo();
    let branch = state.branch();
    let sha = read_file(state, path)?.and_then(|file| file.sha);

    let mut body = json!({
        "message": message,
        "content": BASE64.encode(text.as_bytes()),
        "branch": branch,
    });
    if let Some(sha) = sha {
        body["sha"] = json!(sha);
    }

    let url = format!("https://api.github.com/repos/{repo}/contents/{path}");
    // 注意：Contents API 创建/更新文件用 PUT（用 POST 会返回 404）
    let response = http::send_json(
        minreq::Method::Put,
        &url,
        state.proxy.as_deref(),
        &token,
        &body.to_string(),
    )
    .with_context(|| format!("写入 {path} 失败"))?;
    let value: Value = serde_json::from_str(&response).unwrap_or(Value::Null);
    let commit = value
        .get("commit")
        .and_then(|c| c.get("sha"))
        .and_then(|s| s.as_str())
        .map(|s| &s[..7.min(s.len())])
        .unwrap_or("?");
    println!("  已提交 {path}（commit {commit}）");
    Ok(())
}

/// 校验令牌并返回登录名。
pub fn verify_token(state: &State) -> Result<String> {
    let token = config::github_token(state)
        .context("没有可用的令牌（配置或 GITHUB_TOKEN 均为空）")?;
    let url = "https://api.github.com/user";
    let text = http::get_text(
        url,
        state.proxy.as_deref(),
        &[
            ("Authorization".into(), format!("token {token}")),
            ("Accept".into(), "application/vnd.github+json".into()),
        ],
    )?;
    let value: Value =
        serde_json::from_str(&text).map_err(|err| anyhow!("解析用户信息失败：{err}"))?;
    let login = value
        .get("login")
        .and_then(|l| l.as_str())
        .context("响应里没有 login")?
        .to_string();
    Ok(login)
}

/// 便于打印 raw 地址（帮助/诊断信息里用）。
#[allow(dead_code)]
pub fn raw_url(state: &State, mod_id: &str) -> String {
    config::raw_url(&state.repo(), &state.branch(), mod_id)
}
