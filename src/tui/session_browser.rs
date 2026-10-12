//! Shared presentation and ordering for the main TUI and Pulse session browsers.
use crate::sessions::Session;
use std::cmp::Ordering;

pub(super) fn identity(s: &Session) -> String {
    format!("{}\u{1f}{}", s.client, s.id)
}

pub(super) fn title(s: &Session) -> String {
    s.title.clone().unwrap_or_else(|| {
        let project = std::path::Path::new(&s.project)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        format!(
            "{} · {}",
            if project.is_empty() {
                "Unknown project"
            } else {
                &project
            },
            s.id.chars().take(8).collect::<String>()
        )
    })
}

pub(super) fn matches(s: &Session, query: &str) -> bool {
    let query = query.to_lowercase();
    query.is_empty()
        || s.title
            .as_ref()
            .is_some_and(|t| t.to_lowercase().contains(&query))
        || s.project.to_lowercase().contains(&query)
        || s.id.to_lowercase().contains(&query)
        || s.models.iter().any(|m| m.to_lowercase().contains(&query))
}

pub(super) fn compare(a: &Session, b: &Session, tokens: bool) -> Ordering {
    let order = if tokens {
        b.tokens
            .known
            .cmp(&a.tokens.known)
            .then_with(|| b.tokens.total().cmp(&a.tokens.total()))
            .then_with(|| b.updated.cmp(&a.updated))
    } else {
        b.updated.cmp(&a.updated)
    };
    order.then_with(|| (a.client, &a.id).cmp(&(b.client, &b.id)))
}

/// Keep the typed end of long Unicode queries and the cursor visible.
pub(super) fn query_tail(query: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut used = 0;
    query
        .chars()
        .rev()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= width
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
