//! Усечение больших выводов инструментов (FR-027).
//!
//! Лимит ~30k символов; полный вывод сохраняется во временный файл,
//! в контекст модели возвращается обрезанная версия + ссылка на файл.

use std::path::Path;

pub const LIMIT: usize = 30_000;

/// Обрезать строку до `n` СИМВОЛОВ (не байт).
/// Байтовый срез `&s[..n]` паникует на границе UTF-8 (кириллица, эмодзи) — C4.
pub fn truncate_chars(s: &str, n: usize) -> &str {
    let end = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

/// Обрезать вывод. Если превышает лимит — сохраняет полный в temp-файл
/// и возвращает обрезанный + метку.
pub fn apply(content: &str, temp_dir: &Path) -> String {
    // Лимит считаем в символах (char), а не байтах: срез по байтам паникует (C4).
    if content.chars().count() <= LIMIT {
        return content.to_string();
    }
    let mut path = temp_dir.to_path_buf();
    path.push(format!("tool-output-{}.txt", chrono::Utc::now().timestamp_millis()));
    if std::fs::write(&path, content).is_ok() {
        format!(
            "{}…\n\n[вывод обрезан: {} символов. Полный вывод: {}]",
            truncate_chars(content, LIMIT),
            content.chars().count(),
            path.display()
        )
    } else {
        format!(
            "{}…\n\n[вывод обрезан: не удалось сохранить полный]",
            truncate_chars(content, LIMIT)
        )
    }
}
