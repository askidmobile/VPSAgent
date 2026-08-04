//! Мастер инициализации VPSAgent (first-run setup).
//!
//! Источник провайдеров — models.dev (api.json), fallback — встроенный.
//! Поддерживаются: известные провайдеры (из реестра), OAuth ChatGPT, Custom
//! (ручной ввод названия/url/api key/протокола).

use std::collections::{HashMap, HashSet};
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

use crate::registry::{
    endpoint_kind, load_registry, provider_category, Category, RegistryProvider,
};

/// Режим шага 1: выбор провайдера.
#[derive(Clone, PartialEq)]
enum Pick {
    /// Из реестра (индекс в `registry`).
    Registry(usize),
    /// OAuth ChatGPT.
    OauthChatGpt,
    /// Ручной ввод.
    Custom,
}

/// Видимая строка на шаге 0 (выбор провайдера).
///
/// Плоский список `rows` пересчитывается при изменении поиска/раскрытия.
/// Фокус — индекс в `rows`, что сохраняет простоту навигации и скролл через
/// `ListState` (провайдеров может быть 180 — нужен авто-скролл курсора).
#[derive(Clone, PartialEq)]
enum Row {
    /// Строка ввода поиска (всегда первая).
    Search,
    /// Заголовок категории — раскрывает/сворачивает группу.
    CategoryHeader(Category),
    /// Провайдер из реестра (индекс в `registry`).
    Provider {
        /// Индекс в `InitState.registry` (для `Pick::Registry(idx)`).
        idx: usize,
    },
    /// OAuth ChatGPT (специальный пункт, вне категорий).
    OAuth,
    /// Custom — ручной ввод url/key/протокола (вне категорий).
    Custom,
}

/// Порядок категорий в списке (top-tier первыми, Other — последней).
const CATEGORY_ORDER: [Category; 5] = [
    Category::FirstParty,
    Category::Cloud,
    Category::Aggregator,
    Category::Local,
    Category::Other,
];

