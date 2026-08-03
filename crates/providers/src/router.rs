//! Роутер моделей (FR-050): правила «тип задачи → модель» + статистика.
//!
//! Роутер решает, каким endpoint'ом/моделью обслуживать запрос, на основе
//! профиля задачи. OmniRoute подключается как обычный OpenAI-compatible endpoint.

use serde::{Deserialize, Serialize};
use vpsagent_core::Config;

/// Профиль задачи (вход для роутера).
#[derive(Debug, Clone, Default)]
pub struct TaskProfile {
    /// Ключевые слова задачи (для keyword-матчинга).
    pub keywords: Vec<String>,
    /// Признаки: сложная задача, много правок, исследование и т.д.
    pub flags: Vec<String>,
}

/// Правило роутинга.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRule {
    /// Паттерн (подстрока ключевых слов, например "деплой", "review").
    pub pattern: String,
    /// Endpoint + модель.
    pub endpoint: String,
    pub model: String,
    /// Приоритет (больше = раньше).
    pub priority: i32,
    /// Источник: ручной (manual) или авто (auto, из A/B-статистики).
    pub source: String,
}

impl RouteRule {
    /// Совпадает ли правило с профилем.
    pub fn matches(&self, profile: &TaskProfile) -> bool {
        if self.pattern.is_empty() {
            return false;
        }
        profile
            .keywords
            .iter()
            .any(|k| k.to_lowercase().contains(&self.pattern.to_lowercase()))
    }
}

/// Роутер: набор правил + fallback.
#[derive(Debug, Clone, Default)]
pub struct Router {
    pub rules: Vec<RouteRule>,
    /// Endpoint/модель по умолчанию (fallback).
    pub default_endpoint: String,
    pub default_model: String,
}

impl Router {
    /// Из конфигурации: fallback + правила из конфига (в Фазе 4 — из RouteRule-списка в конфиге).
    pub fn from_config(config: &Config, rules: Vec<RouteRule>) -> Self {
        let default_endpoint = config
            .endpoint_for_model(&config.default_model)
            .map(|e| e.name.clone())
            .unwrap_or_default();
        Self {
            rules,
            default_endpoint,
            default_model: config.default_model.clone(),
        }
    }

    /// Разрешить endpoint+модель для профиля.
    pub fn resolve(&self, profile: &TaskProfile) -> (String, String) {
        let mut best: Option<&RouteRule> = None;
        for rule in &self.rules {
            if rule.matches(profile) {
                // Правила отсортированы по приоритету; берём первое совпадение
                // с максимальным приоритетом.
                if best.map(|b| rule.priority > b.priority).unwrap_or(true) {
                    best = Some(rule);
                }
            }
        }
        match best {
            Some(rule) => (rule.endpoint.clone(), rule.model.clone()),
            None => (self.default_endpoint.clone(), self.default_model.clone()),
        }
    }

    /// Сортировать правила по приоритету (для стабильности).
    pub fn sort(&mut self) {
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
    }
}
