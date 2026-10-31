//! Drawing. The look follows oud.so: one narrow column of lowercase
//! monospace text, links underlined, secondary text dimmed, and the player as
//! a single inverted bar. Only modifiers are used, never fixed colours, so it
//! sits right in both light and dark terminals.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{
    Account, App, ExploreTab, LINKS, LibraryTab, Load, NowPlaying, PickerKind, SCREENS, SIGN_IN_WAYS, Screen,
    SignIn, Signal, Target, YOU_ROWS,
};
use crate::api::Station;
use crate::visual::Energy;

const MARGIN_X: u16 = 3;
const MAX_WIDTH: u16 = 76;
const PLAY: &str = "▷";
const PLAYING: &str = "▶";
const SAVED: &str = "■";

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}
fn link() -> Style {
    Style::new().add_modifier(Modifier::UNDERLINED)
}
fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}
fn hushed() -> Style {
    dim().add_modifier(Modifier::ITALIC)
}

/// Lowercase the way the site displays names. A dotted capital I would
/// otherwise turn into "i" plus a combining dot.
fn lower(s: &str) -> String {
    s.replace('İ', "i").to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut `s` to `width` columns, ending in an ellipsis when shortened.
fn fit(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// A line with `left` and `right` pushed to opposite edges of `width`.
fn spread<'a>(mut left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: usize) -> Line<'a> {
    let used: usize = left.iter().chain(&right).map(|s| s.content.width()).sum();
    left.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    left.extend(right);
    Line::from(left)
}

/// A shortcut as it appears beside what it does: dim, in brackets.
fn key_hint<'a>(key: &str) -> Span<'a> {
    Span::styled(format!("[{key}]"), dim())
}

/// First row to show so `sel` stays in view, scrolling a page at a time.
fn scroll_start(sel: usize, total: usize, height: usize) -> usize {
    if height == 0 || total <= height {
        return 0;
    }
    (sel / height * height).min(total - height)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let full = f.area();
    let width = full.width.saturating_sub(MARGIN_X * 2).min(MAX_WIDTH);
    let column = Rect { x: full.x + MARGIN_X, y: full.y + 1, width, height: full.height.saturating_sub(2) };

    let [header, rule, _, body, _, track, bar, _, nav] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(column);

    // Clickable spots are collected afresh as each frame is drawn.
    app.links.clear();
    draw_header(f, app, header);
    f.render_widget(Paragraph::new("─".repeat(width as usize)).style(dim()), rule);

    // Details take the bottom of the body, just above the player.
    let body = if app.show_details && app.details_station().is_some() {
        let [rest, details] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(DETAILS_HEIGHT)]).areas(body);
        draw_details(f, app, details);
        rest
    } else {
        body
    };

    match app.screen {
        Screen::Explore => draw_explore(f, app, body),
        Screen::Stations => draw_stations(f, app, body),
        Screen::Library => draw_library(f, app, body),
        Screen::About => draw_about(f, app, body),
        Screen::You => draw_you(f, app, body),
    }

    draw_track(f, app, track);
    draw_bar(f, app, bar);
    draw_nav(f, app, nav);

    // Wide terminals get the visual in the free space to the right,
    // spanning from the header down to the player bar.
    let side_x = column.x + width + 4;
    let side_w = full.right().saturating_sub(side_x + MARGIN_X);
    if app.store.visual && side_w >= VISUAL_MIN_WIDTH {
        let side = Rect { x: side_x, y: header.y, width: side_w, height: bar.bottom() - header.y };
        draw_visual(f, app, side);
    }
}

fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    let mut left = vec![Span::styled("oud.", bold()), Span::raw(" ")];
    let crumb = |s: String| Span::raw(s);
    match app.screen {
        Screen::Explore => match &app.open_list {
            Some(open) => left.extend([
                Span::styled("featured", link()),
                Span::raw(" • "),
                crumb(lower(&open.list.name)),
            ]),
            None => left.push(Span::styled("your signal.", hushed())),
        },
        Screen::Stations => {
            left.push(crumb("stations".into()));
            if let Some(g) = &app.genre {
                left.extend([Span::raw(" • "), crumb(lower(g.label()))]);
            }
            if let Some(c) = &app.country {
                left.extend([Span::raw(" • "), crumb(lower(&c.name))]);
            }
        }
        Screen::Library => match &app.open_user_list {
            Some(open) => left.extend([
                Span::styled("library", link()),
                Span::raw(" • "),
                crumb(lower(&open.list.name)),
            ]),
            None => left.push(crumb("library".into())),
        },
        Screen::About => left.push(crumb("about".into())),
        Screen::You => left.push(crumb("you".into())),
    }

    // What esc (or ?) would do from here, when it does anything.
    let filtered = !app.query.is_empty() || app.genre.is_some() || app.country.is_some();
    let hints: &[(&str, &str)] = match app.screen {
        Screen::Stations if app.editing => &[("search ", "enter"), ("  cancel ", "esc")],
        Screen::Stations if app.picker.is_some() => &[("pick ", "enter"), ("  close ", "esc")],
        Screen::Stations if filtered => &[("clear ", "esc")],
        Screen::Explore if app.open_list.is_some() => &[("back ", "esc")],
        Screen::Library if app.open_user_list.is_some() => &[("back ", "esc")],
        Screen::About => &[("back ", "?")],
        Screen::You => &[("back ", "u")],
        _ => &[],
    };
    let mut right: Vec<Span> =
        hints.iter().flat_map(|(label, key)| [Span::styled(*label, dim()), key_hint(key)]).collect();

    // The account, always in the top right, as on the site. Elsewhere it
    // links to the account screen; there, the hint above says how to leave.
    if app.screen != Screen::You {
        if !right.is_empty() {
            right.push(Span::raw("   "));
        }
        right.extend([Span::styled("you", link()), Span::raw(" "), key_hint("u")]);
        let width = "you [u]".width() as u16;
        let x = area.right().saturating_sub(width);
        app.links.push((Rect { x, y: area.y, width, height: 1 }, Target::Screen(Screen::You)));
    }
    f.render_widget(Paragraph::new(spread(left, right, area.width as usize)), area);
}

// ---- rows --------------------------------------------------------------

fn station_row<'a>(app: &App, station: &Station, selected: bool, width: usize) -> Line<'a> {
    let live = app.now.as_ref().filter(|n| n.station().id == station.id).map(|n| &n.signal);
    let icon = match live {
        Some(Signal::Connecting | Signal::OnAir(_) | Signal::Reconnecting) => PLAYING,
        _ => PLAY,
    };

    let mut aside = String::new();
    let tags: Vec<String> = station.tags.iter().take(3).map(|t| lower(t)).collect();
    if !tags.is_empty() {
        aside = tags.join(", ");
    }
    let saved = app.bookmarked.contains(&station.id);
    let right_width = if saved { 2 } else { 0 };
    // Tags give way to the name when space is short.
    let name_room = width.saturating_sub(2 + right_width + 2);
    let name = fit(&lower(&station.name), name_room.max(8));
    let aside = fit(&aside, width.saturating_sub(2 + name.width() + right_width + 2));

    let name_style = if selected { Style::new().add_modifier(Modifier::REVERSED) } else { link() };
    let icon_style = if selected || live.is_some() { Style::new() } else { dim() };
    let mut right = vec![Span::styled(aside, dim())];
    if saved {
        right.push(Span::styled(format!(" {SAVED}"), dim()));
    }
    spread(vec![Span::styled(format!("{icon} "), icon_style), Span::styled(name, name_style)], right, width)
}

fn link_row<'a>(label: String, aside: String, selected: bool, width: usize) -> Line<'a> {
    let label_style = if selected { Style::new().add_modifier(Modifier::REVERSED) } else { link() };
    let icon_style = if selected { Style::new() } else { dim() };
    let label = fit(&label, width.saturating_sub(4 + aside.width()));
    let mut spans = vec![Span::styled(format!("{PLAY} "), icon_style), Span::styled(label, label_style)];
    if !aside.is_empty() {
        spans.push(Span::styled(format!(" · {aside}"), dim()));
    }
    Line::from(spans)
}

fn status_line<'a, T>(load: &Load<T>, empty: &'a str) -> Line<'a> {
    match load {
        Load::Loading => Line::styled("tuning in…", hushed()),
        Load::Failed(e) => Line::styled(format!("couldn't load. {}", lower(e)), hushed()),
        Load::Ready(_) => Line::styled(empty, hushed()),
    }
}

/// A note for a list that is still loading or failed; `None` once it is ready.
fn status_of<'a, T>(load: &Load<T>) -> Option<Line<'a>> {
    match load {
        Load::Ready(_) => None,
        other => Some(status_line(other, "")),
    }
}

/// Render `rows` into `area`, scrolled so `sel` is visible.
fn draw_rows(f: &mut Frame, rows: Vec<Line<'_>>, sel: usize, area: Rect) {
    let start = scroll_start(sel, rows.len(), area.height as usize);
    let visible: Vec<Line> = rows.into_iter().skip(start).take(area.height as usize).collect();
    f.render_widget(Paragraph::new(visible), area);
}

fn tabs<'a>(items: &[(&'a str, bool)], hint: &'a str) -> Line<'a> {
    let mut spans = Vec::new();
    for (i, (label, active)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(if *active {
            Span::styled(*label, bold())
        } else {
            Span::styled(*label, link().add_modifier(Modifier::DIM))
        });
    }
    spans.push(Span::raw("  "));
    spans.push(key_hint(hint));
    Line::from(spans)
}

