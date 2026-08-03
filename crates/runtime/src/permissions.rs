//! Пайплайн прав (FR-012, FR-027).
//!
//! Фаза 1: упрощённая версия — режим + простой allow/deny список по имени инструмента.
//! Полный пайплайн (hooks → правила → пользователь) — Фаза 8.

use vpsagent_core::{PermissionMode, Result};

/// Решение пайплайна прав.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Разрешено, выполнять.
    Allow,
    /// Запрещено.
    Deny(String),
    /// Спросить пользователя (режим Default).
    Ask,
}

/// Политика прав.
#[derive(Debug, Clone)]
pub struct Permissions {
    pub mode: PermissionMode,
    /// Allow-list (имена инструментов / glob в будущем).
    pub allow: Vec<String>,
    /// Deny-list.
    pub deny: Vec<String>,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            mode: PermissionMode::AcceptEdits,
            allow: vec![],
            // shell в default-режиме требует подтверждения; в Фазе 1 для скелета — разрешаем.
            deny: vec![],
        }
    }
}

impl Permissions {
    /// Классифицировать инструмент на чтение/правку/опасное.
    fn risk(name: &str) -> Risk {
        match name {
            "read" | "ls" | "grep" | "web_fetch" | "todo" => Risk::ReadOnly,
            "write" | "edit" => Risk::Edit,
            "shell" => Risk::Shell,
            _ => Risk::Unknown,
        }
    }

    /// Решение по пайплайну (упрощённый, без hooks).
    pub fn check(&self, tool: &str) -> Decision {
        if self.deny.iter().any(|d| matches(d, tool)) {
            return Decision::Deny(format!("'{tool}' в deny-листе"));
        }
        if self.allow.iter().any(|a| matches(a, tool)) {
            return Decision::Allow;
        }
        let risk = Self::risk(tool);
        match self.mode {
            PermissionMode::BypassPermissions => Decision::Allow,
            PermissionMode::AcceptEdits => match risk {
                Risk::ReadOnly | Risk::Edit => Decision::Allow,
                Risk::Shell | Risk::Unknown => Decision::Ask,
            },
            PermissionMode::Plan => match risk {
                Risk::ReadOnly => Decision::Allow,
                _ => Decision::Deny("plan-режим: только чтение".into()),
            },
            PermissionMode::Default => match risk {
                Risk::ReadOnly => Decision::Allow,
                _ => Decision::Ask,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Risk {
    ReadOnly,
    Edit,
    Shell,
    Unknown,
}

fn matches(pattern: &str, name: &str) -> bool {
    pattern == name
}

/// Проверка разрешения — если Ask, возвращает Ok(false) (нужен ответ пользователя).
pub fn check_allowed(perm: &Permissions, tool: &str) -> Result<bool> {
    Ok(perm.check(tool) == Decision::Allow)
}
