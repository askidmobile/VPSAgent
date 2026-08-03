//! Grep-инструмент: поиск по содержимому файлов (движок `ignore` — ripgrep-семантика).

use async_trait::async_trait;
use ignore::WalkBuilder;
use serde_json::{json, Value};

use super::{Tool, ToolContext, ToolOutput};

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "Поиск по содержимому файлов (ripgrep-семантика: уважает .gitignore). \
         Режимы: content (по умолчанию, выводит строки), files (только имена), \
         count (число совпадений на файл)."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Регулярное выражение (Rust regex)." },
                "path": { "type": "string", "description": "Каталог поиска (по умолчанию cwd).", "default": "." },
                "mode": { "type": "string", "enum": ["content", "files", "count"], "default": "content" },
                "include": { "type": "string", "description": "Glob для имён файлов (например '*.rs')." }
            },
            "required": ["pattern"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let mode = input
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("content");
        let include = input.get("include").and_then(|v| v.as_str());
        let full = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            ctx.cwd.join(path)
        };
        let regex = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("некорректный regex: {e}")),
        };
        let mut walker = WalkBuilder::new(&full);
        walker
            .hidden(true)
            .ignore(true)
            .git_ignore(true)
            .git_exclude(true);
        if let Some(glob) = include {
            // Простой фильтр по расширению/имени через сравнение suffix.
            // (Полноценный glob-оверрайд — Фаза 2 с types/types-matcher.)
            let _ = glob;
        }
        let mut out = String::new();
        let mut total = 0usize;
        for entry in walker.build() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            // Дополнительный фильтр по include-glob (post-walk, простая проверка suffix/eq).
            if let Some(glob) = include {
                let name = entry.file_name().to_string_lossy().to_string();
                if !matches_glob(glob, &name) {
                    continue;
                }
            }
            let path = entry.path();
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut file_count = 0usize;
            for (i, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    file_count += 1;
                    total += 1;
                    if mode == "content" && total <= 200 {
                        out.push_str(&format!("{}:{}: {}\n", path.display(), i + 1, line));
                    }
                }
            }
            if mode == "files" && file_count > 0 {
                out.push_str(&format!("{}\n", path.display()));
            } else if mode == "count" && file_count > 0 {
                out.push_str(&format!("{}: {}\n", path.display(), file_count));
            }
            if total > 500 {
                out.push_str("\n(обрезано, >500 совпадений)\n");
                break;
            }
        }
        if out.is_empty() {
            ToolOutput::ok("(нет совпадений)")
        } else {
            ToolOutput::ok(out)
        }
    }
}

/// Простейший glob-матчер: поддерживает `*.ext` и точное имя.
fn matches_glob(pattern: &str, name: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*") {
        name.ends_with(suffix)
    } else {
        pattern == name
    }
}
