//! Drawing the [`Model`]: a header, the process table, and one status / prompt line. No
//! colours, only bold and reverse video, so it reads the same on any terminal and with
//! `NO_COLOR`; narrow terminals shrink the name column.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Cell, Paragraph, Row as TableRow, Table, TableState};

use crate::model::{Mode, Model};

const HELP: &str = "↑↓/jk move · / search · d end · D end tree · q quit";

/// `12 KB`, `3.4 MB`, `1.20 GB`.
#[must_use]
#[allow(clippy::cast_precision_loss)] // display only
pub fn bytes(b: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let v = b as f64;
    if v < MIB {
        format!("{:.0} KB", v / KIB)
    } else if v < GIB {
        format!("{:.1} MB", v / MIB)
    } else {
        format!("{:.2} GB", v / GIB)
    }
}

/// The last line: help or a message, the search being typed, or the kill prompt.
fn status(m: &Model) -> String {
    match &m.mode {
        Mode::Search => format!("/{}", m.search),
        Mode::Confirm {
            key,
            tree,
            name,
            warning,
        } => {
            let what = if *tree {
                " and everything it started"
            } else {
                ""
            };
            let warn = warning
                .as_ref()
                .map_or_else(String::new, |w| format!("  ({w})"));
            format!("End {name} ({}){what}? [y/N]{warn}", key.pid)
        }
        Mode::Normal => m.message.clone().unwrap_or_else(|| HELP.to_owned()),
    }
}

/// Draws one frame.
pub fn draw(f: &mut Frame<'_>, m: &Model) {
    let [top, middle, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let total = m.frame.as_ref().map_or(0, |f| f.snapshot.procs.len());
    let search = if m.search.is_empty() {
        String::new()
    } else {
        format!(" · /{}", m.search)
    };
    let title = format!("Atropos · {} of {total} processes{search}", m.rows.len());
    f.render_widget(Paragraph::new(Line::from(title)), top);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let header = TableRow::new(["PID", "NAME", "CPU %", "MEMORY", "USER"]).style(bold);
    let rows = m.rows.iter().map(|r| {
        TableRow::new([
            Cell::from(r.key.pid.to_string()),
            Cell::from(r.name.clone()),
            Cell::from(
                r.cpu_pct
                    .map_or_else(|| "—".to_owned(), |c| format!("{c:.1}")),
            ),
            Cell::from(bytes(r.memory)),
            Cell::from(r.user.clone()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected((!m.rows.is_empty()).then_some(m.selected));
    f.render_stateful_widget(table, middle, &mut state);

    f.render_widget(Paragraph::new(Line::from(status(m))), bottom);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{k, model};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn screen(m: &Model, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, m)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn bytes_scale() {
        assert_eq!(bytes(12 * 1024), "12 KB");
        assert_eq!(bytes(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
        assert_eq!(bytes(1_288_490_189), "1.20 GB");
    }

    #[test]
    fn the_table_shows_the_processes_busiest_first_with_help() {
        let s = screen(&model(), 80, 24);
        assert!(s.starts_with("Atropos · 5 of 5 processes"), "{s}");
        let header = s.lines().nth(1).unwrap();
        for col in ["PID", "NAME", "CPU %", "MEMORY", "USER"] {
            assert!(header.contains(col), "{header}");
        }
        let first = s.lines().nth(2).unwrap();
        assert!(
            first.contains("node") && first.contains("30.0") && first.contains("300.0 MB"),
            "{first}"
        );
        assert!(s.lines().last().unwrap().contains("d end"), "{s}");
    }

    #[test]
    fn search_and_confirm_show_on_the_last_line() {
        let mut m = model();
        m.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        m.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        let s = screen(&m, 80, 24);
        assert_eq!(s.lines().last().unwrap(), "/v");
        assert!(s.starts_with("Atropos · 1 of 5 processes · /v"), "{s}");
        m.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        m.on_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));
        let s = screen(&m, 80, 24);
        assert!(
            s.lines()
                .last()
                .unwrap()
                .contains("End vim (3) and everything it started? [y/N]"),
            "{s}"
        );
        assert_eq!(m.selected_key(), Some(k(3)));
    }

    #[test]
    fn a_tiny_terminal_still_draws() {
        let s = screen(&model(), 40, 6);
        assert!(s.contains("node"), "{s}");
        let _ = screen(&model(), 10, 2);
    }
}