// ---- screens -----------------------------------------------------------

fn draw_explore(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width as usize;
    if let Some(open) = &app.open_list {
        let [about, _, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let description = open.list.description.as_deref().map(lower).unwrap_or_default();
        f.render_widget(Paragraph::new(Line::styled(fit(&description, w), hushed())), about);
        match open.stations.ready() {
            Some(stations) if !stations.is_empty() => {
                let rows =
                    stations.iter().enumerate().map(|(i, s)| station_row(app, s, i == open.sel, w)).collect();
                draw_rows(f, rows, open.sel, list);
            }
            _ => f.render_widget(Paragraph::new(status_line(&open.stations, "an empty list.")), list),
        }
        return;
    }

    let [line, something, _, tab_row, subtitle, _, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);

    f.render_widget(Paragraph::new(Line::styled(app.daypart.line, hushed())), line);
    let mut play = link_row("play something".into(), app.something_tag.into(), app.explore_sel == 0, w);
    if app.explore_sel != 0 {
        play.spans[1].style = link().add_modifier(Modifier::BOLD);
    }
    f.render_widget(Paragraph::new(play), something);

    let featured = app.explore_tab == ExploreTab::Featured;
    let hint = "f";
    f.render_widget(
        Paragraph::new(tabs(&[("featured", featured), ("small signals", !featured)], hint)),
        tab_row,
    );
    let about =
        if featured { "the lists we stand behind." } else { "good stations almost nobody has found." };
    f.render_widget(Paragraph::new(Line::styled(about, hushed())), subtitle);

    let sel = app.explore_sel.wrapping_sub(1);
    if featured {
        match app.featured.ready() {
            Some(lists) if !lists.is_empty() => {
                let rows = lists
                    .iter()
                    .enumerate()
                    .map(|(i, l)| link_row(lower(&l.name), l.item_count.to_string(), i == sel, w))
                    .collect();
                draw_rows(f, rows, sel.min(lists.len()), list);
            }
            _ => f.render_widget(Paragraph::new(status_line(&app.featured, "nothing featured yet.")), list),
        }
    } else {
        match app.signals.ready() {
            Some(stations) if !stations.is_empty() => {
                let mut rows: Vec<Line> =
                    stations.iter().enumerate().map(|(i, s)| station_row(app, s, i == sel, w)).collect();
                rows.push(Line::raw(""));
                rows.push(Line::from(vec![
                    Span::styled(
                        if app.shuffling { "shuffling…" } else { "another eight" },
                        link().add_modifier(Modifier::DIM),
                    ),
                    Span::raw(" "),
                    key_hint("r"),
                ]));
                draw_rows(f, rows, sel.min(stations.len()), list);
            }
            _ => {
                f.render_widget(Paragraph::new(status_line(&app.signals, "nothing to show right now.")), list)
            }
        }
    }
}

fn draw_stations(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width as usize;
    let [fields, underline, _, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);

    // The filter row: free text on the left, genre and country on the right.
    let genre = app.genre.as_ref().map(|g| lower(g.label()));
    let country = app.country.as_ref().map(|c| lower(&c.name));
    // Each field carries its key: "any genre [g]".
    let genre_label = genre.clone().unwrap_or_else(|| "any genre".into());
    let country_label = country.clone().unwrap_or_else(|| "any country".into());
    const KEY_WIDTH: usize = 4; // " [g]"
    let genre_width = genre_label.width() + KEY_WIDTH;
    let country_width = country_label.width() + KEY_WIDTH;
    let query_width = w.saturating_sub(genre_width + country_width + 4 + 2);

    let query_text = if app.editing {
        Span::raw(format!("{}▏", fit(&app.query, query_width.saturating_sub(5))))
    } else if app.query.is_empty() {
        Span::styled("search stations...", dim())
    } else {
        Span::raw(fit(&app.query, query_width.saturating_sub(4)))
    };
    let field = |label: String, set: bool| if set { Span::raw(label) } else { Span::styled(label, dim()) };
    let mut left = vec![query_text];
    if !app.editing {
        left.extend([Span::raw(" "), key_hint("/")]);
    }
    let right = vec![
        field(genre_label.clone(), genre.is_some()),
        Span::raw(" "),
        key_hint("g"),
        Span::raw("    "),
        field(country_label.clone(), country.is_some()),
        Span::raw(" "),
        key_hint("c"),
    ];
    f.render_widget(Paragraph::new(spread(left, right, w)), fields);
    let rules = spread(
        vec![Span::styled("─".repeat(query_width), dim())],
        vec![
            Span::styled("─".repeat(genre_width), dim()),
            Span::raw("    "),
            Span::styled("─".repeat(country_width), dim()),
        ],
        w,
    );
    f.render_widget(Paragraph::new(rules), underline);

    if let Some(picker) = &app.picker {
        let [filter, _, rows_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(list);
        let placeholder = match picker.kind {
            PickerKind::Genre => "search genre…",
            PickerKind::Country => "search country…",
        };
        let filter_line = if picker.filter.is_empty() {
            Line::styled(placeholder, hushed())
        } else {
            Line::raw(format!("{}▏", picker.filter))
        };
        f.render_widget(Paragraph::new(filter_line), filter);
        let rows = app
            .picker_items(picker)
            .into_iter()
            .enumerate()
            .map(|(i, (label, aside, _))| {
                let style =
                    if i == picker.sel { Style::new().add_modifier(Modifier::REVERSED) } else { link() };
                spread(
                    vec![Span::styled(fit(&lower(&label), w.saturating_sub(10)), style)],
                    vec![Span::styled(aside, dim())],
                    w,
                )
            })
            .collect();
        let mut rows: Vec<Line> = rows;
        // Say why the list is short instead of showing "any …" alone.
        let source_state = match picker.kind {
            PickerKind::Genre => status_of(&app.genres),
            PickerKind::Country => status_of(&app.countries),
        };
        if let Some(state) = source_state {
            rows.push(state);
        } else if rows.is_empty() {
            rows.push(Line::styled("nothing matches.", hushed()));
        }
        draw_rows(f, rows, picker.sel, rows_area);
        return;
    }

    match app.results.ready() {
        Some(stations) if !stations.is_empty() => {
            let rows = stations
                .iter()
                .enumerate()
                .map(|(i, s)| station_row(app, s, i == app.results_sel, w))
                .collect();
            draw_rows(f, rows, app.results_sel, list);
        }
        _ => f.render_widget(Paragraph::new(status_line(&app.results, "nothing on that frequency.")), list),
    }
}

fn draw_library(f: &mut Frame, app: &mut App, area: Rect) {
    let w = area.width as usize;
    if !matches!(app.account, Account::SignedIn(_)) {
        let lines = vec![Line::styled("sign in to keep your bookmarks and lists.", hushed()), Line::raw("")];
        let row = area.y + lines.len() as u16;
        let mut lines = lines;
        lines.push(Line::from(vec![Span::styled("sign in", link()), Span::raw(" "), key_hint("u")]));
        app.links.push((Rect { x: area.x, y: row, width: 7, height: 1 }, Target::Screen(Screen::You)));
        f.render_widget(Paragraph::new(lines), area);
        return;
    }

    if let Some(open) = &app.open_user_list {
        let [about, _, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let kind =
            if open.list.is_smart { "a smart list, filled by its rules." } else { "one of your lists." };
        f.render_widget(Paragraph::new(Line::styled(kind, hushed())), about);
        match open.stations.ready() {
            Some(stations) if !stations.is_empty() => {
                let rows =
                    stations.iter().enumerate().map(|(i, s)| station_row(app, s, i == open.sel, w)).collect();
                draw_rows(f, rows, open.sel, list);
            }
            _ => f.render_widget(
                Paragraph::new(status_line(&open.stations, "nothing in this list yet.")),
                list,
            ),
        }
        return;
    }

    let [tab_row, _, list] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let on_bookmarks = app.library_tab == LibraryTab::Bookmarks;
    f.render_widget(
        Paragraph::new(tabs(&[("bookmarks", on_bookmarks), ("lists", !on_bookmarks)], "t")),
        tab_row,
    );

    if on_bookmarks {
        match app.bookmarks.ready() {
            Some(stations) if !stations.is_empty() => {
                let rows = stations
                    .iter()
                    .enumerate()
                    .map(|(i, s)| station_row(app, s, i == app.library_sel, w))
                    .collect();
                draw_rows(f, rows, app.library_sel, list);
            }
            _ => f.render_widget(
                Paragraph::new(status_line(
                    &app.bookmarks,
                    "no bookmarks yet. press s on a station to keep it.",
                )),
                list,
            ),
        }
    } else {
        match app.lists.ready() {
            Some(lists) if !lists.is_empty() => {
                let rows = lists
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        let aside = if l.is_smart {
                            format!("{} · smart", l.item_count)
                        } else {
                            l.item_count.to_string()
                        };
                        link_row(lower(&l.name), aside, i == app.lists_sel, w)
                    })
                    .collect();
                draw_rows(f, rows, app.lists_sel, list);
            }
            _ => f.render_widget(
                Paragraph::new(status_line(&app.lists, "no lists yet. make one on oud.so.")),
                list,
            ),
        }
    }
}

/// A selectable row with its key on the right edge; remembered for clicks.
fn action_row<'a>(
    app: &mut App,
    area: Rect,
    row: u16,
    label: &'a str,
    key: &'a str,
    selected: bool,
    target: Target,
) -> Line<'a> {
    let label_style = if selected { Style::new().add_modifier(Modifier::REVERSED) } else { link() };
    let icon_style = if selected { Style::new() } else { dim() };
    if row < area.bottom() {
        let width = (2 + label.width()) as u16;
        app.links.push((Rect { x: area.x, y: row, width, height: 1 }, target));
    }
    spread(
        vec![Span::styled(format!("{PLAY} "), icon_style), Span::styled(label, label_style)],
        vec![key_hint(key)],
        area.width as usize,
    )
}

