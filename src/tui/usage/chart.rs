use super::*;
use chrono::{NaiveDate, Timelike};

struct Series {
    bins: Vec<Totals>,
    first: String,
    last: String,
    step: usize,
    hourly: bool,
    start: NaiveDate,
}

fn series(snapshot: &Snapshot, page: &UsagePage) -> Series {
    let hourly = page.range == 0;
    let today = snapshot.today();
    let end =
        NaiveDate::parse_from_str(if page.range == 3 { &today } else { &page.day }, "%Y-%m-%d")
            .unwrap();
    let selected: Vec<_> = snapshot
        .rows
        .iter()
        .filter(|r| {
            r.kind == "generation"
                && page.includes(&r.day)
                && page.client().is_none_or(|c| c == r.client)
                && page.provider.as_deref().is_none_or(|p| p == r.provider)
        })
        .collect();
    let start = page
        .start_day()
        .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
        .or_else(|| {
            selected
                .iter()
                .filter_map(|r| NaiveDate::parse_from_str(&r.day, "%Y-%m-%d").ok())
                .min()
        })
        .unwrap_or(end);
    let hours = if page.day == snapshot.today() {
        let zone = chrono::FixedOffset::east_opt(snapshot.offset).unwrap();
        chrono::Utc::now().with_timezone(&zone).hour() as usize + 1
    } else {
        24
    };
    let count = if hourly {
        hours
    } else {
        (end - start).num_days().max(0) as usize + 1
    };
    let step = count.div_ceil(256).max(1);
    let mut bins = vec![Totals::default(); count.div_ceil(step)];
    for row in selected {
        let index = if hourly {
            row.hour as usize
        } else {
            let Ok(day) = NaiveDate::parse_from_str(&row.day, "%Y-%m-%d") else {
                continue;
            };
            let days = (day - start).num_days();
            if days < 0 {
                continue;
            }
            days as usize
        };
        if index < count {
            bins[index / step].add(&row.totals);
        }
    }
    Series {
        bins,
        first: if hourly {
            "00:00".into()
        } else {
            start.format("%m/%d").to_string()
        },
        last: if hourly {
            format!("{:02}:00", hours - 1)
        } else {
            end.format("%m/%d").to_string()
        },
        step,
        hourly,
        start,
    }
}

fn coarsen(bins: &[Totals], group: usize) -> Vec<Totals> {
    bins.chunks(group.max(1))
        .map(|chunk| {
            let mut total = Totals::default();
            for bin in chunk {
                total.add(bin);
            }
            total
        })
        .collect()
}

fn short_value(value: u64, width: u16) -> String {
    let exact = value.to_string();
    if exact.len() <= width as usize {
        return exact;
    }
    for (unit, divisor) in [("K", 1_000u64), ("M", 1_000_000), ("B", 1_000_000_000)] {
        if value >= divisor {
            let compact = format!("{}{unit}", value / divisor);
            if compact.len() <= width as usize {
                return compact;
            }
        }
    }
    String::new()
}

pub(super) fn lines(
    snapshot: &Snapshot,
    page: &UsagePage,
    tokens: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let series = series(snapshot, page);
    let group = series
        .bins
        .len()
        .div_ceil(usize::from(width / 2).max(1))
        .max(1);
    let bins = coarsen(&series.bins, group);
    let values: Vec<Option<u64>> = bins
        .iter()
        .map(|bin| {
            if tokens && bin.unknown > 0 {
                None
            } else {
                Some(if tokens {
                    bin.input.saturating_add(bin.output).max(0) as u64
                } else {
                    bin.calls.max(0) as u64
                })
            }
        })
        .collect();
    plot(&series, &values, group, tokens, width)
}

