//! Скиллы: загрузка SKILL.md (толерантный парсер), реестр, вызов.
//!
//! Формат совместим с экосистемой Claude Code: markdown с YAML frontmatter.
//! Толерантное чтение (D1): обязательны только `name` и `description`;
//! неизвестные поля frontmatter игнорируются с warning.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Скилл.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Тело SKILL.md (инструкции агента).
    pub body: String,
    /// Все raw-поля frontmatter (для расширений в будущем).
    pub frontmatter: std::collections::HashMap<String, serde_json::Value>,
    /// Источник файла.
    pub source: PathBuf,
}

impl Skill {
    /// Парсер: толерантный. name+description обязательны.
    pub fn parse(content: &str, source: PathBuf) -> anyhow::Result<Skill> {
        let (frontmatter_str, body) = split_frontmatter(content);
        let meta: SkillMeta = serde_yaml::from_str(&frontmatter_str)
            .map_err(|e| anyhow::anyhow!("некорректный frontmatter в {source:?}: {e}"))?;
        if meta.name.trim().is_empty() {
            anyhow::bail!("скилл {source:?}: отсутствует 'name'");
        }
        if meta.description.trim().is_empty() {
            anyhow::bail!("скилл {source:?}: отсутствует 'description'");
        }
        let mut frontmatter = std::collections::HashMap::new();
        if let Ok(map) = serde_yaml::from_str::<std::collections::HashMap<String, serde_yaml::Value>>(&frontmatter_str) {
            for (k, v) in map {
                frontmatter.insert(k, serde_json::to_value(&v).unwrap_or(serde_json::Value::Null));
            }
        }
        Ok(Self {
            name: meta.name.trim().to_string(),
            description: meta.description.trim().to_string(),
            body: body.trim().to_string(),
            frontmatter,
            source,
        })
    }

    /// Загрузить из файла.
    pub fn load(path: &Path) -> anyhow::Result<Skill> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content, path.to_path_buf())
    }

    /// Полное описание для индекса системного промпта.
    pub fn index_line(&self) -> String {
        format!("{} — {}", self.name, self.description)
    }
}

/// Frontmatter скилла.
#[derive(Debug, Deserialize)]
struct SkillMeta {
    name: String,
    description: String,
}

/// Разделить frontmatter (между --- и ---) и body.
fn split_frontmatter(content: &str) -> (String, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (String::new(), content.to_string());
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end_idx = Some(i);
            break;
        }
    }
    match end_idx {
        Some(end) => {
            let frontmatter = lines[1..end].join("\n");
            let body = lines[end + 1..].join("\n");
            (frontmatter, body)
        }
        None => (String::new(), content.to_string()),
    }
}

/// Реестр скиллов из каталогов.
#[derive(Default, Clone)]
pub struct SkillRegistry {
    pub skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Загрузить все скиллы из стандартных каталогов.
    pub fn load_defaults() -> Self {
        let mut dirs = vec![];
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".vpsagent/skills"));
        }
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(cwd.join(".vpsagent/skills"));
        }
        Self::load_from(&dirs)
    }

    pub fn load_from(dirs: &[PathBuf]) -> Self {
        let mut skills = vec![];
        for dir in dirs {
            Self::scan_dir(dir, &mut skills);
        }
        Self { skills }
    }

    fn scan_dir(dir: &Path, out: &mut Vec<Skill>) {
        if !dir.is_dir() {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Вложенный SKILL.md (скилл может лежать в подкаталоге).
                    let nested = path.join("SKILL.md");
                    if nested.is_file() {
                        if let Ok(s) = Skill::load(&nested) {
                            out.push(s);
                        }
                        continue;
                    }
                    Self::scan_dir(&path, out);
                } else if path.extension().map(|e| e == "md").unwrap_or(false)
                    && path.file_name().map(|f| f != "SKILL.md").unwrap_or(true)
                {
                    if let Ok(s) = Skill::load(&path) {
                        out.push(s);
                    }
                }
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Индекс для системного промпта.
    pub fn index(&self) -> String {
        self.skills
            .iter()
            .map(|s| s.index_line())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Зарегистрировать новый скилл (самогенерация).
    pub fn register(&mut self, skill: Skill) {
        // Замена по имени.
        if let Some(existing) = self.skills.iter_mut().find(|s| s.name == skill.name) {
            *existing = skill;
        } else {
            self.skills.push(skill);
        }
    }
}