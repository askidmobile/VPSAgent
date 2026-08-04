//! Мастер инициализации VPSAgent (first-run setup).
//!
//! Источник провайдеров — models.dev (api.json), fallback — встроенный.
//! Поддерживаются: известные провайдеры (из реестра), OAuth ChatGPT, Custom
//! (ручной ввод названия/url/api key/протокола).

use std::collections::HashMap;
use std::io::stdout;

use anyhow::Result;
use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use vpsagent_core::{Config, EndpointKind, ModelEndpoint};

use crate::registry::{endpoint_kind, load_registry, RegistryProvider};

/// Специальный пункт меню (после реестра).
const MENU_OAUTH_CHATGPT: &str = "oauth-chatgpt";
const MENU_CUSTOM: &str = "custom";

/// Режим шага 1: выбор провайдера.
#[derive(Clone, PartialEq)]
enum Pick {
    /// Из реестра (индекс).
    Registry(usize),
    /// OAuth ChatGPT.
    OauthChatGpt,
    /// Ручной ввод.
    Custom,
}

/// Состояние мастера.
struct InitState {
    /// Шаг: 0=выбор провайдера, 1=ключ/url/протокол, 2=модель, 3=запись.
    step: usize,
    /// Реестр провайдеров (с models.dev).
    registry: Vec<RegistryProvider>,
    /// Всего пунктов меню (реестр + OAuth + Custom).
    menu_len: usize,
    /// Текущий фокус в меню (0..menu_len-1).
    focus: usize,
    /// Выбранный режим.
    pick: Option<Pick>,
    /// Поля для custom-ввода.
    custom_name: String,
    custom_url: String,
    custom_key: String,
    /// Протокол для custom (0=openai-compat, 1=responses, 2=anthropic).
    custom_kind_idx: usize,
    /// Поле ввода для шага 1 (ключ / url / name).
    input: String,
    /// Текущий редактируемый индекс (для OAuth/custom мультишага).
    field_idx: usize,
    /// Модели выбранного провайдера.
    models: Vec<String>,
    /// Дефолтная модель.
    default_model: String,
    /// Статус/ошибка.
    status: String,
    /// Завершено.
    done: bool,
}

impl InitState {
    async fn new() -> Self {
        let registry = load_registry().await;
        let menu_len = registry.len() + 2; // + OAuth ChatGPT + Custom
        Self {
            step: 0,
            registry,
            menu_len,
            focus: 0,
            pick: None,
            custom_name: String::new(),
            custom_url: String::new(),
            custom_key: String::new(),
            custom_kind_idx: 0,
            input: String::new(),
            field_idx: 0,
            models: vec![],
            default_model: String::new(),
            status: "↑/↓ — выбор, Enter — подтвердить, Esc — выход".into(),
            done: false,
        }
    }

    /// Имя пункта меню по индексу.
    fn menu_label(&self, idx: usize) -> String {
        if idx < self.registry.len() {
            let p = &self.registry[idx];
            let url = p.api.as_deref().unwrap_or("(дефолт)");
            format!("{} — {}", p.name, url)
        } else if idx == self.registry.len() {
            "OAuth ChatGPT (логин через браузер)".to_string()
        } else {
            "Custom (название, url, api key, протокол)".to_string()
        }
    }