/// The account: signing in, or who is signed in.
fn draw_you(f: &mut Frame, app: &mut App, area: Rect) {
    let w = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let next_row = |lines: &Vec<Line>| area.y + lines.len() as u16;

    match &app.account {
        Account::Checking => lines.push(Line::styled("checking your session…", hushed())),
        Account::Unreachable(e) => {
            lines.push(Line::styled(format!("couldn't reach oud. {}", lower(e)), hushed()));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("try again", link()),
                Span::raw(" "),
                key_hint("enter"),
            ]));
        }
        Account::SignedIn(user) => {
            let user = user.clone();
            let name = user.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
            lines.push(Line::styled(fit(&lower(name.unwrap_or(&user.email)), w), bold()));
            if name.is_some() {
                lines.push(Line::styled(fit(&user.email, w), dim()));
            }
            let counts = match (app.bookmarks.ready(), app.lists.ready()) {
                (Some(b), Some(l)) => format!(
                    "{} bookmark{} · {} list{}",
                    b.len(),
                    if b.len() == 1 { "" } else { "s" },
                    l.len(),
                    if l.len() == 1 { "" } else { "s" }
                ),
                _ => "…".into(),
            };
            lines.push(Line::styled(counts, dim()));
            lines.push(Line::raw(""));
            for (i, (label, key)) in YOU_ROWS.iter().enumerate() {
                let target = if *key == "o" { Target::SignOut } else { Target::Screen(Screen::Library) };
                let row = next_row(&lines);
                let line = action_row(app, area, row, label, key, i == app.you_sel, target);
                lines.push(line);
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled("the same account as oud.so and the mac app.", dim()));
        }
        Account::SignedOut => match &app.sign_in {
            SignIn::Choose => {
                lines.push(Line::styled("sign in to keep your favourite stations and lists.", hushed()));
                lines.push(Line::raw(""));
                for (i, (label, key)) in SIGN_IN_WAYS.iter().enumerate() {
                    let row = next_row(&lines);
                    let line = action_row(app, area, row, label, key, i == app.you_sel, Target::SignIn(i));
                    lines.push(line);
                }
                lines.push(Line::raw(""));
                lines.push(Line::styled("the same account as oud.so and the mac app.", dim()));
                lines.push(Line::styled("signing in for the first time makes one.", dim()));
            }
            SignIn::Browser { code, failed, .. } => {
                lines.push(Line::styled("connect this terminal.", hushed()));
                lines.push(Line::raw(""));
                match code {
                    None if failed.is_none() => lines.push(Line::styled("asking for a code…", dim())),
                    None => {}
                    Some(code) => {
                        let shown = spaced_code(&code.user_code);
                        lines.push(Line::from(vec![
                            Span::styled("your code  ", dim()),
                            Span::styled(shown, bold()),
                        ]));
                        lines.push(Line::raw(""));
                        let address = code.verification_uri.trim_start_matches("https://").to_string();
                        let row = next_row(&lines);
                        let url = code.verification_uri_complete.clone();
                        let width = (2 + address.width()) as u16;
                        app.links.push((Rect { x: area.x, y: row, width, height: 1 }, Target::Url(url)));
                        lines.push(spread(
                            vec![Span::styled(format!("{PLAY} "), dim()), Span::styled(address, link())],
                            vec![Span::styled("open again ", dim()), key_hint("enter")],
                            w,
                        ));
                        lines.push(Line::raw(""));
                        if failed.is_none() {
                            let dots = ".".repeat((app.started.elapsed().as_millis() / 500 % 4) as usize);
                            lines.push(Line::styled(
                                format!("approve it in the browser. waiting{dots}"),
                                dim(),
                            ));
                        }
                    }
                }
                if let Some(e) = failed {
                    lines.push(Line::styled(lower(e), hushed()));
                }
                lines.push(Line::raw(""));
                lines.push(Line::from(vec![Span::styled("back ", dim()), key_hint("esc")]));
            }
            SignIn::Email(step) => {
                lines.push(Line::styled("sign in with a code by email.", hushed()));
                lines.push(Line::raw(""));
                let cursor = |on: bool| if on { "▏" } else { "" };
                // An empty field shows the cursor first, then what goes there.
                let email = if step.email.is_empty() && !step.sent {
                    vec![Span::raw(cursor(true)), Span::styled("you@example.com", dim())]
                } else {
                    vec![Span::raw(step.email.clone()), Span::raw(cursor(!step.sent))]
                };
                lines.push(Line::from([vec![Span::styled("email  ", dim())], email].concat()));
                if step.sent {
                    let code = if step.code.is_empty() {
                        vec![Span::raw(cursor(true)), Span::styled("6 digits", dim())]
                    } else {
                        vec![Span::styled(step.code.clone(), bold()), Span::raw(cursor(true))]
                    };
                    lines.push(Line::from([vec![Span::styled("code   ", dim())], code].concat()));
                }
                lines.push(Line::raw(""));
                let note = match (&step.error, step.busy, step.sent) {
                    (Some(e), _, _) => Line::styled(lower(e), hushed()),
                    (None, true, false) => Line::styled("sending…", dim()),
                    (None, true, true) => Line::styled("signing in…", dim()),
                    (None, false, true) => {
                        Line::styled(format!("we sent a code to {}.", step.email.trim()), dim())
                    }
                    (None, false, false) => Line::raw(""),
                };
                lines.push(note);
                lines.push(Line::raw(""));
                let (go, back) =
                    if step.sent { ("sign in ", "other email ") } else { ("send a code ", "back ") };
                lines.push(Line::from(vec![
                    Span::styled(go, dim()),
                    key_hint("enter"),
                    Span::raw("   "),
                    Span::styled(back, dim()),
                    key_hint("esc"),
                ]));
            }
        },
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Device codes read better in two halves: "ABCD-EFGH".
fn spaced_code(code: &str) -> String {
    if code.len() == 8 && code.is_ascii() {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_string()
    }
}

