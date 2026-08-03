//! Определения субагентов (markdown + frontmatter, совместимо с `~/.claude/agents`).
//!
//! Формат файла (YAML frontmatter + body — системный промпт):
//! ```md
//! ---
//! name: code-reviewer
//! description: Ревьюер кода. Находит баги и проблемы.
//! tools: [read, grep, shell]
//! model: gpt-4o            # опционально
//! fork: false              # опционально: наследовать контекст родителя
//! ---
//! Ты — ревьюер кода...
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Определение типа субагента.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub prompt: String,
    /// Разрешённые инструменты (suffix: пусто = все).
    pub tools: Vec<String>,
    pub model: Option<String>,
    /// Наследовать контекст родителя (по мотивам codex-rs SpawnAgentForkMode).
    pub fork: bool,
    /// Источник файла.
    pub source: PathBuf,
}

impl AgentDefinition {
    /// Распарсить из markdown+frontmatter.
    pub fn parse(content: &str, source: PathBuf) -> anyhow::Result<Self> {
        let (frontmatter, body) = split_frontmatter(content)?;
        let meta: AgentMeta = serde_yaml::from_str(&frontmatter)
            .map_err(|e| anyhow::anyhow!("некорректный frontmatter в {source:?}: {e}"))?;
        if meta.name.is_empty() {
            anyhow::bail!("frontmatter в {source:?} не содержит 'name'");
        }
        Ok(Self {
            name: meta.name,
            description: meta.description.unwrap_or_default(),
            prompt: body.trim().to_string(),
            tools: meta.tools.unwrap_or_default(),
            model: meta.model,
            fork: meta.fork.unwrap_or(false),
            source,
        })
    }

    /// Загрузить определение из файла.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content, path.to_path_buf())
    }
}

/// Mетаданные frontmatter.
#[derive(Debug, Deserialize)]
struct AgentMeta {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    fork: Option<bool>,
}

/// Разделить frontmatter (между --- и ---) и body. Возвращает строки.
fn split_frontmatter(content: &str) -> anyhow::Result<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        // Нет frontmatter — весь файл = промпт.
        return Ok((String::new(), content.to_string()));
    }
    // Найдём конец первой --- и начало второй.
    let lines: Vec<&str> = content.lines().collect();
    // Первая строка должна быть "---".
    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end_idx = Some(i);
            break;
        }
    }
    let end_idx = end_idx.ok_or_else(|| anyhow::anyhow!("frontmatter не закрыт: нет второй '---'"))?;
    // frontmatter = строки 1..=end_idx-1 (исключая --- строки).
    let frontmatter = lines[1..end_idx].join("\n");
    // body = всё после второй --- (end_idx + 1...).
    let body = lines[end_idx + 1..].join("\n");
    Ok((frontmatter, body))
}

/// Реестр определений из каталогов (~/.claude/agents, .vpsagent/agents, ./.vpsagent/agents).
pub struct DefinitionRegistry {
    pub defs: Vec<AgentDefinition>,
}

impl DefinitionRegistry {
    /// Загрузить все определения из стандартных каталогов.
    pub fn load_defaults() -> Self {
        let mut dirs = vec![];
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".claude/agents"));
            dirs.push(home.join(".vpsagent/agents"));
        }
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(cwd.join(".vpsagent/agents"));
        }
        Self::load_from(&dirs)
    }

    pub fn load_from(dirs: &[PathBuf]) -> Self {
        let mut defs = vec![];
        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let path = entry.path();
                    if path.extension().map(|e| e == "md").unwrap_or(false) {
                        if let Ok(def) = AgentDefinition::load(&path) {
                            defs.push(def);
                        } else {
                            tracing::warn!(path=%path.display(), "не удалось загрузить определение субагента");
                        }
                    }
                }
            }
        }
        Self { defs }
    }

    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.defs.iter().find(|d| d.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.defs.iter().map(|d| d.name.as_str()).collect()
    }
}