    fn next_step(&mut self) {
        match self.step {
            0 => {
                self.pick = Some(if self.focus < self.registry.len() {
                    Pick::Registry(self.focus)
                } else if self.focus == self.registry.len() {
                    Pick::OauthChatGpt
                } else {
                    Pick::Custom
                });
                self.input.clear();
                self.field_idx = 0;
                match self.pick.as_ref().unwrap() {
                    Pick::Registry(idx) => {
                        let p = &self.registry[*idx];
                        self.models = p.models.clone();
                        self.status = format!("введите API-ключ для {} (Enter — дальше)", p.name);
                        self.step = 1;
                    }
                    Pick::OauthChatGpt => {
                        self.status =
                            "OAuth ChatGPT: вставьте токен из браузера (Enter — дальше)".into();
                        self.step = 1;
                    }
                    Pick::Custom => {
                        self.status = "Custom: введите название провайдера (Enter — дальше)".into();
                        self.step = 1;
                    }
                }
            }
            1 => {
                // Мультишаг для Custom: name → url → key → kind.
                if matches!(self.pick, Some(Pick::Custom)) {
                    match self.field_idx {
                        0 => {
                            self.custom_name = self.input.trim().to_string();
                            if self.custom_name.is_empty() {
                                self.status = "название не может быть пустым".into();
                                return;
                            }
                            self.input.clear();
                            self.field_idx = 1;
                            self.status =
                                "введите base_url (например http://localhost:8000/v1)".into();
                        }
                        1 => {
                            self.custom_url = self.input.trim().to_string();
                            if self.custom_url.is_empty() {
                                self.status = "url не может быть пустым".into();
                                return;
                            }
                            self.input.clear();
                            self.field_idx = 2;
                            self.status = "введите API-ключ (можно пустым для локального)".into();
                        }
                        2 => {
                            self.custom_key = self.input.trim().to_string();
                            self.field_idx = 3;
                            self.status = "протокол: 1=openai-compat, 2=responses, 3=anthropic (введите цифру)".into();
                        }
                        3 => {
                            // Протокол выбран цифрой; default_model — вводим модель вручную.
                            self.input.clear();
                            self.step = 2;
                            self.status = "введите дефолтную модель".into();
                        }
                        _ => {}
                    }
                } else if matches!(self.pick, Some(Pick::OauthChatGpt)) {
                    // OAuth: токен = ключ. default_model — gpt-5 по умолчанию.
                    self.custom_key = self.input.trim().to_string();
                    self.custom_name = "chatgpt-oauth".to_string();
                    self.custom_url = "https://api.openai.com/v1".to_string();
                    self.custom_kind_idx = 1; // Responses
                    self.models = vec!["gpt-5".into(), "gpt-4.1".into()];
                    self.default_model = "gpt-5".to_string();
                    self.step = 2;
                    self.status = "введите дефолтную модель (Enter — сохранить)".into();
                } else {
                    // Registry: ключ → модель из списка.
                    self.custom_key = self.input.trim().to_string();
                    self.input.clear();
                    self.step = 2;
                    let default = self.models.first().cloned().unwrap_or_default();
                    self.default_model = default;
                    self.status = "введите дефолтную модель (Enter — сохранить)".into();
                }
            }
            2 => {
                if !self.input.trim().is_empty() {
                    self.default_model = self.input.trim().to_string();
                }
                if self.default_model.trim().is_empty() {
                    self.status = "модель не может быть пустой".into();
                    return;
                }
                self.step = 3;
                self.done = true;
            }
            _ => {}
        }
    }

    /// Построить ModelEndpoint по выбору.
    fn build_endpoint(&self) -> ModelEndpoint {
        match self.pick.as_ref().unwrap() {
            Pick::Registry(idx) => {
                let p = &self.registry[*idx];
                ModelEndpoint {
                    name: p.id.clone(),
                    kind: endpoint_kind(p.npm.as_deref()),
                    base_url: p.api.clone().unwrap_or_else(|| default_url(p.id.as_str())),
                    api_key_ref: p.id.clone(),
                    models: self.models.clone(),
                }
            }
            Pick::OauthChatGpt | Pick::Custom => ModelEndpoint {
                name: self.custom_name.clone(),
                kind: match self.custom_kind_idx {
                    1 => EndpointKind::OpenaiResponses,
                    2 => EndpointKind::Anthropic,
                    _ => EndpointKind::OpenaiCompat,
                },
                base_url: self.custom_url.clone(),
                api_key_ref: self.custom_name.clone(),
                models: self.models.clone(),
            },
        }
    }
}

fn default_url(id: &str) -> String {
    match id {
        "openai" => "https://api.openai.com/v1".into(),
        "anthropic" => "https://api.anthropic.com".into(),
        _ => format!("https://{id}.example.com/v1"),
    }
}

