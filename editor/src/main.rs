//! Jongyeol's Mods I18n 汉化列表编辑器（跨平台 CLI + 交互式菜单）
//!
//! 主要工作流：
//!   拉取云端原文 → 导出待翻译内容 → 贴给 AI 翻译 → 把 AI 结果贴回来导入 → 上传 GitHub
//!
//! 也保留逐条手动汉化（`edit`）与非交互改单条（`save`）。

mod config;
mod edit;
mod github;
mod http;
mod sheet;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use config::{State, Table};

const HELP: &str = r#"Jongyeol's Mods I18n 汉化列表编辑器

用法：
  i18n-editor [全局选项] <命令> [参数]
  i18n-editor                 不带参数运行会进入交互式菜单（双击 exe 即可）

推荐的 AI 翻译工作流：
  i18n-editor export --only                   导出待翻译内容（默认写到 out\待翻译.txt）
  i18n-editor export --only --stdout          直接打印到终端，方便复制
       ↓ 把内容整段贴给 AI，让它按说明返回译文
  i18n-editor import                          把 AI 的回复整段粘贴进来，回车后按 Ctrl+Z 再回车结束
  i18n-editor push                            上传到 GitHub

命令：
  menu                          进入交互式菜单
  init                          初始化配置（写入 GitHub 仓库/代理等信息）
  auth [token]                  校验并保存 GitHub 令牌（不写参数则只校验现有令牌）
  status                        查看各模组汉化进度
  fetch [模组...] [--base]      从作者云端表格拉取原文（--base 同时导出原文兜底表）
  export [模组...] [选项]       导出配置/待翻译内容，便于贴给 AI
  import [模组] [--file <路径>] 导入译文（从 stdin 粘贴，或从文件读取）
  edit <模组>                   逐条手动汉化（回车跳过、= 用原文、:q 退出并保存）
  save <模组> <键> [译文]       非交互修改/查看单条（不给译文则打印当前值）
  push [模组...]                把本地汉化表提交到 GitHub
  pull [模组...]                从 GitHub 覆盖本地汉化表
  data [模组]                   打印汉化表 JSON 到 stdout
  info                          显示当前配置与数据文件路径

export 选项：
  --only                        只导出未翻译或为空的条目（给 AI 翻译时用这个）
  --json                        以 JSON 输出（默认；AI 易解析）
  --out <文件>                  写入指定文件（默认 out\<模组>-<类型>.txt）
  --stdout                      直接打印到终端，不写文件
  --all                         全部模组（默认）

import 说明：
  能自动跳过 Markdown 代码围栏与说明文字，并从以下任一格式提取译文：
    - 本工具导出的 JSON（{"key": "译文", ...} 或 {"translations": {...}}）
    - 纯键值 JSON 对象
    - TSV（每行 键<TAB>译文）
  粘贴结束后按 Ctrl+Z 再回车（Windows）或 Ctrl+D（Linux/macOS）表示结束。

全局选项：
  --repo <owner/name>           GitHub 仓库（默认 FYWanye/JongyeolModsI18n）
  --branch <name>               分支（默认 main）
  --proxy <url>                 网络代理，如 http://127.0.0.1:7897
  --data <目录>                 数据目录（默认 <仓库根>/data）
  --token <token>               GitHub 令牌（优先用环境变量 GITHUB_TOKEN）
  --base                        fetch 时同时导出 <模组Id>.BaseEnglish.json（原文兜底表）

示例：
  i18n-editor export --only --stdout
  i18n-editor import --file out\JALib-待翻译.txt
  i18n-editor edit JipperResourcePack
  i18n-editor push
"#;

/// 给 AI 的翻译说明（随导出的 JSON 一起输出）。
const AI_PROMPT: &str = r#"你是专业的游戏模组本地化译者。请把下面 JSON 里每个条目的 "text" 翻译成简体中文，
并把结果写入同一条目的 "zh" 字段，保持 JSON 结构与键名完全不变。

要求：
1. 只输出 JSON，不要输出任何解释文字，也不要用 Markdown 代码围栏；
2. 不要增删条目、不要改动键名（key 原样保留）；
3. 术语参考：Perfect=完美、XPerfect=完美+、EP=稍快、LP=稍慢、Calibration=校准、
   Timing=时序、Checkpoint=检查点、KeyViewer=键显、Combo=连击、Best=最佳、Attempt=尝试；
   TBPM/CBPM/KPS/FPS/MS 等缩写保留英文；
4. "text" 里可能含有 \n 与 <color=...> 之类的富文本标记，请原样保留；
5. 字符串里的 {0}、{1} 等占位符必须原样保留，位置可调整；
6. 如果某条 "text" 已经是中文，请原样填入 "zh"。
"#;

#[derive(Default, Clone)]
struct Args {
    repo: Option<String>,
    branch: Option<String>,
    proxy: Option<String>,
    data: Option<PathBuf>,
    token: Option<String>,
    /// fetch 时额外导出原文兜底表
    base: bool,
    /// export：只导出未翻译/为空的条目
    only: bool,
    /// export：以 JSON 输出（目前默认就是 JSON）
    json: bool,
    /// export：输出文件
    out: Option<PathBuf>,
    /// export/import：走 stdout/stdin，而不是文件
    stdout: bool,
    /// import：从文件读取
    file: Option<PathBuf>,
    command: Option<String>,
    rest: Vec<String>,
}

