//! 拉取并解析作者的 Google 表格（gviz 端点返回的是包了一层 JS 的 JSON）。

use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde_json::Value;

use crate::config::{self, SheetRow};
use crate::http;

/// 云端表格的一条完整快照。
#[derive(Debug, Clone)]
pub struct Sheet {
    pub mod_id: String,
    /// 语言列名（来自表格第 0 行，如 ["Korean", "English"]）
    pub languages: Vec<String>,
    pub rows: Vec<SheetRow>,
}

impl Sheet {
    /// 取某条目的参考文本（默认英文）；没有就退回任意一个非空语言。
    pub fn reference(&self, row: &SheetRow) -> String {
        if let Some(text) = row.values.get("English") {
            if !text.is_empty() {
                return text.clone();
            }
        }
        row.values
            .iter()
            .find(|(_, text)| !text.is_empty())
            .map(|(_, text)| text.clone())
            .unwrap_or_default()
    }
}

/// 拉取某个模组的表格。仅本地维护的模组没有云端表格，会直接报错。
pub fn fetch(mod_id: &str, proxy: Option<&str>) -> Result<Sheet> {
    let gid = match config::find_mod(mod_id).map(|def| def.source) {
        Some(config::Source::Sheet { gid }) => gid,
        Some(config::Source::LocalOnly) => anyhow::bail!(
            "{mod_id} 没有云端表格（上游未配置 Gid），只能本地编辑后用 push 上传"
        ),
        None => anyhow::bail!("未知模组：{mod_id}（可用：{}）", available()),
    };
    let url = config::sheet_url(gid);
    let body = http::get_text(&url, proxy, &[]).with_context(|| format!("拉取表格失败：{url}"))?;
    parse(&body, mod_id)
}

pub fn available() -> String {
    config::sheet_backed_ids().join(", ")
}

/// 解析 gviz 响应：`/*O_o*/ google.visualization.Query.setResponse({...});`
pub fn parse(body: &str, mod_id: &str) -> Result<Sheet> {
    let start = body.find('{').context("响应里找不到 JSON 起始符")?;
    let end = body.rfind('}').context("响应里找不到 JSON 结束符")?;
    let json: Value = serde_json::from_str(&body[start..=end]).context("解析 gviz JSON 失败")?;

    let rows = json
        .get("table")
        .and_then(|t| t.get("rows"))
        .and_then(|r| r.as_array())
        .context("响应里没有 table.rows")?;
    if rows.is_empty() {
        anyhow::bail!("表格为空");
    }

    // 第 0 行的 c[1..] 是语言列名
    let languages = cell_array(&rows[0])
        .iter()
        .skip(1)
        .map(|cell| cell_text(cell))
        .collect::<Vec<_>>();

    let mut result = Vec::new();
    for row in rows.iter().skip(1) {
        let cells = cell_array(row);
        let key = cells.first().map(|cell| cell_text(cell)).unwrap_or_default();
        if key.trim().is_empty() {
            continue;
        }
        let mut values = IndexMap::new();
        for (index, name) in languages.iter().enumerate() {
            if name.trim().is_empty() {
                continue;
            }
            let text = cells.get(index + 1).map(|cell| cell_text(cell)).unwrap_or_default();
            values.insert(name.clone(), text);
        }
        result.push(SheetRow { key, values });
    }

    Ok(Sheet {
        mod_id: mod_id.to_string(),
        languages: languages
            .into_iter()
            .filter(|name| !name.trim().is_empty())
            .collect(),
        rows: result,
    })
}

fn cell_array(row: &Value) -> Vec<Value> {
    row.get("c")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default()
}

/// gviz 的单元格：`{"v": "文本"}`；空单元格是 null。
fn cell_text(cell: &Value) -> String {
    if cell.is_null() {
        return String::new();
    }
    match cell.get("v") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gviz_payload() {
        // 真实结构：第 0 行的 c[1..] 是语言列名，之后才是数据行
        let body = r#"/*O_o*/
google.visualization.Query.setResponse({"table":{"cols":[],"rows":[
{"c":[{"v":"Key"},{"v":"Korean"},{"v":"English"}]},
{"c":[{"v":"Default"},{"v":"기본"},{"v":"Default"}]},
{"c":[{"v":"ModBranch"},{"v":"모드 분기"},{"v":"Mod Branch"}]}
]}});"#;
        let sheet = parse(body, "JALib").unwrap();
        assert_eq!(sheet.languages, vec!["Korean", "English"]);
        assert_eq!(sheet.rows.len(), 2);
        assert_eq!(sheet.rows[0].key, "Default");
        assert_eq!(sheet.rows[1].key, "ModBranch");
        assert_eq!(sheet.reference(&sheet.rows[1]), "Mod Branch");
        // 空单元格不应报错
        let sparse = r#"{"table":{"rows":[{"c":[{"v":"Key"},{"v":"English"}]},{"c":[{"v":"K"},null]}]}}"#;
        let sheet = parse(sparse, "JALib").unwrap();
        assert_eq!(sheet.rows[0].key, "K");
        assert_eq!(sheet.reference(&sheet.rows[0]), "");
    }
}
