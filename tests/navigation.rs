use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ramo::config::Config;
use ramo::model::{Entry, EntryType, Payload};
use ramo::picker::Picker;
use ratatui::{Terminal, backend::TestBackend, style::Color};

const RESOLUTIONS: [(u16, u16); 4] = [(40, 12), (80, 24), (120, 30), (160, 50)];

fn dir_entry(name: &str) -> Entry {
    Entry {
        kind: EntryType::Dir,
        label: name.into(),
        path: format!("/tmp/ramo-e2e/{name}").into(),
        changes: None,
        branch: None,
        is_open: false,
        is_running: false,
        pending: false,
        depth: 0,
        ancestors: vec![],
        is_last: false,
        search_text: name.into(),
        goto: None,
        parent: None,
        connector: String::new(),
        search_text_lower: name.to_lowercase(),
    }
}

/// Build a key press from the first token of a bind spec (e.g. "up,ctrl-p"
/// → `Up`). Single source of truth stays in `Config`: if the default
/// binding changes, the test follows it instead of rotting.
fn key_for_token(token: &str) -> KeyEvent {
    let lowered = token.trim().to_lowercase();
    let parts: Vec<&str> = lowered.split('-').collect();
    let (mods, name) = (
        &parts[..parts.len() - 1],
        parts[parts.len() - 1].to_string(),
    );
    let mut modifiers = KeyModifiers::empty();
    for m in mods {
        match *m {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            _ => panic!("unsupported modifier in bind token: {token}"),
        }
    }
    let code = match name.as_str() {
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "enter" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
        _ => panic!("unsupported key in bind token: {token}"),
    };
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    }
}

fn nav_up_key(config: &Config) -> KeyEvent {
    let token = config.bind_nav_up.split(',').next().unwrap();
    let key = key_for_token(token);
    assert!(Config::key_matches(&config.bind_nav_up, key));
    key
}

fn screen_text(term: &Terminal<TestBackend>) -> String {
    let buf = term.backend().buffer();
    let (w, h) = (buf.area.width as usize, buf.area.height as usize);
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x as u16, y as u16)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn selected_row(term: &Terminal<TestBackend>) -> Option<usize> {
    let buf = term.backend().buffer();
    let area = buf.area;
    (0..area.height)
        .find(|&y| (0..area.width).any(|x| buf[(x, y)].bg == Color::DarkGray))
        .map(|y| y as usize)
}

#[test]
fn nav_up_moves_selection_up_at_every_resolution() {
    for (w, h) in RESOLUTIONS {
        let config = Config::default();
        let mut picker = Picker::new(Payload {
            entries: vec![dir_entry("alpha"), dir_entry("beta"), dir_entry("gamma")],
            config: config.clone(),
            feedbacks: vec![],
            entries_found: 3,
        });
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();

        term.draw(|f| picker.render(f, &config)).unwrap();
        let text = screen_text(&term);
        assert!(
            text.contains("alpha") && text.contains("beta") && text.contains("gamma"),
            "{w}x{h}: all entries visible:\n{text}"
        );
        let rows: Vec<usize> = ["alpha", "beta", "gamma"]
            .iter()
            .map(|l| text.lines().position(|line| line.contains(l)).unwrap())
            .collect();

        let before = selected_row(&term).expect("{w}x{h}: a row is selected");
        picker.handle_input(nav_up_key(&config));
        term.draw(|f| picker.render(f, &config)).unwrap();

        let after = selected_row(&term).expect("{w}x{h}: a row still selected");
        let at = rows.iter().position(|&r| r == before).unwrap();
        assert_eq!(after, rows[(at + 3 - 1) % 3], "{w}x{h}: moved up one");
    }
}