impl Args {
    fn for_mod(&self, mod_id: &str) -> Args {
        let mut clone = self.clone();
        clone.rest = vec![mod_id.to_string()];
        clone
    }
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--repo" => args.repo = iter.next(),
            "--branch" => args.branch = iter.next(),
            "--proxy" => args.proxy = iter.next(),
            "--data" => args.data = iter.next().map(PathBuf::from),
            "--token" => args.token = iter.next(),
            "--out" => args.out = iter.next().map(PathBuf::from),
            "--file" => args.file = iter.next().map(PathBuf::from),
            "--base" => args.base = true,
            "--only" => args.only = true,
            "--json" => args.json = true,
            "--stdout" => args.stdout = true,
            other if other.starts_with('-') => {
                anyhow::bail!("未知选项：{other}（用 --help 查看用法）")
            }
            other => {
                if args.command.is_none() {
                    args.command = Some(other.to_string());
                } else {
                    args.rest.push(other.to_string());
                }
            }
        }
    }
    Ok(args)
}

fn apply_overrides(state: &mut State, args: &Args) {
    if args.repo.is_some() {
        state.repo = args.repo.clone();
    }
    if args.branch.is_some() {
        state.branch = args.branch.clone();
    }
    if args.proxy.is_some() {
        state.proxy = args.proxy.clone();
    }
    if args.token.is_some() {
        state.token = args.token.clone();
    }
}

/// 解析要处理的模组列表；未指定则返回全部（含仅本地维护的）。
fn target_mods(rest: &[String]) -> Result<Vec<String>> {
    if rest.is_empty() {
        return Ok(config::MODS.iter().map(|def| def.id.to_string()).collect());
    }
    let mut mods = Vec::new();
    for name in rest {
        if config::find_mod(name).is_none() {
            anyhow::bail!("未知模组：{name}（可用：{}）", all_mod_ids());
        }
        mods.push(name.clone());
    }
    Ok(mods)
}

fn all_mod_ids() -> String {
    config::MODS
        .iter()
        .map(|def| def.id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// fetch 只能处理有云端表格的模组。
fn sheet_mods(rest: &[String]) -> Result<Vec<String>> {
    if rest.is_empty() {
        return Ok(config::sheet_backed_ids()
            .into_iter()
            .map(|id| id.to_string())
            .collect());
    }
    let mut mods = Vec::new();
    for name in rest {
        match config::find_mod(name) {
            Some(def) if matches!(def.source, config::Source::Sheet { .. }) => {
                mods.push(name.clone())
            }
            Some(_) => anyhow::bail!(
                "{name} 没有云端表格，无法 fetch（可用 {}）",
                sheet::available()
            ),
            None => anyhow::bail!("未知模组：{name}（可用：{}）", all_mod_ids()),
        }
    }
    Ok(mods)
}

fn root(args: &Args) -> Result<PathBuf> {
    match &args.data {
        Some(dir) => Ok(dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))),
        None => config::workspace_root(),
    }
}

fn data_dir(args: &Args, root: &Path) -> PathBuf {
    args.data.clone().unwrap_or_else(|| root.join(config::DATA_DIR))
}

fn table_path(args: &Args, root: &Path, mod_id: &str) -> PathBuf {
    data_dir(args, root).join(format!("{mod_id}.ChineseSimplified.json"))
}