fn draw_about(f: &mut Frame, app: &mut App, area: Rect) {
    const KEYS: &[(&str, &str)] = &[
        ("↑ ↓", "move"),
        ("enter", "play / open"),
        ("space", "stop / resume"),
        ("← →", "previous / next station"),
        ("- +", "volume"),
        ("s", "bookmark / remove"),
        ("/", "search stations"),
        ("g  c", "pick genre / country"),
        ("esc", "back / clear"),
        ("e  E", "next / previous visual"),
        ("v", "show / hide visual"),
        ("i", "station details"),
        ("w", "station homepage, from details"),
        ("tab  1-4", "explore · stations · library · about"),
        ("u", "you: sign in or out"),
        ("?", "about, and back"),
        ("q", "quit"),
    ];
    let w = area.width as usize;
    let mut lines = vec![Line::styled("your signal, in the terminal.", hushed()), Line::raw("")];

    // The links come first and remember where they were drawn, for clicks.
    for (i, item) in LINKS.iter().enumerate() {
        let row = area.y + lines.len() as u16;
        let selected = i == app.about_sel;
        let label_style = if selected { Style::new().add_modifier(Modifier::REVERSED) } else { link() };
        let icon_style = if selected { Style::new() } else { dim() };
        let width = (2 + item.label.width()) as u16;
        if row < area.bottom() {
            app.links.push((Rect { x: area.x, y: row, width, height: 1 }, Target::Url(item.url.to_string())));
        }
        lines.push(spread(
            vec![Span::styled(format!("{PLAY} "), icon_style), Span::styled(item.label, label_style)],
            vec![Span::styled(format!("[{}]", item.key), dim())],
            w,
        ));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(format!("maps {}.", crate::map::ATTRIBUTION), dim()));
    lines.push(Line::raw(""));
    lines.push(Line::styled("keys.", hushed()));
    lines.push(Line::raw(""));
    lines.extend(KEYS.iter().map(|(k, what)| {
        Line::from(vec![Span::styled(format!("{k:<12}"), bold()), Span::styled(*what, dim())])
    }));
    f.render_widget(Paragraph::new(lines), area);
}

