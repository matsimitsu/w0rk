//! Renders [`App`] with ratatui, laid out like the option A mock-up.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::state::{App, Hit, HitTarget, Modal, RowRef, ToastKind};
use super::views::{
    self, BLUE, CYAN, DIM, GREEN, MAGENTA, RED, YELLOW, age, dim_line, heading, kv, title_line,
};
use crate::model::{Item, Run, RunState};

/// What each dot means, shown in the help screen.
const LEGEND: [(char, Color, &str); 11] = [
    ('●', BLUE, "new review request"),
    ('●', MAGENTA, "Dependabot update"),
    ('◆', YELLOW, "re-review: you reviewed it, now asked again"),
    (
        '●',
        RED,
        "open incident nobody is on, or changes requested on your PR",
    ),
    (
        '◐',
        YELLOW,
        "someone is on it: incident WIP or assigned, your PR has reviews",
    ),
    ('●', GREEN, "your PR is approved"),
    (
        '○',
        DIM,
        "nothing for you: your PR awaits review, incident closed",
    ),
    ('⠋', CYAN, "your agent is working on it"),
    ('?', YELLOW, "your agent is waiting for an answer"),
    ('!', YELLOW, "your agent finished and wants you to look"),
    ('✓', GREEN, "your agent finished"),
];

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SELECTED_BG: Color = Color::Rgb(31, 42, 55);
const KEY_BG: Color = Color::Rgb(42, 53, 66);
const FRESH_BG: Color = Color::Rgb(44, 39, 23);

pub fn draw(frame: &mut Frame, app: &mut App, now: DateTime<Utc>) {
    let area = frame.area();
    if area.width < 60 || area.height < 12 {
        frame.render_widget(
            Paragraph::new("Terminal too small for cockpit (need 60x12)."),
            area,
        );
        return;
    }
    let [top, body, status, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);

    let mut hits = Vec::new();
    draw_top_bar(frame, app, top, now);
    draw_list(frame, app, left, now, &mut hits);
    draw_details(frame, app, right, now);
    draw_status(frame, app, status, now);
    draw_keybar(frame, app, keys, &mut hits);
    if let Some(modal) = app.modal.clone() {
        draw_modal(frame, app, &modal, area);
    }
    app.hits = hits;
}

fn state_style(state: RunState) -> Style {
    match state {
        RunState::Running => Style::default().fg(CYAN),
        RunState::Waiting => Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
        RunState::Queued | RunState::Stopped => Style::default().fg(DIM),
        RunState::Done => Style::default().fg(GREEN),
    }
}

fn glyph(app: &App, state: RunState) -> char {
    match state {
        RunState::Running => SPINNER[app.tick % SPINNER.len()],
        RunState::Queued => '·',
        RunState::Waiting => '?',
        RunState::Done => '✓',
        RunState::Stopped => '✗',
    }
}

/// Like [`fit`], but when a reference such as `appsignal-processor-rs#2427` is too long the
/// repository name gives way and the number stays visible: `appsignal-processo…#2427`.
fn fit_reference(reference: &str, width: usize) -> String {
    if reference.chars().count() <= width {
        return fit(reference, width);
    }
    match reference.rsplit_once('#') {
        Some((name, number)) => {
            let suffix = format!("#{number}");
            let room = width.saturating_sub(suffix.chars().count() + 1);
            if room == 0 {
                return fit(reference, width);
            }
            let kept: String = name.chars().take(room).collect();
            fit(&format!("{kept}…{suffix}"), width)
        }
        None => fit(reference, width),
    }
}

/// Truncate with an ellipsis, or pad with spaces, to exactly `width` characters.
fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
        if width > 0 {
            out.push('…');
        }
        out
    } else {
        format!("{text}{}", " ".repeat(width - count))
    }
}

fn elapsed(run: &Run, now: DateTime<Utc>) -> String {
    let end = if run.state.is_active() {
        now
    } else {
        run.updated_at
    };
    age(end, run.created_at)
}

