//! Реестр провайдеров LLM: загрузка с models.dev (api.json) с fallback на встроенный.
//!
//! models.dev — открытый реестр провайдеров и моделей (AI SDK). Используем для
//! списка известных провайдеров, их base_url и моделей. Сетевой сбой → встроенный список.
//!
//! Провайдеры классифицируются по [`Category`] (top-tier ручной список, остальные —
//! `Other`) для группировки в init-мастере.

use serde::Deserialize;
use std::collections::HashMap;

/// Категория провайдера для группировки в init-мастере.
///
/// Порядок вариантов важен: он задаёт сортировку категорий в UI (top-tier первыми,
/// `Other` — последним). Ручная классификация top-25 провайдеров models.dev.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// Вендоры моделей прямого API: OpenAI, Anthropic, Google, xAI, Mistral ...
    FirstParty,
    /// Облака и inference-провайдеры: Azure, Bedrock, Groq, Deep Infra, ...
    Cloud,
    /// Агрегаторы/роутеры поверх многих моделей: OpenRouter, Poe, ...
    Aggregator,
    /// Локальный self-hosted (Ollama и т.п.).
    Local,
    /// Всё остальное (длинный хвост models.dev).
    Other,
}

impl Category {
    /// Человекочитаемое имя группы для UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::FirstParty => "First-party API",
            Self::Cloud => "Cloud / inference",
            Self::Aggregator => "Агрегаторы",
            Self::Local => "Local / self-hosted",
            Self::Other => "Другие",
        }
    }

    /// Порядок сортировки (меньше = выше в списке).
    pub fn order(self) -> u8 {
        match self {
            Self::FirstParty => 0,
            Self::Cloud => 1,
            Self::Aggregator => 2,
            Self::Local => 3,
            Self::Other => 4,
        }
    }
}

/// Классифицировать провайдера по id в категорию (ручной список top-tier).
///
/// Провайдеры, не попавшие в список — `Category::Other`. Список основан на
/// анализе api.json models.dev (180 провайдеров, ~25 в категориях).
pub fn provider_category(id: &str) -> Category {
    match id {
        // First-party: вендоры моделей прямого API.
        "openai" | "anthropic" | "google" | "xai" | "mistral" | "cohere" | "perplexity"
        | "zhipuai" | "deepseek" | "qwen" => Category::FirstParty,
        // Cloud / inference: хостинг и ускоренный вывод моделей.
        "azure" | "amazon-bedrock" | "google-vertex" | "together" | "fireworks" | "deepinfra"
        | "groq" | "cerebras" | "nvidia" | "nebius" | "baseten" | "siliconflow" | "scaleway" => {
            Category::Cloud
        }
        // Агрегаторы: роутеры поверх многих вендоров.
        "openrouter" | "anyapi" | "poe" | "aihubmix" | "merge-gateway" | "ai-gateway" => {
            Category::Aggregator
        }
        // Local / self-hosted.
        "ollama" => Category::Local,
        _ => Category::Other,
    }
}

/// Провайдер из реестра.
#[derive(Debug, Clone)]
pub struct RegistryProvider {
    pub id: String,
    pub name: String,
    /// base_url (может отсутствовать у некоторых — тогда дефолт по типу).
    pub api: Option<String>,
    /// npm-пакет (используем для определения типа: openai/anthropic/...).
    pub npm: Option<String>,
    /// Модели (id).
    pub models: Vec<String>,
}

/// Запись провайдера из api.json.
#[derive(Debug, Deserialize)]
struct ApiProvider {
    #[serde(default)]
    name: String,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    models: HashMap<String, serde_json::Value>,
}

/// Загрузить реестр с models.dev. При сбое сети — встроенный fallback.
pub async fn load_registry() -> Vec<RegistryProvider> {
    match try_load().await {
        Ok(v) if !v.is_empty() => v,
        _ => fallback(),
    }
}

async fn try_load() -> anyhow::Result<Vec<RegistryProvider>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("vpsagent/0.1")
        .build()?;
    let resp = client.get("https://models.dev/api.json").send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("models.dev HTTP {}", resp.status());
    }
    let raw: HashMap<String, ApiProvider> = resp.json().await?;
    let mut out: Vec<RegistryProvider> = raw
        .into_iter()
        .map(|(id, p)| RegistryProvider {
            name: if p.name.is_empty() {
                id.clone()
            } else {
                p.name
            },
            id,
            api: p.api,
            npm: p.npm,
            models: p.models.into_keys().collect(),
        })
        .collect();
    // Сортируем: по категории (top-tier первыми), внутри категории — по имени.
    out.sort_by(|a, b| {
        provider_category(&a.id)
            .order()
            .cmp(&provider_category(&b.id).order())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// Встроенный fallback (если models.dev недоступен).
fn fallback() -> Vec<RegistryProvider> {
    vec![
        RegistryProvider {
            id: "openai".into(),
            name: "OpenAI".into(),
            api: Some("https://api.openai.com/v1".into()),
            npm: Some("@ai-sdk/openai".into()),
            models: vec!["gpt-5".into(), "gpt-4.1".into(), "gpt-4.1-mini".into()],
        },
        RegistryProvider {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            api: Some("https://api.anthropic.com".into()),
            npm: Some("@ai-sdk/anthropic".into()),
            models: vec!["claude-sonnet-4-5".into(), "claude-haiku-4-5".into()],
        },
        RegistryProvider {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            api: Some("https://openrouter.ai/api/v1".into()),
            npm: Some("@openrouter/ai-sdk-provider".into()),
            models: vec!["qwen/qwen3-coder".into(), "deepseek/deepseek-coder".into()],
        },
    ]
}

/// Определить EndpointKind по npm-пакету провайдера.
pub fn endpoint_kind(npm: Option<&str>) -> vpsagent_core::EndpointKind {
    match npm {
        Some(n) if n.contains("anthropic") => vpsagent_core::EndpointKind::Anthropic,
        Some(n) if n.contains("openai") && !n.contains("compatible") => {
            // Нативный OpenAI SDK → Responses API.
            vpsagent_core::EndpointKind::OpenaiResponses
        }
        _ => vpsagent_core::EndpointKind::OpenaiCompat,
    }
}

// Используем anyhow/reqwest/serde_json.
#[allow(dead_code)]
fn _ensure(_: &vpsagent_core::EndpointKind) {}