fn plot(
    series: &Series,
    values: &[Option<u64>],
    group: usize,
    tokens: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let peak = values.iter().flatten().copied().max().unwrap_or(0);
    let step = series.step * group;
    let color = if tokens { ROUTE } else { ENABLED };
    let mut out = vec![Line::from(vec![
        Span::styled(
            if tokens { "TOKENS" } else { "CALLS" },
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " / {} {} · peak {}",
                step,
                if series.hourly { "hour(s)" } else { "day(s)" },
                short_value(peak, 10)
            ),
            Style::default().fg(FIELD_LABEL),
        ),
    ])];
    let cell_width = (usize::from(width) / values.len().max(1)).max(1);
    for row in (0..6).rev() {
        let mut spans = vec![];
        for value in values {
            let units = value.map_or(0, |v| {
                if peak == 0 {
                    0
                } else {
                    ((v as f64 / peak as f64) * 48.0).round() as usize
                }
            });
            let fill = units.saturating_sub(row * 8).min(8);
            let symbol = if value.is_none() && row == 0 {
                "?"
            } else if *value == Some(0) && row == 0 {
                "0"
            } else {
                [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"][fill]
            };
            let bar_width = cell_width.saturating_sub(1).max(1);
            let paint = if row == 0 && (value.is_none() || *value == Some(0)) {
                let left = bar_width / 2;
                format!(
                    "{}{}{}",
                    " ".repeat(left),
                    symbol,
                    " ".repeat(bar_width - left - 1)
                )
            } else {
                symbol.repeat(bar_width)
            };
            spans.push(Span::styled(
                paint,
                Style::default().fg(if value.is_none() {
                    WARNING
                } else if fill == 0 {
                    MUTED
                } else {
                    color
                }),
            ));
            if cell_width > 1 {
                spans.push(Span::raw(" "));
            }
        }
        out.push(Line::from(spans));
    }
    let middle = if series.hourly {
        format!("{:02}:00", values.len() / 2 * step)
    } else {
        (series.start + chrono::Duration::days((values.len() / 2 * step) as i64))
            .format("%m/%d")
            .to_string()
    };
    let occupied = series.first.len() + middle.len() + series.last.len();
    let spaces = usize::from(width).saturating_sub(occupied);
    let ticks = if usize::from(width) >= occupied {
        format!(
            "{}{}{}{}{}",
            series.first,
            " ".repeat(spaces / 2),
            middle,
            " ".repeat(spaces - spaces / 2),
            series.last
        )
    } else if usize::from(width) > series.first.len() + series.last.len() {
        format!(
            "{}{}{}",
            series.first,
            " ".repeat(usize::from(width) - series.first.len() - series.last.len()),
            series.last
        )
    } else {
        series.first.chars().take(usize::from(width)).collect()
    };
    out.push(Line::styled(ticks, Style::default().fg(MUTED)));
    out.push(Line::styled(
        if tokens {
            "0 known zero · ? usage unknown"
        } else {
            "0 no calls · gateway requests"
        },
        Style::default().fg(MUTED),
    ));
    out
}

struct SessionSeries {
    axis: Series,
    tokens: Vec<u64>,
    active: Vec<BTreeSet<usize>>,
    partial: usize,
    has_activity: bool,
}

fn session_series(
    rows: &[&crate::sessions::Session],
    snapshot: &Snapshot,
    page: &UsagePage,
) -> SessionSeries {
    let zone = chrono::FixedOffset::east_opt(snapshot.offset)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).unwrap());
    let hourly = page.range == 0;
    let today = snapshot.today();
    let end =
        NaiveDate::parse_from_str(if page.range == 3 { &today } else { &page.day }, "%Y-%m-%d")
            .unwrap();
    let start = page
        .start_day()
        .and_then(|day| NaiveDate::parse_from_str(&day, "%Y-%m-%d").ok())
        .or_else(|| {
            rows.iter()
                .flat_map(|s| s.activity.keys())
                .filter_map(|timestamp| chrono::DateTime::from_timestamp(*timestamp, 0))
                .map(|time| time.with_timezone(&zone).date_naive())
                .filter(|day| *day <= end)
                .min()
        })
        .unwrap_or(end);
    let hours = if hourly && page.day == today {
        chrono::Utc::now().with_timezone(&zone).hour() as usize + 1
    } else {
        24
    };
    let count = if hourly {
        hours
    } else {
        (end - start).num_days().max(0) as usize + 1
    };
    let step = count.div_ceil(256).max(1);
    let mut tokens = vec![0u64; count.div_ceil(step)];
    let mut active = vec![BTreeSet::new(); tokens.len()];
    let mut has_activity = false;
    for (index, session) in rows.iter().enumerate() {
        for (&timestamp, &amount) in &session.activity {
            let Some(time) = chrono::DateTime::from_timestamp(timestamp, 0) else {
                continue;
            };
            let local = time.with_timezone(&zone);
            let day = local.date_naive();
            if day < start || day > end {
                continue;
            }
            let bin = if hourly {
                local.hour() as usize
            } else {
                (day - start).num_days() as usize
            };
            if bin >= count {
                continue;
            }
            tokens[bin / step] = tokens[bin / step].saturating_add(amount.max(0) as u64);
            active[bin / step].insert(index);
            has_activity = true;
        }
    }
    let partial = rows
        .iter()
        .filter(|s| {
            !s.tokens.known || s.incomplete || (s.tokens.total() > 0 && s.activity.is_empty())
        })
        .count();
    SessionSeries {
        axis: Series {
            bins: vec![],
            start,
            hourly,
            step,
            first: if hourly {
                "00:00".into()
            } else {
                start.format("%m/%d").to_string()
            },
            last: if hourly {
                format!("{:02}:00", hours - 1)
            } else {
                end.format("%m/%d").to_string()
            },
        },
        tokens,
        active,
        partial,
        has_activity,
    }
}

