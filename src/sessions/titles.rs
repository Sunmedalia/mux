//! Title metadata only. Never derive a display title from conversation bodies.
use super::*;
use rusqlite::{Connection, OpenFlags};

type Signature = Vec<Option<(u64, SystemTime)>>;
#[derive(Default)]
pub(super) struct Database {
    signature: Option<Signature>,
    names: BTreeMap<String, String>,
}
pub(super) fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(|value| {
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect::<String>()
        })
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
fn latest_database(home: &Path) -> Option<PathBuf> {
    fs::read_dir(home)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let version = name
                .to_str()?
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u32>()
                .ok()?;
            Some((version, entry.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}
fn signature(path: &Path) -> Signature {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    [path.to_path_buf(), PathBuf::from(wal)]
        .iter()
        .map(|path| {
            let meta = fs::metadata(path).ok()?;
            Some((meta.len(), meta.modified().ok()?))
        })
        .collect()
}
fn read_database(path: &Path) -> rusqlite::Result<BTreeMap<String, String>> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_millis(100))?;
    let mut schema = db.prepare("PRAGMA table_info(threads)")?;
    let columns = schema
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    if !columns.contains("id") {
        return Ok(BTreeMap::new());
    }
    let name = if columns.contains("name") {
        "name"
    } else {
        "NULL"
    };
    let title = if columns.contains("title") {
        "title"
    } else {
        "NULL"
    };
    let mut query = db.prepare(&format!("SELECT id, {name}, {title} FROM threads"))?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut names = BTreeMap::new();
    for row in rows {
        let (id, name, title) = row?;
        if let Some(title) = clean(name.as_deref()).or_else(|| clean(title.as_deref())) {
            names.insert(id, title);
        }
    }
    Ok(names)
}
impl Database {
    fn update(&mut self, path: &Path) {
        let next = signature(path);
        if self.signature.as_ref() == Some(&next) {
            return;
        }
        self.names.clear();
        self.signature = None;
        if let Ok(names) = read_database(path) {
            // Retry on the next check if a writer changed the database while reading.
            if next == signature(path) {
                self.signature = Some(next);
            }
            self.names = names;
        }
    }
}
#[derive(Default)]
pub(super) struct Summary {
    signature: Option<Option<(u64, SystemTime)>>,
    title: Option<String>,
}
impl Summary {
    fn update(&mut self, path: &Path) {
        let next = fs::metadata(path)
            .ok()
            .and_then(|meta| Some((meta.len(), meta.modified().ok()?)));
        if self.signature.as_ref() == Some(&next) {
            return;
        }
        self.title = None;
        self.signature = None;
        if next.is_none() {
            self.signature = Some(None);
            return;
        }
        if let Ok(file) = File::open(path)
            && let Ok(value) = serde_json::from_reader::<_, Value>(file.take(1024 * 1024))
        {
            self.title = clean(value.get("generated_title").and_then(Value::as_str));
            self.signature = Some(next);
        }
    }
}
impl Reader {
    /// Called on the session worker. Title failures never alter token health.
    pub fn refresh_titles(&mut self, snapshot: &mut Snapshot) -> bool {
        let mut databases = BTreeMap::new();
        let mut grok = BTreeMap::new();
        for (path, file) in &self.files {
            if file.session.client == "Codex" {
                if let Some(home) = path
                    .ancestors()
                    .find(|dir| {
                        dir.file_name()
                            .is_some_and(|name| name == "sessions" || name == "archived_sessions")
                    })
                    .and_then(Path::parent)
                {
                    databases.insert(home.to_path_buf(), latest_database(home));
                }
            } else if file.session.client == "Grok"
                && let Some(parent) = path.parent()
            {
                grok.insert(file.session.id.clone(), parent.join("summary.json"));
            }
        }
        let paths: BTreeSet<_> = databases.into_values().flatten().collect();
        self.title_databases.retain(|path, _| paths.contains(path));
        for path in paths {
            self.title_databases
                .entry(path.clone())
                .or_default()
                .update(&path);
        }
        let summaries: BTreeSet<_> = grok.values().cloned().collect();
        self.title_summaries
            .retain(|path, _| summaries.contains(path));
        for path in summaries {
            self.title_summaries
                .entry(path.clone())
                .or_default()
                .update(&path);
        }
        let mut changed = false;
        for row in &mut snapshot.rows {
            let title = match row.client {
                "Codex" => self
                    .title_databases
                    .values()
                    .find_map(|database| database.names.get(&row.id).cloned()),
                "Grok" => grok
                    .get(&row.id)
                    .and_then(|path| self.title_summaries.get(path))
                    .and_then(|summary| summary.title.clone()),
                _ => continue,
            };
            changed |= row.title != title;
            row.title = title;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn append(path: &Path, value: Value) {
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(path)
                .unwrap(),
            "{value}"
        )
        .unwrap();
    }
    fn codex_log(home: &Path) -> PathBuf {
        let root = home.join("sessions");
        fs::create_dir_all(&root).unwrap();
        append(
            &root.join("log.jsonl"),
            serde_json::json!({"type":"session_meta","payload":{"id":"thread","cwd":"/project"}}),
        );
        root
    }
    #[test]
    fn codex_native_name_precedes_title_and_wal_renames_refresh_without_token_reads() {
        let temp = tempfile::tempdir().unwrap();
        let root = codex_log(temp.path());
        let db = Connection::open(temp.path().join("state_7.sqlite")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT, name TEXT, title TEXT, first_user_message TEXT); INSERT INTO threads VALUES('thread','Custom name','Generated title','never read');").unwrap();
        let mut reader = Reader::default();
        let mut snapshot = reader.read(&[(root, false)]);
        assert_eq!(snapshot.rows[0].title.as_deref(), Some("Custom name"));
        let tokens = snapshot.rows[0].tokens.clone();
        assert!(!reader.refresh_titles(&mut snapshot));
        db.execute("UPDATE threads SET name='Renamed 中文'", [])
            .unwrap();
        assert!(reader.refresh_titles(&mut snapshot));
        assert_eq!(snapshot.rows[0].title.as_deref(), Some("Renamed 中文"));
        db.execute("UPDATE threads SET name='   '", []).unwrap();
        reader.refresh_titles(&mut snapshot);
        assert_eq!(snapshot.rows[0].title.as_deref(), Some("Generated title"));
        assert_eq!(snapshot.rows[0].tokens, tokens);
        db.execute("UPDATE threads SET title=NULL", []).unwrap();
        reader.refresh_titles(&mut snapshot);
        assert!(snapshot.rows[0].title.is_none());
    }
    #[test]
    fn codex_missing_columns_busy_corrupt_and_newer_database_fall_back_and_recover() {
        let temp = tempfile::tempdir().unwrap();
        let root = codex_log(temp.path());
        let db = Connection::open(temp.path().join("state_1.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads(id TEXT,title TEXT); INSERT INTO threads VALUES('thread','Legacy title')").unwrap();
        let mut reader = Reader::default();
        let mut snapshot = reader.read(&[(root, false)]);
        assert_eq!(snapshot.rows[0].title.as_deref(), Some("Legacy title"));
        db.execute_batch("BEGIN EXCLUSIVE; UPDATE threads SET title='Busy renamed'")
            .unwrap();
        // Force a check even when an exclusive transaction has not flushed to disk.
        reader
            .title_databases
            .values_mut()
            .for_each(|c| c.signature = None);
        reader.refresh_titles(&mut snapshot);
        assert!(snapshot.rows[0].title.is_none());
        assert_eq!(snapshot.warnings, 0);
        db.execute_batch("COMMIT").unwrap();
        reader.refresh_titles(&mut snapshot);
        assert_eq!(snapshot.rows[0].title.as_deref(), Some("Busy renamed"));
        let newer = temp.path().join("state_2.sqlite");
        fs::write(&newer, b"corrupt").unwrap();
        reader.refresh_titles(&mut snapshot);
        assert!(snapshot.rows[0].title.is_none());
        fs::remove_file(&newer).unwrap();
        let db2 = Connection::open(newer).unwrap();
        db2.execute_batch("CREATE TABLE threads(id TEXT); INSERT INTO threads VALUES('thread')")
            .unwrap();
        reader.refresh_titles(&mut snapshot);
        assert!(snapshot.rows[0].title.is_none());
        db2.execute_batch(
            "ALTER TABLE threads ADD COLUMN name TEXT; UPDATE threads SET name='Recovered'",
        )
        .unwrap();
        reader.refresh_titles(&mut snapshot);
        assert_eq!(snapshot.rows[0].title.as_deref(), Some("Recovered"));
    }
    #[test]
    fn claude_custom_titles_override_ai_titles_and_never_use_message_text() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        append(
            &path,
            serde_json::json!({"type":"user","sessionId":"main","message":{"content":"never a title"}}),
        );
        let mut reader = Reader::default();
        let roots = [(temp.path().to_path_buf(), true)];
        assert!(reader.read(&roots).rows[0].title.is_none());
        for (kind, field, title, expected) in [
            ("ai-title", "aiTitle", "AI title", "AI title"),
            ("custom-title", "customTitle", "Custom 中文", "Custom 中文"),
            ("ai-title", "aiTitle", "New AI", "Custom 中文"),
            ("custom-title", "customTitle", "", "New AI"),
        ] {
            append(&path, serde_json::json!({"type":kind,field:title}));
            assert_eq!(reader.read(&roots).rows[0].title.as_deref(), Some(expected));
        }
    }
    #[test]
    fn grok_summary_titles_refresh_fall_back_and_do_not_change_usage_health() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let session = root.join("project/session");
        fs::create_dir_all(&session).unwrap();
        append(&session.join("updates.jsonl"), serde_json::json!({}));
        let path = session.join("summary.json");
        fs::write(
            &path,
            r#"{"generated_title":"Grok title","session_summary":"never a title"}"#,
        )
        .unwrap();
        let mut reader = Reader::default();
        let mut snapshot = reader.read(&[(root, false)]);
        assert_eq!(snapshot.rows[0].title.as_deref(), Some("Grok title"));
        fs::write(&path, r#"{"generated_title":"Renamed"}"#).unwrap();
        reader.refresh_titles(&mut snapshot);
        assert_eq!(snapshot.rows[0].title.as_deref(), Some("Renamed"));
        fs::write(&path, b"invalid").unwrap();
        reader.refresh_titles(&mut snapshot);
        assert!(snapshot.rows[0].title.is_none());
        assert_eq!(snapshot.warnings, 0);
        fs::remove_file(&path).unwrap();
        assert!(!reader.refresh_titles(&mut snapshot));
        assert!(clean(Some(" \u{1b}中文\n title ")).as_deref() == Some("中文 title"));
    }
}