fn draw_top_bar(frame: &mut Frame, app: &App, area: Rect, now: DateTime<Utc>) {
    let mut spans = vec![Span::styled(
        " ◆ cockpit ",
        // No background: Rex paints the top padding with the first row's background, which makes
        // a coloured bar look two rows tall.
        Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
    )];
    let dim = Style::default().fg(DIM);
    for (section, items) in app.sections.iter().zip(app.section_items()) {
        spans.push(Span::raw("  "));
        let color = if items.is_empty() {
            Color::Reset
        } else {
            Color::White
        };
        spans.push(Span::styled(
            format!("{} {}", items.len(), section.title.to_lowercase()),
            Style::default().fg(color),
        ));
    }
    let runs = app.visible_runs();
    let running = runs
        .iter()
        .filter(|r| matches!(r.state, RunState::Running | RunState::Queued))
        .count();
    let waiting = runs.iter().filter(|r| r.state == RunState::Waiting).count();
    spans.push(Span::styled("  ·  ", dim));
    spans.push(Span::styled(
        format!("{running} running"),
        Style::default().fg(CYAN),
    ));
    spans.push(Span::styled("  ·  ", dim));
    let waiting_style = if waiting > 0 {
        Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)
    } else {
        dim
    };
    spans.push(Span::styled(
        format!("{waiting} waiting on you"),
        waiting_style,
    ));
    if app.ignored_count() > 0 {
        let shown = if app.showing_ignored() { " shown" } else { "" };
        spans.push(Span::styled(
            format!("  ·  {} ignored{shown}", app.ignored_count()),
            dim,
        ));
    }
    if app.hidden_count() > 0 {
        spans.push(Span::styled(
            format!("  ·  {} hidden", app.hidden_count()),
            dim,
        ));
    }

    let right = match app.last_sync {
        Some(at) => format!("synced {} ago  ", age(now, at)),
        None => "syncing…  ".to_owned(),
    };
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if let Some(pad) = (area.width as usize).checked_sub(used + right.chars().count()) {
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(right, dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Column widths that shrink with the list: (tag, reference, agent badge).
fn columns(width: usize) -> (usize, usize, usize) {
    let tag = 10;
    let reference = (width / 3).clamp(10, 30);
    let badge = if width < 60 { 2 } else { 11 };
    (tag, reference, badge)
}

/// The dot in front of an item: a running agent, one waiting on you or one that finished takes
/// over from the item's own state.
fn row_dot(app: &App, item: &Item, own: views::Dot) -> (char, Color) {
    use crate::model::Verdict;
    match app.run_for_item(&item.id) {
        Some(run) if run.state == RunState::Waiting => ('?', YELLOW),
        Some(run) if matches!(run.state, RunState::Running | RunState::Queued) => {
            (glyph(app, run.state), CYAN)
        }
        Some(run) if run.state == RunState::Done && run.verdict == Some(Verdict::NeedsYou) => {
            ('!', YELLOW)
        }
        Some(run) if run.state == RunState::Done => ('✓', GREEN),
        _ => (own.glyph, own.color),
    }
}

fn item_row(
    app: &App,
    item: &Item,
    selected: bool,
    width: usize,
    now: DateTime<Utc>,
) -> Line<'static> {
    let parts = views::view_for(item).row(item);
    let fresh = app.is_fresh(&item.id, now);
    let agent = app.run_for_item(&item.id).map(|r| {
        let label = if r.state == RunState::Waiting {
            "waiting"
        } else {
            "agent"
        };
        (
            format!("{} {label}", glyph(app, r.state)),
            state_style(r.state),
        )
    });
    let ignored = app.is_ignored(&item.id);
    let tag = if ignored {
        "ignored".to_owned()
    } else if fresh {
        "NEW".to_owned()
    } else {
        parts.tag.clone()
    };
    let tag_style = if ignored {
        Style::default().fg(DIM)
    } else if fresh {
        Style::default().fg(RED).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(parts.tag_color)
    };
    let (tag_width, ref_width, badge_width) = columns(width);
    let title_width = width.saturating_sub(2 + 2 + tag_width + ref_width + 1 + badge_width + 4);
    let (dot, dot_color) = row_dot(app, item, parts.dot);
    let mut spans = vec![
        Span::styled(
            if selected { "▸ " } else { "  " },
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{dot} "),
            Style::default().fg(dot_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(fit(&tag, tag_width), tag_style),
        Span::styled(
            fit_reference(&parts.reference, ref_width),
            Style::default().add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        Span::raw(" "),
        Span::raw(fit(&parts.title, title_width)),
    ];
    let (badge, badge_style) = agent.unwrap_or_default();
    spans.push(Span::styled(
        format!(" {}", fit(&badge, badge_width - 1)),
        badge_style,
    ));
    spans.push(Span::styled(
        format!("{:>4}", age(now, parts.age)),
        Style::default().fg(DIM),
    ));
    let mut line = Line::from(spans);
    if fresh {
        line = line.style(Style::default().bg(FRESH_BG));
    }
    if selected {
        line = line.style(Style::default().bg(SELECTED_BG));
    }
    line
}

fn run_row(
    app: &App,
    run: &Run,
    selected: bool,
    width: usize,
    now: DateTime<Utc>,
) -> Line<'static> {
    let reference = app.item(&run.item_id).map_or_else(
        || run.item_id.to_string(),
        |item| views::view_for(item).row(item).reference,
    );
    let style = state_style(run.state);
    let label = match run.state {
        RunState::Waiting => "waiting".to_owned(),
        other => other.as_str().to_owned(),
    };
    let (tag_width, ref_width, _) = columns(width);
    let title_width = width.saturating_sub(2 + 2 + tag_width + ref_width + 1 + 4);
    let mut line = Line::from(vec![
        Span::styled(
            if selected { "▸ " } else { "  " },
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", glyph(app, run.state)),
            style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(fit(&label, tag_width), style),
        Span::styled(
            fit_reference(&reference, ref_width),
            Style::default().add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        Span::raw(" "),
        Span::raw(fit(run.profile.as_str(), title_width)),
        Span::styled(
            format!("{:>4}", elapsed(run, now)),
            Style::default().fg(DIM),
        ),
    ]);
    if selected {
        line = line.style(Style::default().bg(SELECTED_BG));
    }
    line
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect, now: DateTime<Utc>, hits: &mut Vec<Hit>) {
    let block = Block::bordered()
        .border_style(Style::default().fg(DIM))
        .title(" To do ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row_of_line: Vec<Option<RowRef>> = Vec::new();
    let selected = app.selected();
    let mut selected_line = 0;

    let mut push = |line: Line<'static>, row: Option<RowRef>, lines: &mut Vec<Line<'static>>| {
        lines.push(line);
        row_of_line.push(row);
    };

    for (section, items) in app.sections.iter().zip(app.section_items()) {
        push(
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    section.title.clone(),
                    Style::default().fg(MAGENTA).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" ({})", items.len()), Style::default().fg(DIM)),
            ]),
            None,
            &mut lines,
        );
        if items.is_empty() {
            push(dim_line("    nothing here"), None, &mut lines);
        }
        for item in items {
            let row = RowRef::Item(item.id.clone());
            let is_selected = selected == Some(&row);
            if is_selected {
                selected_line = lines.len();
            }
            push(
                item_row(app, item, is_selected, width, now),
                Some(row),
                &mut lines,
            );
        }
        push(Line::default(), None, &mut lines);
    }

    let runs = app.visible_runs();
    push(
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "AGENTS",
                Style::default().fg(MAGENTA).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" ({})", runs.len()), Style::default().fg(DIM)),
        ]),
        None,
        &mut lines,
    );
    if runs.is_empty() {
        push(
            dim_line("    No agents yet. Select an item and press s."),
            None,
            &mut lines,
        );
    }
    for run in runs {
        let row = RowRef::Run(run.id);
        let is_selected = selected == Some(&row);
        if is_selected {
            selected_line = lines.len();
        }
        push(
            run_row(app, run, is_selected, width, now),
            Some(row),
            &mut lines,
        );
    }

    // Keep the selected line on screen.
    let height = inner.height as usize;
    let offset = selected_line.saturating_sub(height.saturating_sub(1));
    for (i, row) in row_of_line.iter().enumerate().skip(offset).take(height) {
        if let Some(row) = row {
            let y = inner.y + (i - offset) as u16;
            hits.push(Hit {
                area: Rect::new(inner.x, y, inner.width, 1),
                target: HitTarget::Row(row.clone()),
            });
        }
    }
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(height).collect();
    frame.render_widget(Paragraph::new(visible), inner);
}