/// Состояние мастера.
struct InitState {
    /// Шаг: 0=выбор провайдера, 1=ключ/url/протокол, 2=модель, 3=запись.
    step: usize,
    /// Реестр провайдеров (с models.dev), отсортированный по категории потом имени.
    registry: Vec<RegistryProvider>,
    /// Видимые строки на шаге 0 (пересчитываются при поиске/раскрытии).
    rows: Vec<Row>,
    /// Раскрытые категории (если категория есть — провайдеры видны).
    expanded: HashSet<Category>,
    /// Текст поиска (фильтрует провайдеров по имени, case-insensitive).
    search: String,
    /// Текущий фокус в `rows` (0..rows.len()-1).
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
        let mut state = Self {
            step: 0,
            registry,
            rows: vec![],
            expanded: HashSet::new(),
            search: String::new(),
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
            status: "↑/↓ — выбор, Enter — раскрыть/выбрать, печатать — поиск, Esc — выход".into(),
            done: false,
        };
        state.rebuild_rows();
        state
    }

    /// Перестроить видимые строки (`rows`) из текущих `registry`/`expanded`/`search`.
    /// Клампит `focus` в `0..rows.len()`. При активном поиске категории
    /// автораскрываются (показываем все совпадения); заголовки без совпадений
    /// скрываются. OAuth/Custom показываются если запрос пуст или совпадает.
    fn rebuild_rows(&mut self) {
        let q = self.search.trim().to_lowercase();
        let filtering = !q.is_empty();
        let mut rows: Vec<Row> = vec![Row::Search];

        // Провайдер проходит фильтр поиска: имя содержит запрос (case-insensitive).
        // При активном поиске также проверяем id/url (ускорение поиска по техническим именам).
        let matches = |p: &RegistryProvider| {
            if !filtering {
                return true;
            }
            p.name.to_lowercase().contains(&q)
                || p.id.to_lowercase().contains(&q)
                || p.api
                    .as_deref()
                    .map(|u| u.to_lowercase().contains(&q))
                    .unwrap_or(false)
        };

        for cat in CATEGORY_ORDER {
            let in_cat: Vec<usize> = self
                .registry
                .iter()
                .enumerate()
                .filter(|(_, p)| provider_category(&p.id) == cat && matches(p))
                .map(|(i, _)| i)
                .collect();
            if in_cat.is_empty() {
                continue;
            }
            // Показывать провайдеров: категория раскрыта ИЛИ активен поиск
            // (при поиске все совпадения видны без ручного раскрытия).
            let show_providers = self.expanded.contains(&cat) || filtering;
            rows.push(Row::CategoryHeader(cat));
            if show_providers {
                for idx in in_cat {
                    rows.push(Row::Provider { idx });
                }
            }
        }

        // OAuth/Custom — внизу, вне категорий. При поиске скрываем, если не совпадает.
        let oauth_match = !filtering || "oauth chatgpt".contains(&q) || "chatgpt".contains(&q);
        let custom_match = !filtering
            || "custom".contains(&q)
            || "ручной ввод".contains(&q.as_str())
            || q.contains("custom");
        if oauth_match {
            rows.push(Row::OAuth);
        }
        if custom_match {
            rows.push(Row::Custom);
        }

        self.rows = rows;
        // Клампим фокус, сохраняя позицию на Search во время ввода поиска.
        if self.focus >= self.rows.len() {
            self.focus = self.rows.len().saturating_sub(1);
        }
    }

    fn next_step(&mut self) {
        match self.step {
            0 => {
                // Выбор по текущему фокусу в `rows` (а не плоскому индексу).
                let row = self.rows.get(self.focus).cloned();
                match row {
                    Some(Row::CategoryHeader(cat)) => {
                        // Toggle раскрытия категории, остаёмся на шаге 0.
                        if self.expanded.contains(&cat) {
                            self.expanded.remove(&cat);
                        } else {
                            self.expanded.insert(cat);
                        }
                        self.rebuild_rows();
                        return;
                    }
                    Some(Row::Provider { idx }) => {
                        self.pick = Some(Pick::Registry(idx));
                        self.input.clear();
                        self.field_idx = 0;
                        let p = &self.registry[idx];
                        self.models = p.models.clone();
                        self.status = format!("введите API-ключ для {} (Enter — дальше)", p.name);
                        self.step = 1;
                    }
                    Some(Row::OAuth) => {
                        self.pick = Some(Pick::OauthChatGpt);
                        self.input.clear();
                        self.field_idx = 0;
                        self.status =
                            "OAuth ChatGPT: вставьте токен из браузера (Enter — дальше)".into();
                        self.step = 1;
                    }
                    Some(Row::Custom) => {
                        self.pick = Some(Pick::Custom);
                        self.input.clear();
                        self.field_idx = 0;
                        self.status = "Custom: введите название провайдера (Enter — дальше)".into();
                        self.step = 1;
                    }
                    Some(Row::Search) => {
                        // Enter в поиске — фокус к первому провайдеру в результатах.
                        if let Some(pos) = self.rows.iter().position(|r| {
                            matches!(r, Row::Provider { .. } | Row::OAuth | Row::Custom)
                        }) {
                            self.focus = pos;
                        }
                        return;
                    }
                    None => return,
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
                // Гарантируем, что default_model есть в списке models эндпоинта.
                // Критично для Custom (models пуст — см. build_endpoint) и для
                // случая выбора модели не из списка реестра: endpoint_for_model
                // ищет эндпоинт по списку models, без этого запись → None →
                // «модель не найдена в конфиге» при спавне агента.
                if !self.models.iter().any(|m| m == &self.default_model) {
                    self.models.push(self.default_model.clone());
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
        0 => {
            // Фокус на строке поиска → печатаем; иначе Enter/стрелки/символ→поиск.
            let on_search = matches!(state.rows.get(state.focus), Some(Row::Search));
            match (code, modifiers, on_search) {
                (KeyCode::Esc, _, _) => return false,
                (KeyCode::Enter, _, false) => state.next_step(),
                // Навигация по списку (фокус не на Search).
                (KeyCode::Up, _, false) => {
                    if state.focus > 0 {
                        state.focus -= 1;
                    }
                }
                (KeyCode::Down, _, false) => {
                    if state.focus + 1 < state.rows.len() {
                        state.focus += 1;
                    }
                }
                // Backspace: удалить символ из поиска (или фокус вниз, если пуст).
                (KeyCode::Backspace, _, true) => {
                    if state.search.is_empty() {
                        if state.focus + 1 < state.rows.len() {
                            state.focus += 1; // уйти с Search вниз
                        }
                    } else {
                        state.search.pop();
                        state.rebuild_rows();
                        state.focus = 0; // остаться на Search
                    }
                }
                // Enter на Search — next_step (фокус к первому результату).
                (KeyCode::Enter, _, true) => state.next_step(),
                (KeyCode::Up, _, true) => {
                    // Up на Search — фокус вниз к списку (Search — верхняя строка).
                    if state.focus + 1 < state.rows.len() {
                        state.focus += 1;
                    }
                }
                (KeyCode::Down, _, true) => {
                    if state.focus + 1 < state.rows.len() {
                        state.focus += 1;
                    }
                }
                // Печать в поиск.
                (KeyCode::Char(c), _, true) => {
                    state.search.push(c);
                    state.rebuild_rows();
                    state.focus = 0; // остаться на Search
                }
                // На списке нажали символ — начинаем поиск: вставить символ,
                // фокус на Search (UX-ускорение «начал печатать → ищет»).
                (KeyCode::Char(c), _, false) => {
                    state.search.push(c);
                    state.rebuild_rows();
                    state.focus = 0;
                }
                (KeyCode::Backspace, _, false) => {
                    // Backspace на списке — стереть последний символ поиска.
                    if state.search.pop().is_some() {
                        state.rebuild_rows();
                    }
                    state.focus = 0; // к Search
                }
                _ => {}
            }
        }
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
        // Рендерим список через ratatui::List: провайдеры сгруппированы по
        // категориям (счётчик в заголовке), сверху — поле поиска. ListState
        // скроллит к выделенной строке (курсор не уходит за пределы экрана).
        let q = state.search.trim();
        let filtering = !q.is_empty();
        let items: Vec<ListItem> = state
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let focused = i == state.focus;
                match row {
                    Row::Search => {
                        let cursor = if focused { "▏" } else { " " };
                        let label = format!("🔍 {}{}", state.search, cursor);
                        ListItem::new(label).style(
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        )
                    }
                    Row::CategoryHeader(cat) => {
                        // Счётчик провайдеров в категории (после фильтра поиска).
                        let count = state
                            .registry
                            .iter()
                            .filter(|p| provider_category(&p.id) == *cat)
                            .filter(|p| {
                                if !filtering {
                                    return true;
                                }
                                let ql = q.to_lowercase();
                                p.name.to_lowercase().contains(&ql)
                                    || p.id.to_lowercase().contains(&ql)
                                    || p.api
                                        .as_deref()
                                        .map(|u| u.to_lowercase().contains(&ql))
                                        .unwrap_or(false)
                            })
                            .count();
                        let mark = if state.expanded.contains(cat) || filtering {
                            "▼"
                        } else {
                            "▶"
                        };
                        let label = format!("{} {} ({})", mark, cat.label(), count);
                        ListItem::new(label).style(
                            Style::default()
                                .fg(Color::Gray)
                                .add_modifier(Modifier::BOLD),
                        )
                    }
                    Row::Provider { idx } => {
                        let p = &state.registry[*idx];
                        let url = p.api.as_deref().unwrap_or("(дефолт)");
                        let prefix = if focused { "▶   " } else { "    " };
                        ListItem::new(format!("{prefix}{} — {}", p.name, url)).style(if focused {
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        })
                    }
                    Row::OAuth => {
                        let prefix = if focused { "▶ " } else { "  " };
                        ListItem::new(format!("{prefix}OAuth ChatGPT (логин через браузер)")).style(
                            if focused {
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            },
                        )
                    }
                    Row::Custom => {
                        let prefix = if focused { "▶ " } else { "  " };
                        ListItem::new(format!("{prefix}Custom (название, url, api key, протокол)"))
                            .style(if focused {
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            })
                    }
                }
            })
            .collect();
        let title = if filtering {
            " Настройка — поиск провайдеров "
        } else {
            " Настройка — выберите провайдера "
        };
        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Состояние с синтетическим реестром (без сети): 5 известных + 1 other.
    fn state_with_registry() -> InitState {
        let registry = vec![
            RegistryProvider {
                id: "openai".into(),
                name: "OpenAI".into(),
                api: Some("https://api.openai.com/v1".into()),
                npm: Some("@ai-sdk/openai".into()),
                models: vec!["gpt-5".into()],
            },
            RegistryProvider {
                id: "anthropic".into(),
                name: "Anthropic".into(),
                api: Some("https://api.anthropic.com".into()),
                npm: Some("@ai-sdk/anthropic".into()),
                models: vec!["claude-sonnet-4-5".into()],
            },
            RegistryProvider {
                id: "groq".into(),
                name: "Groq".into(),
                api: Some("https://api.groq.com/openai/v1".into()),
                npm: Some("@ai-sdk/groq".into()),
                models: vec![],
            },
            RegistryProvider {
                id: "openrouter".into(),
                name: "OpenRouter".into(),
                api: Some("https://openrouter.ai/api/v1".into()),
                npm: None,
                models: vec![],
            },
            RegistryProvider {
                id: "ollama".into(),
                name: "Ollama".into(),
                api: None,
                npm: None,
                models: vec![],
            },
            RegistryProvider {
                id: "kimi-for-coding".into(),
                name: "Kimi for Coding".into(),
                api: Some("https://api.kimi.com/coding".into()),
                npm: None,
                models: vec!["k3".into()],
            },
        ];
        let mut s = InitState {
            step: 0,
            registry,
            rows: vec![],
            expanded: HashSet::new(),
            search: String::new(),
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
            status: String::new(),
            done: false,
        };
        s.rebuild_rows();
        s
    }

    /// По умолчанию все категории свёрнуты: в rows только Search, заголовки
    /// категорий с провайдерами, OAuth и Custom. Без самих провайдеров.
    #[test]
    fn rebuild_rows_collapsed_by_default() {
        let s = state_with_registry();
        // Search + 4 заголовка (FirstParty, Cloud, Aggregator, Local) + OAuth + Custom.
        // Other (Kimi for Coding) — категория есть, заголовок тоже.
        let headers = s
            .rows
            .iter()
            .filter(|r| matches!(r, Row::CategoryHeader(_)))
            .count();
        assert_eq!(headers, 5, "5 категорий с провайдерами → 5 заголовков");
        let providers = s
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Provider { .. }))
            .count();
        assert_eq!(providers, 0, "свёрнуты → 0 провайдеров в rows");
        assert!(matches!(s.rows.first(), Some(Row::Search)));
        assert!(s.rows.iter().any(|r| matches!(r, Row::OAuth)));
        assert!(s.rows.iter().any(|r| matches!(r, Row::Custom)));
    }

    /// Раскрытие категории показывает провайдеров внутри.
    #[test]
    fn expand_category_shows_providers() {
        let mut s = state_with_registry();
        s.expanded.insert(Category::FirstParty);
        s.rebuild_rows();
        let providers: Vec<_> = s
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Provider { idx } => Some(s.registry[*idx].name.clone()),
                _ => None,
            })
            .collect();
        assert!(
            providers.iter().any(|n| n == "OpenAI"),
            "OpenAI должен появиться: {providers:?}"
        );
        assert!(
            providers.iter().any(|n| n == "Anthropic"),
            "Anthropic должен появиться: {providers:?}"
        );
        assert!(
            !providers.iter().any(|n| n == "Groq"),
            "Groq — Cloud, категория не раскрыта: {providers:?}"
        );
    }

    /// Поиск фильтрует провайдеров по имени (case-insensitive) и
    /// автораскрывает совпавшие категории.
    #[test]
    fn search_filters_by_name() {
        let mut s = state_with_registry();
        s.search = "kimi".into();
        s.rebuild_rows();
        let providers: Vec<_> = s
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Provider { idx } => Some(s.registry[*idx].name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(providers, vec!["Kimi for Coding".to_string()]);
        // При активном поиске Other-категория автораскрывается.
        assert!(s
            .rows
            .iter()
            .any(|r| matches!(r, Row::CategoryHeader(Category::Other))));
    }

    /// Поиск по id/url тоже работает (ускорение по техническим именам).
    #[test]
    fn search_by_id_and_url() {
        let mut s = state_with_registry();
        s.search = "openai".into();
        s.rebuild_rows();
        let providers: Vec<_> = s
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Provider { idx } => Some(s.registry[*idx].id.clone()),
                _ => None,
            })
            .collect();
        assert!(
            providers.contains(&"openai".to_string()),
            "по id: {providers:?}"
        );
        // Очистка — полный список.
        s.search.clear();
        s.rebuild_rows();
        let p = s
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Provider { .. }))
            .count();
        assert_eq!(p, 0, "поиск пуст, все свёрнуты → 0 провайдеров");
    }

    /// Выбор провайдера через Row::Provider ставит Pick::Registry(idx).
    #[test]
    fn next_step_selects_provider_by_row() {
        let mut s = state_with_registry();
        s.expanded.insert(Category::FirstParty);
        s.rebuild_rows();
        // Фокус на первый Row::Provider (OpenAI, idx 0 в registry).
        let pos = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Provider { .. }))
            .unwrap();
        s.focus = pos;
        s.next_step();
        assert_eq!(s.step, 1, "должен перейти на шаг 1");
        assert!(
            matches!(s.pick, Some(Pick::Registry(0))),
            "Pick::Registry(0)"
        );
        assert_eq!(s.models, vec!["gpt-5".to_string()]);
    }

    /// Enter на заголовке категории — toggle раскрытия, остаёмся на шаге 0.
    #[test]
    fn next_step_toggles_category() {
        let mut s = state_with_registry();
        let pos = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::CategoryHeader(Category::FirstParty)))
            .unwrap();
        s.focus = pos;
        s.next_step();
        assert_eq!(s.step, 0, "остаёмся на шаге 0");
        assert!(
            s.expanded.contains(&Category::FirstParty),
            "категория должна раскрыться"
        );
        // Повторный Enter — сворачивает.
        s.next_step();
        assert!(
            !s.expanded.contains(&Category::FirstParty),
            "категория должна свернуться"
        );
    }

    /// Классификация провайдеров по категориям.
    #[test]
    fn provider_category_classification() {
        assert_eq!(provider_category("openai"), Category::FirstParty);
        assert_eq!(provider_category("anthropic"), Category::FirstParty);
        assert_eq!(provider_category("groq"), Category::Cloud);
        assert_eq!(provider_category("openrouter"), Category::Aggregator);
        assert_eq!(provider_category("ollama"), Category::Local);
        assert_eq!(provider_category("kimi-for-coding"), Category::Other);
        assert_eq!(provider_category("random-unknown-id"), Category::Other);
    }
}