// ---- player ------------------------------------------------------------

fn is_live(now: &NowPlaying) -> bool {
    matches!(now.signal, Signal::Connecting | Signal::OnAir(_) | Signal::Reconnecting)
}

fn draw_track(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width as usize;
    let (text, aside) = match (app.notice(), &app.now) {
        (Some(notice), _) => (notice.to_string(), String::new()),
        (None, Some(now)) => track_text(now, app),
        (None, None) if app.store.last.is_some() => ("space to tune back in.".into(), String::new()),
        (None, None) => ("pick a station and press enter.".into(), String::new()),
    };
    let text = fit(&lower(&text), w.saturating_sub(aside.width() + 3));
    f.render_widget(
        Paragraph::new(spread(
            vec![Span::styled(format!("  {text}"), dim())],
            vec![Span::styled(aside, dim())],
            w,
        )),
        area,
    );
}

fn track_text(now: &NowPlaying, app: &App) -> (String, String) {
    match &now.signal {
        Signal::Connecting => {
            let fill = app.player.deck().fill_level();
            let dots = "·".repeat((fill * 3.0) as usize);
            (format!("connecting…{dots}"), String::new())
        }
        Signal::Reconnecting => ("signal lost. reconnecting…".into(), String::new()),
        Signal::Stopped => ("stopped.".into(), String::new()),
        Signal::Lost(reason) => (format!("off air. {reason}"), String::new()),
        Signal::OnAir(format) => {
            let title = now.title.clone().unwrap_or_else(|| now.source.clone());
            let gap = if app.player.deck().is_filling() { "buffering… " } else { "" };
            (title, format!("{gap}{format}"))
        }
    }
}

