//! All rendering. The pane is a `tui-term` widget over the session's vt100
//! screen; everything else is chrome drawn around it.

use std::time::{Duration, SystemTime};

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use tui_term::widget::{Cursor, PseudoTerminal};

use crate::{
    app::{App, Mode},
    config, theme, usage,
};

pub const SIDEBAR_WIDTH: u16 = 36;

/// Columns kept clear either side of a limit bar, so it never runs into the
/// sidebar's border.
const USAGE_GUTTER: usize = 1;
/// Eighth-blocks, narrowest first. A bar this wide still moves a whole cell
/// only every few percent; the partial cell is what keeps a slow window
/// visibly moving between them.
const EIGHTHS: [char; 7] = [
    '\u{258f}', '\u{258e}', '\u{258d}', '\u{258c}', '\u{258b}', '\u{258a}', '\u{2589}',
];
/// Past this, the cached limits are old enough that the footer says so instead
/// of quietly showing them as current.
const USAGE_STALE: Duration = Duration::from_secs(20 * 60);

pub fn draw(f: &mut Frame, app: &mut App) {
    let [body, status] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let [sidebar, pane] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(20)]).areas(body);

    draw_sidebar(f, app, sidebar);
    draw_pane(f, app, pane);
    draw_status(f, app, status);

    match app.mode {
        Mode::NewSession => draw_new_session(f, app),
        Mode::Help => draw_help(f),
        Mode::ConfirmKill => draw_confirm_kill(f, app),
        Mode::ConfirmRestart => draw_confirm_restart(f, app),
        Mode::ConfirmMkdir => {
            // The form stays visible underneath: the question is about the path
            // still standing in it.
            draw_new_session(f, app);
            draw_confirm_mkdir(f, app);
        }
        Mode::Resume => draw_resume(f, app),
        _ => {}
    }
}

/// The pane rectangle for a given terminal size, so the event loop can resize
/// PTYs without waiting for a render.
pub fn pane_area(full: Rect) -> Rect {
    let [body, _] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(full);
    let [_, pane] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(20)]).areas(body);
    pane
}

/// Inner size of the terminal pane, which is what a PTY must be resized to.
pub fn pane_inner(area: Rect) -> (u16, u16) {
    let inner = pane_inner_rect(area);
    (inner.height.max(1), inner.width.max(1))
}