/// Запустить мастер инициализации.
pub async fn run_init() -> Result<Config> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = InitState::new().await;
    let mut events = EventStream::new();
    let mut result: Option<Config> = None;

    loop {
        terminal.draw(|f| render(f, &state))?;

        if state.done {
            match save_config(&state) {
                Ok(cfg) => {
                    result = Some(cfg);
                    break;
                }
                Err(e) => {
                    state.done = false;
                    state.step = 2;
                    state.status = format!("ошибка сохранения: {e}. Enter — повторить.");
                }
            }
        }

        let maybe_ev = events.next().await;
        let Some(Ok(ev)) = maybe_ev else { break };
        if !handle_key(ev, &mut state) {
            break;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result.ok_or_else(|| anyhow::anyhow!("инициализация отменена"))
}

fn save_config(state: &InitState) -> Result<Config> {
    let mut config = Config::load().unwrap_or_default();
    let endpoint = state.build_endpoint();
    let key_ref = endpoint.api_key_ref.clone();
    // Ключ берём из поля custom_key (для Registry оно пустое — ключ
    // подставится из secrets по api_key_ref на этапе auth).
    let key_value = state.custom_key.clone();
    // Обновить/добавить endpoint.
    if let Some(existing) = config
        .endpoints
        .iter_mut()
        .find(|e| e.name == endpoint.name)
    {
        *existing = endpoint;
    } else {
        config.endpoints.push(endpoint);
    }
    config.default_model = state.default_model.trim().to_string();
    config.save()?;

    // Секрет.
    let mut secrets = load_existing_secrets(&config);
    secrets.insert(key_ref, key_value);
    config.save_secrets(&secrets)?;
    Ok(config)
}

fn load_existing_secrets(config: &Config) -> HashMap<String, String> {
    let path = config.paths.data_dir.join("secrets.toml");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

fn handle_key(ev: CrosstermEvent, state: &mut InitState) -> bool {
    let CrosstermEvent::Key(KeyEvent {
        code, modifiers, ..
    }) = ev
    else {
        return true;
    };
    match state.step {
        0 => match (code, modifiers) {
            (KeyCode::Esc, _) => return false,
            (KeyCode::Up, _) => {
                if state.focus > 0 {
                    state.focus -= 1;
                }
            }
            (KeyCode::Down, _) => {
                if state.focus + 1 < state.menu_len {
                    state.focus += 1;
                }
            }
            (KeyCode::Enter, _) => state.next_step(),
            _ => {}
        },
        1 => {
            if matches!(state.pick, Some(Pick::Custom)) && state.field_idx == 3 {
                // Протокол — только цифра.
                if let KeyCode::Char(c @ '1'..='3') = code {
                    state.custom_kind_idx = (c as usize) - ('1' as usize);
                    state.next_step();
                }
                return true;
            }
            match (code, modifiers) {
                (KeyCode::Esc, _) => return false,
                (KeyCode::Enter, _) => state.next_step(),
                (KeyCode::Char(c), _) => state.input.push(c),
                (KeyCode::Backspace, _) => {
                    state.input.pop();
                }
                _ => {}
            }
        }
        2 => match (code, modifiers) {
            (KeyCode::Esc, _) => return false,
            (KeyCode::Enter, _) => state.next_step(),
            (KeyCode::Char(c), _) => state.input.push(c),
            (KeyCode::Backspace, _) => {
                state.input.pop();
            }
            _ => {}
        },
        _ => {}
    }
    true
}

fn render(f: &mut ratatui::Frame, state: &InitState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(f.area());

    let title = Paragraph::new(Line::from(vec![Span::styled(
        " Инициализация VPSAgent ",
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Cyan),
    )]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    let body_text = match state.step {
        0 => {
            // Шаг выбора провайдера рендерится отдельно ratatui::List ниже —
            // List с ListState сам скроллит к выделенному элементу (фикс бага,
            // когда курсор уходил за пределы экрана при длинном реестре).
            String::new()
        }
        1 => {
            if matches!(state.pick, Some(Pick::Custom)) {
                let labels = [
                    "название провайдера",
                    "base_url",
                    "API-ключ",
                    "протокол (1/2/3)",
                ];
                format!(
                    "Custom (шаг {}/4): {}\n\n> {}",
                    state.field_idx + 1,
                    labels[state.field_idx.min(3)],
                    state.input
                )
            } else if matches!(state.pick, Some(Pick::OauthChatGpt)) {
                format!(
                    "OAuth ChatGPT:\n\nВставьте токен из браузера:\n> {}",
                    mask(&state.input)
                )
            } else {
                format!("API-ключ:\n\n> {}", mask(&state.input))
            }
        }
        2 => {
            let models_list = if state.models.is_empty() {
                "(введите вручную)".to_string()
            } else {
                format!("Доступные: {}", state.models.join(", "))
            };
            format!(
                "Дефолтная модель:\n\n{}\n\n> {}",
                models_list,
                if state.input.is_empty() {
                    &state.default_model
                } else {
                    &state.input
                }
            )
        }
        3 => "Сохранение…".to_string(),
        _ => String::new(),
    };

    if state.step == 0 {
        // Рендерим список провайдеров через ratatui::List — ListState
        // автоматически поддерживает скролл к выделенному элементу, чтобы
        // курсор не уходил за пределы видимой области (фикс scroll-бага).
        let items: Vec<ListItem> = (0..state.menu_len)
            .map(|i| {
                let label = state.menu_label(i);
                if i == state.focus {
                    ListItem::new(format!("▶ {}", label)).style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ListItem::new(format!("  {}", label))
                }
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Настройка — выберите провайдера "),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
        let mut list_state = ListState::default();
        list_state.select(Some(state.focus));
        f.render_stateful_widget(list, chunks[1], &mut list_state);
    } else {
        let body = Paragraph::new(body_text)
            .block(Block::default().borders(Borders::ALL).title(" Настройка "))
            .wrap(Wrap { trim: false });
        f.render_widget(body, chunks[1]);
    }

    let status = Paragraph::new(state.status.as_str())
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(status, chunks[2]);
}

fn mask(key: &str) -> String {
    if key.len() <= 6 {
        "*".repeat(key.len())
    } else {
        format!("{}...{}", &key[..3], &key[key.len() - 3..])
    }
}

// Используем константы меню для полноты.
#[allow(dead_code)]
fn _ensure_menu() {
    let _ = (MENU_OAUTH_CHATGPT, MENU_CUSTOM);
}
