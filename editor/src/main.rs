//! Jongyeol's Mods I18n 汉化列表编辑器（跨平台 CLI）
//!
//! 用途：从作者的 Google 表格拉取原文 → 手动汉化 → 存到本地仓库 data/ →
//! 需要时用凭证直接提交到 GitHub。模组启动时会从 GitHub 读取这份列表。

mod config;
mod edit;
mod github;
mod http;
mod sheet;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};

use config::{State, Table};

const HELP: &str = r#"Jongyeol's Mods I18n 汉化列表编辑器

用法：
  i18n-editor [全局选项] <命令> [参数]
  i18n-editor                 不带参数运行会进入交互式菜单（双击 exe 即可）

命令：
  init                          初始化配置（写入 GitHub 仓库/代理等信息）
  auth [token]                  校验并保存 GitHub 令牌（不写参数则只校验现有令牌）
  status                        查看各模组汉化进度
  fetch [模组...]               从作者云端表格拉取原文（默认全部模组）
  edit <模组>                   逐条手动汉化（回车跳过、= 用原文、:q 退出并保存）
  save <模组> <键> [译文]       非交互修改/查看单条（不给译文则打印当前值）
  push [模组...]                把本地汉化表提交到 GitHub
  pull [模组...]                从 GitHub 覆盖本地汉化表
  data [模组]                   打印汉化表 JSON 到 stdout
  info                          显示当前配置与数据文件路径

全局选项：
  --repo <owner/name>           GitHub 仓库（默认 FYWanye/JongyeolModsI18n）
  --branch <name>               分支（默认 main）
  --proxy <url>                 网络代理，如 http://127.0.0.1:7897
  --data <目录>                 数据目录（默认 <仓库根>/data）
  --token <token>               GitHub 令牌（优先用环境变量 GITHUB_TOKEN）
  --base                        fetch 时同时导出 <模组Id>.BaseEnglish.json（原文兜底表）
  -h, --help                    显示本帮助

示例：
  i18n-editor init --repo FYWanye/JongyeolModsI18n --proxy http://127.0.0.1:7897
  i18n-editor fetch
  i18n-editor edit JipperResourcePack
  i18n-editor push
"#;

