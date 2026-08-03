//! Файловые инструменты: read, write, edit, ls.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::{Tool, ToolContext, ToolOutput};

/// Разрешить путь и проверить границы (C2: защита от path traversal).
///
/// Относительный путь резолвится от cwd, затем всё каноникализуется
/// (для `create_parents` сначала создаются родительские каталоги — иначе
/// canonicalize несуществующего файла падает). Пути вне cwd отклоняются,
/// КРОМЕ opt-in списка `ctx.allow_paths`.
fn resolve_checked(
    ctx: &ToolContext,
    p: &str,
    create_parents: bool,
) -> Result<PathBuf, ToolOutput> {
    let raw = PathBuf::from(p);
    let joined = if raw.is_absolute() {
        raw
    } else {
        ctx.cwd.join(raw)
    };

    // Каноникализация: раскрывает `..`, симлинки, относительные сегменты.
    let canonical = if joined.exists() {
        joined.canonicalize()
    } else {
        // Файла нет (write нового): каноникализуем родителя + имя файла.
        let parent = joined.parent().unwrap_or(Path::new("/"));
        if create_parents {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(ToolOutput::err(format!("mkdir {}: {e}", parent.display())));
            }
        }
        match parent.canonicalize() {
            Ok(canon_parent) => {
                let name = joined.file_name().unwrap_or_default();
                return check_bounds(ctx, p, &canon_parent.join(name), &joined);
            }
            Err(e) => return Err(ToolOutput::err(format!("путь '{p}' недоступен: {e}"))),
        }
    };
    let canonical = match canonical {
        Ok(c) => c,
        Err(e) => return Err(ToolOutput::err(format!("путь '{p}' недоступен: {e}"))),
    };
    check_bounds(ctx, p, &canonical, &joined)
}

/// Проверка, что канонический путь внутри cwd или allow_paths.
fn check_bounds(
    ctx: &ToolContext,
    original: &str,
    canonical: &Path,
    _fallback: &Path,
) -> Result<PathBuf, ToolOutput> {
    let root = ctx.cwd.canonicalize().unwrap_or_else(|_| ctx.cwd.clone());
    let in_cwd = canonical.starts_with(&root);
    let in_allow = ctx.allow_paths.iter().any(|allow| {
        let allow_root = allow.canonicalize().unwrap_or_else(|_| allow.clone());
        canonical.starts_with(&allow_root)
    });
    if in_cwd || in_allow {
        Ok(canonical.to_path_buf())
    } else {
        Err(ToolOutput::err(format!(
            "путь '{original}' ({}) вне рабочего каталога {} — доступ запрещён. \
             Добавьте каталог в allow_paths для opt-in доступа.",
            canonical.display(),
            root.display()
        )))
    }
}

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &'static str {
        "read"
    }
    fn description(&self) -> &'static str {
        "Прочитать файл. Возвращает содержимое с нумерацией строк (cat -n стиль)."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Путь к файлу (относительно cwd или абсолютный)." },
                "offset": { "type": "integer", "description": "Начальная строка (1-индексация), по умолчанию 1.", "default": 1 },
                "limit": { "type": "integer", "description": "Сколько строк прочитать (по умолчанию 2000).", "default": 2000 }
            },
            "required": ["path"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let offset = input
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .max(1) as usize;
        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;
        let full = match resolve_checked(ctx, path, false) {
            Ok(p) => p,
            Err(out) => return out,
        };
        match std::fs::read_to_string(&full) {
            Ok(content) => {
                let mut out = String::new();
                for (i, line) in content.lines().enumerate() {
                    let n = i + 1;
                    if n < offset {
                        continue;
                    }
                    if n > offset + limit - 1 {
                        break;
                    }
                    out.push_str(&format!("{n:>6}\t{line}\n"));
                }
                if out.is_empty() {
                    ToolOutput::ok(format!(
                        "Файл пуст или диапазон вне границ: {}",
                        full.display()
                    ))
                } else {
                    ToolOutput::ok(out)
                }
            }
            Err(e) => ToolOutput::err(format!("read {}: {e}", full.display())),
        }
    }
}