/// The pane's inner rectangle, borders excluded. A mouse event carries screen
/// coordinates, and a child that tracks the mouse wants them pane-relative.
pub fn pane_inner_rect(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.mode, Mode::Focus);
    let mut items: Vec<ListItem> = Vec::new();

    for (i, s) in app.sessions.iter().enumerate() {
        let entry = app.entry_for(i);
        let (glyph, glyph_color, state_text) = if !s.is_alive() {
            // Finished cards expire on their own; show how long they have left.
            let ttl = config::finished_ttl();
            let left = s
                .finished_for()
                .map(|d| ttl.saturating_sub(d))
                .unwrap_or(ttl);
            ("x", theme::dead(), fmt_countdown(left))
        } else {
            let labels = config::labels();
            match entry.map(|e| e.status.as_str()) {
                Some("busy") => ("*", theme::busy(), labels.busy),
                Some("idle") => ("o", theme::idle(), labels.idle),
                // Stopped on a dialog or a permission prompt: nothing is
                // running and nothing will, until someone answers it.
                Some("waiting") => ("?", theme::ask(), labels.waiting),
                // Spawned, but has not written its registry entry yet.
                _ => ("-", theme::muted(), labels.starting),
            }
        };

        let name = entry
            .map(|e| e.name.clone())
            .unwrap_or_else(|| s.label.clone());
        let name = truncate(&name, 16);
        let hotkey = if i < 9 {
            format!("F{}", i + 1)
        } else {
            String::new()
        };
        let used = 4 + name.chars().count() + hotkey.chars().count();
        let pad = (SIDEBAR_WIDTH as usize).saturating_sub(used + 2);

        let name_style = if i == app.selected {
            Style::default().fg(theme::text()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };

        items.push(ListItem::new(vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled(glyph, Style::default().fg(glyph_color)),
                Span::raw(" "),
                Span::styled(name, name_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(hotkey, Style::default().fg(theme::faint())),
            ]),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    truncate(&s.cwd_label(), 12),
                    Style::default().fg(theme::muted()),
                ),
                Span::raw(" "),
                Span::styled(state_text, Style::default().fg(glyph_color)),
                Span::raw(" "),
                Span::styled(
                    fmt_uptime(s.started.elapsed()),
                    Style::default().fg(theme::faint()),
                ),
            ]),
        ]));
    }

    // The next free function key is a button as much as the cards above it are,
    // so it gets a row of its own rather than living only in the help.
    if !app.sessions.is_empty() && app.sessions.len() < 9 {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(" + ", Style::default().fg(theme::accent())),
            Span::styled("new session", Style::default().fg(theme::muted())),
            Span::raw(" ".repeat(
                (SIDEBAR_WIDTH as usize).saturating_sub(3 + "new session".len() + 4),
            )),
            Span::styled(
                format!("F{}", app.sessions.len() + 1),
                Style::default().fg(theme::faint()),
            ),
        ])));
    }

    if app.update_ready {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(" ^ ", Style::default().fg(theme::ask())),
            Span::styled("new build", Style::default().fg(theme::ask()).bold()),
            Span::raw(" ".repeat(
                (SIDEBAR_WIDTH as usize).saturating_sub(3 + "new build".len() + 3),
            )),
            Span::styled("r", Style::default().fg(theme::ask())),
        ])));
    }

    let foreign = app.foreign();
    if !foreign.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            " --- UNREACHABLE",
            Style::default().fg(theme::faint()),
        ))));
        for e in foreign {
            let color = match e.status.as_str() {
                "busy" => theme::busy(),
                "waiting" => theme::ask(),
                _ => theme::idle(),
            };
            let state = if e.is_waiting() && !e.waiting_for.is_empty() {
                format!("{}: {}", config::labels().waiting, e.waiting_for)
            } else if e.is_waiting() {
                config::labels().waiting
            } else {
                e.status.clone()
            };
            items.push(ListItem::new(vec![
                Line::from(vec![
                    Span::styled(" ! ", Style::default().fg(theme::faint())),
                    Span::styled(truncate(&e.name, 20), Style::default().fg(theme::muted())),
                ]),
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        truncate(&e.cwd_label(), 12),
                        Style::default().fg(theme::faint()),
                    ),
                    Span::raw(" "),
                    Span::styled(truncate(&state, 12), Style::default().fg(color).dim()),
                ]),
            ]));
        }
    }

    if items.is_empty() {
        items.push(ListItem::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  no sessions",
                Style::default().fg(theme::muted()),
            )),
            Line::from(Span::styled(
                "  press n",
                Style::default().fg(theme::faint()),
            )),
        ]));
    }

    // The accent border marks which half of the screen has the keyboard.
    let border_color = if focused { theme::faint() } else { theme::accent() };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(vec![
            Span::styled(" CLAUDE ", Style::default().fg(theme::accent()).bold()),
            Span::styled("FLEET ", Style::default().fg(theme::muted())),
        ]))
        .title_bottom(Line::from(Span::styled(
            format!(" {} of our own ", app.sessions.len()),
            Style::default().fg(theme::faint()),
        )));

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme::surface())
            .fg(theme::text())
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    if !app.sessions.is_empty() {
        state.select(Some(app.selected));
    }

    // The limits are a footer, not a card, so they take the bottom rows and the
    // list takes what is left. That means drawing the frame first and putting
    // two widgets inside it, rather than letting the list own the block.
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut limits = usage_lines(app, inner.width as usize);
    // A sidebar too short for both keeps the sessions: the limits are the half
    // one can also read with `/usage`.
    let mut reserved = (limits.len() as u16).min(inner.height.saturating_sub(4));
    if reserved < limits.len() as u16 {
        // Each window is a heading and the bar under it. Half a window reads as
        // a bug, so what does not fit whole is dropped whole.
        reserved = if reserved >= 3 { 1 + (reserved - 1) / 2 * 2 } else { 0 };
        limits.truncate(reserved as usize);
    }
    let [list_area, limits_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(reserved)]).areas(inner);

    f.render_stateful_widget(list, list_area, &mut state);
    if reserved > 0 {
        f.render_widget(Paragraph::new(limits), limits_area);
    }
}

