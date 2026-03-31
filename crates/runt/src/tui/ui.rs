use ansi_to_tui::IntoText;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::state::{App, CellView};

/// Render the entire TUI.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let [top_bar, cell_area, bottom_bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_top_bar(frame, app, top_bar);
    render_cells(frame, app, cell_area);
    render_bottom_bar(frame, bottom_bar);
}

fn render_top_bar(frame: &mut Frame, app: &App, area: Rect) {
    let path = shorten_path(&app.notebook_path);

    let status_color = match app.kernel_status.as_str() {
        "idle" => Color::Green,
        "busy" => Color::Yellow,
        "starting" => Color::Cyan,
        "not_started" => Color::DarkGray,
        _ if app.kernel_status.starts_with("error") => Color::Red,
        _ => Color::White,
    };

    let lang = if app.kernel_language.is_empty() {
        String::new()
    } else {
        format!(" ({})", app.kernel_language)
    };

    let status_text = format!(" kernel: {}{} ", app.kernel_status, lang);
    let status_len = status_text.len() as u16;
    let path_max = area.width.saturating_sub(status_len);

    let path_display = if path.len() as u16 > path_max {
        format!(
            "...{}",
            &path[path.len().saturating_sub(path_max as usize - 3)..]
        )
    } else {
        path.clone()
    };

    let line = Line::from(vec![
        Span::styled(
            format!(" {}", path_display),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(
            " ".repeat(
                area.width
                    .saturating_sub(path_display.len() as u16 + 1 + status_len)
                    as usize,
            ),
        ),
        Span::styled(
            status_text,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        area,
    );
}

fn render_cells(frame: &mut Frame, app: &App, area: Rect) {
    if app.cells.is_empty() {
        let empty = Paragraph::new("  No cells").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, area);
        return;
    }

    // Calculate heights for each cell and find the scroll position
    let cell_heights: Vec<u16> = app
        .cells
        .iter()
        .enumerate()
        .map(|(i, cell)| cell_height(cell, area.width, i == app.selected))
        .collect();

    // Find the y-offset so the selected cell is visible
    let total_before_selected: u16 = cell_heights[..app.selected].iter().sum();
    let selected_height = cell_heights[app.selected];
    let visible_height = area.height;

    let scroll_y = if total_before_selected < app.scroll_offset {
        total_before_selected
    } else if total_before_selected + selected_height > app.scroll_offset + visible_height {
        (total_before_selected + selected_height).saturating_sub(visible_height)
    } else {
        app.scroll_offset
    };

    // Render cells into a virtual canvas, then clip to the viewport
    let mut y: u16 = 0;
    for (i, cell) in app.cells.iter().enumerate() {
        let h = cell_heights[i];
        let cell_top = y;
        let cell_bottom = y + h;

        // Skip if entirely above viewport
        if cell_bottom <= scroll_y {
            y += h;
            continue;
        }
        // Stop if entirely below viewport
        if cell_top >= scroll_y + visible_height {
            break;
        }

        // Calculate clipped region
        let render_y = area.y + cell_top.saturating_sub(scroll_y);
        let render_height = h
            .min(scroll_y + visible_height - cell_top.max(scroll_y))
            .min(area.y + area.height - render_y);

        if render_height > 0 {
            let cell_area = Rect::new(area.x, render_y, area.width, render_height);
            render_cell(frame, cell, cell_area, i == app.selected);
        }

        y += h;
    }
}

fn cell_height(cell: &CellView, width: u16, _selected: bool) -> u16 {
    let source_lines = cell.source.lines().count().max(1) as u16;
    // Prompt line (In [N]:) + source block (with borders = +2) + output lines + 1 gap
    let border_overhead: u16 = 2; // top + bottom border
    let source_height = source_lines + border_overhead;

    let output_lines: u16 = cell
        .outputs
        .iter()
        .map(|o| {
            let text = CellView::output_text(o);
            let lines = text.lines().count().max(0) as u16;
            // Limit output display to prevent massive cells
            lines.min(20)
        })
        .sum();

    let output_height = if output_lines > 0 {
        output_lines + 1 // +1 for the Out[N]: prompt
    } else {
        0
    };

    let _ = width; // reserved for wrapping calc
    source_height + output_height + 1 // +1 gap between cells
}

fn render_cell(frame: &mut Frame, cell: &CellView, area: Rect, selected: bool) {
    if area.height == 0 {
        return;
    }

    let is_code = cell.cell_type == "code";

    // Prompt text
    let prompt = if is_code {
        match cell.execution_count {
            Some(n) => format!("In [{}]:", n),
            None => "In [ ]:".to_string(),
        }
    } else {
        "Md:".to_string()
    };

    let prompt_width = 9; // "In [99]: " — fixed width for alignment

    // Selection marker
    let marker = if selected { "► " } else { "  " };
    let left_margin = marker.len() + prompt_width;

    // Source block
    let border_style = if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let source_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);

    let source_text = Paragraph::new(cell.source.as_str())
        .block(source_block)
        .wrap(Wrap { trim: false });

    let source_lines = cell.source.lines().count().max(1) as u16;
    let source_height = (source_lines + 2).min(area.height); // +2 for borders

    // Render prompt
    let prompt_style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Blue)
    };

    let prompt_line = Line::from(vec![
        Span::styled(marker, Style::default().fg(Color::Cyan)),
        Span::styled(
            format!("{:>width$}", prompt, width = prompt_width),
            prompt_style,
        ),
    ]);

    if area.height >= 1 {
        frame.render_widget(
            Paragraph::new(prompt_line),
            Rect::new(area.x, area.y, left_margin as u16, 1),
        );
    }

    // Render source
    let source_x = area.x + left_margin as u16;
    let source_w = area.width.saturating_sub(left_margin as u16 + 1);
    if source_w > 2 && source_height > 0 {
        frame.render_widget(
            source_text,
            Rect::new(source_x, area.y, source_w, source_height),
        );
    }

    // Render outputs below source
    if is_code && !cell.outputs.is_empty() {
        let output_y = area.y + source_height;
        let remaining = area.height.saturating_sub(source_height);
        if remaining > 0 {
            render_outputs(frame, cell, area.x, output_y, area.width, remaining);
        }
    }
}