fn agent_lines(app: &App, run: &Run, height: usize, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let style = state_style(run.state);
    let state_text = if run.state == RunState::Waiting {
        "waiting on you"
    } else {
        run.state.as_str()
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{} ", glyph(app, run.state)),
            style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(state_text.to_owned(), style.add_modifier(Modifier::BOLD)),
    ])];
    lines.push(title_line(run.profile.as_str()));
    if let Some(item) = app.item(&run.item_id) {
        lines.push(dim_line(&format!(
            "{} · {}",
            views::view_for(item).row(item).reference,
            item.title
        )));
    } else {
        lines.push(dim_line(run.item_id.as_str()));
    }
    lines.push(Line::default());
    if let Some(block) = &run.block {
        lines.push(kv("pane", block.to_string(), Some(CYAN)));
    }
    lines.push(kv("time", elapsed(run, now), None));
    if let Some(verdict) = run.verdict {
        let color = if verdict == crate::model::Verdict::Ok {
            GREEN
        } else {
            YELLOW
        };
        lines.push(kv("verdict", verdict.as_str(), Some(color)));
    }
    if let Some(summary) = &run.summary {
        lines.push(kv("summary", summary.clone(), None));
    }
    lines.push(Line::default());
    lines.push(heading("OUTPUT"));
    let reserve = if run.state == RunState::Waiting { 2 } else { 0 };
    let room = height.saturating_sub(lines.len() + reserve).max(1);
    let captured = run.block.as_ref().and_then(|b| app.capture(b));
    match captured {
        Some(text) => {
            let tail: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            let start = tail.len().saturating_sub(room);
            lines.extend(tail[start..].iter().map(|l| Line::from(format!("│ {l}"))));
        }
        None if run.state == RunState::Queued => lines.push(dim_line("│ waiting for a free slot…")),
        None => lines.push(dim_line("│ …")),
    }
    if run.state == RunState::Waiting {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Press y or n to answer here, or g to open the pane.",
            Style::default().fg(YELLOW),
        )));
    }
    lines
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect, now: DateTime<Utc>) {
    let block = Block::bordered()
        .border_style(Style::default().fg(DIM))
        .title(" Details ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };

    let lines = match app.selected() {
        None => vec![dim_line("Nothing selected.")],
        Some(RowRef::Run(id)) => app.run(*id).map_or_else(Vec::new, |run| {
            agent_lines(app, run, inner.height as usize, now)
        }),
        Some(RowRef::Item(_)) => {
            let Some(item) = app.selected_item() else {
                frame.render_widget(Paragraph::new(dim_line("Nothing selected.")), inner);
                return;
            };
            let mut lines = views::view_for(item).detail(item);
            lines.push(Line::default());
            lines.push(heading("AGENT"));
            match app.run_for_item(&item.id) {
                None => lines.push(dim_line(
                    "None yet. Press s to start a profile or p to write a prompt.",
                )),
                Some(run) => {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{} {}", glyph(app, run.state), run.state.as_str()),
                            state_style(run.state).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("  {}", run.profile), Style::default().fg(DIM)),
                    ]));
                    if let Some(summary) = &run.summary {
                        lines.push(Line::from(format!("│ {summary}")));
                    }
                }
            }
            lines
        }
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect, now: DateTime<Utc>) {
    let line = if let Some(toast) = app.toast(now) {
        let color = match toast.kind {
            ToastKind::Info => Color::Reset,
            ToastKind::Good => GREEN,
            ToastKind::Attention => YELLOW,
            ToastKind::Bad => RED,
        };
        Line::from(Span::styled(
            format!(" {}", toast.text),
            Style::default().fg(color),
        ))
    } else if let Some((source, error)) = app.health.iter().next() {
        Line::from(Span::styled(
            format!(" ! {source}: {error}"),
            Style::default().fg(RED),
        ))
    } else {
        Line::default()
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn keybar_items(app: &App) -> Vec<(&'static str, &'static str, KeyCode)> {
    let waiting = app
        .selected_run()
        .is_some_and(|r| r.state == RunState::Waiting);
    let x_label = match app.selected() {
        Some(RowRef::Run(id)) if app.run(*id).is_some_and(|r| r.state.is_active()) => "stop",
        Some(RowRef::Run(_)) => "clear",
        _ => "dismiss",
    };
    let mut keys = vec![
        ("j/k", "move", KeyCode::Char('j')),
        ("⏎", "open", KeyCode::Enter),
        ("s", "run", KeyCode::Char('s')),
        ("p", "ask", KeyCode::Char('p')),
        ("g", "rex", KeyCode::Char('g')),
    ];
    if waiting {
        keys.push(("y/n", "reply", KeyCode::Char('y')));
    }
    keys.extend([
        ("x", x_label, KeyCode::Char('x')),
        ("i", "ignore", KeyCode::Char('i')),
        ("f", "filter", KeyCode::Char('f')),
        ("r", "sync", KeyCode::Char('r')),
        ("?", "help", KeyCode::Char('?')),
        ("q", "quit", KeyCode::Char('q')),
    ]);
    keys
}

fn draw_keybar(frame: &mut Frame, app: &App, area: Rect, hits: &mut Vec<Hit>) {
    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    for (key, label, code) in keybar_items(app) {
        let key_text = format!(" {key} ");
        let label_text = format!(" {label} ");
        let width = (key_text.chars().count() + label_text.chars().count()) as u16;
        if x + width > area.x + area.width {
            break;
        }
        hits.push(Hit {
            area: Rect::new(x, area.y, width, 1),
            target: HitTarget::Key(code),
        });
        spans.push(Span::styled(
            key_text,
            Style::default().bg(KEY_BG).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(label_text, Style::default().fg(DIM)));
        x += width;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn modal_block(title: &str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(format!(" {title} "))
        .style(Style::default().bg(Color::Rgb(23, 29, 37)))
}

fn draw_modal(frame: &mut Frame, app: &App, modal: &Modal, area: Rect) {
    match modal {
        Modal::Spawn {
            item,
            choices,
            index,
        } => {
            let rect = centered(area, 78, choices.len() as u16 + 7);
            frame.render_widget(Clear, rect);
            let block = modal_block("Start an agent", BLUE);
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let mut lines = vec![Line::from(vec![
                Span::styled(" on ", Style::default().fg(DIM)),
                Span::styled(
                    app.item(item)
                        .map_or_else(|| item.to_string(), |i| i.title.clone()),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ])];
            lines.push(Line::default());
            for (i, choice) in choices.iter().enumerate() {
                let selected = i == *index;
                let mut line = Line::from(vec![
                    Span::styled(
                        if selected { " ▸ " } else { "   " },
                        Style::default().fg(BLUE),
                    ),
                    Span::styled(
                        format!("{:<20}", choice.name),
                        Style::default().add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                    ),
                    Span::styled(
                        fit(&choice.description, inner.width.saturating_sub(26) as usize),
                        Style::default().fg(DIM),
                    ),
                ]);
                if selected {
                    line = line.style(Style::default().bg(SELECTED_BG));
                }
                lines.push(line);
            }
            lines.push(Line::default());
            lines.push(dim_line(" ⏎ start · j/k choose · esc cancel"));
            frame.render_widget(Paragraph::new(lines), inner);
        }
        Modal::Prompt { item, base, text } => {
            let rect = centered(area, 78, 9);
            frame.render_widget(Clear, rect);
            let block = modal_block("Prompt an agent", YELLOW);
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let cursor = if app.tick % 6 < 3 { "█" } else { " " };
            let room = inner.width.saturating_sub(6) as usize;
            let shown: String = if text.chars().count() > room {
                format!(
                    "…{}",
                    text.chars()
                        .skip(text.chars().count() - room + 1)
                        .collect::<String>()
                )
            } else {
                text.clone()
            };
            let lines = vec![
                Line::from(vec![
                    Span::styled(" on ", Style::default().fg(DIM)),
                    Span::styled(
                        app.item(item)
                            .map_or_else(|| item.to_string(), |i| i.title.clone()),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::default(),
                Line::from(vec![
                    Span::styled(" › ", Style::default().fg(YELLOW)),
                    Span::raw(shown),
                    Span::raw(cursor),
                ]),
                Line::default(),
                dim_line(&format!(
                    " runs in the `{base}` profile's directory, with the item attached"
                )),
                Line::default(),
                dim_line(" ⏎ start · esc cancel"),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
        }
        Modal::Filter { index } => {
            let namespaces = app.namespaces();
            let rect = centered(area, 54, namespaces.len().max(1) as u16 + 7);
            frame.render_widget(Clear, rect);
            let block = modal_block("Namespaces", BLUE);
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let mut lines = vec![
                dim_line(" Incidents in unchecked namespaces are hidden."),
                Line::default(),
            ];
            if namespaces.is_empty() {
                lines.push(dim_line("   no namespaces seen yet"));
            }
            for (i, ns) in namespaces.iter().enumerate() {
                let on = !app.is_hidden(ns);
                let mut line = Line::from(vec![
                    Span::styled(
                        if i == *index { " ▸ " } else { "   " },
                        Style::default().fg(BLUE),
                    ),
                    Span::styled(
                        if on { "[x] " } else { "[ ] " },
                        Style::default().fg(if on { GREEN } else { DIM }),
                    ),
                    Span::styled(
                        ns.to_string(),
                        Style::default().fg(if on { Color::Reset } else { DIM }),
                    ),
                ]);
                if i == *index {
                    line = line.style(Style::default().bg(SELECTED_BG));
                }
                lines.push(line);
            }
            lines.push(Line::default());
            lines.push(dim_line(" space toggle · j/k move · ⏎ done"));
            frame.render_widget(Paragraph::new(lines), inner);
        }
        Modal::Help { dots } => {
            let rows = [
                ("j k ↑ ↓", "move"),
                ("tab", "next section"),
                ("⏎", "open in browser; on an agent, jump to its item"),
                ("s", "start a profile on this item"),
                ("p", "start an agent with your own prompt"),
                ("g", "show the agent's pane in Rex"),
                ("y n", "answer a waiting agent"),
                ("x", "dismiss an item; stop, then clear an agent"),
                ("i", "ignore for good (again to restore)"),
                ("I", "show or hide ignored items"),
                ("u", "undo dismiss or ignore"),
                ("f", "choose hidden namespaces"),
                ("r", "sync now"),
                ("mouse", "click selects, double-click opens, wheel scrolls"),
                ("q", "quit (agents keep running in Rex)"),
            ];
            let (title, count, footer) = if *dots {
                (
                    "Dots",
                    LEGEND.len(),
                    "  tab for keys · any other key closes",
                )
            } else {
                (
                    "Keys",
                    rows.len(),
                    "  tab for the dot legend · any other key closes",
                )
            };
            let rect = centered(area, 78, count as u16 + 6);
            frame.render_widget(Clear, rect);
            let block = modal_block(title, BLUE);
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let mut lines: Vec<Line<'static>> = vec![Line::default()];
            if *dots {
                lines.extend(LEGEND.iter().map(|(glyph, color, meaning)| {
                    Line::from(vec![
                        Span::styled(
                            format!("  {glyph}   "),
                            Style::default().fg(*color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled((*meaning).to_owned(), Style::default().fg(DIM)),
                    ])
                }));
            } else {
                lines.extend(rows.iter().map(|(k, d)| {
                    Line::from(vec![
                        Span::styled(
                            format!("  {k:<10}"),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled((*d).to_owned(), Style::default().fg(DIM)),
                    ])
                }));
            }
            lines.push(Line::default());
            lines.push(dim_line(footer));
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    use super::*;
    use crate::model::{BlockId, Fingerprint, ItemId, ItemKind, ProfileName, RunId, SourceId};
    use crate::ui::state::ProfileChoice;

    fn now() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH + Duration::seconds(10_000)
    }

    fn app() -> App {
        let mut app = App::new(
            Vec::new(),
            Vec::new(),
            vec![ProfileChoice {
                name: ProfileName::new("code-review"),
                description: "review the diff".to_owned(),
            }],
        );
        let pr = Item {
            id: ItemId::new("github:appsignal/appsignal-server#1"),
            source: SourceId::new("github"),
            kind: ItemKind::new("pr.review"),
            title: "Fix sampler race".to_owned(),
            url: "https://x".to_owned(),
            fingerprint: Fingerprint::new("a"),
            updated_at: now() - Duration::seconds(7200),
            attrs: json!({"repo": "appsignal/appsignal-server", "number": 1, "author": "alex"}),
        };
        let incident = Item {
            id: ItemId::new("appsignal:AppSignal#9"),
            source: SourceId::new("appsignal:AppSignal"),
            kind: ItemKind::new("incident.exception"),
            title: "NoMethodError".to_owned(),
            url: String::new(),
            fingerprint: Fingerprint::new("OPEN"),
            updated_at: now() - Duration::seconds(300),
            attrs: json!({"app": "AppSignal", "number": 9, "namespace": "web", "message": "undefined method"}),
        };
        app.set_items(SourceId::new("github"), vec![pr], now());
        app.set_items(SourceId::new("appsignal:AppSignal"), vec![incident], now());
        app.set_runs(
            vec![Run {
                id: RunId::new(1),
                item_id: ItemId::new("github:appsignal/appsignal-server#1"),
                fingerprint: Fingerprint::new("a"),
                profile: ProfileName::new("code-review"),
                block: Some(BlockId::new("block:1")),
                state: RunState::Waiting,
                verdict: None,
                summary: None,
                created_at: now() - Duration::seconds(120),
                updated_at: now(),
            }],
            now(),
        );
        app
    }

    fn render(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|f| draw(f, app, now())).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_sections_rows_agents_and_chrome() {
        let mut app = app();
        let screen = render(&mut app);
        for expected in [
            "cockpit",
            "TO REVIEW (1)",
            "MY PULL REQUESTS (0)",
            "INCIDENTS (1)",
            "AGENTS (1)",
            "appsignal-server#1",
            "Fix sampler race",
            "NoMethodError",
            "review",
            "waiting",
            "1 waiting on you",
            "Details",
            "quit",
        ] {
            assert!(
                screen.contains(expected),
                "missing {expected:?} in:\n{screen}"
            );
        }
    }

    #[test]
    fn details_follow_the_selection() {
        let mut app = app();
        app.select(RowRef::Item(ItemId::new("appsignal:AppSignal#9")));
        let screen = render(&mut app);
        assert!(
            screen.contains("BACKTRACE") || screen.contains("undefined method"),
            "{screen}"
        );

        app.select(RowRef::Run(RunId::new(1)));
        app.set_capture(
            BlockId::new("block:1"),
            "Allow this edit? (y/n)\n".to_owned(),
        );
        let screen = render(&mut app);
        assert!(screen.contains("waiting on you"));
        assert!(screen.contains("Allow this edit? (y/n)"));
        assert!(screen.contains("Press y or n to answer"));
    }

    #[test]
    fn drawing_records_clickable_rows_and_keys() {
        let mut app = app();
        render(&mut app);
        assert!(
            app.hits
                .iter()
                .any(|h| matches!(&h.target, HitTarget::Row(RowRef::Run(_))))
        );
        assert!(
            app.hits
                .iter()
                .any(|h| matches!(&h.target, HitTarget::Key(KeyCode::Char('s'))))
        );
        assert!(
            app.hits
                .iter()
                .any(|h| matches!(&h.target, HitTarget::Key(KeyCode::Char('y')))),
            "y/n shows while waiting"
        );
    }

    #[test]
    fn ignored_items_show_in_the_top_bar_and_only_when_revealed() {
        let mut app = app();
        app.select(RowRef::Item(ItemId::new("appsignal:AppSignal#9")));
        app.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::Char('i'),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
            now(),
        );
        let screen = render(&mut app);
        assert!(screen.contains("1 ignored"), "{screen}");
        assert!(
            screen.contains("INCIDENTS (0)"),
            "hidden from the list:\n{screen}"
        );

        app.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::Char('I'),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
            now(),
        );
        let screen = render(&mut app);
        assert!(screen.contains("1 ignored shown"), "{screen}");
        assert!(screen.contains("ignored   AppSignal#9"), "{screen}");
    }

    #[test]
    fn side_panel_shows_pr_author_and_labels_and_incident_state_and_assignee() {
        let mut app = app();
        let screen = render(&mut app);
        assert!(screen.contains("author"), "{screen}");
        assert!(screen.contains("labels"), "{screen}");

        app.select(RowRef::Item(ItemId::new("appsignal:AppSignal#9")));
        let screen = render(&mut app);
        assert!(screen.contains("state"), "{screen}");
        assert!(screen.contains("assigned"), "{screen}");
    }

    fn pr(id: &str, attrs: serde_json::Value, title: &str) -> Item {
        Item {
            id: ItemId::new(id),
            source: SourceId::new("github"),
            kind: ItemKind::new(if attrs["mine"] == true {
                "pr.mine"
            } else {
                "pr.review"
            }),
            title: title.to_owned(),
            url: String::new(),
            fingerprint: Fingerprint::new(id),
            updated_at: now() - Duration::seconds(60),
            attrs,
        }
    }

    #[test]
    fn dots_follow_the_documented_pattern() {
        let mut app = App::new(Vec::new(), Vec::new(), Vec::new());
        let repo = json!({"repo": "appsignal/server", "author": "alex"});
        let with = |extra: serde_json::Value| {
            let mut attrs = repo.clone();
            attrs
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            attrs
        };
        app.set_items(
            SourceId::new("github"),
            vec![
                pr("a", with(json!({"number": 1})), "plain review"),
                pr(
                    "b",
                    with(json!({"number": 2, "rereview": true})),
                    "needs rereview",
                ),
                pr(
                    "c",
                    with(json!({"number": 3, "author": "dependabot[bot]"})),
                    "Bump stripe",
                ),
                pr(
                    "d",
                    with(json!({"number": 4, "mine": true, "review_decision": "APPROVED"})),
                    "mine approved",
                ),
                pr(
                    "e",
                    with(
                        json!({"number": 5, "mine": true, "review_decision": "CHANGES_REQUESTED"}),
                    ),
                    "mine changes",
                ),
                pr(
                    "f",
                    with(json!({"number": 6, "mine": true, "review_decision": "REVIEW_REQUIRED"})),
                    "mine waiting",
                ),
            ],
            now(),
        );
        let screen = render(&mut app);
        let cases = [
            ("plain review", "● review"),
            ("needs rereview", "◆ re-review"),
            ("Bump stripe", "● deps"),
            ("mine approved", "● approved"),
            ("mine changes", "● changes"),
            ("mine waiting", "○ waiting"),
        ];
        for (title, expected) in cases {
            let row = screen
                .lines()
                .find(|l| l.contains(title) && l.contains("server#"))
                .unwrap_or_default();
            assert!(
                row.contains(expected),
                "{title}: want {expected:?} in {row:?}\n{screen}"
            );
        }
    }

    #[test]
    fn an_agent_takes_over_the_dot_from_the_item() {
        let mut app = app();
        // The PR has a waiting agent: `?` replaces its blue review dot.
        let screen = render(&mut app);
        let row = screen
            .lines()
            .find(|l| l.contains("Fix sampler race") && l.contains("review"))
            .unwrap();
        assert!(row.contains("? review"), "{row}");
    }

    #[test]
    fn help_documents_the_dots() {
        let mut app = app();
        app.modal = Some(Modal::Help { dots: true });
        let screen = render(&mut app);
        for expected in [
            "Dots",
            "re-review: you reviewed it",
            "your agent is waiting",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}\n{screen}");
        }
    }

    #[test]
    fn modals_render() {
        let mut app = app();
        app.modal = Some(Modal::Help { dots: false });
        assert!(render(&mut app).contains("start a profile"));

        app.modal = Some(Modal::Filter { index: 0 });
        let screen = render(&mut app);
        assert!(
            screen.contains("Namespaces") && screen.contains("[x] web"),
            "{screen}"
        );

        app.modal = Some(Modal::Prompt {
            item: ItemId::new("appsignal:AppSignal#9"),
            base: ProfileName::new("code-review"),
            text: "hello".to_owned(),
        });
        assert!(render(&mut app).contains("› hello"));
    }

    #[test]
    fn tiny_terminals_get_a_message_instead_of_a_panic() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(30, 8)).unwrap();
        terminal.draw(|f| draw(f, &mut app, now())).unwrap();
    }

    #[test]
    fn narrow_lists_keep_a_gap_between_tag_and_reference() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app, now())).unwrap();
        let buffer = terminal.backend().buffer();
        let screen: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!screen.contains("reviewappsignal"), "{screen}");
        assert!(screen.contains("review    appsignal-"), "{screen}");
    }

    #[test]
    fn newtypes_honor_width_when_formatted() {
        assert_eq!(format!("{:<6}|", ProfileName::new("ab")), "ab    |");
    }

    #[test]
    fn long_references_keep_their_number() {
        assert_eq!(
            fit_reference("appsignal-processor-rs#2427", 24),
            "appsignal-processo…#2427"
        );
        assert_eq!(fit_reference("server#1", 12), "server#1    ");
        assert_eq!(fit_reference("AppSignal#14306", 10), "App…#14306");
    }

    #[test]
    fn the_header_row_has_no_background() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|f| draw(f, &mut app, now())).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            (0..buffer.area.width).all(|x| buffer[(x, 0)].bg == Color::Reset),
            "a coloured header bleeds into Rex's padding and looks two rows tall"
        );
    }

    #[test]
    fn fit_pads_and_truncates() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("x", 0), "");
    }
}
