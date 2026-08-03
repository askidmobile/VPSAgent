//! Кастомные slash-команды (FR-043).
//!
//! Markdown-файлы с frontmatter в каталогах команд (глобальные/проектные).
//! `$ARGUMENTS` в теле команды подставляется аргументами пользователя.
//! Формат:
//! ```md
//! ---
//! description: Краткое описание
//! ---
//! Ты — ... Выполни: $ARGUMENTS
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Slash-команда.
#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub body: String,
    pub source: PathBuf,
}

impl Command {
    /// Распарсить из markdown+frontmatter.
    pub fn parse(name: &str, content: &str, source: PathBuf) -> anyhow::Result<Command> {
        let (frontmatter, body) = split_frontmatter(content);
        let meta: CommandMeta = if frontmatter.is_empty() {
            CommandMeta { description: String::new() }
        } else {
            serde_yaml::from_str(&frontmatter)
                .map_err(|e| anyhow::anyhow!("некорректный frontmatter в {source:?}: {e}"))?
        };
        Ok(Command {
            name: name.trim_start_matches('/').to_string(),
            description: meta.description,
            body: body.trim().to_string(),
            source,
        })
    }

    /// Тело с подставленными $ARGUMENTS.
    pub fn expand(&self, args: &str) -> String {
        self.body.replace("$ARGUMENTS", args)
    }
}

#[derive(Debug, Deserialize)]
struct CommandMeta {
    #[serde(default)]
    description: String,
}

/// Разделить frontmatter и body.
fn split_frontmatter(content: &str) -> (String, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (String::new(), content.to_string());
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut end = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end = Some(i);
            break;
        }
    }
    match end {
        Some(end) => (lines[1..end].join("\n"), lines[end + 1..].join("\n")),
        None => (String::new(), content.to_string()),
    }
}

/// Реестр slash-команд.
#[derive(Default, Clone)]
pub struct CommandRegistry {
    pub commands: Vec<Command>,
}

impl CommandRegistry {
    pub fn load_defaults() -> Self {
        let mut dirs = vec![];
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".vpsagent/commands"));
        }
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(cwd.join(".vpsagent/commands"));
        }
        Self::load_from(&dirs)
    }

    pub fn load_from(dirs: &[PathBuf]) -> Self {
        let mut commands = vec![];
        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let path = entry.path();
                    if path.extension().map(|e| e == "md").unwrap_or(false) {
                        let name = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Ok(cmd) = Command::parse(&name, &content, path) {
                                commands.push(cmd);
                            }
                        }
                    }
                }
            }
        }
        Self { commands }
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.commands
            .iter()
            .find(|c| c.name == name.trim_start_matches('/'))
    }

    pub fn names(&self) -> Vec<&str> {
        self.commands.iter().map(|c| c.name.as_str()).collect()
    }
}

// Использование Path для полноты API.
#[allow(dead_code)]
fn _ensure_path(_: &Path) {}