pub(super) fn session_charts(
    rows: &[&crate::sessions::Session],
    snapshot: &Snapshot,
    page: &UsagePage,
    width: u16,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let series = session_series(rows, snapshot, page);
    (
        session_lines(rows, &series, true, width),
        session_lines(rows, &series, false, width),
    )
}

fn session_lines(
    rows: &[&crate::sessions::Session],
    series: &SessionSeries,
    tokens: bool,
    width: u16,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return vec![Line::styled(
            "No matching sessions",
            Style::default().fg(MUTED),
        )];
    }
    if !series.has_activity && series.partial > 0 {
        return vec![
            Line::styled(
                "Activity timestamps unavailable",
                Style::default().fg(WARNING),
            ),
            Line::styled(
                "See lifetime totals in the list",
                Style::default().fg(MUTED),
            ),
        ];
    }
    let group = series
        .tokens
        .len()
        .div_ceil(usize::from(width / 2).max(1))
        .max(1);
    let values: Vec<_> = if tokens {
        series
            .tokens
            .chunks(group)
            .map(|chunk| Some(chunk.iter().copied().fold(0u64, u64::saturating_add)))
            .collect()
    } else {
        series
            .active
            .chunks(group)
            .map(|chunk| {
                let unique: BTreeSet<_> =
                    chunk.iter().flat_map(|set| set.iter().copied()).collect();
                Some(unique.len() as u64)
            })
            .collect()
    };
    let mut lines = plot(&series.axis, &values, group, tokens, width);
    if !tokens {
        lines[0].spans[0].content = if width >= 40 {
            "ACTIVE SESSIONS"
        } else {
            "SESSIONS"
        }
        .into();
    }
    let note = if series.partial > 0 {
        format!("Partial activity · {} sessions", series.partial)
    } else if tokens {
        "Recorded tokens · selected period".into()
    } else {
        "Unique sessions · not gateway calls".into()
    };
    if let Some(last) = lines.last_mut() {
        *last = Line::styled(
            note,
            Style::default().fg(if series.partial > 0 { WARNING } else { MUTED }),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bar_groups_preserve_totals_and_missing_usage() {
        let bins = vec![
            Totals {
                calls: 2,
                input: 10,
                output: 2,
                ..Default::default()
            },
            Totals {
                calls: 3,
                unknown: 1,
                ..Default::default()
            },
            Totals {
                calls: 4,
                ..Default::default()
            },
        ];
        let grouped = coarsen(&bins, 2);
        assert_eq!(grouped.len(), 2);
        assert_eq!(
            (grouped[0].calls, grouped[0].input, grouped[0].unknown),
            (5, 10, 1)
        );
        assert_eq!(grouped[1].calls, 4);
        assert_eq!(short_value(10000, 3), "10K");
    }
    #[test]
    fn ranges_and_chart_buckets_match_and_fill_quiet_days() {
        let (_temp, mut app) = crate::tui::tests::persisted_app();
        app.open_usage();
        let mut page = app.usage.page.take().unwrap();
        page.day = "2026-09-18".into();
        page.follow_today = false;
        let mut snapshot = Snapshot::default();
        for (day, hour, calls) in [
            ("2026-09-18", 3, 2),
            ("2026-09-18", 8, 3),
            ("2026-09-12", 7, 5),
            ("2026-09-11", 2, 7),
            ("2026-08-20", 3, 11),
            ("2026-08-19", 1, 13),
        ] {
            snapshot.rows.push(crate::usage::Row {
                day: day.into(),
                hour,
                client: "Claude".into(),
                provider: "p".into(),
                name: "P".into(),
                model: "m".into(),
                kind: "generation".into(),
                totals: Totals {
                    calls,
                    ..Default::default()
                },
            });
        }
        assert_eq!(
            page.range_total(&snapshot, None, None, "generation").calls,
            5
        );
        page.day = "2026-09-11".into();
        let hourly = series(&snapshot, &page);
        assert_eq!(hourly.bins.len(), 24);
        assert_eq!(hourly.bins[2].calls, 7);
        assert_eq!(hourly.bins[1].calls, 0);
        page.day = "2026-09-18".into();
        page.range = 1;
        assert_eq!(
            page.range_total(&snapshot, None, None, "generation").calls,
            10
        );
        let s = series(&snapshot, &page);
        assert_eq!(s.bins.len(), 7);
        assert_eq!(s.bins[0].calls, 5);
        assert_eq!(s.bins[1].calls, 0);
        assert_eq!(s.bins[6].calls, 5);
        page.range = 2;
        assert_eq!(
            page.range_total(&snapshot, None, None, "generation").calls,
            28
        );
        assert_eq!(series(&snapshot, &page).bins.len(), 30);
        page.range = 3;
        assert_eq!(
            page.range_total(&snapshot, None, None, "generation").calls,
            41
        );
        page.provider = Some("other".into());
        assert_eq!(
            page.range_total(&snapshot, None, Some("other"), "generation")
                .calls,
            0
        );
        assert!(series(&snapshot, &page).bins.iter().all(|b| b.calls == 0));
    }
    #[test]
    fn session_activity_uses_local_time_and_deduplicates_sessions_per_bar() {
        let (_temp, mut app) = crate::tui::tests::persisted_app();
        app.open_usage();
        let mut page = app.usage.page.take().unwrap();
        page.day = "2026-09-18".into();
        page.follow_today = false;
        let snapshot = Snapshot {
            offset: 8 * 3600,
            ..Default::default()
        };
        let stamp = |text: &str| {
            chrono::DateTime::parse_from_rfc3339(text)
                .unwrap()
                .timestamp()
        };
        let mut first = crate::sessions::Session {
            client: "Claude",
            id: "same-id".into(),
            tokens: crate::sessions::Tokens {
                input: 100,
                known: true,
                ..Default::default()
            },
            ..Default::default()
        };
        first.activity = [
            (stamp("2026-09-17T17:00:00Z"), 20),
            (stamp("2026-09-17T17:30:00Z"), 30),
            (stamp("2026-09-17T20:00:00Z"), 50),
            (stamp("2026-09-17T15:00:00Z"), 700),
        ]
        .into();
        let mut second = first.clone();
        second.client = "Codex";
        second.activity = [(stamp("2026-09-17T17:45:00Z"), 40)].into();
        let rows = [&first, &second];
        let series = session_series(&rows, &snapshot, &page);
        assert_eq!(series.tokens.len(), 24);
        assert_eq!(series.tokens[1], 90);
        assert_eq!(series.tokens[4], 50);
        assert_eq!(series.tokens[0], 0);
        assert_eq!(series.active[1].len(), 2);
        assert_eq!(series.active[4].len(), 1);
        let (tokens, active) = session_charts(&rows, &snapshot, &page, 8);
        assert!(tokens[0].to_string().contains("peak 140"));
        assert!(active[0].to_string().contains("peak 2"));
        // Daily buckets include activity on the previous day, without using lifetime totals.
        page.range = 1;
        let series = session_series(&rows, &snapshot, &page);
        assert_eq!(series.tokens.len(), 7);
        assert_eq!(series.tokens[5], 700);
        assert_eq!(series.tokens[6], 140);
        page.range = 3;
        assert_eq!(
            session_series(&rows, &snapshot, &page)
                .tokens
                .iter()
                .sum::<u64>(),
            840
        );
    }

    #[test]
    fn session_charts_explain_missing_partial_and_quiet_activity() {
        let (_temp, mut app) = crate::tui::tests::persisted_app();
        app.open_usage();
        let mut page = app.usage.page.take().unwrap();
        page.day = "2026-09-18".into();
        page.follow_today = false;
        let snapshot = Snapshot {
            offset: 0,
            ..Default::default()
        };
        let mut session = crate::sessions::Session {
            tokens: crate::sessions::Tokens {
                input: 100,
                known: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let (tokens, _) = session_charts(&[&session], &snapshot, &page, 36);
        assert!(tokens[0].to_string().contains("unavailable"));
        session.activity.insert(
            chrono::DateTime::parse_from_rfc3339("2026-09-18T01:00:00Z")
                .unwrap()
                .timestamp(),
            80,
        );
        session.incomplete = true;
        let (tokens, _) = session_charts(&[&session], &snapshot, &page, 36);
        assert!(
            tokens
                .last()
                .unwrap()
                .to_string()
                .contains("Partial activity")
        );
        session.incomplete = false;
        page.day = "2026-09-19".into();
        let (tokens, active) = session_charts(&[&session], &snapshot, &page, 36);
        assert!(tokens[0].to_string().contains("peak 0"));
        assert!(active[0].to_string().contains("peak 0"));
        assert!(!tokens.iter().any(|line| line.to_string().contains('█')));
    }
}