pub struct Write;

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        "Записать файл (полная замена содержимого). Создаёт родительские каталоги."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
        // create_parents=true: write нового файла, родителей создаём до canonicalize.
        let full = match resolve_checked(ctx, path, true) {
            Ok(p) => p,
            Err(out) => return out,
        };
        if let Some(parent) = full.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutput::err(format!("mkdir {}: {e}", parent.display()));
            }
        }
        match std::fs::write(&full, content) {
            Ok(_) => ToolOutput::ok(format!(
                "Записан {} ({} байт)",
                full.display(),
                content.len()
            )),
            Err(e) => ToolOutput::err(format!("write {}: {e}", full.display())),
        }
    }
}

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }
    fn description(&self) -> &'static str {
        "Точная замена одной подстроки в файле. old_string должен быть уникален в файле."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string", "description": "Точная строка для замены (с отступами)." },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean", "default": false }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let old = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let full = match resolve_checked(ctx, path, false) {
            Ok(p) => p,
            Err(out) => return out,
        };
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("read {}: {e}", full.display())),
        };
        let count = content.matches(old).count();
        if count == 0 {
            return ToolOutput::err(format!("old_string не найден в {}", full.display()));
        }
        if !replace_all && count > 1 {
            return ToolOutput::err(format!(
                "old_string встречается {count} раз в {}. Уточните (более длинная строка) или replace_all=true.",
                full.display()
            ));
        }
        let new_content = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        if let Err(e) = std::fs::write(&full, &new_content) {
            return ToolOutput::err(format!("write {}: {e}", full.display()));
        }
        ToolOutput::ok(format!(
            "Замена в {} ({} вхождений)",
            full.display(),
            if replace_all { count } else { 1 }
        ))
    }
}

pub struct Ls;

#[async_trait]
impl Tool for Ls {
    fn name(&self) -> &'static str {
        "ls"
    }
    fn description(&self) -> &'static str {
        "Листинг каталога (имена + тип файл/директория)."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Каталог (по умолчанию cwd).", "default": "." }
            }
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let full = match resolve_checked(ctx, path, false) {
            Ok(p) => p,
            Err(out) => return out,
        };
        match std::fs::read_dir(&full) {
            Ok(rd) => {
                let mut entries: Vec<_> = rd
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let kind = if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            "dir"
                        } else {
                            "file"
                        };
                        format!("{kind}\t{}", e.file_name().to_string_lossy())
                    })
                    .collect();
                entries.sort();
                ToolOutput::ok(entries.join("\n"))
            }
            Err(e) => ToolOutput::err(format!("ls {}: {e}", full.display())),
        }
    }
}

pub struct MultiEdit;

#[async_trait]
impl Tool for MultiEdit {
    fn name(&self) -> &'static str {
        "multi_edit"
    }
    fn description(&self) -> &'static str {
        "Серия точных замен в одном файле атомарно. Каждая замена — old_string→new_string."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let edits = input
            .get("edits")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if path.is_empty() || edits.is_empty() {
            return ToolOutput::err("нужны 'path' и непустой 'edits'");
        }
        let full = match resolve_checked(ctx, path, false) {
            Ok(p) => p,
            Err(out) => return out,
        };
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("read {}: {e}", full.display())),
        };
        let mut new_content = content.clone();
        let mut applied = 0usize;
        for edit in &edits {
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
            if new_content.matches(old).count() == 0 {
                return ToolOutput::err(format!(
                    "old_string не найден в {}: {old:?}",
                    full.display()
                ));
            }
            // Атомарность: каждая замена применяется один раз к текущему состоянию.
            new_content = match new_content.replacen(old, new, 1) {
                c if c != new_content => c,
                _ => continue,
            };
            applied += 1;
        }
        if let Err(e) = std::fs::write(&full, &new_content) {
            return ToolOutput::err(format!("write {}: {e}", full.display()));
        }
        ToolOutput::ok(format!("Применено замен: {applied} в {}", full.display()))
    }
}
