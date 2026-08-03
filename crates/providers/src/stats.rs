//! Статистика роутера (FR-054): агрегация результатов A/B → рекомендации моделей.
//!
//! Накапливает метрики по моделям (время, токены, стоимость, прохождение тестов)
//! и рекомендует лучшую модель для класса задач. В Фазе 4 — in-memory;
//! персистентность в SQLite — Фаза 5 (A/B-раннер).

use std::collections::HashMap;

/// Одна запись прогона модели.
#[derive(Debug, Clone)]
pub struct ModelRunMetric {
    pub model: String,
    pub duration_ms: u64,
    pub tokens_input: u32,
    pub tokens_output: u32,
    pub tests_passed: u32,
    pub tests_total: u32,
}

/// Агрегат по модели.
#[derive(Debug, Clone, Default)]
pub struct ModelStats {
    pub runs: u64,
    pub total_duration_ms: u64,
    pub total_tokens_input: u64,
    pub total_tokens_output: u64,
    pub tests_passed: u64,
    pub tests_total: u64,
}

impl ModelStats {
    fn add(&mut self, m: &ModelRunMetric) {
        self.runs += 1;
        self.total_duration_ms += m.duration_ms;
        self.total_tokens_input += m.tokens_input as u64;
        self.total_tokens_output += m.tokens_output as u64;
        self.tests_passed += m.tests_passed as u64;
        self.tests_total += m.tests_total as u64;
    }

    fn avg_duration_ms(&self) -> u64 {
        if self.runs == 0 {
            0
        } else {
            self.total_duration_ms / self.runs
        }
    }

    fn pass_rate(&self) -> f32 {
        if self.tests_total == 0 {
            return 1.0; // нет тестов — считаем условно успешной
        }
        self.tests_passed as f32 / self.tests_total as f32
    }
}

/// Статистика по моделям.
#[derive(Debug, Default)]
pub struct ModelStatsRegistry {
    pub by_model: HashMap<String, ModelStats>,
}

impl ModelStatsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, m: &ModelRunMetric) {
        self.by_model.entry(m.model.clone()).or_default().add(m);
    }

    /// Лучшая модель: наибольший pass_rate, при равенстве — меньшее время.
    pub fn best_model(&self) -> Option<String> {
        self.by_model
            .iter()
            .filter(|(_, s)| s.runs > 0)
            .max_by(|(_, a), (_, b)| {
                let ar = a
                    .pass_rate()
                    .partial_cmp(&b.pass_rate())
                    .unwrap_or(std::cmp::Ordering::Equal);
                if ar != std::cmp::Ordering::Equal {
                    ar
                } else {
                    b.avg_duration_ms().cmp(&a.avg_duration_ms())
                }
            })
            .map(|(k, _)| k.clone())
    }

    /// Краткая сводка (для команды /router).
    pub fn summary(&self) -> String {
        if self.by_model.is_empty() {
            return "(нет данных A/B)".to_string();
        }
        let mut out = String::new();
        for (model, s) in &self.by_model {
            out.push_str(&format!(
                "{model}: {}/{} тестов, ср. {}мс, {} прогонов\n",
                s.tests_passed,
                s.tests_total,
                s.avg_duration_ms(),
                s.runs
            ));
        }
        out
    }
}