/// The account's rate-limit windows as the rows at the foot of the sidebar:
/// how much of each window is spent, and how long until it resets.
///
/// Nothing is drawn when Claude Code has never cached them. An empty footer
/// beats dashes standing in for numbers nobody has.
fn usage_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let Some(u) = app.usage.current.as_ref() else {
        return Vec::new();
    };
    if u.session.is_none() && u.weekly.is_none() {
        return Vec::new();
    }

    // Claude Code refreshes that cache; fleet only reads it — but it does ask
    // for a refresh when the numbers go stale, and that ask is worth showing:
    // the age stops moving for a moment and then jumps.
    let stale = if app.usage_refreshing() {
        Some("refreshing ".to_string())
    } else {
        (u.fetched_ago > USAGE_STALE).then(|| format!("{} ago ", fmt_uptime(u.fetched_ago)))
    };
    let title = " LIMITS";
    let used = title.chars().count() + stale.as_ref().map_or(0, |s| s.chars().count());
    let mut head = vec![
        Span::styled(title, Style::default().fg(theme::muted())),
        Span::raw(" ".repeat(width.saturating_sub(used))),
    ];
    if let Some(stale) = stale {
        head.push(Span::styled(stale, Style::default().fg(theme::faint())));
    }

    let mut lines = vec![Line::from(head)];
    for (label, window) in [("session", &u.session), ("weekly", &u.weekly)] {
        if let Some(w) = window {
            lines.extend(usage_rows(label, w, width));
        }
    }
    lines
}

/// One window: what it is and how much of it is left, then a bar across the
/// whole sidebar under it.
///
/// Two lines rather than one, because a bar squeezed in beside the numbers had
/// five cells to say everything in — a third spent and half spent drew the
/// same.
fn usage_rows(label: &str, w: &usage::Window, width: usize) -> Vec<Line<'static>> {
    // A window whose reset time has passed describes a window that is over, so
    // its percentage belongs to that one too. The rows go faint rather than
    // pretending the number is about the window running now.
    //
    // The bar itself is the accent and always the accent: it says how much is
    // spent by its length, so a colour that changed with the number would be
    // saying the same thing twice. The percentage keeps the green-to-red
    // warning, which is the part a glance reads.
    let (label_color, value_color, bar_color, empty_color) = if w.expired {
        (
            theme::faint(),
            theme::faint(),
            theme::faint(),
            theme::surface(),
        )
    } else {
        (
            theme::muted(),
            usage_color(w.pct),
            theme::bar(),
            theme::bar_empty(),
        )
    };

    let head = format!(" {label}");
    let pct = format!("{:>3}%", w.pct);
    let reset = format!("{:>6} ", fmt_reset(w));
    let used = head.chars().count() + pct.chars().count() + reset.chars().count() + 1;

    let (filled, empty) = usage_bar(w.pct, width.saturating_sub(USAGE_GUTTER * 2));

    vec![
        Line::from(vec![
            Span::styled(head, Style::default().fg(label_color)),
            Span::raw(" ".repeat(width.saturating_sub(used))),
            Span::styled(pct, Style::default().fg(value_color).bold()),
            Span::raw(" "),
            Span::styled(reset, Style::default().fg(theme::faint())),
        ]),
        Line::from(vec![
            Span::raw(" ".repeat(USAGE_GUTTER)),
            Span::styled(filled, Style::default().fg(bar_color)),
            Span::styled(empty, Style::default().fg(empty_color)),
            Span::raw(" ".repeat(USAGE_GUTTER)),
        ]),
    ]
}

