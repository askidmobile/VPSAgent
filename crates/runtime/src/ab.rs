//! A/B-раннер (FR-053): параллельные прогоны одной задачи на нескольких моделях.
//!
//! Спавнит субагентов с одинаковой задачей и разными моделями в изолированных
//! worktree'ах, собирает метрики (время, токены, тесты) и отдаёт в роутер-stats.
//!
//! ВАЖНО: `start` НЕ пишет синтетические метрики в stats — это отравляет роутер.
//! Реальные метрики фиксируются через [`AbRunner::record_real_result`] после
//! настоящего прогона модели.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;
use vpsagent_core::{EventKind, Id, SessionStatus};
use vpsagent_providers::{ModelRunMetric, ModelStatsRegistry};
use vpsagent_storage::Storage;

/// Прогон A/B.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbRun {
    pub id: Id,
    pub task: String,
    pub status: String, // pending_manual | running | done
    pub verdict: Option<String>,
    pub created_at: String,
}

/// Результат одного участника A/B.
#[derive(Debug, Clone)]
pub struct AbResult {
    pub run_id: Id,
    pub model: String,
    pub duration_ms: u64,
    pub tests_passed: u32,
    pub tests_total: u32,
}

/// A/B-раннер (in-memory; персистентность — в storage позже).
#[derive(Clone)]
pub struct AbRunner {
    storage: Storage,
    pub stats: Arc<tokio::sync::Mutex<ModelStatsRegistry>>,
}

impl AbRunner {
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            stats: Arc::new(tokio::sync::Mutex::new(ModelStatsRegistry::new())),
        }
    }

    /// Запустить A/B-прогон.
    ///
    /// Реальный параллельный спавн субагентов по worktree — отдельная итерация;
    /// до неё прогон создаётся в статусе `pending_manual` и НЕ трогает stats
    /// (никаких синтетических метрик — они отравляют роутер).
    pub async fn start(&self, task: &str, models: &[String]) -> anyhow::Result<AbRun> {
        if models.is_empty() {
            anyhow::bail!("A/B-прогон: нужен хотя бы один участник (models пуст)");
        }
        let run = AbRun {
            id: Uuid::new_v4(),
            task: task.to_string(),
            status: "pending_manual".into(),
            verdict: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        tracing::info!(run_id=%run.id, models=?models, "A/B-прогон создан (ожидает реального прогона)");
        Ok(run)
    }

    /// Зафиксировать РЕАЛЬНУЮ метрику прогона модели (после настоящего запуска).
    /// Единственная точка записи в stats — сюда попадают только фактические
    /// измерения, не синтетика.
    pub async fn record_real_result(&self, run_id: Id, metric: ModelRunMetric) -> anyhow::Result<()> {
        tracing::info!(run_id=%run_id, model=%metric.model, "A/B: реальная метрика");
        self.stats.lock().await.record(&metric);
        Ok(())
    }

    /// Зафиксировать вердикт (лучшая модель).
    pub async fn set_verdict(&self, run_id: Id, winner: String) -> anyhow::Result<()> {
        tracing::info!(run_id=%run_id, winner=%winner, "A/B вердикт");
        Ok(())
    }
}

// Держим типы для будущей полной реализации.
#[allow(dead_code)]
fn _ensure(_: &Storage, _: EventKind, _: SessionStatus, _: mpsc::UnboundedSender<EventKind>) {}
