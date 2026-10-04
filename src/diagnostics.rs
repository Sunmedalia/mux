//! Bounded upstream diagnostics. Successful conversation content is never redacted.
use crate::config::Credential;
use serde_json::Value;

pub const DETAIL_LIMIT: usize = 1024;

pub fn clean(text: &str, credential: &Credential, local_token: &str) -> String {
    let mut text = text.to_owned();
    for secret in [credential.value().unwrap_or_default(), local_token] {
        if !secret.is_empty() {
            text = text.replace(secret, "[redacted]");
        }
    }
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut end = text.len().min(DETAIL_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

pub fn detail(bytes: &[u8], credential: &Credential, local_token: &str) -> Option<String> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let error = value.get("error").unwrap_or(&value);
    let message = error.get("message")?.as_str()?;
    if message.trim().is_empty() {
        return None;
    }
    let text = match error
        .get("param")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(param) => format!("{param}: {message}"),
        None => message.to_owned(),
    };
    Some(clean(&text, credential, local_token))
}

pub fn redact(value: &mut Value, credential: &Credential, local_token: &str) {
    match value {
        Value::String(text) => *text = clean(text, credential, local_token),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|v| redact(v, credential, local_token)),
        Value::Object(fields) => {
            // Debug headers can contain credentials other than the configured key.
            fields.retain(|key, _| {
                !matches!(
                    key.to_ascii_lowercase().as_str(),
                    "headers"
                        | "authorization"
                        | "x-api-key"
                        | "api-key"
                        | "access_token"
                        | "refresh_token"
                )
            });
            fields
                .values_mut()
                .for_each(|v| redact(v, credential, local_token));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn errors_are_bounded_and_secrets_are_removed_before_truncation() {
        let key = Credential::Bearer {
            value: "upstream-secret".into(),
        };
        let bytes = serde_json::to_vec(&json!({"error":{"message":format!("upstream-secret local-secret\n{}", "中".repeat(1000)),"headers":{"Authorization":"another-secret"}}})).unwrap();
        let text = detail(&bytes, &key, "local-secret").unwrap();
        assert!(!text.contains("secret"));
        assert!(!text.contains('\n'));
        assert!(text.len() <= DETAIL_LIMIT);
        assert!(detail(b"<html>private debug page</html>", &key, "").is_none());
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        redact(&mut value, &key, "local-secret");
        assert!(value["error"].get("headers").is_none());
        assert!(!value.to_string().contains("secret"));
    }
}
