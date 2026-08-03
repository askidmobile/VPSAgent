//! Реестр провайдеров LLM: загрузка с models.dev (api.json) с fallback на встроенный.
//!
//! models.dev — открытый реестр провайдеров и моделей (AI SDK). Используем для
//! списка известных провайдеров, их base_url и моделей. Сетевой сбой → встроенный список.

use serde::Deserialize;
use std::collections::HashMap;

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
    // Сортируем: известные первыми, остальные по имени.
    out.sort_by_key(|p| !is_known(&p.id));
    out.sort_by(|a, b| {
        let ak = !is_known(&a.id);
        let bk = !is_known(&b.id);
        ak.cmp(&bk).then(a.name.cmp(&b.name))
    });
    Ok(out)
}

/// Известные (приоритетные в списке).
fn is_known(id: &str) -> bool {
    matches!(
        id,
        "openai"
            | "anthropic"
            | "openrouter"
            | "azure"
            | "google"
            | "ollama"
            | "groq"
            | "xai"
            | "mistral"
    )
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