/// The filled and unfilled halves of a bar `width` cells wide.
///
/// The fill is measured in eighths so the last cell can be a partial block: a
/// whole cell is three percent or so at this width, and a bar that only moved
/// in whole cells would sit still for a quarter of an hour at a time.
fn usage_bar(pct: u8, width: usize) -> (String, String) {
    let eighths = usize::from(pct.min(100)) * width * 8 / 100;
    let whole = (eighths / 8).min(width);
    let rest = eighths % 8;

    let mut filled = "\u{2588}".repeat(whole);
    if whole < width && rest > 0 {
        filled.push(EIGHTHS[rest - 1]);
    }
    let empty = "\u{2591}".repeat(width - filled.chars().count());
    (filled, empty)
}

fn usage_color(pct: u8) -> Color {
    match pct {
        0..=49 => theme::idle(),
        50..=84 => theme::busy(),
        _ => theme::dead(),
    }
}

/// How long until a window resets, in the widest unit that still says
/// something: `2h14m`, `47m`, `6d3h`.
fn fmt_reset(w: &usage::Window) -> String {
    if w.expired {
        return "stale".to_string();
    }
    let Some(d) = w.resets_in else {
        return "-".to_string();
    };
    let mins = d.as_secs() / 60;
    match mins {
        0..=59 => format!("{mins}m"),
        60..=1439 => format!("{}h{:02}m", mins / 60, mins % 60),
        _ => format!("{}d{}h", mins / 1440, (mins % 1440) / 60),
    }
}

