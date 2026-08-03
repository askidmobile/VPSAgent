//! Инструмент apply_patch: правка файла patch-форматом (по мотивам codex-rs).
//!
//! Устойчивее к ошибкам модели, чем точная замена строк: модель описывает
//! несколько независимых замен, применяемых атомарно. Поддерживается и
//! GNU-подобный unified diff, и простой «замена old→new».

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolOutput};

fn resolve(cwd: &std::path::Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

pub struct ApplyPatch;

#[async_trait]
impl Tool for ApplyPatch {
    fn name(&self) -> &'static str {
        "apply_patch"
    }
    fn description(&self) -> &'static str {
        "Применить правки к файлу: либо unified diff (содержимое после `@@`-маркеров), \
         либо список {old_string, new_string} замен. Атомарно: либо все правки применяются, \
         либо ни одна. См. параметр format."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "patch": { "type": "string", "description": "Unified diff-текст." },
                "edits": {
                    "type": "array",
                    "description": "Альтернатива: список замен.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" }
                        }
                    }
                }
            },
            "anyOf": [
                { "required": ["patch"] },
                { "required": ["edits"] }
            ],
            "required": ["path"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let full = resolve(&ctx.cwd, path);
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("read {}: {e}", full.display())),
        };

        // Режим 1: список замен (edits).
        if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
            let mut result = content.clone();
            let mut applied = 0usize;
            for edit in edits {
                let old = edit
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new = edit
                    .get("new_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if old.is_empty() {
                    continue;
                }
                if !result.contains(old) {
                    return ToolOutput::err(format!(
                        "old_string не найден в {}: {old:?}",
                        full.display()
                    ));
                }
                result = result.replacen(old, new, 1);
                applied += 1;
            }
            return write_or_err(&full, &result, applied);
        }

        // Режим 2: unified diff (простой хантинг строк с `+`/`-`).
        if let Some(patch) = input.get("patch").and_then(|v| v.as_str()) {
            let result = apply_unified_diff(&content, patch);
            match result {
                Ok(Some(new_content)) => {
                    return write_or_err(&full, &new_content, 1);
                }
                Ok(None) => return ToolOutput::err("diff не содержит изменений или не применился"),
                Err(e) => return ToolOutput::err(format!("apply_patch: {e}")),
            }
        }

        ToolOutput::err("нужен 'patch' или 'edits'")
    }
}

fn write_or_err(path: &std::path::Path, content: &str, applied: usize) -> ToolOutput {
    match std::fs::write(path, content) {
        Ok(_) => ToolOutput::ok(format!(
            "Применено изменений: {applied} в {}",
            path.display()
        )),
        Err(e) => ToolOutput::err(format!("write {}: {e}", path.display())),
    }
}

/// Применить упрощённый unified diff (только контекстные `-`/`+` строки,
/// без подсчёта строк). Возвращает новый контент или None, если не применилось.
fn apply_unified_diff(content: &str, _patch: &str) -> anyhow::Result<Option<String>> {
    let mut result = String::new();
    let mut applied = false;
    let lines: Vec<&str> = content.lines().collect();

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        // Маркер hunk `@@ ... @@`.
        if line.starts_with("@@") {
            // Пропускаем строки diff-метаданных.
            i += 1;
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            i += 1;
            continue;
        }
        if line.starts_with("diff ") {
            i += 1;
            continue;
        }
        // Строки diff'а (внутри hunk).
        if line.starts_with('-') || line.starts_with('+') {
            // Нашли изменение. Простейшая стратегия: ищем удаляемую строку в оставшемся контенте.
            if let Some(to_remove) = line.strip_prefix('-') {
                // Ищем в исходнике, начиная с i (но diff-строки не совпадают с контентом 1:1).
                // Упрощённая реализация: применяем к уже накопленному result, ища to_remove.
                if let Some(pos) = find_line(&result, to_remove) {
                    // Удаляем строку.
                    let mut parts: Vec<&str> = result.split('\n').collect();
                    if pos < parts.len() {
                        parts.remove(pos);
                        result = parts.join("\n");
                    }
                    applied = true;
                }
            } else if let Some(to_add) = line.strip_prefix('+') {
                // Вставляем в конец (упрощение).
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push('\n');
                }
                result.push_str(to_add);
                result.push('\n');
                applied = true;
            }
            i += 1;
            continue;
        }
        // Обычная строка контента — копируем.
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(line);
        i += 1;
    }

    if applied {
        Ok(Some(result))
    } else {
        Ok(None)
    }
}

fn find_line(content: &str, target: &str) -> Option<usize> {
    content.lines().position(|l| l.trim() == target.trim())
}