/// The inverted bar: station on the left, the keys that drive it on the right.
fn draw_bar(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width as usize;
    let inverted = Style::new().add_modifier(Modifier::REVERSED);
    let key = inverted.add_modifier(Modifier::BOLD);
    let volume = (app.player.deck().volume() * 100.0).round() as u32;

    let (name, live) = match &app.now {
        Some(now) => (lower(&now.station().name), is_live(now)),
        None => match &app.store.last {
            Some(last) => (lower(&last.name), false),
            None => ("nothing playing".into(), false),
        },
    };
    let space_does = if live { "stop" } else { "play" };
    // Spell the keys out when there is room; fall back to the bare keys.
    let full: Vec<(&str, String)> = vec![
        ("←", " prev  ".into()),
        ("space", format!(" {space_does}  ")),
        ("→", " next  ".into()),
        ("i", " info   ".into()),
        ("-", format!(" {volume} ")),
        ("+", " ".into()),
    ];
    let compact: Vec<(&str, String)> = vec![
        ("←", " ".into()),
        ("space", " ".into()),
        ("→", " ".into()),
        ("i", "  ".into()),
        ("-", format!(" {volume} ")),
        ("+", " ".into()),
    ];
    let width_of = |parts: &[(&str, String)]| parts.iter().map(|(k, l)| k.width() + l.width()).sum::<usize>();
    let parts = if width_of(&full) + 24 <= w { full } else { compact };

    let right: Vec<Span> = parts
        .iter()
        .flat_map(|(k, label)| [Span::styled(k.to_string(), key), Span::styled(label.clone(), inverted)])
        .collect();
    let name = fit(&name, w.saturating_sub(width_of(&parts) + 4));
    let line = spread(vec![Span::styled(format!("  {name}"), key)], right, w);
    // Paint the whole row inverted, gaps included.
    f.render_widget(Paragraph::new(line).style(inverted), area);
}

// ---- details -------------------------------------------------------------

/// Rows the details panel takes, its top rule included.
const DETAILS_HEIGHT: u16 = 11;
/// Map columns, when the column is wide enough to fit it beside the text.
const MAP_COLS: u16 = 30;