fn draw_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.mode, Mode::Focus);

    let Some(session) = app.selected_session() else {
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No sessions running.",
                Style::default().fg(theme::muted()),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("n", Style::default().fg(theme::accent()).bold()),
                Span::styled(
                    "  new session in a directory you pick",
                    Style::default().fg(theme::muted()),
                ),
            ]),
            Line::from(vec![
                Span::styled("?", Style::default().fg(theme::accent()).bold()),
                Span::styled("  keyboard shortcuts", Style::default().fg(theme::muted())),
            ]),
            Line::from(vec![
                Span::styled("q", Style::default().fg(theme::accent()).bold()),
                Span::styled("  quit", Style::default().fg(theme::muted())),
            ]),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::faint())),
        );
        f.render_widget(hint, area);
        return;
    };

    let mode_tag = if !session.is_alive() {
        Span::styled(" FINISHED ", Style::default().fg(theme::dead()).bold())
    } else if focused {
        // Naming the way out in the title means it is on screen even when the
        // status bar is showing a transient message.
        Span::styled(
            " FOCUS — F10 leaves ",
            Style::default()
                .bg(theme::accent())
                .fg(theme::surface())
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" VIEW ONLY ", Style::default().fg(theme::muted()))
    };

    let mut title = vec![
        Span::styled(
            format!(" {} ", session.label),
            Style::default().fg(theme::text()).bold(),
        ),
        mode_tag,
    ];
    // A session stopped on a question is the one thing worth saying on the
    // frame: the pane shows the dialog, but the title says what it wants.
    if let Some(entry) = app.entry_for(app.selected)
        && entry.is_waiting()
    {
        let what = if entry.waiting_for.is_empty() {
            " QUESTION ".to_string()
        } else {
            format!(" QUESTION — {} ", entry.waiting_for)
        };
        title.push(Span::styled(
            what,
            Style::default()
                .bg(theme::ask())
                .fg(theme::surface())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if session.prompt_pending() {
        title.push(Span::styled(
            " understand project… ",
            Style::default().fg(theme::ask()),
        ));
    }
    if session.scrollback > 0 {
        title.push(Span::styled(
            format!(" ^{} ", session.scrollback),
            Style::default().fg(theme::busy()),
        ));
    }
    let queued = session.queued_input();
    if queued > 0 {
        title.push(Span::styled(
            format!(" pasting {} ", fmt_bytes(queued)),
            Style::default().fg(theme::busy()).bold(),
        ));
    }

    let border_color = if focused { theme::accent() } else { theme::faint() };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(title))
        .title_bottom(Line::from(Span::styled(
            format!(" {} ", shorten_path(&session.cwd, 44)),
            Style::default().fg(theme::faint()),
        )))
        .title_bottom(
            Line::from(Span::styled(
                format!(" {}x{} ", session.cols, session.rows),
                Style::default().fg(theme::faint()),
            ))
            .right_aligned(),
        );

    let Ok(parser) = session.parser.read() else {
        return;
    };
    let screen = parser.screen();

    // Only the pane taking keystrokes should show a cursor.
    let cursor = Cursor::default()
        .visibility(focused && session.is_alive() && !screen.hide_cursor())
        .style(Style::default().fg(theme::accent()));

    let term = PseudoTerminal::new(screen).block(block).cursor(cursor);
    f.render_widget(term, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, _)) = &app.status {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" > ", Style::default().fg(theme::accent())),
            Span::styled(msg.clone(), Style::default().fg(theme::text())),
        ]));
        f.render_widget(p, area);
        return;
    }

    let hints: Vec<(&str, &str)> = match app.mode {
        Mode::Nav if app.update_ready => vec![
            ("r", "RESTART INTO THE NEW BUILD"),
            ("up/dn", "select"),
            ("enter", "focus"),
            ("n", "new"),
            ("u", "understand project"),
            ("q", "quit"),
        ],
        Mode::Nav => vec![
            ("up/dn", "select"),
            ("enter", "focus"),
            ("n", "new"),
            ("R", "resume a conversation"),
            ("u", "understand project"),
            ("x", "kill"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Mode::Focus => vec![
            ("F10", "LEAVE FOCUS"),
            ("F1-F9", "session"),
            ("F11", "new"),
            ("F12", "help"),
        ],
        Mode::NewSession => vec![("enter", "start"), ("up/dn", "recent"), ("esc", "cancel")],
        Mode::Help => vec![("any key", "close")],
        Mode::ConfirmKill => vec![("y", "yes"), ("n/esc", "no")],
        Mode::ConfirmMkdir => vec![("y/enter", "create it"), ("n/esc", "back to the path")],
        Mode::ConfirmRestart => vec![("y", "restart"), ("n/esc", "leave it")],
        Mode::Resume => vec![
            ("enter", "resume"),
            ("up/dn", "pick a conversation"),
            ("esc", "close"),
        ],
        Mode::Understand => vec![
            ("F1-F9", "that session"),
            ("u", "new session"),
            ("n", "new, pick a directory"),
            ("enter", "the selected one"),
            ("esc", "cancel"),
        ],
    };

    let mut spans = vec![Span::raw(" ")];
    for (i, (k, v)) in hints.iter().enumerate() {
        // In Focus the escape hatch is the one thing that must not be missed,
        // so it gets the inverted chip rather than the usual accent text.
        let key_style = if app.mode == Mode::Focus && i == 0 {
            Style::default()
                .bg(theme::accent())
                .fg(theme::surface())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(
            if app.mode == Mode::Focus && i == 0 {
                format!(" {k} ")
            } else {
                (*k).to_string()
            },
            key_style,
        ));
        if !v.is_empty() {
            spans.push(Span::styled(
                format!(" {v}"),
                Style::default().fg(theme::muted()),
            ));
        }
        spans.push(Span::raw("   "));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_new_session(f: &mut Frame, app: &App) {
    let Some(form) = &app.form else { return };

    let height = (form.recent.len() as u16).min(12) + 6;
    let area = centered(70, height, f.area());
    f.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(Span::styled(
            " working directory:",
            Style::default().fg(theme::muted()),
        )),
        Line::from(vec![
            Span::styled(" > ", Style::default().fg(theme::accent())),
            Span::styled(
                form.input.clone(),
                if form.cursor == 0 {
                    Style::default()
                        .fg(theme::text())
                        .add_modifier(Modifier::UNDERLINED)
                } else {
                    Style::default().fg(theme::muted())
                },
            ),
            Span::styled(
                if form.cursor == 0 { "_" } else { "" },
                Style::default().fg(theme::accent()),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " recent projects:",
            Style::default().fg(theme::muted()),
        )),
    ];

    for (i, p) in form.recent.iter().take(12).enumerate() {
        let selected = form.cursor == i + 1;
        let label = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string());
        lines.push(Line::from(vec![
            Span::styled(
                if selected { " > " } else { "   " },
                Style::default().fg(theme::accent()),
            ),
            Span::styled(
                format!("{:<22}", truncate(&label, 22)),
                if selected {
                    Style::default().fg(theme::text()).bold()
                } else {
                    Style::default().fg(theme::text())
                },
            ),
            Span::styled(
                shorten_path(p, 36),
                Style::default().fg(theme::faint()),
            ),
        ]));
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::accent()))
        .title(Line::from(Span::styled(
            " new session ",
            Style::default().fg(theme::accent()).bold(),
        )));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The list of past conversations: where each was held, what it was asked
/// for, and how long ago it was last alive.
fn draw_resume(f: &mut Frame, app: &App) {
    let Some(picker) = &app.resume else { return };

    let area = centered(92, (picker.items.len() as u16).min(20) + 4, f.area());
    f.render_widget(Clear, area);

    // The summary takes whatever the fixed columns leave, so a narrow terminal
    // shortens the prompt rather than pushing the age off the edge.
    let inner = area.width.saturating_sub(2) as usize;
    let summary_width = inner.saturating_sub(3 + 18 + 1 + 4 + 1);

    let mut lines = vec![Line::from(Span::styled(
        " enter resumes, esc closes",
        Style::default().fg(theme::faint()),
    ))];

    let now = SystemTime::now();
    for (i, c) in picker.items.iter().enumerate().take(20) {
        let selected = i == picker.cursor;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { " > " } else { "   " },
                Style::default().fg(theme::accent()),
            ),
            Span::styled(
                format!("{:<18}", truncate(&c.cwd_label(), 17)),
                Style::default().fg(theme::muted()),
            ),
            Span::styled(
                format!("{:<summary_width$}", truncate(&c.summary, summary_width)),
                if selected {
                    Style::default().fg(theme::text()).bold()
                } else {
                    Style::default().fg(theme::text())
                },
            ),
            Span::styled(
                format!(" {:>4}", fmt_age(now, c.modified)),
                Style::default().fg(theme::faint()),
            ),
        ]));
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::accent()))
        .title(Line::from(Span::styled(
            " resume a conversation ",
            Style::default().fg(theme::accent()).bold(),
        )));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_confirm_kill(f: &mut Frame, app: &App) {
    let label = app
        .selected_session()
        .map(|s| s.label.clone())
        .unwrap_or_default();
    let area = centered(46, 5, f.area());
    f.render_widget(Clear, area);

    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  kill session ", Style::default().fg(theme::text())),
            Span::styled(label, Style::default().fg(theme::accent()).bold()),
            Span::styled("?  ", Style::default().fg(theme::text())),
        ]),
        Line::from(Span::styled(
            "  y = yes      n / esc = no",
            Style::default().fg(theme::muted()),
        )),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::dead())),
    );
    f.render_widget(p, area);
}