fn load_table(args: &Args, root: &Path, mod_id: &str) -> Result<Table> {
    let path = table_path(args, root, mod_id);
    if !path.is_file() {
        return Ok(Table::new());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("读取失败：{}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("解析 JSON 失败：{}", path.display()))
}

fn save_table(args: &Args, root: &Path, mod_id: &str, table: &Table) -> Result<PathBuf> {
    let dir = data_dir(args, root);
    std::fs::create_dir_all(&dir)?;
    let path = table_path(args, root, mod_id);
    let mut text = serde_json::to_string_pretty(table)?;
    text.push('\n');
    std::fs::write(&path, text).with_context(|| format!("写入失败：{}", path.display()))?;
    Ok(path)
}

/// 原文兜底表（AI 翻译时作为参考；只有云端表格存在的模组才有）。
fn base_table(args: &Args, root: &Path, mod_id: &str) -> Table {
    let path = data_dir(args, root).join(format!("{mod_id}.BaseEnglish.json"));
    if !path.is_file() {
        return Table::new();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn main() -> ExitCode {
    prepare_console();
    let bare = std::env::args().len() <= 1;
    let code = match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("\n错误：{err:#}");
            ExitCode::FAILURE
        }
    };
    // 双击运行（无参数）时控制台会随进程退出而关闭，等一次按键避免"一闪而过"
    if bare && is_console_attached() {
        pause();
    }
    code
}

/// Windows 控制台默认代码页可能不是 UTF-8，中文会显示成乱码。
fn prepare_console() {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "chcp", "65001"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // SAFETY: 只设置当前控制台代码页
        unsafe {
            SetConsoleOutputCP(65001);
            SetConsoleCP(65001);
        }
    }
}

#[cfg(windows)]
extern "system" {
    fn SetConsoleOutputCP(code_page: u32) -> i32;
    fn SetConsoleCP(code_page: u32) -> i32;
    fn GetConsoleWindow() -> isize;
}

/// 是否挂在控制台上（双击 .exe 为 true；输出被重定向时为 false）。
fn is_console_attached() -> bool {
    #[cfg(windows)]
    {
        unsafe { GetConsoleWindow() != 0 }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

fn pause() {
    println!("\n按回车键关闭窗口…");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn run() -> Result<()> {
    let args = parse_args()?;
    let mut state = State::load();
    apply_overrides(&mut state, &args);
    let root = root(&args)?;

    let Some(command) = args.command.clone() else {
        return interactive_menu(&mut state, &args, &root);
    };

    match command.as_str() {
        "help" => {
            print!("{HELP}");
            Ok(())
        }
        "menu" => interactive_menu(&mut state, &args, &root),
        "init" => cmd_init(&state, &args, &root),
        "auth" => cmd_auth(&mut state, &args),
        "info" => cmd_info(&state, &args, &root),
        "status" => cmd_status(&args, &root),
        "fetch" => cmd_fetch(&state, &args, &root),
        "export" => cmd_export(&args, &root, state.proxy.as_deref()),
        "import" => cmd_import(&args, &root),
        "edit" => cmd_edit(&state, &args, &root, true),
        "review" => cmd_edit(&state, &args, &root, false),
        "save" => cmd_save(&args, &root),
        "push" => cmd_push(&state, &args, &root, false),
        "pull" => cmd_push(&state, &args, &root, true),
        "data" => cmd_data(&args, &root),
        other => anyhow::bail!("未知命令：{other}（用 --help 查看用法）"),
    }
}

// --------------------------------------------------------------------- 交互式菜单

fn interactive_menu(state: &mut State, args: &Args, root: &Path) -> Result<()> {
    loop {
        println!("\n============================================================");
        println!("  Jongyeol's Mods I18n —— 汉化列表编辑器");
        println!("============================================================");
        println!("  仓库     : {}", state.repo());
        println!(
            "  代理     : {}",
            state.proxy.as_deref().unwrap_or("（未设置）")
        );
        println!(
            "  令牌     : {}",
            if config::github_token(state).is_some() {
                "已配置"
            } else {
                "未配置（上传需要）"
            }
        );
        println!("  数据目录 : {}", data_dir(args, root).display());
        println!("------------------------------------------------------------");
        let mut total_todo = 0;
        for def in config::MODS {
            let table = load_table(args, root, def.id).unwrap_or_default();
            let done = table
                .values()
                .filter(|value| !value.trim().is_empty())
                .count();
            let todo = table.len() - done;
            total_todo += todo;
            let origin = match def.source {
                config::Source::Sheet { .. } => "云端可拉取",
                config::Source::LocalOnly => "仅本地",
            };
            println!(
                "  {:<20} {:>4}/{:<4} 已翻译   待翻译 {:<4} {}",
                def.id,
                done,
                table.len(),
                todo,
                origin
            );
        }
        println!("------------------------------------------------------------");
        println!(
            "  待翻译总计 {} 条{}",
            total_todo,
            if total_todo == 0 {
                "（如需重译请用 5 导出全部）"
            } else {
                ""
            }
        );
        println!("------------------------------------------------------------");
        println!("  推荐流程：1 → 4 → 5 → 6 → 7");
        println!("   1) 拉取云端原文（新增/更新条目）");
        println!("   2) 查看进度");
        println!("   3) 逐条手动汉化");
        println!("   4) 导出待翻译内容（贴给 AI）");
        println!("   5) 导出全部配置（贴给 AI 重译）");
        println!("   6) 导入译文（粘贴 AI 的回复）");
        println!("   7) 上传到 GitHub");
        println!("   8) 从 GitHub 覆盖本地");
        println!("   9) 查看配置与连通性");
        println!("   0) 退出");
        print!("\n请选择：");
        std::io::stdout().flush()?;

        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            println!();
            return Ok(());
        }
        // 与 edit 一样做输入消毒（管道输入可能带 BOM）
        let choice = edit::normalize_input(&line);

        // 需要额外交互的项单独处理；其余走统一分发（出错只提示，不退出菜单）
        let result: Result<()> = match choice.as_str() {
            "" | "0" | "q" | "Q" => return Ok(()),
            "1" => cmd_fetch(state, args, root),
            "2" => cmd_status(args, root),
            "3" => match pick_mod(args, root)? {
                Some(mod_id) => cmd_edit(state, &args.for_mod(&mod_id), root, true),
                None => Ok(()),
            },
            "4" => export_with_picker(state, args, root, true),
            "5" => export_with_picker(state, args, root, false),
            "6" => import_from_clipboard(args, root),
            "7" => cmd_push(state, args, root, false),
            "8" => cmd_push(state, args, root, true),
            "9" => cmd_info(state, args, root),
            other => {
                println!("未知选项：{other}");
                continue;
            }
        };
        if let Err(err) = result {
            eprintln!("\n操作失败：{err:#}");
        }
    }
}

/// 让用户挑一个模组；回车取消。
fn pick_mod(args: &Args, root: &Path) -> Result<Option<String>> {
    let mods: Vec<&str> = config::MODS.iter().map(|def| def.id).collect();
    println!("\n选择模组：");
    for (index, def) in config::MODS.iter().enumerate() {
        let table = load_table(args, root, def.id).unwrap_or_default();
        let done = table
            .values()
            .filter(|value| !value.trim().is_empty())
            .count();
        println!(
            "  {}) {:<20} 待翻译 {} 条（共 {} 条）",
            index + 1,
            def.id,
            table.len() - done,
            table.len()
        );
    }
    print!("输入序号或模组名（回车取消）：");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let trimmed = edit::normalize_input(&line);
    if trimmed.is_empty() {
        return Ok(None);
    }
    if let Ok(number) = trimmed.parse::<usize>() {
        if (1..=mods.len()).contains(&number) {
            return Ok(Some(mods[number - 1].to_string()));
        }
    }
    if mods.contains(&trimmed.as_str()) {
        return Ok(Some(trimmed));
    }
    println!("无效的选择：{trimmed}");
    Ok(None)
}

/// 菜单里的导出：先选模组，再按"仅待翻译/全部"导出。
fn export_with_picker(
    state: &State,
    args: &Args,
    root: &Path,
    only: bool,
) -> Result<()> {
    let Some(mod_id) = pick_mod(args, root)? else {
        return Ok(());
    };
    let mut sub = args.for_mod(&mod_id);
    sub.only = only;
    cmd_export(&sub, root, state.proxy.as_deref())
}

/// 菜单里的导入：提示粘贴方式，然后从 stdin 读到 EOF。
fn import_from_clipboard(args: &Args, root: &Path) -> Result<()> {
    let Some(mod_id) = pick_mod(args, root)? else {
        return Ok(());
    };
    println!("\n请把 AI 返回的内容整段粘贴到下面，然后：");
    println!("  - Windows：按 Ctrl+Z 再按回车");
    println!("  - Linux/macOS：按 Ctrl+D");
    println!("（若内容里含 ``` 代码围栏，程序会自动忽略）\n");
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let count = apply_translations(args, root, &mod_id, &buf)?;
    report_import(&mod_id, count);
    Ok(())
}

// --------------------------------------------------------------------- 命令实现

fn cmd_init(state: &State, args: &Args, root: &Path) -> Result<()> {
    state.save()?;
    std::fs::create_dir_all(data_dir(args, root))?;
    println!("配置已写入：{}", State::path().display());
    println!("仓库：{}   分支：{}", state.repo(), state.branch());
    match &state.proxy {
        Some(proxy) => println!("代理：{proxy}"),
        None => println!("代理：（未设置）"),
    }
    println!("数据目录：{}", data_dir(args, root).display());
    Ok(())
}

fn cmd_auth(state: &mut State, args: &Args) -> Result<()> {
    if let Some(token) = args.rest.first() {
        state.token = Some(token.clone());
        state.save()?;
        println!("令牌已保存到 {}", State::path().display());
    }
    let login = github::verify_token(state)?;
    println!("令牌有效，登录名：{login}");
    Ok(())
}

fn cmd_info(state: &State, args: &Args, root: &Path) -> Result<()> {
    println!("配置文件   ：{}", State::path().display());
    println!("仓库       ：{}", state.repo());
    println!("分支       ：{}", state.branch());
    println!(
        "代理       ：{}",
        state.proxy.as_deref().unwrap_or("（未设置，必要时加 --proxy）")
    );
    println!(
        "令牌       ：{}",
        if config::github_token(state).is_some() {
            "已配置"
        } else {
            "未配置（push 需要）"
        }
    );
    println!("仓库根目录 ：{}", root.display());
    println!("数据目录   ：{}", data_dir(args, root).display());
    println!("\n各模组汉化表：");
    for def in config::MODS {
        let table = load_table(args, root, def.id).unwrap_or_default();
        let done = table
            .values()
            .filter(|value| !value.trim().is_empty())
            .count();
        let origin = match def.source {
            config::Source::Sheet { gid } => format!("云端 Gid={gid}"),
            config::Source::LocalOnly => "仅本地维护".to_string(),
        };
        println!(
            "  {:<20} {:<16} 参考列={:<8} {} / {} 已翻译",
            def.id,
            origin,
            def.reference,
            done,
            table.len()
        );
        let raw = config::raw_url(&state.repo(), &state.branch(), def.id);
        match http::ping(&raw, state.proxy.as_deref()) {
            Ok(()) => println!("      GitHub 可访问"),
            Err(_) => println!("      GitHub 不可访问（联网失败时模组会回退内嵌译文）"),
        }
    }
    Ok(())
}

fn cmd_status(args: &Args, root: &Path) -> Result<()> {
    println!(
        "{:<22}{:>8}{:>10}{:>10}  来源",
        "模组", "总数", "已翻译", "未翻译"
    );
    for def in config::MODS {
        let table = load_table(args, root, def.id)?;
        let total = table.len();
        let done = table.values().filter(|v| !v.trim().is_empty()).count();
        let origin = match def.source {
            config::Source::Sheet { .. } => "云端可拉取",
            config::Source::LocalOnly => "仅本地",
        };
        println!(
            "{:<22}{:>8}{:>10}{:>10}  {}",
            def.id,
            total,
            done,
            total - done,
            origin
        );
    }
    Ok(())
}

fn cmd_fetch(state: &State, args: &Args, root: &Path) -> Result<()> {
    let mods = sheet_mods(&args.rest)?;
    for mod_id in mods {
        println!("\n=== 拉取 {mod_id} ===");
        let sheet = sheet::fetch(&mod_id, state.proxy.as_deref())?;
        println!(
            "  表格「{}」{} 行，语言列：{}",
            sheet.mod_id,
            sheet.rows.len(),
            sheet.languages.join(", ")
        );

        let mut table = load_table(args, root, &mod_id)?;
        let mut added = 0;
        let mut references = Vec::new();
        for row in &sheet.rows {
            let reference = sheet.reference(row);
            references.push((row.key.clone(), reference));
            if !table.contains_key(&row.key) {
                table.insert(row.key.clone(), String::new());
                added += 1;
            }
        }

        let removed: Vec<String> = table
            .keys()
            .filter(|key| !sheet.rows.iter().any(|row| &row.key == *key))
            .cloned()
            .collect();

        let path = save_table(args, root, &mod_id, &table)?;
        println!("  新增 {added} 条，本地共 {} 条", table.len());
        if !removed.is_empty() {
            println!(
                "  提示：{} 条已不在表格中（保留在本地）：{}",
                removed.len(),
                removed.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            );
        }
        println!("  已保存 {}", path.display());

        if args.base {
            let mut base = Table::new();
            for (key, value) in &references {
                base.insert(key.clone(), value.clone());
            }
            let base_path = data_dir(args, root).join(format!("{mod_id}.BaseEnglish.json"));
            let mut text = serde_json::to_string_pretty(&base)?;
            text.push('\n');
            std::fs::write(&base_path, text)
                .with_context(|| format!("写入失败：{}", base_path.display()))?;
            println!("  原文兜底表已保存 {}", base_path.display());
        }
    }
    println!("\n完成。下一步：i18n-editor export --only");
    Ok(())
}

/// 导出配置：既可直接给 AI 翻译，也可当备份。
///
/// `proxy` 由调用方从已加载的配置里取（命令行 --proxy 会覆盖配置），
/// 不能直接用 args.proxy —— 那只是命令行参数。
fn cmd_export(args: &Args, root: &Path, proxy: Option<&str>) -> Result<()> {
    let mods = target_mods(&args.rest)?;
    let mut written: Vec<PathBuf> = Vec::new();
    let mut printed_any = false;

    for mod_id in &mods {
        let table = load_table(args, root, mod_id)?;
        if table.is_empty() {
            println!("跳过 {mod_id}：本地没有数据（先执行 fetch）");
            continue;
        }
        let mut base = base_table(args, root, mod_id);

        // 原文兜底表是上次 fetch 的快照，可能缺少新条目。
        // 这里按需从云端补一次，保证 AI 拿到的 text 不为空。
        let missing: Vec<String> = table
            .keys()
            .filter(|key| base.get(*key).map(|v| v.trim().is_empty()).unwrap_or(true))
            .cloned()
            .collect();
        if !missing.is_empty() {
            if let Some(def) = config::find_mod(mod_id) {
                if matches!(def.source, config::Source::Sheet { .. }) {
                    match sheet::fetch(mod_id, proxy) {
                        Ok(sheet) => {
                            let mut filled = 0;
                            for row in &sheet.rows {
                                let reference = sheet.reference(row);
                                if reference.trim().is_empty() {
                                    continue;
                                }
                                let need = missing.iter().any(|key| key == &row.key);
                                if need && base.get(&row.key).map(|v| v.trim().is_empty()).unwrap_or(true)
                                {
                                    base.insert(row.key.clone(), reference);
                                    filled += 1;
                                }
                            }
                            if filled > 0 {
                                println!("（{mod_id}：已从云端补全 {filled} 条原文）");
                            }
                        }
                        Err(err) => {
                            println!("（{mod_id}：补全原文失败，部分条目 text 可能为空：{err}）");
                        }
                    }
                }
            }
        }

        // 选出要导出的条目
        let mut items = Vec::new();
        for (key, value) in &table {
            let blank = value.trim().is_empty();
            if args.only && !blank {
                continue; // --only：只导出未翻译的
            }
            let text = if blank {
                base.get(key).cloned().unwrap_or_default()
            } else {
                value.clone()
            };
            let mut item = json!({ "key": key, "text": text, "zh": value });
            if let Some(original) = base.get(key) {
                if !blank {
                    item["original"] = json!(original);
                }
            }
            items.push(item);
        }

        if items.is_empty() {
            println!("{mod_id}：没有需要导出的条目（都已翻译）");
            continue;
        }

        let payload = json!({
            "mod": mod_id,
            "说明": "把每个条目的 text 翻译成简体中文写入 zh 字段；保持结构与键名不变。",
            "count": items.len(),
            "translationPrompt": AI_PROMPT,
            "items": items,
        });
        let body = serde_json::to_string_pretty(&payload)?;

        if args.stdout {
            if printed_any {
                println!();
            }
            println!("{body}");
            printed_any = true;
        } else {
            let name = if args.only {
                format!("{mod_id}-待翻译.json")
            } else {
                format!("{mod_id}-全部.json")
            };
            let path = args.out.clone().unwrap_or_else(|| root.join("out").join(name));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &body)
                .with_context(|| format!("写入失败：{}", path.display()))?;
            written.push(path);
        }
    }

    if !written.is_empty() {
        println!("已导出 {} 个文件：", written.len());
        for path in &written {
            println!("  {}", path.display());
        }
        println!(
            "\n下一步：打开文件，把**整份内容**贴给 AI（文件里已包含翻译说明），\n\
             再把 AI 的回复用 `i18n-editor import` 粘贴回来。"
        );
    }
    Ok(())
}

fn cmd_import(args: &Args, root: &Path) -> Result<()> {
    let mod_id = match args.rest.first() {
        Some(id) => {
            if config::find_mod(id).is_none() {
                anyhow::bail!("未知模组：{id}（可用：{}）", all_mod_ids());
            }
            id.clone()
        }
        None => config::MODS
            .iter()
            .map(|def| def.id.to_string())
            .next()
            .context("没有可用的模组")?,
    };

    let payload = match &args.file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("读取失败：{}", path.display()))?,
        None => {
            if is_console_attached() && !args.stdout {
                println!("请粘贴译文内容，结束后：");
                println!("  - Windows：按 Ctrl+Z 再按回车");
                println!("  - Linux/macOS：按 Ctrl+D\n");
            }
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };

    let count = apply_translations(args, root, &mod_id, &payload)?;
    report_import(&mod_id, count);
    Ok(())
}

fn report_import(mod_id: &str, count: usize) {
    if count == 0 {
        println!("\n没有导入任何译文（可能是格式不对，或所有条目都未变化）。");
    } else {
        println!(
            "\n已导入 {count} 条到 {mod_id}。\n下一步：i18n-editor push 上传到 GitHub。"
        );
    }
}

// --------------------------------------------------------------------- 导入解析

/// 从任意"AI 回复文本"里提取译文并写回本地表，返回实际更新的条数。
fn apply_translations(args: &Args, root: &Path, mod_id: &str, payload: &str) -> Result<usize> {
    let entries = extract_translations(payload);
    if entries.is_empty() {
        return Ok(0);
    }
    let mut table = load_table(args, root, mod_id)?;
    let mut updated = 0;
    let mut unknown = 0;
    for (key, value) in entries {
        if !table.contains_key(&key) {
            unknown += 1;
            continue; // 不认识的键：忽略，避免误加
        }
        if table.get(&key).map(|old| old == &value).unwrap_or(false) {
            continue;
        }
        table.insert(key, value);
        updated += 1;
    }
    if unknown > 0 {
        println!("（有 {unknown} 条键名不在本地表中，已忽略）");
    }
    // 没有实际变化就不要重写文件
    if updated > 0 {
        save_table(args, root, mod_id, &table)?;
    }
    Ok(updated)
}

/// 容错解析：剥掉 Markdown 围栏与说明文字，然后尝试 JSON / 文本块 / TSV 三种格式。
fn extract_translations(payload: &str) -> Vec<(String, String)> {
    let cleaned = strip_code_fences(payload)
        .replace('\u{feff}', "")
        .replace("\r\n", "\n");

    // 1) 严格 JSON
    if let Some(entries) = parse_json_pairs(&cleaned) {
        if !entries.is_empty() {
            return entries;
        }
    }

    // 2) 取第一个 { 到最后一个 } 之间的子串再试（AI 常在前言/后记里包 JSON）
    if let (Some(start), Some(end)) = (cleaned.find('{'), cleaned.rfind('}')) {
        if start < end {
            if let Some(entries) = parse_json_pairs(&cleaned[start..=end]) {
                if !entries.is_empty() {
                    return entries;
                }
            }
        }
    }

    // 3) 文本块：<key> 与 </key>（或 ===key=== 形式）
    let blocks = parse_key_blocks(&cleaned);
    if !blocks.is_empty() {
        return blocks;
    }

    // 4) TSV / key=value
    parse_tsv(&cleaned)
}

/// 去掉 ``` 与 ```json 之类的围栏，只保留内部内容。
fn strip_code_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 解析 JSON：返回 (key, 译文, 原值) 三元组；支持三种形态。
fn collect_json_entries(value: &Value, out: &mut Vec<(String, String, Option<String>)>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_json_entries(item, out);
            }
        }
        Value::Object(map) => {
            // 单条记录：{"key": "...", "zh": "..."}
            if let Some(key) = map.get("key").and_then(|k| k.as_str()) {
                let translated = map
                    .get("zh")
                    .or_else(|| map.get("translation"))
                    .or_else(|| map.get("译文"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(translated) = translated {
                    let original = map
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    out.push((key.to_string(), translated, original));
                    return;
                }
            }
            // 容器：{"translations": {...}} / {"items": [...]} / 平铺的 key→译文
            let mut handled_container = false;
            for field in ["translations", "items", "entries", "data", "result"] {
                if let Some(inner) = map.get(field) {
                    collect_json_entries(inner, out);
                    handled_container = true;
                }
            }
            if handled_container {
                return;
            }
            for (key, entry) in map {
                match entry {
                    Value::String(text) => out.push((key.clone(), text.clone(), None)),
                    Value::Object(_) | Value::Array(_) => {
                        // {"k": {"zh": "..."}} 这种也吃下
                        let mut inner = Vec::new();
                        collect_json_entries(entry, &mut inner);
                        for (inner_key, text, original) in inner {
                            if inner_key == *key {
                                out.push((key.clone(), text, original));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Value::String(_) => {}
        _ => {}
    }
}

fn parse_json_pairs(text: &str) -> Option<Vec<(String, String)>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let mut out = Vec::new();
    collect_json_entries(&value, &mut out);
    if out.is_empty() {
        return None;
    }
    let entries = out
        .into_iter()
        .map(|(key, text, _)| (key, restore_newlines(text)))
        .collect();
    Some(entries)
}

/// 还原被 AI 转义掉的换行：
///   - JSON 里写 `"\\n"` 会解析成字面量 `\n`（两个字符），这里换回真换行；
///   - 只有原本就没有真换行时才处理，避免把正文里的反斜杠+n 误伤。
fn restore_newlines(text: String) -> String {
    if text.contains('\n') {
        return text;
    }
    if text.contains("\\n") {
        return text.replace("\\n", "\n");
    }
    text
}

/// 解析形如下面的文本块（也接受 `<translation>` / `<译文>` 等变体）：
///
/// ```text
/// <key>progress.showFPS</key>
/// <text>Show FPS</text>
/// <zh>显示 FPS</zh>
/// ```
///
/// 既支持「标签独占一行、内容在下一行」的块式写法，也支持 `<key>值</key>` 单行写法。
fn parse_key_blocks(text: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut key: Option<String> = None;
    let mut translated: Option<String> = None;
    let mut current: Option<&'static str> = None;
    let mut buffer = String::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        // 情况 A：单行 <tag>值</tag>
        if let Some((name, value)) = single_line_tag(line) {
            assign_field(Some(name), value, &mut key, &mut translated);
            // <zh> 出现即认为这一条结束
            if matches!(name, "zh" | "translation" | "译文") {
                push_block(&mut result, &mut key, &mut translated);
            }
            continue;
        }

        // 情况 B：开标签（内容在后续行）
        if let Some(name) = opening_tag(line) {
            if let Some(field) = current.take() {
                assign_field(Some(field), buffer.trim().to_string(), &mut key, &mut translated);
            }
            buffer.clear();
            current = Some(name);
            continue;
        }

        // 情况 C：闭标签
        if let Some(name) = closing_tag(line) {
            let field = current.take();
            assign_field(
                field.or(Some(name)),
                buffer.trim().to_string(),
                &mut key,
                &mut translated,
            );
            buffer.clear();
            if matches!(name, "zh" | "translation" | "译文") {
                push_block(&mut result, &mut key, &mut translated);
            }
            continue;
        }

        // 情况 D：===key: xxx=== / ===zh: xxx=== 简易写法
        if let Some(rest) = line.strip_prefix("===") {
            if let Some(rest) = rest.strip_suffix("===") {
                if let Some(value) = rest.strip_prefix("key:") {
                    key = Some(value.trim().to_string());
                } else if let Some(value) = rest.strip_prefix("zh:") {
                    translated = Some(value.trim().to_string());
                    push_block(&mut result, &mut key, &mut translated);
                }
                continue;
            }
        }

        // 情况 E：块内容
        if current.is_some() {
            if !buffer.is_empty() {
                buffer.push('\n');
            }
            buffer.push_str(raw);
        }
    }

    // 收尾
    if let Some(field) = current.take() {
        assign_field(
            Some(field),
            buffer.trim().to_string(),
            &mut key,
            &mut translated,
        );
        push_block(&mut result, &mut key, &mut translated);
    }
    result
}

/// 把当前累积的内容写入对应字段（`<text>`/`<original>` 只是参考，不参与导入）。
fn assign_field(
    field: Option<&str>,
    value: String,
    key: &mut Option<String>,
    translated: &mut Option<String>,
) {
    match field {
        Some("key") => *key = Some(value),
        Some("zh") | Some("translation") | Some("译文") => *translated = Some(value),
        _ => {}
    }
}

fn push_block(
    result: &mut Vec<(String, String)>,
    key: &mut Option<String>,
    translated: &mut Option<String>,
) {
    if let (Some(k), Some(v)) = (key.take(), translated.take()) {
        if !k.is_empty() && !v.is_empty() {
            result.push((k, v));
        }
    } else {
        // 不完整就清掉，避免污染下一条
        *key = None;
        *translated = None;
    }
}

/// 匹配 `<tag>值</tag>` 并返回 (tag, 值)。
fn single_line_tag(line: &str) -> Option<(&'static str, String)> {
    for name in ["key", "original", "text", "zh", "translation", "译文"] {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        if line.starts_with(&open) && line.ends_with(&close) {
            let value = &line[open.len()..line.len() - close.len()];
            return Some((name, value.trim().to_string()));
        }
    }
    None
}

/// 匹配单行开标签 `<tag>`。
fn opening_tag(line: &str) -> Option<&'static str> {
    for name in ["key", "original", "text", "zh", "translation", "译文"] {
        if line == format!("<{name}>") {
            return Some(name);
        }
    }
    None
}

/// 匹配单行闭标签 `</tag>`。
fn closing_tag(line: &str) -> Option<&'static str> {
    for name in ["key", "original", "text", "zh", "translation", "译文"] {
        if line == format!("</{name}>") {
            return Some(name);
        }
    }
    None
}

/// 解析 TSV（键<TAB>译文）或 key=value。
fn parse_tsv(text: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('\t') {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            entries.push((key.to_string(), restore_newlines(value.to_string())));
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if key.contains(' ') || key.is_empty() {
                continue;
            }
            entries.push((key.to_string(), restore_newlines(value.trim().to_string())));
        }
    }
    entries
}

// --------------------------------------------------------------------- 逐条手动汉化

fn cmd_edit(state: &State, args: &Args, root: &Path, interactive: bool) -> Result<()> {
    let mod_id = args
        .rest
        .first()
        .cloned()
        .context("用法：i18n-editor edit <模组>")?;
    let def = config::find_mod(&mod_id)
        .with_context(|| format!("未知模组：{mod_id}（可用：{}）", all_mod_ids()))?;

    // 参考文本：有云端表格就拉（失败只提示）
    let references = match def.source {
        config::Source::Sheet { .. } => match sheet::fetch(&mod_id, state.proxy.as_deref()) {
            Ok(sheet) => sheet
                .rows
                .iter()
                .map(|row| (row.key.clone(), sheet.reference(row)))
                .collect::<Vec<_>>(),
            Err(err) => {
                println!("提示：拉取原文失败（{err}），将只显示键名。");
                Vec::new()
            }
        },
        config::Source::LocalOnly => Vec::new(),
    };

    let mut table = load_table(args, root, &mod_id)?;
    if table.is_empty() {
        for (key, _) in &references {
            table.insert(key.clone(), String::new());
        }
        println!(
            "本地没有 {mod_id} 的汉化表，已按云端表格初始化 {} 条。",
            table.len()
        );
    }
    let table_len = table.len();

    let changed;
    if interactive {
        let mut session = edit::Session::new(&mut table, references);
        session.seek_first_untranslated();
        session.run()?;
        changed = session.changed_count();
    } else {
        for (index, (key, value)) in table.iter().enumerate() {
            println!("[{}/{}] {}", index + 1, table_len, key);
            let reference = references
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            if !reference.is_empty() {
                println!(
                    "  原文: {}",
                    reference.lines().collect::<Vec<_>>().join(" ⏎ ")
                );
            }
            println!(
                "  译文: {}",
                if value.is_empty() { "（未翻译）" } else { value }
            );
        }
        changed = 0;
    }

    let done = table.values().filter(|v| !v.trim().is_empty()).count();
    if changed == 0 {
        println!(
            "\n本次没有修改（进度 {}/{}），未改动 {}",
            done,
            table.len(),
            table_path(args, root, &mod_id).display()
        );
        return Ok(());
    }
    let path = save_table(args, root, &mod_id, &table)?;
    println!(
        "\n已保存 {}（本次修改 {} 条，进度 {}/{}）",
        path.display(),
        changed,
        done,
        table.len()
    );
    Ok(())
}

fn cmd_save(args: &Args, root: &Path) -> Result<()> {
    let mod_id = args
        .rest
        .first()
        .cloned()
        .context("用法：i18n-editor save <模组> <键> [译文]")?;
    if config::find_mod(&mod_id).is_none() {
        anyhow::bail!("未知模组：{mod_id}（可用：{}）", all_mod_ids());
    }
    let key = args
        .rest
        .get(1)
        .cloned()
        .context("用法：i18n-editor save <模组> <键> [译文]")?;
    let value = args
        .rest
        .get(2)
        .cloned()
        .map(|raw| edit::normalize_input(&raw));

    let mut table = load_table(args, root, &mod_id)?;
    match value {
        Some(text) => {
            if text.is_empty() {
                table.shift_remove(&key);
                println!("已删除 {mod_id} / {key}");
            } else {
                table.insert(key.clone(), text.clone());
                println!("已写入 {mod_id} / {key} = {text}");
            }
            let path = save_table(args, root, &mod_id, &table)?;
            println!("已保存 {}", path.display());
        }
        None => match table.get(&key) {
            Some(text) => println!("{mod_id} / {key} = {text}"),
            None => println!("{mod_id} / {key} 不在本地表中"),
        },
    }
    Ok(())
}

fn cmd_data(args: &Args, root: &Path) -> Result<()> {
    if let Some(mod_id) = args.rest.first() {
        let table = load_table(args, root, mod_id)?;
        println!("{}", serde_json::to_string_pretty(&table)?);
        return Ok(());
    }
    let mut all = serde_json::Map::new();
    for def in config::MODS {
        let table = load_table(args, root, def.id)?;
        all.insert(def.id.to_string(), serde_json::to_value(table)?);
    }
    println!("{}", serde_json::to_string_pretty(&all)?);
    Ok(())
}

/// pull = true 时从 GitHub 覆盖本地；否则把本地推到 GitHub。
fn cmd_push(state: &State, args: &Args, root: &Path, pull: bool) -> Result<()> {
    let mods = target_mods(&args.rest)?;
    const KINDS: &[(&str, &str)] = &[
        ("ChineseSimplified", "更新简体中文汉化表"),
        ("BaseEnglish", "更新原文兜底表"),
    ];

    if pull {
        for mod_id in mods {
            for (kind, _) in KINDS {
                let path = format!("{}/{mod_id}.{kind}.json", config::DATA_DIR);
                match github::read_file(state, &path)? {
                    Some(file) => {
                        let table: Table = serde_json::from_str(&file.text)
                            .with_context(|| format!("远端 {path} 不是合法的表"))?;
                        let target = data_dir(args, root).join(format!("{mod_id}.{kind}.json"));
                        std::fs::create_dir_all(data_dir(args, root))?;
                        let mut text = serde_json::to_string_pretty(&table)?;
                        text.push('\n');
                        std::fs::write(&target, text)?;
                        println!("已拉取 {path}（{} 条）→ {}", table.len(), target.display());
                    }
                    None => println!("远端还没有 {path}，跳过"),
                }
            }
        }
        return Ok(());
    }

    for mod_id in mods {
        for (kind, note) in KINDS {
            let local = data_dir(args, root).join(format!("{mod_id}.{kind}.json"));
            if !local.is_file() {
                if *kind == "ChineseSimplified" {
                    println!("跳过 {mod_id}：本地没有 {kind}（先执行 fetch）");
                }
                continue;
            }
            let table: Table = serde_json::from_str(&std::fs::read_to_string(&local)?)
                .with_context(|| format!("解析失败：{}", local.display()))?;
            if table.is_empty() {
                continue;
            }
            let mut text = serde_json::to_string_pretty(&table)?;
            text.push('\n');
            let path = format!("{}/{mod_id}.{kind}.json", config::DATA_DIR);

            if let Some(remote) = github::read_file(state, &path)? {
                if remote.text.trim() == text.trim() {
                    println!("{mod_id}.{kind} 与 GitHub 一致，跳过");
                    continue;
                }
            }
            let message = format!("i18n({mod_id}): {note}");
            github::write_file(state, &path, &text, &message)?;
        }
    }
    println!("\n完成。模组下次启动即可从 GitHub 读取最新汉化。");
    Ok(())
}

// --------------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_exported_items() {
        let payload = r#"{
          "mod": "JALib",
          "items": [
            {"key": "Default", "text": "Default", "zh": "正式版"},
            {"key": "Beta", "text": "Beta", "zh": "测试版"}
          ]
        }"#;
        let entries = extract_translations(payload);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].0, "Beta");
        assert_eq!(entries[1].1, "测试版");
    }

    #[test]
    fn ignores_markdown_fence_and_prose() {
        let payload = "好的，这是翻译结果：\n```json\n{\"Default\": \"正式版\"}\n```\n希望有帮助！";
        let entries = extract_translations(payload);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "正式版");
    }

    #[test]
    fn parses_tag_blocks() {
        let payload = "<key>Default</key>\n<text>Default</text>\n<zh>正式版</zh>\n\n<key>Beta</key>\n<text>Beta</text>\n<zh>测试版</zh>";
        let entries = extract_translations(payload);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "Default");
        assert_eq!(entries[1].1, "测试版");
    }

    #[test]
    fn parses_tsv_and_restores_newlines() {
        let payload = "Default\t正式版\nMulti\t第一行\\n第二行";
        let entries = extract_translations(payload);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1, "正式版");
        assert_eq!(entries[1].1, "第一行\n第二行");
    }

    #[test]
    fn parses_flat_json() {
        let entries = extract_translations(r#"{"A": "甲", "B": "乙"}"#);
        assert_eq!(entries.len(), 2);
    }
}
