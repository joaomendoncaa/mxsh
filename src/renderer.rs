use crate::clickable::{Action, Clickable, HOVER_BG, HOVER_FG};
use crate::model::{Entry, EntryType, FeedbackEntry, FeedbackType, LINE_VERT};
use crate::picker::{Mode, Picker};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

const CONNECTOR: Color = Color::DarkGray;
const CURSOR_BG: Color = Color::DarkGray;
const CURSOR_FG: Color = Color::White;
const DEFAULT_BG: Color = Color::Black;
const CMD_DIM: Color = Color::Rgb(100, 100, 100);
const SPINNER_DAEMON: &[char] = &['─', '╲', '│', '╱'];

fn layout_constraints(gap: u16, hide_footer: bool, feedbacks: usize) -> Vec<Constraint> {
    let mut c = vec![Constraint::Min(1)];
    if gap > 0 {
        c.push(Constraint::Length(gap));
    }
    c.push(Constraint::Length(1));
    if gap > 0 {
        c.push(Constraint::Length(gap));
    }
    if !hide_footer {
        c.push(Constraint::Length(1));
    }
    for _ in 0..feedbacks {
        c.push(Constraint::Length(1));
    }
    c
}

struct Slots {
    entries: Rect,
    input: Rect,
    after: usize,
    gap: u16,
    raw: Vec<Rect>,
}
fn prepare_slots(picker: &mut Picker, area: Rect) -> Slots {
    let gap = picker.config.style_entries_gap.min(u64::from(u16::MAX)) as u16;
    let raw = Layout::vertical(layout_constraints(
        gap,
        picker.config.hide_hints_footer,
        picker.feedbacks.len(),
    ))
    .split(area)
    .to_vec();
    let input_idx = if gap > 0 { 2 } else { 1 };
    let after = input_idx + 1 + usize::from(gap > 0);
    let entries = raw[0];
    let input = raw[input_idx];
    picker.slot_entries = entries;
    Slots {
        entries,
        input,
        after,
        gap,
        raw,
    }
}
fn render_bottom(frame: &mut Frame, picker: &mut Picker, slots: &Slots) {
    if slots.gap > 0 {
        frame.render_widget(Paragraph::new(""), slots.raw[1]);
        let input_idx = if slots.gap > 0 { 2 } else { 1 };
        frame.render_widget(Paragraph::new(""), slots.raw[input_idx + 1]);
    }
    let mut idx = slots.after;
    if !picker.config.hide_hints_footer {
        let hovered = picker
            .clickables
            .iter()
            .find(|c| c.contains(picker.last_mouse_col, picker.last_mouse_row))
            .map(|c| c.action);
        frame.render_widget(Paragraph::new(hints_line(picker, hovered)), slots.raw[idx]);
        idx += 1;
    }
    for fb in &picker.feedbacks {
        frame.render_widget(Paragraph::new(feedback(fb)), slots.raw[idx]);
        idx += 1;
    }
}

