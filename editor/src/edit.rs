//! 交互式汉化会话：逐条显示原文与当前译文，直接输入即可保存。

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use crate::config;

pub struct Session<'a> {
    pub table: &'a mut config::Table,
    pub references: Vec<(String, String)>,
    pub keys: Vec<String>,
    pub index: usize,
    /// 被改过的键 → 改动前的值（None 表示原本不存在）。用于精确统计"真正变化的条目数"。
    touched: HashMap<String, Option<String>>,
}

impl<'a> Session<'a> {
    pub fn new(table: &'a mut config::Table, references: Vec<(String, String)>) -> Self {
        let keys = table.keys().cloned().collect();
        Session {
            table,
            references,
            keys,
            index: 0,
            touched: HashMap::new(),
        }
    }

    /// 真正发生内容变化的条目数（不是"输入了几次"）。
    pub fn changed_count(&self) -> usize {
        self.touched
            .iter()
            .filter(|(key, before)| match before {
                Some(old) => self.table.get(*key) != Some(old),
                None => self.table.get(*key).is_some(),
            })
            .count()
    }

    /// 写入并记录改动前的值（只在第一次修改某个键时记录）。
    fn set(&mut self, key: &str, value: String) {
        self.touched
            .entry(key.to_string())
            .or_insert_with(|| self.table.get(key).cloned());
        self.table.insert(key.to_string(), value);
    }

    fn reference_of(&self, key: &str) -> String {
        self.references
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    /// 定位到第一条未翻译的条目。
    pub fn seek_first_untranslated(&mut self) {
        if let Some(pos) = self.keys.iter().position(|key| {
            self.table
                .get(key)
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        }) {
            self.index = pos;
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        let stdin = io::stdin();
        loop {
            if self.index >= self.keys.len() {
                println!("\n已到最后一条。");
                break;
            }
            let key = self.keys[self.index].clone();
            let current = self.table.get(&key).cloned().unwrap_or_default();
            let reference = self.reference_of(&key);

            println!("\n[{}/{}] {}", self.index + 1, self.keys.len(), key);
            if !reference.is_empty() {
                println!("原文: {}", one_line(&reference));
            }
            println!(
                "当前: {}",
                if current.is_empty() { "（未翻译）" } else { current.as_str() }
            );
            print!("输入译文 [回车跳过 / =原文 / :n 下一条 / :p 上一条 / :q 退出]: ");
            io::stdout().flush()?;

            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? == 0 {
                break; // EOF（例如管道输入结束）
            }
            let input = normalize_input(&line);

            match input.as_str() {
                "" => self.index += 1,
                ":q" => break,
                ":n" => self.index = (self.index + 1).min(self.keys.len().saturating_sub(1)),
                ":p" => self.index = self.index.saturating_sub(1),
                "=" => {
                    if reference.is_empty() {
                        println!("（原文为空，忽略）");
                    } else {
                        self.set(&key, reference);
                        self.index += 1;
                    }
                }
                other => {
                    // 与当前值相同就不算改动（避免"输入同一个值"被计成修改）
                    if other == current {
                        self.index += 1;
                    } else {
                        self.set(&key, other.to_string());
                        self.index += 1;
                    }
                }
            }
        }
        Ok(())
    }
}

/// 清洗一行输入：
///   - 去掉行尾的 CR/LF；
///   - **去掉 UTF-8 BOM**（PowerShell 管道、以及从别处复制粘贴都可能带上，
///     否则会把它当成译文写进 JSON）。
pub(crate) fn normalize_input(line: &str) -> String {
    line.trim_end_matches(['\r', '\n'])
        .trim_start_matches('\u{feff}')
        .to_string()
}

/// 把多行原文压成一行，便于在终端里阅读。
fn one_line(text: &str) -> String {
    text.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ⏎ ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_multiline() {
        assert_eq!(one_line("a\n\nb"), "a ⏎ b");
        assert_eq!(one_line("  单行  "), "单行");
    }

    #[test]
    fn strips_bom_and_newlines_from_input() {
        assert_eq!(normalize_input("\u{feff}连击\r\n"), "连击");
        assert_eq!(normalize_input("连击\n"), "连击");
        assert_eq!(normalize_input("\u{feff}\n"), "");
        assert_eq!(normalize_input("\n"), "");
    }

    #[test]
    fn finds_first_untranslated() {
        let mut table = config::Table::new();
        table.insert("A".into(), "已翻译".into());
        table.insert("B".into(), "".into());
        table.insert("C".into(), "".into());
        let mut session = Session::new(&mut table, vec![]);
        session.seek_first_untranslated();
        assert_eq!(session.keys[session.index], "B");
    }

    #[test]
    fn counts_only_real_changes() {
        let mut table = config::Table::new();
        table.insert("A".into(), "旧值".into());
        table.insert("B".into(), "".into());
        let mut session = Session::new(&mut table, vec![]);

        // 写入相同值 → 不算改动
        session.set("A", "旧值".into());
        assert_eq!(session.changed_count(), 0);

        // 写入新值 → 算 1 条
        session.set("A", "新值".into());
        assert_eq!(session.changed_count(), 1);

        // 改回原值 → 又变回 0 条
        session.set("A", "旧值".into());
        assert_eq!(session.changed_count(), 0);

        // 原本为空 → 填写 → 算 1 条
        session.set("B", "填上了".into());
        assert_eq!(session.changed_count(), 1);
    }
}
