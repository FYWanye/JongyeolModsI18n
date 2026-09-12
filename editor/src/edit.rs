//! 交互式汉化会话：逐条显示原文与当前译文，直接输入即可保存。

use std::io::{self, BufRead, Write};

use crate::config;

pub struct Session<'a> {
    pub table: &'a mut config::Table,
    pub references: Vec<(String, String)>,
    pub keys: Vec<String>,
    pub index: usize,
    pub changed: usize,
}

impl<'a> Session<'a> {
    pub fn new(table: &'a mut config::Table, references: Vec<(String, String)>) -> Self {
        let keys = table.keys().cloned().collect();
        Session {
            table,
            references,
            keys,
            index: 0,
            changed: 0,
        }
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
            let input = line.trim_end_matches(['\r', '\n']);

            match input {
                "" => self.index += 1,
                ":q" => break,
                ":n" => self.index = (self.index + 1).min(self.keys.len().saturating_sub(1)),
                ":p" => self.index = self.index.saturating_sub(1),
                "=" => {
                    if reference.is_empty() {
                        println!("（原文为空，忽略）");
                    } else {
                        self.table.insert(key, reference);
                        self.changed += 1;
                        self.index += 1;
                    }
                }
                other => {
                    self.table.insert(key, other.to_string());
                    self.changed += 1;
                    self.index += 1;
                }
            }
        }
        Ok(())
    }
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
    fn finds_first_untranslated() {
        let mut table = config::Table::new();
        table.insert("A".into(), "已翻译".into());
        table.insert("B".into(), "".into());
        table.insert("C".into(), "".into());
        let mut session = Session::new(&mut table, vec![]);
        session.seek_first_untranslated();
        assert_eq!(session.keys[session.index], "B");
    }
}
