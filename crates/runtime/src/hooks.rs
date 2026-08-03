//! Hooks (FR-060): пользовательские скрипты на события (pre/post tool).
//!
//! В Фазе 8 — упрощённый движок: конфигурация из TOML, выполнение внешних
//! команд по событиям. Полный пайплайн (модификация аргументов) — следующая.

use std::path::PathBuf;

use serde::Deserialize;

/// Событие, на которое вешается hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreTool,
    PostTool,
    Message,
}

/// Hook-конфигурация.
#[derive(Debug, Clone, Deserialize)]
pub struct HookConfig {
    pub event: HookEvent,
    /// Команда (shell), получает событие через env.
    pub command: String,
    /// Паттерн инструмента (для PreTool/PostTool; пусто = все).
    #[serde(default)]
    pub tool_pattern: String,
}

/// Движок хуков.
#[derive(Debug, Default)]
pub struct Hooks {
    pub hooks: Vec<HookConfig>,
}

impl Hooks {
    /// Загрузить из ~/.vpsagent/hooks.toml.
    pub fn load_defaults() -> Self {
        let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        dir.push(".vpsagent/hooks.toml");
        if !dir.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&dir) {
            Ok(content) => {
                #[derive(Deserialize)]
                struct Cfg {
                    #[serde(default)]
                    hooks: Vec<HookConfig>,
                }
                let cfg: Cfg = match toml::from_str(&content) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(err=%e, "не удалось прочитать hooks.toml");
                        return Self::default();
                    }
                };
                Self { hooks: cfg.hooks }
            }
            Err(_) => Self::default(),
        }
    }

    /// Запустить хуки для события. Асинхронно (не блокирует агента долго).
    pub fn fire(&self, event: HookEvent, tool: &str) {
        for hook in &self.hooks {
            if hook.event != event {
                continue;
            }
            if !hook.tool_pattern.is_empty() && !tool.contains(&hook.tool_pattern) {
                continue;
            }
            let command = hook.command.clone();
            let tool = tool.to_string();
            tokio::spawn(async move {
                let mut cmd = tokio::process::Command::new("sh");
                cmd.arg("-c").arg(&command);
                cmd.env("VPSAGENT_EVENT", format!("{event:?}"));
                cmd.env("VPSAGENT_TOOL", tool);
                let _ = cmd.output().await;
            });
        }
    }
}