pub fn render(frame: &mut Frame, picker: &mut Picker) {
    if picker.is_help() {
        return render_help(frame, picker);
    }
    let slots = prepare_slots(picker, frame.area());
    let slot_entries = slots.entries;
    let slot_entries_height = slot_entries.height as usize;
    let slot_input = slots.input;
    let after_input_idx = slots.after;

    if picker.entries.is_empty() {
        loader(frame, frame.area(), picker.spinner);
        return;
    }

    let cursor = picker.cursor;
    let n = picker.filtered.len();
    let rows = picker.rows();
    let total = rows.len();
    let cursor_row = rows.iter().position(|&r| r == Some(cursor)).unwrap_or(0);

    let scroll = if picker.mouse_hover {
        picker.scroll
    } else {
        let s = if total <= slot_entries_height {
            0
        } else if cursor_row < picker.scroll {
            cursor_row
        } else if cursor_row >= picker.scroll + slot_entries_height {
            let for_after = total.saturating_sub(slot_entries_height);
            if for_after <= cursor_row {
                for_after
            } else {
                cursor_row
            }
        } else {
            picker.scroll
        };
        picker.scroll = s.min(total.saturating_sub(slot_entries_height));
        picker.scroll
    };

    let spinner = picker.spinner;
    let buttons = picker.cursor_entry_buttons();
    let entry_text_width = if !buttons.is_empty() && cursor < n {
        let e = &picker.entries[picker.filtered[cursor]];
        let mut w = e.connector().chars().count() + 2 + e.label.chars().count();
        if e.kind == EntryType::Worktree && e.depth == 0 && e.is_open {
            w += 2;
        }
        if let Some(branch) = &e.branch {
            w += 1 + branch.chars().count();
        }
        if let Some(changes) = &e.changes
            && !changes.has_none()
        {
            w += 1
                + format!("+{}", changes.additions).len()
                + 1
                + format!("-{}", changes.deletions).len();
        }
        w as u16
    } else {
        0
    };
    let padding = slot_entries_height.saturating_sub(total);

    picker.clickables.clear();
    if !buttons.is_empty() {
        let by = slot_entries.y + padding as u16 + (cursor_row - scroll) as u16;
        let start_x = slot_entries.x + entry_text_width + 2;
        let mut bx = start_x;
        for (text, action) in &buttons {
            let w = text.len() as u16;
            picker.clickables.push(Clickable {
                rect: Rect::new(bx, by, w, 1),
                action: *action,
            });
            bx += w + 1;
        }
    }
    if !picker.config.hide_hints_footer {
        build_hints_clickables(picker, slots.raw[after_input_idx]);
    }

    let entries = &picker.entries;
    let filtered = &picker.filtered;
    let cmd = picker.mode == Mode::Command;
    let mut lines: Vec<Line> = Vec::with_capacity(slot_entries_height);
    for _ in 0..slot_entries_height.saturating_sub(total) {
        lines.push(Line::raw(""));
    }
    let mut r = scroll;
    while r < total && lines.len() < slot_entries_height {
        match rows[r] {
            Some(p) => {
                let e = &entries[filtered[p]];
                let mut line = entry(e, spinner, p == cursor, cmd);
                if cmd && p == cursor && !buttons.is_empty() {
                    line.spans
                        .push(Span::styled("  ", Style::default().bg(Color::Reset)));
                    for (bi, (text, _action)) in buttons.iter().enumerate() {
                        let is_hovered = picker.clickables.iter().any(|c| {
                            c.action == *_action
                                && c.contains(picker.last_mouse_col, picker.last_mouse_row)
                        });
                        let (key, desc) = text.split_at(1.min(text.len()));
                        if is_hovered {
                            line.spans.push(Span::styled(
                                key,
                                Style::default()
                                    .bg(HOVER_BG)
                                    .fg(HOVER_FG)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            line.spans.push(Span::styled(
                                desc,
                                Style::default().bg(HOVER_BG).fg(HOVER_FG),
                            ));
                        } else {
                            line.spans.push(Span::styled(
                                key,
                                Style::default()
                                    .fg(Color::Reset)
                                    .bg(Color::Reset)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            line.spans.push(Span::styled(
                                desc,
                                Style::default()
                                    .fg(Color::Reset)
                                    .bg(Color::Reset)
                                    .add_modifier(Modifier::DIM),
                            ));
                        }
                        if bi < buttons.len() - 1 {
                            line.spans
                                .push(Span::styled(" ", Style::default().bg(Color::Reset)));
                        }
                    }
                }
                lines.push(line);
            }
            None => {
                let next = rows
                    .get(r + 1)
                    .copied()
                    .flatten()
                    .map(|p| &entries[filtered[p]]);
                lines.push(match next {
                    Some(e) => gap_line(e, cmd),
                    None => Line::raw(""),
                });
            }
        }
        r += 1;
    }

    frame.render_widget(Paragraph::new(lines), slot_entries);
    frame.render_widget(Paragraph::new(input_line(picker)), slot_input);
    render_bottom(frame, picker, &slots);
}

fn render_help(frame: &mut Frame, picker: &mut Picker) {
    let slots = prepare_slots(picker, frame.area());
    let slot_entries = slots.entries;
    let slot_entries_height = slot_entries.height as usize;
    let slot_input = slots.input;
    let after_input_idx = slots.after;

    let display_lines = picker.help_visible_lines();
    let total = display_lines.len();
    let cursor_line = picker.help_cursor_line();
    let scroll = if picker.mouse_hover {
        picker.help_scroll
    } else {
        let s = if total <= slot_entries_height {
            0
        } else if cursor_line < picker.help_scroll {
            cursor_line
        } else if cursor_line >= picker.help_scroll + slot_entries_height {
            cursor_line + 1 - slot_entries_height
        } else {
            picker.help_scroll
        };
        let s = s.min(total.saturating_sub(slot_entries_height));
        picker.help_scroll = s;
        s
    };

    let mut lines: Vec<Line> = Vec::with_capacity(slot_entries_height);
    let padding = slot_entries_height.saturating_sub(total);
    for _ in 0..padding {
        lines.push(Line::raw(""));
    }
    let rows = picker.help_rows();
    for idx in scroll..(scroll + slot_entries_height).min(total) {
        if lines.len() >= slot_entries_height {
            break;
        }
        let raw = &display_lines[idx];
        let is_cursor = idx == cursor_line;
        let is_selectable = rows[idx].is_some();
        let base_style = if is_cursor {
            Style::default().bg(CURSOR_BG).fg(CURSOR_FG)
        } else if !is_selectable {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default()
        };
        // inline `# comment` is virtual: dimmed, not part of selectable content, no cursor bg
        if is_selectable {
            if let Some(hash_idx) = raw.find(" #") {
                let (main, suffix) = raw.split_at(hash_idx);
                let suffix_style = Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM);
                lines.push(Line::from(vec![
                    Span::styled(main, base_style),
                    Span::styled(suffix, suffix_style),
                ]));
            } else if let Some(hash_idx) = raw.find('#') {
                // fallback: `k#` without space
                let (main, suffix) = raw.split_at(hash_idx);
                let main = main.trim_end();
                let suffix_style = Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM);
                lines.push(Line::from(vec![
                    Span::styled(main, base_style),
                    Span::styled(format!(" {}", suffix.trim()), suffix_style),
                ]));
            } else {
                lines.push(Line::from(vec![Span::styled(raw.as_str(), base_style)]));
            }
        } else {
            lines.push(Line::from(vec![Span::styled(raw.as_str(), base_style)]));
        }
    }
    while lines.len() < slot_entries_height {
        lines.push(Line::raw(""));
    }

    picker.clickables.clear();
    if !picker.config.hide_hints_footer {
        build_hints_clickables(picker, slots.raw[after_input_idx]);
    }

    frame.render_widget(Paragraph::new(lines), slot_entries);
    frame.render_widget(Paragraph::new(help_input(picker)), slot_input);
    render_bottom(frame, picker, &slots);
}

pub fn entry(entry: &Entry, spinner: usize, is_cursor: bool, dimmed: bool) -> Line<'_> {
    let effective_dim = dimmed && !is_cursor;
    let marker_fg = if effective_dim {
        CMD_DIM
    } else if is_cursor && entry.kind == EntryType::Dir && entry.is_open {
        CURSOR_FG
    } else {
        match entry.kind {
            EntryType::Dir => Color::Reset,
            EntryType::Worktree => Color::Cyan,
            EntryType::Agent => {
                if entry.is_running {
                    Color::Yellow
                } else {
                    Color::Green
                }
            }
        }
    };
    let marker_style =
        if entry.kind == EntryType::Dir && entry.is_open && !effective_dim && !is_cursor {
            Style::default().fg(marker_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(marker_fg)
        };
    let connector_style = if effective_dim {
        Style::default().fg(CMD_DIM)
    } else if is_cursor {
        Style::default().fg(CURSOR_FG)
    } else {
        Style::default().fg(CONNECTOR)
    };
    let label_style = if effective_dim {
        Style::default().fg(CMD_DIM)
    } else {
        Style::default()
    };
    let mut spans = vec![Span::styled(entry.connector(), connector_style)];
    // Hoisted worktree roots are open sessions too: `* ⑂ label`.
    if entry.kind == EntryType::Worktree && entry.depth == 0 && entry.is_open {
        let star_style = if effective_dim {
            Style::default().fg(CMD_DIM)
        } else if is_cursor {
            Style::default().fg(CURSOR_FG).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Reset).add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled("* ", star_style));
    }
    spans.push(Span::styled(
        format!("{} ", entry.marker(spinner)),
        marker_style,
    ));
    spans.push(Span::styled(entry.label.as_str(), label_style));
    if let Some(branch) = &entry.branch {
        let branch_style = if effective_dim { Style::default().fg(CMD_DIM) } else if is_cursor { Style::default().fg(CURSOR_FG).add_modifier(Modifier::DIM) } else { Style::default().fg(Color::DarkGray) };
        spans.push(Span::styled(format!(" {}", branch), branch_style));
    }
    if let Some(changes) = &entry.changes
        && !changes.has_none()
    {
        spans.push(Span::raw(" "));
        let add_style = if effective_dim {
            Style::default().fg(CMD_DIM)
        } else {
            Style::default().fg(Color::Green)
        };
        spans.push(Span::styled(format!("+{}", changes.additions), add_style));
        spans.push(Span::raw(" "));
        let del_style = if effective_dim {
            Style::default().fg(CMD_DIM)
        } else {
            Style::default().fg(Color::Red)
        };
        spans.push(Span::styled(format!("-{}", changes.deletions), del_style));
    }
    let mut line = Line::from(spans);
    if is_cursor {
        line = line.style(Style::default().bg(CURSOR_BG).fg(CURSOR_FG));
    }
    line
}

fn gap_line(entry: &Entry, dimmed: bool) -> Line<'static> {
    let mut spine = String::new();
    for &last in &entry.ancestors {
        spine.push_str(if last { "  " } else { "│ " });
    }
    if entry.depth > 0 {
        spine.push(LINE_VERT);
    }
    let style = if dimmed {
        Style::default().fg(CMD_DIM)
    } else {
        Style::default().fg(CONNECTOR)
    };
    Line::from(vec![Span::styled(spine, style)])
}

