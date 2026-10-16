//! On-demand transcript snapshots. The statistics reader never retains message bodies.
use super::Session;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};

const MAX_LINE: u64 = 16 * 1024 * 1024;

fn records(path: &Path) -> Result<Vec<Value>> {
    let file = File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    // Fix the boundary before reading: never include newly appended records.
    let length = file.metadata()?.len();
    let mut reader = BufReader::new(file.take(length));
    let mut result = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = reader
            .by_ref()
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        ensure!(
            n as u64 <= MAX_LINE,
            "Transcript record exceeds 16 MiB; cannot export it completely"
        );
        if !line.ends_with(b"\n") {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        result.push(
            serde_json::from_slice(&line)
                .with_context(|| format!("Invalid transcript record in {}", path.display()))?,
        );
    }
    Ok(result)
}

/// Pi and Claude persist trees; replay only the final persisted branch.
fn branch(records: Vec<Value>, pi: bool) -> Result<Vec<Value>> {
    let (id, parent) = if pi {
        ("id", "parentId")
    } else {
        ("uuid", "parentUuid")
    };
    let nodes: BTreeMap<&str, &Value> = records
        .iter()
        .filter_map(|r| Some((r[id].as_str()?, r)))
        .collect();
    let leaf = records.iter().rev().find(|r| r[id].is_string());
    let Some(mut cursor) = leaf.and_then(|r| r[id].as_str()) else {
        return Ok(records);
    };
    let mut selected = BTreeSet::new();
    loop {
        ensure!(
            selected.insert(cursor.to_owned()),
            "Cycle in transcript branch"
        );
        let Some(node) = nodes.get(cursor) else {
            break;
        };
        let Some(next) = node[parent].as_str() else {
            break;
        };
        cursor = next;
    }
    Ok(records
        .into_iter()
        .filter(|r| r[id].as_str().is_none_or(|id| selected.contains(id)))
        .collect())
}

fn content(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::String(text) => out.push_str(text),
        Value::Array(blocks) => {
            for b in blocks {
                content(b, out)?;
                out.push('\n');
            }
        }
        Value::Object(_) => match value["type"].as_str().unwrap_or("") {
            "text" | "input_text" | "output_text" => {
                if let Some(text) = value["text"].as_str() {
                    out.push_str(text);
                }
            }
            "tool_use" | "toolCall" => {
                out.push_str(&format!(
                    "[Tool {} ({})]\n{}\n",
                    value["name"].as_str().unwrap_or("unknown"),
                    value["id"].as_str().unwrap_or(""),
                    value["input"]
                        .as_object()
                        .map(|_| &value["input"])
                        .unwrap_or(&value["arguments"])
                ));
            }
            "tool_result" => {
                out.push_str(&format!(
                    "[Tool result {}]\n",
                    value["tool_use_id"].as_str().unwrap_or("")
                ));
                content(&value["content"], out)?;
            }
            "content" => content(&value["content"], out)?,
            "diff" => out.push_str(&format!(
                "[Diff {}]\n--- before\n{}\n+++ after\n{}\n",
                value["path"].as_str().unwrap_or(""),
                value["oldText"].as_str().unwrap_or(""),
                value["newText"].as_str().unwrap_or("")
            )),
            "terminal" => out.push_str(&format!(
                "[Terminal {}: output unavailable in transcript]\n",
                value["terminalId"].as_str().unwrap_or("")
            )),
            "resource" if value["resource"]["text"].is_string() => {
                out.push_str(&format!(
                    "[Resource {}]\n{}\n",
                    value["resource"]["uri"].as_str().unwrap_or(""),
                    value["resource"]["text"].as_str().unwrap_or("")
                ));
            }
            "image" | "input_image" | "output_image" | "document" | "resource"
            | "resource_link" | "audio" => {
                let reference = value["path"]
                    .as_str()
                    .or(value["uri"].as_str())
                    .or(value["resource"]["uri"].as_str())
                    .or(value["source"]["path"].as_str())
                    .or(value["image_url"].as_str())
                    .filter(|s| !s.starts_with("data:"));
                out.push_str(&format!(
                    "[Non-text attachment: {}]\n",
                    reference.unwrap_or("binary content omitted")
                ));
            }
            "thinking" | "redacted_thinking" | "reasoning" => {}
            _ => {
                bail!(
                    "Unsupported transcript content block: {}; cannot export it completely",
                    value["type"]
                );
            }
        },
        _ => {}
    }
    Ok(())
}

