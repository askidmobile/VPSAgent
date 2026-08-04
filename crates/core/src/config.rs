//! Конфигурация VPSAgent (TOML, иерархия: дефолт → ~/.vpsagent/config.toml → CWD).
//!
//! Секреты (API-ключи) лежат в отдельном файле `secrets.toml` с правами 0600
//! и НЕ попадают в основной конфиг.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Режим прав доступа к инструментам (FR-012).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Спрашивать перед каждым потенциально опасным действием.
    #[default]
    Default,
    /// Автоматически принимать правки файлов.
    AcceptEdits,
    /// Только планирование, без исполнения.
    Plan,
    /// Всё разрешено (CI, доверенная среда).
    BypassPermissions,
}

/// Тип провайдера LLM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    /// OpenAI-совместимый Chat Completions API (базовый случай: OpenAI, OpenRouter, OmniRoute, vLLM, ...).
    OpenaiCompat,
    /// OpenAI Responses API (native GPT: /v1/responses).
    OpenaiResponses,
    /// Anthropic Messages API.
    Anthropic,
}

/// Endpoint LLM-провайдера.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEndpoint {
    /// Уникальное имя (для ссылок из роутера и A/B).
    pub name: String,
    pub kind: EndpointKind,
    pub base_url: String,
    /// Имя ключа в secrets.toml (не сам ключ!).
    pub api_key_ref: String,
    /// Список доступных моделей (для валидации и UI).
    pub models: Vec<String>,
}

/// Корень конфигурации.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub endpoints: Vec<ModelEndpoint>,
    /// Модель по умолчанию (для главного агента новой сессии).
    pub default_model: String,
    /// Режим прав по умолчанию.
    pub permission_mode: PermissionMode,
    pub paths: Paths,
}

/// Пути к ресурсам демона.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paths {
    /// Каталог данных (~/.vpsagent по умолчанию).
    pub data_dir: PathBuf,
    /// Unix-сокет демона.
    pub socket: PathBuf,
    /// SQLite-база.
    pub db: PathBuf,
    /// Каталог сессий (для фьютурного экспорта/JSONL-бэкапа).
    pub sessions_dir: PathBuf,
    /// Файл лога демона при daemonize (FR-003). Пустая строка — дефолт
    /// (`<data_dir>/daemon.log`), вычисляется в [`Paths::default`] и
    /// [`Paths::or_default_dirs`].
    #[serde(default)]
    pub log_file: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let data_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".vpsagent");
        Self {
            socket: data_dir.join("daemon.sock"),
            db: data_dir.join("vpsagent.db"),
            sessions_dir: data_dir.join("sessions"),
            log_file: data_dir.join("daemon.log"),
            data_dir,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoints: vec![ModelEndpoint {
                name: "openai".into(),
                kind: EndpointKind::OpenaiCompat,
                base_url: "https://api.openai.com/v1".into(),
                api_key_ref: "openai".into(),
                models: vec!["gpt-4o".into(), "gpt-4o-mini".into()],
            }],
            default_model: "gpt-4o-mini".into(),
            permission_mode: PermissionMode::Default,
            paths: Paths::default(),
        }
    }
}

impl Config {
    /// Загрузка с иерархией: дефолт → ~/.vpsagent/config.toml → CWD/.vpsagent/config.toml.
    /// Отсутствие файлов — не ошибка (используем дефолт).
    pub fn load() -> anyhow::Result<Self> {
        let mut cfg = Self::default();
        for path in [
            dirs::home_dir().map(|h| h.join(".vpsagent/config.toml")),
            std::env::current_dir()
                .ok()
                .map(|c| c.join(".vpsagent/config.toml")),
        ]
        .into_iter()
        .flatten()
        {
            if path.exists() {
                let raw = std::fs::read_to_string(&path)?;
                let loaded: Config = toml::from_str(&raw)?;
                cfg = loaded;
                cfg.paths = cfg.paths.or_default_dirs(&path);
                break;
            }
        }
        Ok(cfg)
    }