pub fn loader(frame: &mut Frame, area: Rect, spinner_frame: usize) {
    let spinner = SPINNER_DAEMON[spinner_frame % SPINNER_DAEMON.len()];
    let text = format!("{} Warming up the engines", spinner);
    let width = text.len() as u16;
    let y = area.y.saturating_add(area.height.saturating_sub(1));
    let paragraph = Paragraph::new(text).style(Style::default().fg(Color::Yellow));
    frame.render_widget(paragraph, Rect::new(area.x, y, width, 1));
}

fn cursor_line(prefix: String, input: &str, pos: usize, dim: bool) -> Line<'_> {
    let dim_style = Style::default().fg(CMD_DIM);
    let prompt_style = if dim {
        dim_style.add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(Color::Reset)
            .add_modifier(Modifier::BOLD)
    };
    let cursor_style = if dim {
        dim_style
    } else {
        Style::default().bg(CURSOR_FG).fg(Color::Black)
    };
    let mut spans = vec![Span::styled(prefix, prompt_style)];
    if input.is_empty() {
        spans.push(Span::styled(" ", cursor_style));
    } else if pos == input.len() {
        let text = if dim {
            Span::styled(input, dim_style)
        } else {
            Span::raw(input)
        };
        spans.push(text);
        spans.push(Span::styled(" ", cursor_style));
    } else {
        let before = if dim {
            Span::styled(&input[..pos], dim_style)
        } else {
            Span::raw(&input[..pos])
        };
        let at = &input[pos..=pos];
        let after = if dim {
            Span::styled(&input[pos + 1..], dim_style)
        } else {
            Span::raw(&input[pos + 1..])
        };
        spans.push(before);
        spans.push(Span::styled(at, cursor_style));
        spans.push(after);
    }
    Line::from(spans)
}

