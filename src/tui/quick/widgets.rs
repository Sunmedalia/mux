use super::*;

pub(super) fn short(n: i64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}
pub(super) fn token_label(t: &Totals) -> String {
    if t.calls > 0 && t.unknown == t.calls {
        return "unknown".into();
    }
    format!(
        "{}{}",
        short(t.input + t.output),
        if t.unknown > 0 { "+?" } else { "" }
    )
}
pub(super) fn token_digits(value: &str, color: Color) -> Vec<Line<'static>> {
    super::meters::digits(value)
        .into_iter()
        .map(|row| line(row, color))
        .collect()
}
pub(super) fn mini_token_total(value: &str, width: u16, color: Color) -> Vec<Line<'static>> {
    if width < 2 {
        return vec![mini_line(value, width, color)];
    }
    let chars: Vec<char> = value.chars().collect();
    chars
        .chunks((usize::from(width) + 1) / 3)
        .flat_map(|chunk| {
            let mut rows = [String::new(), String::new(), String::new()];
            for (index, ch) in chunk.iter().enumerate() {
                // Two-column glyphs retain a middle row so all ten digits are distinct.
                let glyph = match ch {
                    '0' => ["▛▜", "▌▐", "▙▟"],
                    '1' => ["▗▌", " ▌", " ▌"],
                    '2' => ["▀▜", "▛▀", "▙▄"],
                    '3' => ["▀▜", " ▜", "▄▟"],
                    '4' => ["▌▐", "▀▜", " ▐"],
                    '5' => ["▛▀", "▀▜", "▄▟"],
                    '6' => ["▛▀", "▛▜", "▙▟"],
                    '7' => ["▀▜", " ▐", " ▐"],
                    '8' => ["▛▜", "▛▜", "▙▟"],
                    '9' => ["▛▜", "▀▜", "▄▟"],
                    '.' => [" ", " ", "▪"],
                    'K' => ["▌▞", "▛▖", "▌▚"],
                    'M' => ["▙▟", "▌▐", "▌▐"],
                    'B' => ["▛▖", "▛▖", "▙▘"],
                    '?' => ["▀▜", " ▘", " ▖"],
                    _ => ["  ", "━━", "  "],
                };
                for (row, part) in rows.iter_mut().zip(glyph) {
                    if index > 0 {
                        row.push(' ');
                    }
                    row.push_str(part);
                }
            }
            rows.into_iter().map(|row| line(row, color))
        })
        .collect()
}