fn draw_confirm_restart(f: &mut Frame, app: &App) {
    let live = app.sessions.iter().filter(|s| s.is_alive()).count();
    let area = centered(62, 8, f.area());
    f.render_widget(Clear, area);

    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  restart the fleet into the new build",
            Style::default().fg(theme::text()),
        )),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{live}"),
                Style::default().fg(theme::dead()).bold(),
            ),
            Span::styled(
                " sessions will die — a child does not outlive the owner",
                Style::default().fg(theme::muted()),
            ),
        ]),
        Line::from(Span::styled(
            "  of its pseudoconsole. They come back in the same directories.",
            Style::default().fg(theme::muted()),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  y = restart      n / esc = leave it",
            Style::default().fg(theme::muted()),
        )),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::ask())),
    );
    f.render_widget(p, area);
}

fn draw_confirm_mkdir(f: &mut Frame, app: &App) {
    let Some(path) = &app.pending_mkdir else { return };
    let area = centered(64, 7, f.area());
    f.render_widget(Clear, area);

    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  directory does not exist:",
            Style::default().fg(theme::text()),
        )),
        Line::from(Span::styled(
            format!("  {}", shorten_path(path, 58)),
            Style::default().fg(theme::accent()).bold(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  create it?   y / enter = yes      n / esc = no",
            Style::default().fg(theme::muted()),
        )),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::accent())),
    );
    f.render_widget(p, area);
}