    /// Загрузка секрета по api_key_ref из ~/.vpsagent/secrets.toml.
    pub fn secret(&self, key_ref: &str) -> anyhow::Result<String> {
        let secrets_path = self.paths.data_dir.join("secrets.toml");
        let raw = std::fs::read_to_string(&secrets_path)?;
        let secrets: std::collections::HashMap<String, String> = toml::from_str(&raw)?;
        secrets
            .get(key_ref)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("секрет '{key_ref}' не найден в secrets.toml"))
    }

    /// Найти endpoint по имени модели.
    pub fn endpoint_for_model(&self, model: &str) -> Option<&ModelEndpoint> {
        self.endpoints
            .iter()
            .find(|e| e.models.iter().any(|m| m == model))
    }

    /// Нужна ли инициализация: нет secrets.toml или в нём нет ключей,
    /// на которые ссылается хотя бы один endpoint.
    pub fn needs_init(&self) -> bool {
        let secrets_path = self.paths.data_dir.join("secrets.toml");
        if !secrets_path.exists() {
            return true;
        }
        // Читаем секреты; если файла нет ключей endpoint'ов — нужна настройка.
        let raw = match std::fs::read_to_string(&secrets_path) {
            Ok(r) => r,
            Err(_) => return true,
        };
        let secrets: std::collections::HashMap<String, String> = match toml::from_str(&raw) {
            Ok(s) => s,
            Err(_) => return true,
        };
        // Достаточно, если хотя бы один endpoint имеет непустой ключ.
        !self.endpoints.iter().any(|e| {
            secrets
                .get(&e.api_key_ref)
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false)
        })
    }

    /// Записать конфиг в ~/.vpsagent/config.toml (создаёт каталог).
    pub fn save(&self) -> anyhow::Result<()> {
        let dir = &self.paths.data_dir;
        std::fs::create_dir_all(dir)?;
        let content = toml::to_string_pretty(self)?;
        std::fs::write(dir.join("config.toml"), content)?;
        Ok(())
    }

    /// Записать секреты в ~/.vpsagent/secrets.toml с правами 0600.
    pub fn save_secrets(
        &self,
        secrets: &std::collections::HashMap<String, String>,
    ) -> anyhow::Result<()> {
        let dir = &self.paths.data_dir;
        std::fs::create_dir_all(dir)?;
        let content = toml::to_string_pretty(secrets)?;
        let path = dir.join("secrets.toml");
        std::fs::write(&path, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Установить модель по умолчанию (для `vpsagent provider use`).
    pub fn set_default_model(&mut self, model: &str) -> anyhow::Result<()> {
        if self.endpoint_for_model(model).is_none() {
            anyhow::bail!("модель '{model}' не найдена ни в одном endpoint");
        }
        self.default_model = model.to_string();
        self.save()
    }

    /// Удалить endpoint по имени (для `vpsagent provider remove`).
    pub fn remove_endpoint(&mut self, name: &str) -> anyhow::Result<()> {
        let len_before = self.endpoints.len();
        self.endpoints.retain(|e| e.name != name);
        if self.endpoints.len() == len_before {
            anyhow::bail!("провайдер '{name}' не найден");
        }
        self.save()
    }

    /// Список настроенных провайдеров (для `vpsagent provider list`).
    pub fn list_endpoints(&self) -> &[ModelEndpoint] {
        &self.endpoints
    }
}

impl Paths {
    /// Если в загруженном конфиге paths заданы частично — подставляем дефолты
    /// относительно каталога конфига.
    fn or_default_dirs(mut self, config_path: &Path) -> Self {
        if self.data_dir.as_os_str().is_empty() {
            self.data_dir = config_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
        }
        if self.socket.as_os_str().is_empty() {
            self.socket = self.data_dir.join("daemon.sock");
        }
        if self.db.as_os_str().is_empty() {
            self.db = self.data_dir.join("vpsagent.db");
        }
        if self.sessions_dir.as_os_str().is_empty() {
            self.sessions_dir = self.data_dir.join("sessions");
        }
        if self.log_file.as_os_str().is_empty() {
            self.log_file = self.data_dir.join("daemon.log");
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Старый конфиг без `log_file` (до FR-003) должен десериализоваться —
    /// поле получает `#[serde(default)]` → пустая PathBuf.
    #[test]
    fn paths_without_log_file_deserializes() {
        let toml = r#"
            data_dir = "/tmp/vps"
            socket = "/tmp/vps/daemon.sock"
            db = "/tmp/vps/vpsagent.db"
            sessions_dir = "/tmp/vps/sessions"
        "#;
        let paths: Paths = toml::from_str(toml).expect("старый конфиг должен десериализоваться");
        assert_eq!(paths.data_dir, PathBuf::from("/tmp/vps"));
        assert!(
            paths.log_file.as_os_str().is_empty(),
            "log_file должен быть пустым по умолчанию"
        );
    }

    /// Новый конфиг с явным `log_file` — значение сохраняется как есть.
    #[test]
    fn paths_with_explicit_log_file() {
        let toml = r#"
            data_dir = "/tmp/vps"
            socket = "/tmp/vps/daemon.sock"
            db = "/tmp/vps/vpsagent.db"
            sessions_dir = "/tmp/vps/sessions"
            log_file = "/var/log/vpsagent.log"
        "#;
        let paths: Paths = toml::from_str(toml).expect("конфиг должен десериализоваться");
        assert_eq!(paths.log_file, PathBuf::from("/var/log/vpsagent.log"));
    }

    /// `or_default_dirs` подставляет дефолтный `log_file` для пустого поля.
    #[test]
    fn or_default_dirs_fills_log_file() {
        let toml = r#"
            data_dir = "/tmp/vps"
            socket = "/tmp/vps/daemon.sock"
            db = "/tmp/vps/vpsagent.db"
            sessions_dir = "/tmp/vps/sessions"
        "#;
        let paths: Paths = toml::from_str(toml).unwrap();
        let config_path = PathBuf::from("/tmp/vps/config.toml");
        let paths = paths.or_default_dirs(&config_path);
        assert_eq!(paths.log_file, PathBuf::from("/tmp/vps/daemon.log"));
    }

    /// `endpoint_for_model` находит эндпоинт по модели из списка `models`
    /// и возвращает None для отсутствующей. Это контракт, на который опирается
    /// init-мастер (модель должна попасть в `models` эндпоинта при сохранении).
    #[test]
    fn endpoint_for_model_finds_custom() {
        let mut config = Config::default();
        let custom = ModelEndpoint {
            name: "kimi".into(),
            kind: EndpointKind::OpenaiCompat,
            base_url: "https://api.moonshot.ai/v1".into(),
            api_key_ref: "kimi".into(),
            models: vec!["kimi-k2".into()],
        };
        config.endpoints.push(custom);

        let found = config.endpoint_for_model("kimi-k2");
        assert!(found.is_some(), "модель из списка models должна находится");
        assert_eq!(found.unwrap().name, "kimi");

        let missing = config.endpoint_for_model("несуществующая-модель");
        assert!(
            missing.is_none(),
            "модель не из списка должна давать None (иначе агент не спавнится)"
        );
    }
}