pub(super) fn line(text: impl Into<String>, color: Color) -> Line<'static> {
    Line::from(Span::styled(text.into(), Style::default().fg(color)))
}
pub(super) fn clipped(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.width() <= width {
        return text.into();
    }
    let mut result = String::new();
    for ch in text.chars() {
        if result.width() + ch.width().unwrap_or(0) + 1 > width {
            break;
        }
        result.push(ch);
    }
    result.push('…');
    result
}
pub(super) fn pair(
    label: &str,
    value: impl Into<String>,
    width: u16,
    color: Color,
) -> Line<'static> {
    let value = value.into();
    let available = usize::from(width).saturating_sub(value.width() + 1);
    let mut used = 0;
    let label: String = label
        .chars()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= available
        })
        .collect();
    let gap = usize::from(width).saturating_sub(label.width() + value.width());
    Line::from(vec![
        Span::styled(label, Style::default().fg(SOFT)),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            value,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}
pub(super) fn duo_line(
    width: u16,
    left: (&str, &str, &str, Color),
    right: (&str, &str, &str, Color),
) -> Line<'static> {
    let full = format!("{} {}  ·  {} {}", left.0, left.2, right.0, right.2);
    let (left_label, right_label, separator) = if full.width() <= usize::from(width) {
        (left.0, right.0, "  ·  ")
    } else {
        (left.1, right.1, " · ")
    };
    let compact = format!(
        "{left_label} {}{separator}{right_label} {}",
        left.2, right.2
    );
    if compact.width() > usize::from(width) {
        return line(clipped(&compact, width.into()), SOFT);
    }
    Line::from(vec![
        Span::styled(format!("{left_label} "), Style::default().fg(SOFT)),
        Span::styled(
            left.2.to_owned(),
            Style::default().fg(left.3).add_modifier(Modifier::BOLD),
        ),
        Span::styled(separator, Style::default().fg(SOFT)),
        Span::styled(format!("{right_label} "), Style::default().fg(SOFT)),
        Span::styled(
            right.2.to_owned(),
            Style::default().fg(right.3).add_modifier(Modifier::BOLD),
        ),
    ])
}
pub(super) fn output_speed(t: &Totals) -> Option<f64> {
    (t.speed_ms > 0).then(|| t.speed_output as f64 * 1000.0 / t.speed_ms as f64)
}
pub(super) fn speed_color(speed: f64) -> Color {
    if speed < 50.0 {
        Color::Rgb(236, 104, 113)
    } else if speed < 100.0 {
        Color::Rgb(180, 133, 222)
    } else if speed < 200.0 {
        Color::Rgb(112, 171, 235)
    } else {
        Color::Rgb(133, 212, 162)
    }
}
pub(super) fn speed_spans(value: &str, speed: Option<f64>) -> Vec<Span<'static>> {
    let style = |color| Style::default().fg(color).add_modifier(Modifier::BOLD);
    match speed {
        None => vec![Span::styled(value.to_owned(), style(SOFT))],
        Some(speed) if speed >= 300.0 => {
            let rainbow = [
                (236, 104, 113),
                (244, 164, 101),
                (239, 214, 111),
                (133, 212, 162),
                (112, 201, 228),
                (112, 171, 235),
                (180, 133, 222),
            ];
            let chars: Vec<char> = value.chars().collect();
            let last = chars.len().saturating_sub(1).max(1);
            chars
                .into_iter()
                .enumerate()
                .map(|(index, ch)| {
                    let color = rainbow[index * (rainbow.len() - 1) / last];
                    Span::styled(ch.to_string(), style(Color::Rgb(color.0, color.1, color.2)))
                })
                .collect()
        }
        Some(speed) => vec![Span::styled(value.to_owned(), style(speed_color(speed)))],
    }
}
pub(super) fn speed_pair(label: &str, totals: &Totals, width: u16) -> Line<'static> {
    let speed = output_speed(totals);
    let value = speed.map_or("—".into(), |n| format!("{n:.1} tok/s"));
    let available = usize::from(width).saturating_sub(value.width() + 1);
    let label = clipped(label, available);
    let gap = usize::from(width).saturating_sub(label.width() + value.width());
    let mut spans = vec![
        Span::styled(label, Style::default().fg(SOFT)),
        Span::raw(" ".repeat(gap)),
    ];
    spans.extend(speed_spans(&value, speed));
    Line::from(spans)
}
pub(super) fn mini_speed_line(totals: &Totals, width: u16) -> Line<'static> {
    let speed = output_speed(totals);
    let value = speed.map_or("—".into(), |n| format!("{n:.1} tok/s"));
    let value = clipped(&value, usize::from(width.saturating_sub(2)));
    let mut spans = vec![Span::styled("↓ ", Style::default().fg(SOFT))];
    spans.extend(speed_spans(&value, speed));
    Line::from(spans)
}
pub(super) fn section(title: &str, width: u16) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            title.to_owned(),
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " {}",
                "─".repeat(usize::from(width).saturating_sub(title.width() + 1))
            ),
            Style::default().fg(RAIL),
        ),
    ])
}
pub(super) fn section_action(title: &str, action: &str, width: u16) -> Line<'static> {
    let available = usize::from(width).saturating_sub(action.width() + 1);
    let title = clipped(title, available);
    let gap = usize::from(width).saturating_sub(title.width() + action.width());
    let separator = if gap >= 2 {
        format!(" {} ", "─".repeat(gap - 2))
    } else {
        " ".into()
    };
    Line::from(vec![
        Span::styled(format!("{title}{separator}"), Style::default().fg(SOFT)),
        Span::styled(
            action.to_string(),
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        ),
    ])
}