#[derive(Default, Clone)]
struct Args {
    repo: Option<String>,
    branch: Option<String>,
    proxy: Option<String>,
    data: Option<PathBuf>,
    token: Option<String>,
    /// fetch 时额外导出原文兜底表（<模组Id>.BaseEnglish.json）
    base: bool,
    command: Option<String>,
    rest: Vec<String>,
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
            "--base" => args.base = true,
            other if other.starts_with('-') => anyhow::bail!("未知选项：{other}（用 --help 查看用法）"),
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
            Some(def) if matches!(def.source, config::Source::Sheet { .. }) => mods.push(name.clone()),
            Some(_) => anyhow::bail!("{name} 没有云端表格，无法 fetch（可用 {}）", sheet::available()),
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

fn data_dir(args: &Args, root: &std::path::Path) -> PathBuf {
    args.data.clone().unwrap_or_else(|| root.join(config::DATA_DIR))
}

fn table_path(args: &Args, root: &std::path::Path, mod_id: &str) -> PathBuf {
    data_dir(args, root).join(format!("{mod_id}.ChineseSimplified.json"))
}

fn load_table(args: &Args, root: &std::path::Path, mod_id: &str) -> Result<Table> {
    let path = table_path(args, root, mod_id);
    if !path.is_file() {
        return Ok(Table::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("读取失败：{}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("解析 JSON 失败：{}", path.display()))
}

fn save_table(args: &Args, root: &std::path::Path, mod_id: &str, table: &Table) -> Result<PathBuf> {
    let dir = data_dir(args, root);
    std::fs::create_dir_all(&dir)?;
    let path = table_path(args, root, mod_id);
    let mut text = serde_json::to_string_pretty(table)?;
    text.push('\n');
    std::fs::write(&path, text).with_context(|| format!("写入失败：{}", path.display()))?;
    Ok(path)
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
        // SAFETY: 这两个 API 只设置当前控制台代码页，无副作用
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

    // 没有给命令（例如双击 exe）→ 进入交互式菜单
    let Some(command) = args.command.clone() else {
        return interactive_menu(&mut state, &args, &root);
    };

    match command.as_str() {
        "help" => {
            print!("{HELP}");
            Ok(())
        }
        "init" => cmd_init(&state, &args, &root),
        "auth" => cmd_auth(&mut state, &args),
        "info" => cmd_info(&state, &args, &root),
        "status" => cmd_status(&args, &root),
        "fetch" => cmd_fetch(&state, &args, &root),
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

/// 无参数运行（双击 exe）时进入的菜单：重复显示选项，直到用户选择退出。
fn interactive_menu(state: &mut State, args: &Args, root: &std::path::Path) -> Result<()> {
    loop {
        println!("\n============================================================");
        println!("  Jongyeol's Mods I18n —— 汉化列表编辑器");
        println!("============================================================");
        println!("  仓库     : {}", state.repo());
        println!("  分支     : {}", state.branch());
        println!(
            "  代理     : {}",
            state.proxy.as_deref().unwrap_or("（未设置，联网失败时用 --proxy 指定）")
        );
        println!(
            "  令牌     : {}",
            if config::github_token(state).is_some() { "已配置" } else { "未配置（上传需要）" }
        );
        println!("  仓库根   : {}", root.display());
        println!("  数据目录 : {}", data_dir(args, root).display());
        println!("------------------------------------------------------------");
        for def in config::MODS {
            let table = load_table(args, root, def.id).unwrap_or_default();
            let done = table.values().filter(|value| !value.trim().is_empty()).count();
            let origin = match def.source {
                config::Source::Sheet { .. } => "云端可拉取",
                config::Source::LocalOnly => "仅本地",
            };
            println!(
                "  {:<20} {:>4}/{:<4} 已翻译   {}",
                def.id,
                done,
                table.len(),
                origin
            );
        }
        println!("------------------------------------------------------------");
        println!("   1) 拉取云端原文（生成/更新待翻译条目）");
        println!("   2) 逐条汉化");
        println!("   3) 查看进度");
        println!("   4) 上传到 GitHub");
        println!("   5) 从 GitHub 覆盖本地");
        println!("   6) 查看配置与连通性");
        println!("   0) 退出");
        print!("\n请选择：");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            println!();
            return Ok(()); // 输入结束（管道）
        }
        let choice = line.trim();

        // 2) 逐条汉化需要先选模组；把它放在 match 之前处理
        if choice == "2" {
            match pick_mod(args, root)? {
                Some(mod_id) => {
                    let mut sub = args.clone();
                    sub.rest = vec![mod_id];
                    let _ = cmd_edit(state, &sub, root, true);
                }
                None => println!("（已取消）"),
            }
        } else {
            let result = match choice {
                "" | "0" | "q" | "Q" => return Ok(()),
                "1" => cmd_fetch(state, args, root),
                "3" => cmd_status(args, root),
                "4" => cmd_push(state, args, root, false),
                "5" => cmd_push(state, args, root, true),
                "6" => cmd_info(state, args, root),
                other => {
                    println!("未知选项：{other}");
                    continue;
                }
            };
            // 菜单里出错不该把整个程序带走，只提示一下继续
            if let Err(err) = result {
                eprintln!("\n操作失败：{err:#}");
            }
        }
    }
}

/// 让用户从模组列表里挑一个（用于菜单里的"逐条汉化"）。
fn pick_mod(args: &Args, root: &std::path::Path) -> Result<Option<String>> {
    let mods: Vec<&str> = config::MODS.iter().map(|def| def.id).collect();
    println!("\n选择要汉化的模组：");
    for (index, def) in config::MODS.iter().enumerate() {
        let table = load_table(args, root, def.id).unwrap_or_default();
        let done = table.values().filter(|value| !value.trim().is_empty()).count();
        let missing = table.len() - done;
        println!(
            "  {}) {:<20} 待翻译 {} 条（共 {} 条）",
            index + 1,
            def.id,
            missing,
            table.len()
        );
    }
    print!("输入序号（回车取消）：");
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.parse::<usize>() {
        Ok(number) if number >= 1 && number <= mods.len() => Ok(Some(mods[number - 1].to_string())),
        _ => {
            // 也允许直接输模组名
            if mods.contains(&trimmed) {
                Ok(Some(trimmed.to_string()))
            } else {
                println!("无效的序号：{trimmed}");
                Ok(None)
            }
        }
    }
}

// --------------------------------------------------------------------- 命令实现

fn cmd_init(state: &State, args: &Args, root: &std::path::Path) -> Result<()> {
    state.save()?;
    std::fs::create_dir_all(data_dir(args, root))?;
    println!("配置已写入：{}", State::path().display());
    println!("仓库：{}   分支：{}", state.repo(), state.branch());
    match &state.proxy {
        Some(proxy) => println!("代理：{proxy}"),
        None => println!("代理：（未设置）"),
    }
    println!("数据目录：{}", data_dir(args, root).display());
    println!("\n下一步：i18n-editor fetch   然后  i18n-editor edit <模组>");
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

fn cmd_info(state: &State, args: &Args, root: &std::path::Path) -> Result<()> {
    println!("配置文件   ：{}", State::path().display());
    println!("仓库       ：{}", state.repo());
    println!("分支       ：{}", state.branch());
    println!(
        "代理       ：{}",
        state.proxy.as_deref().unwrap_or("（未设置，必要时加 --proxy）")
    );
    println!(
        "令牌       ：{}",
        if config::github_token(state).is_some() { "已配置" } else { "未配置（push 需要）" }
    );
    println!("仓库根目录 ：{}", root.display());
    println!("数据目录   ：{}", data_dir(args, root).display());
    println!("\n各模组汉化表：");
    for def in config::MODS {
        let path = table_path(args, root, def.id);
        let table = load_table(args, root, def.id).unwrap_or_default();
        let done = table.values().filter(|v| !v.trim().is_empty()).count();
        let origin = match def.source {
            config::Source::Sheet { gid } => format!("云端 Gid={gid}"),
            config::Source::LocalOnly => "仅本地维护".to_string(),
        };
        println!(
            "  {:<20} {:<18} 参考列={:<8} 本地 {} 条  {}",
            def.id,
            origin,
            def.reference,
            done,
            if path.is_file() { "" } else { "（文件缺失）" }
        );
        let raw = config::raw_url(&state.repo(), &state.branch(), def.id);
        match http::ping(&raw, state.proxy.as_deref()) {
            Ok(()) => println!("      GitHub 可访问：{raw}"),
            Err(err) => println!("      GitHub 不可访问（{err}）"),
        }
    }
    Ok(())
}

fn cmd_status(args: &Args, root: &std::path::Path) -> Result<()> {
    println!("{:<22}{:>8}{:>10}{:>10}  来源", "模组", "总数", "已翻译", "未翻译");
    for def in config::MODS {
        let table = load_table(args, root, def.id)?;
        let total = table.len();
        let done = table.values().filter(|v| !v.trim().is_empty()).count();
        let origin = match def.source {
            config::Source::Sheet { .. } => "云端可拉取",
            config::Source::LocalOnly => "仅本地",
        };
        println!("{:<22}{:>8}{:>10}{:>10}  {}", def.id, total, done, total - done, origin);
    }
    Ok(())
}

fn cmd_fetch(state: &State, args: &Args, root: &std::path::Path) -> Result<()> {
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

        // 表格里已删除的键：保留但提示，避免误删已翻译内容
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

        // 可选：导出原文兜底表。模组在「保持原文」时用它取回英文原词，
        // 因为作者表格里没有中文列，云端回退不到英文。
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
    println!("\n完成。下一步：i18n-editor edit <模组>");
    Ok(())
}

fn cmd_edit(state: &State, args: &Args, root: &std::path::Path, interactive: bool) -> Result<()> {
    let mod_id = args
        .rest
        .first()
        .cloned()
        .context("用法：i18n-editor edit <模组>")?;
    let def = config::find_mod(&mod_id)
        .with_context(|| format!("未知模组：{mod_id}（可用：{}）", all_mod_ids()))?;

    // 参考文本：有云端表格就拉（失败只提示），仅本地维护的模组用本地文件里的参考语言
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
        // 本地还没有这个模组的表，先从表格补齐
        for (key, _) in &references {
            table.insert(key.clone(), String::new());
        }
        println!("本地没有 {mod_id} 的汉化表，已按云端表格初始化 {} 条。", table.len());
    }
    let table_len = table.len();

    let changed;
    if interactive {
        let mut session = edit::Session::new(&mut table, references);
        session.seek_first_untranslated();
        session.run()?;
        changed = session.changed_count();
    } else {
        // review：只读浏览，不修改
        for (index, (key, value)) in table.iter().enumerate() {
            println!("[{}/{}] {}", index + 1, table_len, key);
            let reference = references
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            if !reference.is_empty() {
                println!("  原文: {}", reference.lines().collect::<Vec<_>>().join(" ⏎ "));
            }
            println!("  译文: {}", if value.is_empty() { "（未翻译）" } else { value });
        }
        changed = 0;
    }

    // 没有任何实际改动就不要重写文件（避免无意义的文件时间戳变化）
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

fn cmd_save(args: &Args, root: &std::path::Path) -> Result<()> {
    let mod_id = args.rest.first().cloned().context("用法：i18n-editor save <模组> <键> [译文]")?;
    if config::find_mod(&mod_id).is_none() {
        anyhow::bail!("未知模组：{mod_id}（可用：{}）", all_mod_ids());
    }
    let key = args
        .rest
        .get(1)
        .cloned()
        .context("用法：i18n-editor save <模组> <键> [译文]")?;
    let value = args.rest.get(2).cloned().map(|raw| {
        // 与交互式输入同样做消毒：命令行/脚本传入的值也可能带 BOM
        edit::normalize_input(&raw)
    });

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

fn cmd_data(args: &Args, root: &std::path::Path) -> Result<()> {
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
fn cmd_push(state: &State, args: &Args, root: &std::path::Path, pull: bool) -> Result<()> {
    let mods = target_mods(&args.rest)?;
    // 需要同步的文件种类：中文译文 + 原文兜底表（模组「保留原文」时用后者换回英文）
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

            // 内容一致就不产生空提交
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