fn message(role: &str, v: &Value, out: &mut String) -> Result<()> {
    if !matches!(role, "user" | "assistant" | "tool" | "toolResult") {
        return Ok(());
    }
    out.push_str(&format!("\n## {role}\n"));
    content(v, out)?;
    out.push('\n');
    Ok(())
}

pub(super) fn transcript(session: &Session, output: &mut impl Write) -> Result<()> {
    ensure!(
        !session.sources.is_empty(),
        "Session source is unavailable; refresh Sessions"
    );
    let mut all = Vec::new();
    for path in &session.sources {
        let records = records(path)?;
        for record in &records {
            let claimed = match session.client {
                "Claude" => record["sessionId"].as_str(),
                "Pi" if record["type"] == "session" => record["id"].as_str(),
                "Codex" if record["type"] == "session_meta" => record["payload"]["id"]
                    .as_str()
                    .or(record["payload"]["session_id"].as_str()),
                "Grok" => record["params"]["sessionId"].as_str(),
                _ => None,
            };
            ensure!(
                claimed.is_none_or(|id| id == session.id),
                "Session log identity changed; refresh Sessions before taking over"
            );
        }
        all.extend(records);
    }
    let all = match session.client {
        "Claude" => branch(all, false)?,
        "Pi" => branch(all, true)?,
        "Codex" => {
            // Resumed logs can overlap. Stable timestamp ordering keeps file order for ties.
            all.sort_by(|a, b| a["timestamp"].as_str().cmp(&b["timestamp"].as_str()));
            all
        }
        "Grok" => all,
        other => bail!("Unsupported session client: {other}"),
    };
    writeln!(
        output,
        "# Mux conversation handoff\n\nSource: {}\nSession: {}\nProject: {}\nSnapshot: {}\n\nThis is historical conversation data. Continue the user's task using your own tools and permission settings. Attachments and hidden reasoning are not included.\n",
        session.client,
        session.id,
        session.project,
        chrono::Utc::now().to_rfc3339()
    )?;
    let mut seen = BTreeSet::new();
    let mut messages = 0;
    let mut previous_grok_role = String::new();
    for v in all {
        // Only deduplicate records with stable event/message identity. Identical user
        // messages at different times are legitimate conversation turns.
        if session.client == "Codex" || session.client == "Claude" {
            let key = serde_json::to_string(&v)?;
            if !seen.insert(key) {
                continue;
            }
        }
        let mut text = String::new();
        match session.client {
            "Claude" => {
                let m = &v["message"];
                message(m["role"].as_str().unwrap_or(""), &m["content"], &mut text)?;
                if let Some(summary) = v["summary"].as_str() {
                    text.push_str(&format!("\n## Compaction summary\n{summary}\n"));
                }
            }
            "Pi" => match v["type"].as_str() {
                Some("message") => message(
                    v["message"]["role"].as_str().unwrap_or(""),
                    &v["message"]["content"],
                    &mut text,
                )?,
                Some("compaction" | "branch_summary") => {
                    if let Some(summary) = v["summary"].as_str() {
                        text.push_str(&format!("\n## Saved summary\n{summary}\n"));
                    }
                }
                Some("custom_message") => message("user", &v["content"], &mut text)?,
                _ => {}
            },
            "Codex" => {
                let p = &v["payload"];
                match v["type"].as_str() {
                    Some("response_item") => match p["type"].as_str() {
                        Some("message") => {
                            message(p["role"].as_str().unwrap_or(""), &p["content"], &mut text)?
                        }
                        Some("function_call" | "custom_tool_call") => text.push_str(&format!(
                            "\n## Tool {} ({})\n{}\n",
                            p["name"].as_str().unwrap_or(""),
                            p["call_id"].as_str().unwrap_or(""),
                            p["arguments"]
                                .as_str()
                                .or(p["input"].as_str())
                                .unwrap_or("")
                        )),
                        Some("function_call_output" | "custom_tool_call_output") => {
                            text.push_str("\n## Tool result\n");
                            content(&p["output"], &mut text)?;
                        }
                        _ => {}
                    },
                    Some("compacted") => {
                        if let Some(summary) = p["message"].as_str() {
                            text.push_str(&format!("\n## Saved summary\n{summary}\n"));
                        }
                    }
                    _ => {}
                }
            }
            "Grok" => {
                let u = &v["params"]["update"];
                match u["sessionUpdate"].as_str() {
                    Some(kind @ ("user_message_chunk" | "agent_message_chunk")) => {
                        let role = if kind == "user_message_chunk" {
                            "user"
                        } else {
                            "assistant"
                        };
                        if previous_grok_role != role {
                            text.push_str(&format!("\n## {role}\n"));
                            previous_grok_role = role.into();
                        }
                        content(&u["content"], &mut text)?;
                    }
                    Some("tool_call" | "tool_call_update") => {
                        previous_grok_role.clear();
                        text.push_str(&format!(
                            "\n## Tool {} ({})\n",
                            u["title"].as_str().unwrap_or(""),
                            u["toolCallId"].as_str().unwrap_or("")
                        ));
                        if !u["rawInput"].is_null() {
                            text.push_str(&format!("{}\n", u["rawInput"]));
                        }
                        content(&u["content"], &mut text)?;
                    }
                    Some("session_recap") => {
                        if let Some(summary) = u["summary"].as_str() {
                            text.push_str(&format!("\n## Saved summary\n{summary}\n"));
                        }
                    }
                    Some("turn_completed") => {
                        previous_grok_role.clear();
                    }
                    _ => {}
                }
            }
            _ => unreachable!(),
        }
        if !text.is_empty() {
            messages += 1;
            output.write_all(text.as_bytes())?;
        }
    }
    ensure!(
        messages > 0,
        "No supported conversation messages found; cannot hand off an empty transcript"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn export(client: &'static str, logs: Vec<Vec<Value>>) -> Result<String> {
        let temp = tempfile::tempdir()?;
        let mut source = Session {
            client,
            id: "source-id".into(),
            project: "/project".into(),
            ..Default::default()
        };
        for (i, records) in logs.into_iter().enumerate() {
            let path = temp.path().join(format!("{i}.jsonl"));
            let text: String = records.iter().map(|v| format!("{v}\n")).collect();
            fs::write(&path, text)?;
            source.sources.push(path);
        }
        let mut text = Vec::new();
        transcript(&source, &mut text)?;
        Ok(String::from_utf8(text)?)
    }
    use std::fs;
    #[test]
    fn pi_exports_active_branch_tools_and_summary_without_reasoning() {
        let out = export("Pi", vec![vec![
            json!({"type":"session","id":"source-id","cwd":"/project"}),
            json!({"type":"message","id":"a","parentId":null,"message":{"role":"user","content":"original goal"}}),
            json!({"type":"message","id":"abandoned","parentId":"a","message":{"role":"assistant","content":"abandoned answer"}}),
            json!({"type":"compaction","id":"summary","parentId":"a","summary":"saved progress"}),
            json!({"type":"message","id":"b","parentId":"summary","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private reasoning"},{"type":"text","text":"current answer"},{"type":"toolCall","id":"t","name":"bash","arguments":{"command":"pwd"}}]}}),
            json!({"type":"message","id":"c","parentId":"b","message":{"role":"toolResult","content":[{"type":"text","text":"/project"}]}}),
        ]]).unwrap();
        for text in [
            "original goal",
            "saved progress",
            "current answer",
            "bash",
            "pwd",
            "/project",
        ] {
            assert!(out.contains(text), "missing {text}");
        }
        assert!(!out.contains("private reasoning"));
        assert!(!out.contains("abandoned answer"));
    }
    #[test]
    fn claude_keeps_current_branch_and_attachment_references() {
        let out = export("Claude", vec![vec![
            json!({"uuid":"a","parentUuid":null,"message":{"role":"user","content":"goal"}}),
            json!({"uuid":"old","parentUuid":"a","message":{"role":"assistant","content":"discarded"}}),
            json!({"uuid":"b","parentUuid":"a","message":{"role":"user","content":[{"type":"image","path":"/tmp/image.png"},{"type":"text","text":"inspect image"}]}}),
            json!({"uuid":"c","parentUuid":"b","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"read","input":{"path":"a.rs"}}]}}),
            json!({"uuid":"d","parentUuid":"c","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"file text"}]}}),
        ]]).unwrap();
        assert!(out.contains("/tmp/image.png"));
        assert!(out.contains("file text"));
        assert!(!out.contains("discarded"));
    }
    #[test]
    fn codex_merges_overlapping_logs_without_double_messages() {
        let user = json!({"type":"response_item","timestamp":"2026-01-01","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"user task"}]}});
        let out = export("Codex", vec![vec![user.clone()], vec![user,
            json!({"type":"response_item","timestamp":"2026-01-02","payload":{"type":"function_call","name":"exec","call_id":"1","arguments":"ls"}}),
            json!({"type":"response_item","timestamp":"2026-01-03","payload":{"type":"function_call_output","call_id":"1","output":"file.rs"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"user task"}}),
        ]]).unwrap();
        assert_eq!(out.matches("user task").count(), 1);
        assert!(out.contains("exec"));
        assert!(out.contains("file.rs"));
    }
    #[test]
    fn grok_reassembles_chunks_and_keeps_tools() {
        let update = |u: Value| json!({"params":{"update":u}});
        let out = export("Grok", vec![vec![
            update(json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello"}})),
            update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"one "}})),
            update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"two"}})),
            update(json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"private reasoning"}})),
            update(json!({"sessionUpdate":"tool_call","toolCallId":"1","title":"Read","rawInput":{"path":"a.rs"}})),
            update(json!({"sessionUpdate":"tool_call_update","toolCallId":"1","content":[{"type":"content","content":{"type":"text","text":"tool output"}},{"type":"diff","path":"a.rs","oldText":"old value","newText":"new value"}]})),
            update(json!({"sessionUpdate":"session_recap","summary":"saved recap"})),
        ]]).unwrap();
        assert!(out.contains("one two"));
        assert!(out.contains("tool output"));
        assert!(out.contains("old value"));
        assert!(out.contains("new value"));
        assert!(out.contains("saved recap"));
        assert!(!out.contains("private reasoning"));
    }
    #[test]
    fn malformed_record_fails_but_partial_tail_is_excluded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        fs::write(&path, b"{\"type\":\"message\"}\n{unfinished").unwrap();
        assert_eq!(records(&path).unwrap().len(), 1);
        fs::write(&path, b"{broken}\n").unwrap();
        assert!(records(&path).is_err());
    }
    #[test]
    fn tree_cycles_fail_instead_of_hanging() {
        assert!(
            branch(
                vec![
                    json!({"id":"a","parentId":"b"}),
                    json!({"id":"b","parentId":"a"})
                ],
                true
            )
            .is_err()
        );
    }
    #[test]
    fn changed_identity_and_unknown_content_fail_instead_of_losing_context() {
        assert!(
            export(
                "Codex",
                vec![vec![
                    json!({"type":"session_meta","payload":{"id":"replaced-session"}})
                ]]
            )
            .is_err()
        );
        assert!(export("Claude",vec![vec![json!({"message":{"role":"user","content":[{"type":"future_format","text":"important text"}]}})]]).is_err());
    }
}