fn render_outputs(frame: &mut Frame, cell: &CellView, x: u16, y: u16, width: u16, max_height: u16) {
    let left_margin = 11u16; // align with source

    let mut current_y = y;

    for output in &cell.outputs {
        if current_y >= y + max_height {
            break;
        }

        let is_error = output.output_type == "error";
        let is_stderr = output.name.as_deref() == Some("stderr");

        let raw_text = CellView::output_text(output);
        if raw_text.is_empty() {
            continue;
        }

        // Convert ANSI to ratatui Text
        let styled_text = raw_text
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(&raw_text));

        // Limit lines
        let max_lines = (y + max_height - current_y) as usize;
        let total_lines = styled_text.lines.len();
        let display_lines = total_lines.min(max_lines).min(20);

        let truncated: Text = if display_lines < total_lines {
            let mut lines: Vec<Line> = styled_text
                .lines
                .into_iter()
                .take(display_lines - 1)
                .collect();
            lines.push(Line::from(Span::styled(
                format!("  ... ({} more lines)", total_lines - display_lines + 1),
                Style::default().fg(Color::DarkGray),
            )));
            Text::from(lines)
        } else {
            styled_text
        };

        // Output prompt for first output
        if current_y == y {
            let out_prompt = if let Some(n) = cell.execution_count {
                format!("Out[{}]:", n)
            } else {
                "Out:".to_string()
            };
            let prompt_style = if is_error || is_stderr {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("  {:>width$}", out_prompt, width = 9),
                    prompt_style,
                )),
                Rect::new(x, current_y, left_margin, 1),
            );
        }

        let output_style = if is_error || is_stderr {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };

        let out_w = width.saturating_sub(left_margin + 1);
        if out_w > 0 {
            frame.render_widget(
                Paragraph::new(truncated)
                    .style(output_style)
                    .wrap(Wrap { trim: false }),
                Rect::new(x + left_margin, current_y, out_w, display_lines as u16),
            );
        }

        current_y += display_lines as u16;
    }
}

fn render_bottom_bar(frame: &mut Frame, area: Rect) {
    let hints = Line::from(vec![
        Span::styled(" j/k", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":navigate  "),
        Span::styled("Ctrl+Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":execute  "),
        Span::styled("g/G", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":first/last  "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":quit"),
    ]);

    frame.render_widget(
        Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        area,
    );
}

fn shorten_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            return format!("~{}", &path[home_str.len()..]);
        }
    }
    path.to_string()
}
