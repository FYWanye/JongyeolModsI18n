//! 全局配置与模组表定义。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// 作者云端表格（与 JALib 源码里的 LOCALIZATION_URL 同一个文档）。
pub const SHEET_DOC: &str =
    "https://docs.google.com/spreadsheets/d/1kx12GMqK9lgpiZimBSAMdj51xY4IuQUSLXzmQFZ6Sk4";

/// 汉化目标模组的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 作者的云端表格（可以 fetch 原文）。
    Sheet { gid: u64 },
    /// 没有云端表格：只能本地编辑与上传（例如上游未给该模组配置 Gid）。
    LocalOnly,
}

/// 汉化目标模组。
#[derive(Debug, Clone, Copy)]
pub struct ModDef {
    pub id: &'static str,
    pub source: Source,
    /// 云端表格里的参考列（供人工比对）
    pub reference: &'static str,
}

pub const MODS: &[ModDef] = &[
    ModDef { id: "JALib", source: Source::Sheet { gid: 1716850936 }, reference: "English" },
    ModDef { id: "JipperResourcePack", source: Source::Sheet { gid: 1313107549 }, reference: "English" },
    // BetterCalibration 的上游 JAModInfo 没有 Gid（JALib 视为 -1，不拉云端），
    // 因此只能本地维护后上传。
    ModDef { id: "BetterCalibration", source: Source::LocalOnly, reference: "Korean" },
];

/// 本项目的 GitHub 仓库（汉化表托管在这里，模组启动时从这里拉取）。
pub const DEFAULT_REPO: &str = "FYWanye/JongyeolModsI18n";
pub const DEFAULT_BRANCH: &str = "main";

/// 汉化表在仓库中的目录。
pub const DATA_DIR: &str = "data";

/// 有序的汉化表：保持插入顺序，便于 git diff 与人工审阅。
pub type Table = IndexMap<String, String>;

/// 云端表格的一行：键 + 各语言取值。
#[derive(Debug, Clone)]
pub struct SheetRow {
    pub key: String,
    /// 语言名 → 文本（如 English / Korean）
    pub values: IndexMap<String, String>,
}

/// 由 Gid 拼出 JALib 实际使用的 gviz 拉取地址。
pub fn sheet_url(gid: u64) -> String {
    format!("{SHEET_DOC}/gviz/tq?tqx=out:json&tq&gid={gid}")
}

/// 汉化表在 GitHub 上的 raw 地址（模组运行时也读这个）。
pub fn raw_url(repo: &str, branch: &str, mod_id: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/{repo}/{branch}/{DATA_DIR}/{mod_id}.ChineseSimplified.json"
    )
}

pub fn find_mod(mod_id: &str) -> Option<&'static ModDef> {
    MODS.iter().find(|def| def.id == mod_id)
}

/// 仅返回有云端表格的模组 Id。
pub fn sheet_backed_ids() -> Vec<&'static str> {
    MODS.iter()
        .filter(|def| matches!(def.source, Source::Sheet { .. }))
        .map(|def| def.id)
        .collect()
}

/// 配置 + 本地状态，保存在仓库之外，避免把令牌提交上去。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    /// GitHub 个人访问令牌（需要 repo 权限）。留空时回退到环境变量 GITHUB_TOKEN。
    #[serde(default)]
    pub token: Option<String>,
    /// 网络代理，例如 http://127.0.0.1:7897
    #[serde(default)]
    pub proxy: Option<String>,
}

impl State {
    pub fn dir() -> PathBuf {
        if let Ok(explicit) = std::env::var("I18N_EDITOR_HOME") {
            return PathBuf::from(explicit);
        }
        #[cfg(windows)]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                return Path::new(&appdata).join("JongyeolModsI18n");
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return Path::new(&xdg).join("jongyeol-mods-i18n");
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        Path::new(&home).join(".config").join("jongyeol-mods-i18n")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        let mut state: State = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if state.repo.is_none() {
            state.repo = Some(DEFAULT_REPO.into());
        }
        if state.branch.is_none() {
            state.branch = Some(DEFAULT_BRANCH.into());
        }
        state
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("创建配置目录失败：{}", dir.display()))?;
        let path = Self::path();
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("写入配置失败：{}", path.display()))?;
        restrict_permissions(&path);
        Ok(())
    }

    pub fn repo(&self) -> String {
        self.repo.clone().unwrap_or_else(|| DEFAULT_REPO.into())
    }

    pub fn branch(&self) -> String {
        self.branch.clone().unwrap_or_else(|| DEFAULT_BRANCH.into())
    }
}

/// 令牌优先级：环境变量 GITHUB_TOKEN > 配置文件。
pub fn github_token(state: &State) -> Option<String> {
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.trim().is_empty() {
            return Some(token.trim().to_string());
        }
    }
    state
        .token
        .clone()
        .filter(|t| !t.trim().is_empty())
        .map(|t| t.trim().to_string())
}

/// 定位仓库根目录（包含 csproj 的那一层）。
pub fn workspace_root() -> Result<PathBuf> {
    let start = std::env::current_dir().context("无法获取当前目录")?;
    let mut dir = start.clone();
    loop {
        if dir.join("JongyeolModsI18n.csproj").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    let mut dir = start;
    loop {
        if dir.join("Cargo.toml").is_file() && dir.join(DATA_DIR).is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    anyhow::bail!("找不到仓库根目录（请在仓库内运行，或用 --data 指定数据目录）")
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}