// Appearance uses the same typography and data components as the live pane.
pub(in crate::tui) fn preview_lines(width: u16) -> Vec<Line<'static>> {
    vec![
        section("TODAY / GATEWAY", width),
        pair("Tokens", "128,400", width, BLUE),
        duo_line(
            width,
            ("Input", "IN", "96K", BLUE),
            ("Output", "OUT", "32.4K", METRIC),
        ),
        Line::default(),
        section("CURRENT SESSION", width),
        pair("Tokens", "42,800", width, METRIC),
        pair("Cache reuse", "68%", width, METRIC),
        Line::default(),
        Line::from(vec![
            Span::styled("● Connected", Style::default().fg(GREEN)),
            Span::styled("  ! 2 retries", Style::default().fg(GOLD)),
        ]),
        line("× Failed request", RED),
    ]
}
pub(super) fn compact_reset(reset: &str) -> String {
    let time = chrono::DateTime::parse_from_rfc3339(reset)
        .or_else(|_| chrono::DateTime::parse_from_str(reset, "%Y-%m-%d %H:%M %:z"));
    if let Ok(time) = time {
        let seconds = time.timestamp() - chrono::Utc::now().timestamp();
        if seconds <= 0 {
            return "Due · r refresh".into();
        }
        let minutes = (seconds + 59) / 60;
        if minutes >= 1440 {
            format!("{}d {}h", minutes / 1440, minutes % 1440 / 60)
        } else if minutes >= 60 {
            format!("{}h {}m", minutes / 60, minutes % 60)
        } else {
            format!("{minutes}m")
        }
    } else {
        reset.into()
    }
}
pub(super) fn compact_age(age: &str) -> String {
    let Some(minutes) = age
        .strip_suffix("m ago")
        .and_then(|m| m.parse::<u64>().ok())
    else {
        return age.into();
    };
    if minutes >= 1440 {
        format!("{}d ago", minutes / 1440)
    } else if minutes >= 60 {
        format!("{}h ago", minutes / 60)
    } else {
        age.into()
    }
}
// Eighth-cell precision keeps small values visible without making 99% look full.
pub(super) fn progress_spans(
    fraction: Option<f64>,
    cells: usize,
    color: Color,
) -> Vec<Span<'static>> {
    super::meters::progress_spans(fraction, cells, color, RAIL)
}
pub(super) fn quota_color(percent: f64) -> Color {
    if percent >= 90.0 {
        RED
    } else if percent >= 75.0 {
        GOLD
    } else {
        BLUE
    }
}
pub(super) fn compact_account_quota(label: &str, percent: f64, width: u16) -> Line<'static> {
    compact_account_meter(label, Some(percent), width)
}
pub(super) fn compact_account_meter(
    label: &str,
    percent: Option<f64>,
    width: u16,
) -> Line<'static> {
    let percent = percent
        .filter(|percent| percent.is_finite())
        .map(|percent| percent.clamp(0.0, 100.0));
    let value = percent
        .map(|percent| format!("{percent:.0}%"))
        .unwrap_or("—".into());
    let color = percent.map(quota_color).unwrap_or(SOFT);
    if usize::from(width) <= value.width() + 2 {
        return line(clipped(&value, width.into()), color);
    }
    let label = label.strip_prefix("codex ").unwrap_or(label);
    let label = clipped(
        label,
        usize::from(width).saturating_sub(value.width() + 2).min(12),
    );
    let cells = usize::from(width).saturating_sub(label.width() + value.width() + 2);
    let mut spans = vec![
        Span::styled(label, Style::default().fg(INK)),
        Span::raw(" "),
    ];
    if let Some(percent) = percent {
        spans.extend(progress_spans(Some(percent / 100.0), cells, color));
    } else {
        spans.push(Span::styled("▒".repeat(cells), Style::default().fg(RAIL)));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        value,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

pub(super) fn meter(label: &str, fraction: Option<f64>, width: u16, color: Color) -> Line<'static> {
    let prefix = format!("{label} ");
    let cells = usize::from(width).saturating_sub(prefix.width());
    let mut spans = vec![Span::styled(
        clipped(&prefix, width.into()),
        Style::default().fg(SOFT),
    )];
    spans.extend(progress_spans(fraction, cells, color));
    Line::from(spans)
}
pub(super) fn health_meter(t: &Totals, width: u16) -> Line<'static> {
    let cells = usize::from(width.saturating_sub(4));
    let counts = [
        t.success.max(0) as u64,
        t.failed.max(0) as u64,
        t.interrupted.max(0) as u64,
    ];
    let total: u64 = counts.iter().sum();
    if total == 0 || cells == 0 {
        return meter("OK ", None, width, GREEN);
    }
    let mut lengths = [0usize; 3];
    let remaining = cells;
    let mut remainders = [0u64; 3];
    for i in 0..3 {
        let weighted = (remaining as u128) * u128::from(counts[i]);
        lengths[i] += (weighted / u128::from(total)) as usize;
        remainders[i] = (weighted % u128::from(total)) as u64;
    }
    let mut leftover = cells.saturating_sub(lengths.iter().sum::<usize>());
    while leftover > 0 {
        let i = (0..3).max_by_key(|&i| remainders[i]).unwrap();
        lengths[i] += 1;
        remainders[i] = 0;
        leftover -= 1;
    }
    Line::from(vec![
        Span::styled("OK  ", Style::default().fg(SOFT)),
        Span::styled("█".repeat(lengths[0]), Style::default().fg(GREEN)),
        Span::styled("█".repeat(lengths[1]), Style::default().fg(RED)),
        Span::styled("█".repeat(lengths[2]), Style::default().fg(GOLD)),
    ])
}
pub(super) fn compact_token_meter(input: i64, output: i64, width: u16) -> Line<'static> {
    let info = format!("I {}  O {}", short(input), short(output));
    let cells = usize::from(width).saturating_sub(info.width() + 5).min(10);
    let total = input.saturating_add(output);
    let input_cells = if total > 0 {
        ((input as f64 / total as f64) * cells as f64).round() as usize
    } else {
        0
    };
    let output_cells = if total > 0 {
        cells.saturating_sub(input_cells)
    } else {
        0
    };
    Line::from(vec![
        Span::styled("I/O ", Style::default().fg(SOFT)),
        Span::styled("█".repeat(input_cells), Style::default().fg(BLUE)),
        Span::styled("█".repeat(output_cells), Style::default().fg(METRIC)),
        Span::styled(
            "░".repeat(cells.saturating_sub(input_cells + output_cells)),
            Style::default().fg(RAIL),
        ),
        Span::raw(" "),
        Span::styled(info, Style::default().fg(INK)),
    ])
}
pub(super) fn compact_cache_meter(
    read: i64,
    write: i64,
    reuse: Option<f64>,
    known: bool,
    width: u16,
) -> Line<'static> {
    let info = format!(
        "{} R {} W {}",
        reuse.map_or("—".into(), |n| format!("{n:.1}%")),
        if known { short(read) } else { "—".into() },
        if known { short(write) } else { "—".into() }
    );
    let cells = usize::from(width).saturating_sub(info.width() + 3).min(10);
    let mut spans = vec![Span::styled("↺ ", Style::default().fg(SOFT))];
    spans.extend(progress_spans(reuse.map(|n| n / 100.0), cells, GREEN));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(info, Style::default().fg(INK)));
    Line::from(spans)
}
pub(super) fn gateway_cache_meter(t: &Totals, unknown: bool, width: u16) -> Vec<Line<'static>> {
    let hit = (t.cache_input > 0).then(|| 100.0 * t.cache_hits as f64 / t.cache_input as f64);
    let percent = hit.map_or("—".into(), |n| format!("{n:.1}%"));
    let read = if unknown {
        "—".into()
    } else {
        short(t.cache_read)
    };
    let write = if unknown {
        "—".into()
    } else {
        short(t.cache_write)
    };
    let info = format!("{percent} R {read} W {write}");
    let available = usize::from(width).saturating_sub("HIT  ".width() + info.width());
    if available < 4 {
        return vec![
            meter("HIT", hit.map(|n| n / 100.0), width, GREEN),
            pair(
                "R / W",
                format!("{read} / {write} · {percent}"),
                width,
                GREEN,
            ),
        ];
    }
    let cells = available.min(10);
    let mut spans = vec![Span::styled("HIT ", Style::default().fg(SOFT))];
    spans.extend(progress_spans(hit.map(|n| n / 100.0), cells, GREEN));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(info, Style::default().fg(INK)));
    vec![Line::from(spans)]
}
pub(super) fn buttons(area: Rect) -> Vec<Rect> {
    let constraints = vec![Constraint::Ratio(1, 5); 5];
    Layout::horizontal(constraints)
        .split(Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            area.width,
            1,
        ))
        .to_vec()
}
pub(super) fn page_tab_rects(area: Rect, reserve: u16) -> [Rect; 2] {
    let available = area.width.saturating_sub(reserve);
    let gap = u16::from(available > 1);
    let content = available.saturating_sub(gap);
    let token = content.div_ceil(2).min(10);
    let git = content.saturating_sub(token).min(8);
    [
        Rect::new(area.x, area.y, token, 1),
        Rect::new(area.x + token + gap, area.y, git, 1),
    ]
}
pub(super) fn pulse_header_area(screen: Rect) -> (Rect, u16) {
    let mini = screen.width < 32 || screen.height < 12;
    (
        if mini {
            screen
        } else {
            screen.inner(Margin::new(2, 0))
        },
        if mini { 4 } else { 9 },
    )
}
pub(super) fn draw_page_tabs(
    frame: &mut ratatui::Frame,
    area: Rect,
    reserve: u16,
    git: bool,
    switchable: bool,
) {
    let tabs = page_tab_rects(area, reserve);
    if tabs[1].x > tabs[0].right() {
        frame.render_widget(
            Paragraph::new("│").style(Style::default().fg(SOFT).bg(RAIL)),
            Rect::new(tabs[0].right(), area.y, 1, 1),
        );
    }
    for (i, rect) in tabs.into_iter().enumerate() {
        let selected = (i == 1) == git;
        let label = match (i, selected) {
            (0, true) => "● TOKEN",
            (0, false) => "○ TOKEN",
            (1, true) => "● GIT",
            _ => "○ GIT",
        };
        let compact = if i == 0 { "TOK" } else { "GIT" };
        frame.render_widget(
            Paragraph::new(clipped(
                if label.width() <= usize::from(rect.width) {
                    label
                } else {
                    compact
                },
                usize::from(rect.width),
            ))
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(if selected { BG } else { SOFT })
                    .bg(if selected { BLUE } else { RAIL })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else if !switchable {
                        Modifier::DIM
                    } else {
                        Modifier::empty()
                    }),
            ),
            rect,
        );
    }
}
pub(super) fn content_body(inner: Rect) -> Rect {
    Rect::new(
        inner.x,
        inner.y + 4,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(7),
    )
}
pub(super) fn mini_body(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(2),
        area.width,
        area.height.saturating_sub(4),
    )
}
pub(super) fn mini_buttons(area: Rect) -> Vec<Rect> {
    Layout::horizontal([Constraint::Ratio(1, 5); 5])
        .split(Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            area.width,
            1,
        ))
        .to_vec()
}
pub(super) fn mini_line(text: impl AsRef<str>, width: u16, color: Color) -> Line<'static> {
    line(clipped(text.as_ref(), usize::from(width)), color)
}
pub(super) fn mini_sparkline(hours: &[i64; 24]) -> String {
    let buckets: Vec<i64> = hours.chunks(3).map(|hours| hours.iter().sum()).collect();
    let peak = buckets.iter().copied().max().unwrap_or(0);
    let bars = ['·', '▁', '▂', '▃', '▄', '▅', '▆', '█'];
    buckets
        .into_iter()
        .map(|count| {
            if peak == 0 || count == 0 {
                bars[0]
            } else {
                bars[((count as f64 / peak as f64 * 7.0).round() as usize).clamp(1, 7)]
            }
        })
        .collect()
}
pub(super) fn mini_hourly_rows(hours: &[i64; 24], width: u16, color: Color) -> Vec<Line<'static>> {
    let peak = hours.iter().copied().max().unwrap_or(0);
    let bar_width = usize::from(width.saturating_sub(12)).max(1);
    hours
        .iter()
        .enumerate()
        .map(|(hour, count)| {
            let filled = if peak > 0 && *count > 0 {
                ((*count as f64 / peak as f64 * bar_width as f64).round() as usize)
                    .clamp(1, bar_width)
            } else {
                0
            };
            mini_line(
                format!(
                    "{hour:02} {:<bar_width$} {}",
                    if filled == 0 {
                        "·".into()
                    } else {
                        "█".repeat(filled)
                    },
                    short(*count),
                ),
                width,
                color,
            )
        })
        .collect()
}
pub(super) fn scrollbar_target(
    body: Rect,
    rail_x: u16,
    column: u16,
    row: u16,
    limit: u16,
) -> Option<u16> {
    if limit == 0 || body.height < 2 || column != rail_x || row < body.y || row >= body.bottom() {
        return None;
    }
    let relative = u32::from(row - body.y);
    let maximum = u32::from(body.height - 1);
    Some(((relative * u32::from(limit) + maximum / 2) / maximum) as u16)
}