/// What the site's station page says, plus the map and recent titles.
fn draw_details(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(station) = app.details_station().cloned() else { return };
    let [rule, _, content] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let w = area.width as usize;
    let rule_line = spread(
        vec![Span::styled("─".repeat(w.saturating_sub(10)), dim())],
        vec![Span::styled("close ", dim()), key_hint("i")],
        w,
    );
    f.render_widget(Paragraph::new(rule_line), rule);

    let show_map = content.width >= MAP_COLS + 30;
    let [text_area, _, map_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(if show_map { 2 } else { 0 }),
        Constraint::Length(if show_map { MAP_COLS } else { 0 }),
    ])
    .areas(content);
    let tw = text_area.width as usize;

    let mut lines = vec![Line::styled(fit(&lower(&station.name), tw), bold())];
    if let Some(country) = station.country.as_deref().filter(|c| !c.trim().is_empty()) {
        let mut spans = vec![Span::raw(lower(country))];
        if let Some(state) = station.state.as_deref().filter(|s| !s.trim().is_empty()) {
            spans.push(Span::styled(format!(" · {}", lower(state)), dim()));
        }
        lines.push(Line::from(spans));
    }
    if !station.tags.is_empty() {
        lines.push(Line::styled(fit(&lower(&station.tags.join(", ")), tw), dim()));
    }
    let format: Vec<String> = [
        station.codec.as_deref().map(|c| c.trim().to_lowercase()).filter(|c| !c.is_empty() && c != "unknown"),
        station.bitrate.filter(|b| *b > 0).map(|b| format!("{b}kbps")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !format.is_empty() {
        lines.push(Line::styled(format.join(" • "), dim()));
    }
    if let Some(home) = station.homepage.as_deref().filter(|h| !h.trim().is_empty()) {
        let row = text_area.y + lines.len() as u16;
        let shown = fit(home.trim(), tw.saturating_sub(6));
        if row < text_area.bottom() {
            let width = (2 + shown.width()) as u16;
            app.links.push((
                Rect { x: text_area.x, y: row, width, height: 1 },
                Target::Url(home.trim().to_string()),
            ));
        }
        lines.push(Line::from(vec![
            Span::styled("↗ ", dim()),
            Span::styled(shown, link()),
            Span::raw(" "),
            key_hint("w"),
        ]));
    }

    // What has been on, when this is the station playing.
    if let Some(now) = app.now.as_ref().filter(|n| n.station().id == station.id) {
        lines.push(Line::raw(""));
        if let Some(title) = &now.title {
            lines.push(Line::styled(fit(&format!("♪ {}", lower(title)), tw), Style::new()));
        }
        for earlier in &now.history {
            lines.push(Line::styled(fit(&format!("  {}", lower(earlier)), tw), dim()));
        }
    }
    f.render_widget(Paragraph::new(lines), text_area);

    if show_map {
        let [map_rows, credit] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(map_area);
        let pulse = (app.started.elapsed().as_millis() / 600).is_multiple_of(2);
        match app.map_drawing(map_rows.width as usize, map_rows.height as usize) {
            Some(drawing) => {
                let lines: Vec<Line> = drawing
                    .cells
                    .iter()
                    .enumerate()
                    .map(|(y, row)| {
                        Line::from(
                            row.iter()
                                .enumerate()
                                .map(|(x, (c, home))| {
                                    if (x, y) == drawing.marker {
                                        Span::styled(if pulse { "●" } else { "○" }, bold())
                                    } else if *home {
                                        Span::raw(c.to_string())
                                    } else {
                                        Span::styled(c.to_string(), dim())
                                    }
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                f.render_widget(Paragraph::new(lines), map_rows);
                f.render_widget(
                    Paragraph::new(Line::styled(crate::map::ATTRIBUTION, dim()).right_aligned()),
                    credit,
                );
            }
            None => f.render_widget(Paragraph::new(Line::styled("no map for this one.", hushed())), map_rows),
        }
    }
}

// ---- the visual ------------------------------------------------------------

/// Columns the side visual needs before it is worth showing.
const VISUAL_MIN_WIDTH: u16 = 24;
const VISUAL_BANDS: usize = 24;

fn draw_visual(f: &mut Frame, app: &mut App, area: Rect) {
    let live = matches!(app.now.as_ref().map(|n| &n.signal), Some(Signal::OnAir(_)));
    let energy = if live {
        let deck = app.player.deck().clone();
        deck.snapshot(&mut app.scope);
        Energy::from_bands(app.spectrum.update(&app.scope, deck.rate, VISUAL_BANDS))
    } else {
        Energy::default()
    };

    let [canvas, caption] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let now = app.started.elapsed().as_secs_f32();
    let cells = app.visual.frame(now, canvas.width as usize, canvas.height as usize, energy, live);
    let lines: Vec<Line> = cells
        .into_iter()
        .map(|row| {
            Line::from(
                row.into_iter()
                    .map(|(c, tone)| {
                        let style = match tone {
                            0 => dim(),
                            1 => Style::new(),
                            _ => bold(),
                        };
                        Span::styled(c.to_string(), style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    f.render_widget(Paragraph::new(lines), canvas);
    let label = Line::from(vec![
        Span::styled(format!("~ {}  ", app.visual.label()), hushed()),
        key_hint("e"),
        Span::raw(" "),
        key_hint("v"),
    ]);
    f.render_widget(Paragraph::new(label.right_aligned()), caption);
}

/// The screens, centred under the player, each with its key. Each one is
/// clickable too.
fn draw_nav(f: &mut Frame, app: &mut App, area: Rect) {
    const GAP: &str = "   ";
    let items: Vec<(Screen, &str, &str)> = SCREENS
        .iter()
        .map(|(screen, label)| {
            let key = match screen {
                Screen::Explore => " [1]",
                Screen::Stations => " [2]",
                Screen::Library => " [3]",
                Screen::About => " [?]",
                Screen::You => " [u]",
            };
            (*screen, *label, key)
        })
        .collect();
    let total: usize =
        items.iter().map(|(_, l, k)| l.width() + k.width()).sum::<usize>() + GAP.len() * (items.len() - 1);
    let start = area.x + (area.width.saturating_sub(total as u16)) / 2;
    let mut x = start;

    let mut spans = Vec::new();
    for (i, (screen, label, key)) in items.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(GAP));
            x += GAP.len() as u16;
        }
        let style = if screen == app.screen { bold().add_modifier(Modifier::UNDERLINED) } else { dim() };
        spans.push(Span::styled(label, style));
        if !key.is_empty() {
            spans.push(Span::styled(key, dim()));
        }
        let width = (label.width() + key.width()) as u16;
        app.links.push((Rect { x, y: area.y, width, height: 1 }, Target::Screen(screen)));
        x += width;
    }
    let line_area = Rect { x: start, width: (total as u16).min(area.width), ..area };
    f.render_widget(Paragraph::new(Line::from(spans)), line_area);
}