pub fn help_input(picker: &Picker) -> Line<'_> {
    if picker.mode == Mode::HelpEditing {
        if let Some(edit) = &picker.help_edit {
            return cursor_line(
                format!("▸ {} = ", edit.key),
                &picker.input,
                picker.input_cursor,
                false,
            );
        }
    }
    if picker.mode == Mode::Help {
        return cursor_line("▸ ".to_string(), &picker.input, picker.input_cursor, false);
    }
    input_line(picker)
}

pub fn input_line(picker: &Picker) -> Line<'_> {
    cursor_line(
        "▸ ".to_string(),
        &picker.input,
        picker.input_cursor,
        picker.mode == Mode::Command,
    )
}

fn build_hints_clickables(picker: &mut Picker, hints_slot: Rect) {
    if picker.config.hide_hints_footer {
        return;
    }
    let y = hints_slot.y;
    let mut x = hints_slot.x;
    match picker.mode {
        Mode::HelpEditing => {
            x += (" Help · Editing ".chars().count() + 2) as u16;
            let w = 3 + " Exit Edit Mode".len();
            picker.clickables.push(Clickable {
                rect: Rect::new(x, y, w as u16, 1),
                action: Action::ExitHelp,
            });
        }
        Mode::Help => {
            x += (" Help ".len() + 2) as u16;
            let w = 3 + " Exit Help".len();
            picker.clickables.push(Clickable {
                rect: Rect::new(x, y, w as u16, 1),
                action: Action::ExitHelp,
            });
        }
        Mode::Command => {
            x += (" Command ".len() + 2) as u16;
            let w = 3 + " Escape Command Mode".len();
            picker.clickables.push(Clickable {
                rect: Rect::new(x, y, w as u16, 1),
                action: Action::ExitCommandMode,
            });
            x += w as u16 + 2;
            let hw = 1 + 1 + " Help/Config".len();
            picker.clickables.push(Clickable {
                rect: Rect::new(x, y, hw as u16, 1),
                action: Action::HelpMode,
            });
        }
        Mode::Normal => {
            let first_w = 1 + " Command Mode  ".len();
            picker.clickables.push(Clickable {
                rect: Rect::new(x, y, first_w as u16, 1),
                action: Action::CommandMode,
            });
            x += first_w as u16;
            let hw = "? Help/Config  ".len() as u16;
            picker.clickables.push(Clickable {
                rect: Rect::new(x, y, hw, 1),
                action: Action::HelpMode,
            });
        }
    }
}