fn draw_help(f: &mut Frame) {
    let rows: &[(&str, &str)] = &[
        ("", "-- NAVIGATION --"),
        ("up/down, j/k", "select a session"),
        ("enter / tab", "enter the session (focus)"),
        ("F1 .. F9", "jump to a session (always works)"),
        ("", "the first free F starts a new session"),
        ("n", "new session"),
        ("u", "understand project (see below)"),
        ("x", "kill the selected session"),
        ("w", "close a finished session's card now"),
        ("", "(finished ones go by themselves after a minute)"),
        ("r", "restart into a new build (when a newer exe exists)"),
        ("", "(sessions come back with their conversations, --resume)"),
        ("R", "resume an old conversation (transcript list)"),
        ("U", "refresh the account limits now"),
        ("", "(a hidden /usage session, no tokens)"),
        ("q", "quit"),
        ("", ""),
        ("", "-- FOCUS --"),
        ("F10", "LEAVE FOCUS"),
        ("everything else", "goes to Claude, including Ctrl+anything"),
        ("mouse wheel", "scroll the history"),
        ("", ""),
        ("", "-- ALWAYS WORKS --"),
        ("F1 .. F9", "jump to a session"),
        ("", "F on the first free slot = new session"),
        ("", "(in the selected session's directory, no dialog)"),
        ("F11", "new session"),
        ("F12", "this help"),
        ("", ""),
        ("", "-- LIVE CONFIG --"),
        ("", "~/.claude/fleet.toml — colours, labels, timings"),
        ("", "a save shows up at once, sessions live on"),
        ("", ""),
        ("", "-- UNDERSTAND PROJECT --"),
        ("u", "arms it, then the target:"),
        ("", "u F1..F9 = that session (free slot = a new one)"),
        ("", "u u = new session, no directory dialog"),
        ("", "u n = new session with the directory dialog"),
        ("", "u enter = the selected session"),
        ("F1..F9, then u", "the same thing, the other way round"),
        ("", "(a lone u counts for 2 s after entering a session)"),
        ("", "the text lands in the prompt, enter sends it"),
        ("", ""),
        ("", "-- FOREIGN SESSIONS --"),
        ("!", "started outside fleet, view only"),
        ("", "their PTY belongs to another terminal"),
    ];

    let area = centered(64, rows.len() as u16 + 2, f.area());
    f.render_widget(Clear, area);

    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            if k.is_empty() {
                Line::from(Span::styled(
                    format!("  {v}"),
                    Style::default().fg(theme::accent_dim()).bold(),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("  {k:<20}"),
                        Style::default().fg(theme::accent()).bold(),
                    ),
                    Span::styled(*v, Style::default().fg(theme::text())),
                ])
            }
        })
        .collect();

    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::accent()))
                .title(Line::from(Span::styled(
                    " shortcuts ",
                    Style::default().fg(theme::accent()).bold(),
                ))),
        ),
        area,
    );
}

fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}~", s.chars().take(max.saturating_sub(1)).collect::<String>())
}