pub(super) fn hourly_chart(hours: &[i64; 24], width: u16, color: Color) -> Vec<Line<'static>> {
    let plot = usize::from(width.saturating_sub(1));
    if plot < 24 {
        return vec![];
    }
    let max = hours.iter().copied().max().unwrap_or(0);
    let mut lines = vec![];
    let blocks = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    for row in (0..6).rev() {
        let mut spans = vec![Span::styled("│", Style::default().fg(RAIL))];
        for (hour, count) in hours.iter().enumerate() {
            let cells = (hour + 1) * plot / 24 - hour * plot / 24;
            let level = if max == 0 || *count == 0 {
                0
            } else {
                ((*count as f64 / max as f64 * 48.0).round() as usize).max(1)
            };
            let fill = level.saturating_sub(row * 8).min(8);
            spans.push(Span::styled(
                blocks[fill].to_string().repeat(cells),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.push(line(format!("└{}", "─".repeat(plot)), RAIL));
    let mut labels = vec![' '; plot];
    for hour in [0, 6, 12, 18, 23] {
        let x = (hour * plot / 24).min(plot.saturating_sub(2));
        for (i, c) in format!("{hour:02}").chars().enumerate() {
            labels[x + i] = c;
        }
    }
    lines.push(line(
        format!(" {}", labels.into_iter().collect::<String>()),
        SOFT,
    ));
    lines
}
