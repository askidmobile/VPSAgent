//! Виджеты TUI: фокус-панель стрима + ввод + статус-бар.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;

/// Главный рендер.
pub fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(deck_height(app)), // deck-обзор агентов
            Constraint::Min(3),                   // стрим агента
            Constraint::Length(3),                // ввод
            Constraint::Length(1),                // статус
        ])
        .split(f.area());

    render_deck(f, app, chunks[0]);
    render_focus(f, app, chunks[1]);

    // Ввод.
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(" ввод (Enter — отправить) ");
    let input_style = if app.agent_busy {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let input = Paragraph::new(app.input.as_str())
        .block(input_block)
        .style(input_style);
    f.render_widget(input, chunks[2]);
    // Курсор (по символам, не байтам — кириллица 2 байта на char).
    let input_area = chunks[2];
    let cursor_x = input_area.x
        + 1
        + (app.input.chars().count() as u16).min(input_area.width.saturating_sub(2));
    let cursor_y = input_area.y + 1;
    f.set_cursor_position((cursor_x, cursor_y));

    // Статус-бар: [provider] model · tokens: input/output · статус.
    let status_style = Style::default().add_modifier(Modifier::REVERSED);
    let busy_span = Span::styled(
        if app.agent_busy {
            " ● работаю "
        } else {
            " ○ готов "
        },
        Style::default().fg(if app.agent_busy {
            Color::Green
        } else {
            Color::DarkGray
        }),
    );
    let mut spans: Vec<Span> = vec![];
    if !app.current_provider.is_empty() {
        spans.push(Span::styled(
            format!("[{}] ", app.current_provider),
            Style::default().fg(Color::Cyan),
        ));
    }
    if !app.current_model.is_empty() {
        spans.push(Span::raw(format!("{} · ", app.current_model)));
    }
    let tu = &app.token_usage;
    if tu.input > 0 || tu.output > 0 {
        spans.push(Span::raw(format!(
            "токены: {}/{} · ",
            format_tokens(tu.input),
            format_tokens(tu.output)
        )));
    }
    spans.push(busy_span);
    spans.push(Span::raw("  "));
    spans.push(Span::styled(&app.status, status_style));
    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line), chunks[3]);
}

/// Формат числа токенов: 1.2K, 456, 1.5M.
fn format_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Высота deck-панели: 1 строка на агента, максимум 5.
fn deck_height(app: &App) -> u16 {
    let n = app.agents.len();
    if n == 0 {
        0
    } else {
        (n as u16 + 1).min(5)
    }
}

/// Рендер deck-обзора: карточки агентов.
fn render_deck(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    if app.agents.is_empty() {
        return;
    }
    let block = Block::default().borders(Borders::ALL).title(" агенты ");
    let mut lines: Vec<Line> = vec![];
    for a in &app.agents {
        let status_char = match a.status {
            vpsagent_core::AgentStatus::Working => "●",
            vpsagent_core::AgentStatus::Done => "✓",
            vpsagent_core::AgentStatus::Failed => "✗",
            _ => "○",
        };
        let color = match a.status {
            vpsagent_core::AgentStatus::Working => Color::Green,
            vpsagent_core::AgentStatus::Done => Color::DarkGray,
            vpsagent_core::AgentStatus::Failed => Color::Red,
            _ => Color::Yellow,
        };
        let selected = app.selected_agent == Some(a.id);
        let sel_mark = if selected { ">" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{sel_mark}{status_char} "),
                Style::default().fg(color),
            ),
            Span::raw(format!("{:<16} ", a.name)),
            Span::raw(format!("{} ", a.last_action)),
            Span::styled(
                format!("{}", a.tokens.input + a.tokens.output),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Рендер фокус-панели стрима агента.
fn render_focus(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let title = match (&app.session, app.agent_busy) {
        (Some(s), true) => format!(" {} — работаю ", s.title),
        (Some(s), false) => format!(" {} ", s.title),
        (None, _) => " нет сессии ".to_string(),
    };
    let stream_block = Block::default().borders(Borders::ALL).title(title);
    let text = app.current_text();
    let stream = if text.is_empty() {
        Paragraph::new("(пусто — отправьте задачу)")
            .block(stream_block)
            .style(Style::default().fg(Color::DarkGray))
    } else {
        // Автоскролл вниз: показываем хвост, а не уводим контент за экран.
        let inner_h = area.height.saturating_sub(2);
        let h = text.lines().count() as u16;
        let scroll = if app.auto_scroll {
            h.saturating_sub(inner_h)
        } else {
            0
        };
        Paragraph::new(text)
            .block(stream_block)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
    };
    f.render_widget(stream, area);
}

/// Область стрима для вычисления прокрутки (заглушка для будущих компонент).
#[allow(dead_code)]
fn stream_area(parent: Rect) -> Rect {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(4)])
        .split(parent)[0]
}