fn shorten_path(p: &std::path::Path, max: usize) -> String {
    let full = p.display().to_string();
    let len = full.chars().count();
    if len <= max {
        return full;
    }
    let tail: String = full.chars().skip(len - max.saturating_sub(1)).collect();
    format!("~{tail}")
}

/// Time left before a finished card is dropped, e.g. `gone in 42s`.
fn fmt_countdown(d: Duration) -> String {
    format!("gone in {}s", d.as_secs())
}

fn fmt_bytes(n: usize) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{} KB", n / 1024),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// How long ago something happened, in one unit: `12m`, `5h`, `3d`.
///
/// A transcript's age is read against the other rows rather than for itself,
/// so the widest unit that still separates them is the useful one.
fn fmt_age(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or_default().as_secs();
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn fmt_uptime(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(pct: u8, secs: u64, expired: bool) -> usage::Window {
        usage::Window {
            pct,
            resets_in: (!expired).then(|| Duration::from_secs(secs)),
            expired,
        }
    }

    #[test]
    fn a_limit_row_fits_the_sidebar() {
        // The sidebar is a fixed width and the rows have no room to wrap, so
        // the widest thing they can hold has to still fit between the borders.
        let inner = usize::from(SIDEBAR_WIDTH) - 2;
        for w in [
            window(0, 59 * 60, false),
            window(100, 6 * 86_400 + 3 * 3600, false),
            window(87, 0, true),
        ] {
            for label in ["session", "weekly"] {
                for row in usage_rows(label, &w, inner) {
                    assert!(
                        row.width() <= inner,
                        "{label} {w:?} takes {} of {inner}",
                        row.width()
                    );
                }
            }
        }
    }

    #[test]
    fn the_bar_spans_the_sidebar_whatever_it_is_filled_to() {
        // Filled and unfilled together are the bar, so the pair always covers
        // the same columns; otherwise the two windows would not line up.
        let inner = usize::from(SIDEBAR_WIDTH) - 2 - USAGE_GUTTER * 2;
        for pct in 0..=100u8 {
            let (filled, empty) = usage_bar(pct, inner);
            assert_eq!(
                filled.chars().count() + empty.chars().count(),
                inner,
                "{pct}% has the wrong width"
            );
        }
    }

    #[test]
    fn an_age_reads_in_one_unit_and_says_now_for_the_last_minute() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 86_400);
        let ago = |secs: u64| fmt_age(now, now - Duration::from_secs(secs));
        assert_eq!(ago(5), "now");
        assert_eq!(ago(12 * 60), "12m");
        assert_eq!(ago(5 * 3600), "5h");
        assert_eq!(ago(3 * 86_400 + 3600), "3d");
        // A stamp from the future is not a negative age.
        assert_eq!(fmt_age(now, now + Duration::from_secs(60)), "now");
    }

    #[test]
    fn a_reset_reads_in_the_widest_unit_that_still_says_something() {
        assert_eq!(fmt_reset(&window(0, 47 * 60, false)), "47m");
        assert_eq!(fmt_reset(&window(0, 2 * 3600 + 14 * 60, false)), "2h14m");
        assert_eq!(fmt_reset(&window(0, 6 * 86_400 + 3 * 3600, false)), "6d3h");
        assert_eq!(fmt_reset(&window(0, 0, true)), "stale");
    }

    #[test]
    fn the_bar_only_fills_completely_at_a_full_window() {
        // A window with anything left in it must not look spent.
        let width = 30;
        assert_ne!(
            usage_bar(99, width).0,
            usage_bar(100, width).0,
            "99% looks full"
        );
        assert_eq!(usage_bar(100, width).0, "\u{2588}".repeat(width));
        assert_eq!(usage_bar(0, width).0, "");
    }

    #[test]
    fn a_partial_cell_marks_what_a_whole_cell_could_not() {
        // Two percentages landing inside the same cell still differ on screen.
        let width = 30;
        assert_ne!(usage_bar(41, width).0, usage_bar(42, width).0);
    }
}
