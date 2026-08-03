//! Memory-файлы (FR-018): иерархия AGENTS.md.
//!
//! Загружаем и объединяем: ~/.vpsagent/AGENTS.md, ./AGENTS.md, ./подкаталог/AGENTS.md.
//! В Фазе 1 — простое чтение (без эскалаций/обновлений); они пойдут в системный промпт.

use std::path::{Path, PathBuf};

/// Собрать иерархию AGENTS.md от домашнего каталога до текущего (включая подкаталоги).
/// Возвращает единый текст для внедрения в системный промпт.
pub fn collect_memory(cwd: &Path) -> String {
    let mut sections: Vec<(String, String)> = vec![];

    // Глобальный пользовательский.
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".vpsagent/AGENTS.md");
        if let Ok(content) = std::fs::read_to_string(&p) {
            sections.push(("пользовательский".into(), content));
        }
    }

    // Проектный (в cwd и вверх до корня ФС или первого без-AGENTS.md — берём только ближайший).
    if let Some(p) = nearest_in_ancestors(cwd, "AGENTS.md") {
        if let Ok(content) = std::fs::read_to_string(&p) {
            sections.push(("проектный".into(), content));
        }
    }

    if sections.is_empty() {
        return String::new();
    }

    let mut out = String::from("# Контекст проекта (AGENTS.md)\n\n");
    for (label, content) in &sections {
        out.push_str(&format!("## {label}\n\n{content}\n\n"));
    }
    out
}

fn nearest_in_ancestors(start: &Path, name: &str) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}