fn hints_line(picker: &Picker, hovered_action: Option<Action>) -> Line<'_> {
    let hk = Style::default()
        .bg(HOVER_BG)
        .fg(HOVER_FG)
        .add_modifier(Modifier::BOLD);
    let hd = Style::default().bg(HOVER_BG).fg(HOVER_FG);
    let sk = Style::default()
        .bg(Color::Reset)
        .add_modifier(Modifier::BOLD);
    let sd = Style::default()
        .bg(Color::Reset)
        .add_modifier(Modifier::DIM);
    match picker.mode {
        Mode::HelpEditing => {
            let exit_hovered = hovered_action == Some(Action::ExitHelp);
            let badge = Style::default()
                .bg(CURSOR_FG)
                .fg(DEFAULT_BG)
                .add_modifier(Modifier::BOLD);
            Line::from(vec![
                Span::styled(" Help · Editing ", badge),
                Span::styled("  ", Style::default()),
                Span::styled("ESC", if exit_hovered { hk } else { sk }),
                Span::styled(" Exit Edit Mode", if exit_hovered { hd } else { sd }),
            ])
        }
        Mode::Help => {
            let exit_hovered = hovered_action == Some(Action::ExitHelp);
            let badge = Style::default()
                .bg(CURSOR_FG)
                .fg(DEFAULT_BG)
                .add_modifier(Modifier::BOLD);
            Line::from(vec![
                Span::styled(" Help ", badge),
                Span::styled("  ", Style::default()),
                Span::styled("ESC", if exit_hovered { hk } else { sk }),
                Span::styled(" Exit Help  ", if exit_hovered { hd } else { sd }),
                Span::styled("↑↓", sk),
                Span::styled(" Navigate  ", sd),
                Span::styled("Return", sk),
                Span::styled(" Edit", sd),
            ])
        }
        Mode::Command => {
            let exit_hovered = hovered_action == Some(Action::ExitCommandMode);
            let help_hovered = hovered_action == Some(Action::HelpMode);
            let badge = Style::default()
                .bg(CURSOR_FG)
                .fg(DEFAULT_BG)
                .add_modifier(Modifier::BOLD);
            Line::from(vec![
                Span::styled(" Command ", badge),
                Span::styled("  ", Style::default()),
                Span::styled("ESC", if exit_hovered { hk } else { sk }),
                Span::styled(" Escape Command Mode  ", if exit_hovered { hd } else { sd }),
                Span::styled(
                    format!("{}", picker.config.bind_help),
                    if help_hovered { hk } else { sk },
                ),
                Span::styled(" Help/Config", if help_hovered { hd } else { sd }),
            ])
        }
        Mode::Normal => {
            let hovered_cmd = hovered_action == Some(Action::CommandMode);
            let hovered_help = hovered_action == Some(Action::HelpMode);
            Line::from(vec![
                Span::styled(
                    format!("{}", picker.config.bind_command_mode),
                    if hovered_cmd { hk } else { sk },
                ),
                Span::styled(" Command Mode  ", if hovered_cmd { hd } else { sd }),
                Span::styled(
                    format!("{}", picker.config.bind_help),
                    if hovered_help { hk } else { sk },
                ),
                Span::styled(" Help/Config", if hovered_help { hd } else { sd }),
            ])
        }
    }
}

pub fn feedback(feedback: &FeedbackEntry) -> Line<'_> {
    let fg = match feedback.level {
        FeedbackType::Error => Color::Rgb(255, 60, 60),
        FeedbackType::Warning => Color::Rgb(255, 200, 50),
    };
    Line::from(vec![Span::styled(
        format!("\u{26a0} {}", feedback.message),
        Style::default().fg(fg),
    )])
}

