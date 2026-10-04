//! Messages/Chat/Responses conversion and stream state.
use super::*;

pub(crate) fn completion_endpoint(base: &str, format: ApiFormat) -> Result<Url> {
    let wanted = match format {
        ApiFormat::OpenaiChat => "chat/completions",
        ApiFormat::OpenaiResponses => "responses",
        ApiFormat::Anthropic => bail!("Anthropic routes do not use the local proxy"),
    };
    api_endpoint(base, wanted)
}

pub fn models_endpoint(base: &str, _format: ApiFormat) -> Result<Url> {
    let mut endpoint = api_endpoint(base, "models")?;
    if endpoint.host_str() == Some("api.deepseek.com") {
        endpoint.set_path("/models");
    }
    Ok(endpoint)
}

pub(crate) fn api_endpoint(base: &str, wanted: &str) -> Result<Url> {
    let mut url = Url::parse(base).context("API URL is invalid")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("API URL must use http or https");
    }
    let path = url.path().trim_end_matches('/');
    let known = ["/chat/completions", "/responses", "/messages", "/models"];
    let explicit_endpoint = known.iter().any(|suffix| path.ends_with(suffix));
    let root = known
        .iter()
        .find_map(|suffix| path.strip_suffix(suffix))
        .unwrap_or(path);
    let next = if explicit_endpoint || root.ends_with("/v1") {
        format!("{root}/{wanted}")
    } else {
        format!("{root}/v1/{wanted}")
    };
    url.set_path(&next);
    Ok(url)
}

pub(super) fn apply_model_limits(profile: &Profile, model: &str, body: &mut Value) -> Result<()> {
    let Some(entry) = profile
        .models
        .iter()
        .find(|entry| config::canonical_model_id(&entry.id) == config::canonical_model_id(model))
    else {
        return Ok(());
    };
    entry.validate()?;
    if let Some(limit) = entry.max_output_tokens {
        let requested = match body.get("max_tokens") {
            Some(value) => value
                .as_u64()
                .filter(|n| *n > 0)
                .context("max_tokens must be a positive integer")?,
            None => u64::from(limit),
        };
        let effective = requested.min(u64::from(limit));
        if body["thinking"]["budget_tokens"]
            .as_u64()
            .is_some_and(|budget| budget >= effective)
        {
            bail!(
                "configured output limit conflicts with thinking budget_tokens; increase output limit or reduce the request budget"
            );
        }
        body["max_tokens"] = json!(effective);
    }
    Ok(())
}

pub(super) fn translate_request(input: &Value, format: ApiFormat) -> Result<Value> {
    translate_request_with_effort(input, format, "high")
}

pub(super) fn translate_request_with_effort(
    input: &Value,
    format: ApiFormat,
    maximum: &str,
) -> Result<Value> {
    if format == ApiFormat::Anthropic {
        return Ok(input.clone());
    }
    let normalized = normalize_client_tool_search(input)?;
    let input = &normalized;
    let object = input
        .as_object()
        .context("request body must be an object")?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .context("model is required")?;
    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .context("messages must be an array")?;
    let system = system_text(object.get("system"))?;
    let mut result = Map::new();
    result.insert("model".into(), Value::String(strip_1m(model)));
    match format {
        ApiFormat::OpenaiChat => {
            let mut converted = Vec::new();
            if object.get("stream").and_then(Value::as_bool) == Some(true) {
                result.insert("stream_options".into(), json!({"include_usage":true}));
            }
            if !system.is_empty() {
                converted.push(json!({"role":"system","content":system}));
            }
            for message in messages {
                converted.extend(chat_messages(message)?);
            }
            result.insert("messages".into(), Value::Array(converted));
            copy_number(object, &mut result, "max_tokens", "max_tokens");
        }
        ApiFormat::OpenaiResponses => {
            if !system.is_empty() {
                result.insert("instructions".into(), Value::String(system));
            }
            let mut converted = Vec::new();
            for message in messages {
                converted.extend(response_items(message)?);
            }
            result.insert("input".into(), Value::Array(converted));
            result.insert("store".into(), Value::Bool(false));
            copy_number(object, &mut result, "max_tokens", "max_output_tokens");
        }
        ApiFormat::Anthropic => unreachable!("handled before translation"),
    }
    if let Some(effort) = mapped_effort(input, maximum)? {
        match format {
            ApiFormat::OpenaiChat => {
                result.insert("reasoning_effort".into(), json!(effort));
            }
            ApiFormat::OpenaiResponses => {
                result.insert(
                    "reasoning".into(),
                    json!({"effort":effort,"summary":"auto"}),
                );
            }
            ApiFormat::Anthropic => unreachable!(),
        }
    }
    for key in ["temperature", "top_p"] {
        if let Some(value) = object.get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    if let Some(stop) = object.get("stop_sequences") {
        result.insert("stop".into(), stop.clone());
    }
    if let Some(stream) = object.get("stream") {
        result.insert("stream".into(), stream.clone());
    }
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        result.insert(
            "tools".into(),
            Value::Array(
                tools
                    .iter()
                    .map(|tool| convert_tool(tool, format))
                    .collect::<Result<Vec<_>>>()?,
            ),
        );
    }
    if let Some(choice) = object.get("tool_choice") {
        result.insert("tool_choice".into(), convert_tool_choice(choice, format)?);
    }
    Ok(Value::Object(result))
}

pub(super) fn copy_number(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    from: &str,
    to: &str,
) {
    if let Some(value) = source.get(from).filter(|value| value.is_number()) {
        target.insert(to.into(), value.clone());
    }
}

pub(super) fn system_text(value: Option<&Value>) -> Result<String> {
    match value {
        None => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(blocks)) => {
            let mut text = Vec::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    bail!("unsupported system content block");
                }
                text.push(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .context("system text block has no text")?,
                );
            }
            Ok(text.join("\n"))
        }
        Some(_) => bail!("system must be a string or text block array"),
    }
}

pub(super) fn content_blocks(message: &Value) -> Result<Vec<Value>> {
    match message.get("content") {
        Some(Value::String(text)) => Ok(vec![json!({"type":"text","text":text})]),
        Some(Value::Array(blocks)) => Ok(blocks.clone()),
        _ => bail!("message content must be a string or array"),
    }
}

pub(super) fn chat_messages(message: &Value) -> Result<Vec<Value>> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .context("message role is required")?;
    let blocks = content_blocks(message)?;
    if role == "system" || role == "developer" {
        let text = text_only_blocks(&blocks, role)?;
        return Ok(vec![json!({"role":role,"content":text})]);
    }
    if role == "assistant" {
        let mut text = String::new();
        let mut calls = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
                Some("tool_use") => calls.push(json!({
                    "id": block.get("id").and_then(Value::as_str).context("tool_use has no id")?,
                    "type":"function",
                    "function":{
                        "name":block.get("name").and_then(Value::as_str).context("tool_use has no name")?,
                        "arguments":serde_json::to_string(block.get("input").unwrap_or(&json!({})))?
                    }
                })),
                Some("thinking" | "redacted_thinking") => {}
                Some(other) => bail!("unsupported assistant content block: {other}"),
                None => bail!("content block has no type"),
            }
        }
        let mut row = Map::from_iter([
            ("role".into(), Value::String("assistant".into())),
            (
                "content".into(),
                if text.is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                },
            ),
        ]);
        if !calls.is_empty() {
            row.insert("tool_calls".into(), Value::Array(calls));
        }
        return Ok(vec![Value::Object(row)]);
    }
    if role != "user" {
        bail!("unsupported message role: {role}");
    }
    let mut result = Vec::new();
    let mut content = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => content.push(json!({"type":"text","text":block.get("text").and_then(Value::as_str).unwrap_or("")})),
            Some("image") => content.push(chat_image(&block)?),
            Some("tool_result") => result.push(json!({
                "role":"tool",
                "tool_call_id":block.get("tool_use_id").and_then(Value::as_str).context("tool_result has no tool_use_id")?,
                "content":formatted_tool_result(&block)?
            })),
            Some(other) => bail!("unsupported user content block: {other}"),
            None => bail!("content block has no type"),
        }
    }
    if !content.is_empty() {
        result.push(json!({"role":"user","content":content}));
    }
    Ok(result)
}

pub(super) fn chat_image(block: &Value) -> Result<Value> {
    let source = block.get("source").context("image has no source")?;
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => format!(
            "data:{};base64,{}",
            source
                .get("media_type")
                .and_then(Value::as_str)
                .context("image has no media_type")?,
            source
                .get("data")
                .and_then(Value::as_str)
                .context("image has no data")?
        ),
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .context("image has no URL")?
            .to_owned(),
        Some(other) => bail!("unsupported image source: {other}"),
        None => bail!("image source has no type"),
    };
    Ok(json!({"type":"image_url","image_url":{"url":url}}))
}

pub(super) fn response_items(message: &Value) -> Result<Vec<Value>> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .context("message role is required")?;
    let blocks = content_blocks(message)?;
    let mut result = Vec::new();
    let mut content = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => content.push(json!({
                "type":if role == "assistant" {"output_text"} else {"input_text"},
                "text":block.get("text").and_then(Value::as_str).unwrap_or("")
            })),
            Some("image") if role == "user" => {
                let image = chat_image(&block)?;
                content.push(json!({"type":"input_image","image_url":image["image_url"]["url"]}));
            }
            Some("tool_use") if role == "assistant" => result.push(json!({
                "type":"function_call",
                "call_id":block.get("id").and_then(Value::as_str).context("tool_use has no id")?,
                "name":block.get("name").and_then(Value::as_str).context("tool_use has no name")?,
                "arguments":serde_json::to_string(block.get("input").unwrap_or(&json!({})))?
            })),
            Some("tool_result") if role == "user" => result.push(json!({
                "type":"function_call_output",
                "call_id":block.get("tool_use_id").and_then(Value::as_str).context("tool_result has no tool_use_id")?,
                "output":formatted_tool_result(&block)?
            })),
            Some("thinking" | "redacted_thinking") if role == "assistant" => {}
            Some(other) => bail!("unsupported {role} content block: {other}"),
            None => bail!("content block has no type"),
        }
    }
    if !content.is_empty() {
        result.insert(0, json!({"role":role,"content":content}));
    }
    Ok(result)
}

pub(super) fn text_only_blocks(blocks: &[Value], role: &str) -> Result<String> {
    let mut text = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            bail!("unsupported {role} content block");
        }
        text.push(
            block
                .get("text")
                .and_then(Value::as_str)
                .context("text content block has no text")?,
        );
    }
    Ok(text.join("\n"))
}

pub(super) fn formatted_tool_result(block: &Value) -> Result<String> {
    let text = flatten_tool_result(block.get("content"))?;
    Ok(if block["is_error"] == true {
        format!("Tool error: {text}")
    } else {
        text
    })
}

pub(super) fn flatten_tool_result(value: Option<&Value>) -> Result<String> {
    match value {
        None => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(blocks)) => {
            let mut result = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => result.push(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    ),
                    Some(other) => bail!("unsupported tool_result content block: {other}"),
                    None => bail!("tool_result content block has no type"),
                }
            }
            Ok(result.join("\n"))
        }
        Some(other) => Ok(other.to_string()),
    }
}

pub(super) fn convert_tool(tool: &Value, format: ApiFormat) -> Result<Value> {
    if tool.get("type").is_some() && tool.get("name").is_none() {
        bail!("server-side Anthropic tools cannot be forwarded");
    }
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .context("tool has no name")?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let schema = tool
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| json!({"type":"object"}));
    Ok(match format {
        ApiFormat::OpenaiChat => {
            json!({"type":"function","function":{"name":name,"description":description,"parameters":schema}})
        }
        ApiFormat::OpenaiResponses => {
            json!({"type":"function","name":name,"description":description,"parameters":schema})
        }
        ApiFormat::Anthropic => unreachable!(),
    })
}

pub(super) fn convert_tool_choice(choice: &Value, format: ApiFormat) -> Result<Value> {
    let kind = choice.get("type").and_then(Value::as_str).unwrap_or("auto");
    Ok(match kind {
        "auto" => Value::String("auto".into()),
        "none" => Value::String("none".into()),
        "any" => Value::String("required".into()),
        "tool" => {
            let name = choice
                .get("name")
                .and_then(Value::as_str)
                .context("tool choice has no name")?;
            if matches!(format, ApiFormat::OpenaiChat) {
                json!({"type":"function","function":{"name":name}})
            } else {
                json!({"type":"function","name":name})
            }
        }
        other => bail!("unsupported tool choice: {other}"),
    })
}

pub(super) fn translate_response(value: &Value, format: ApiFormat) -> Result<Value> {
    match format {
        ApiFormat::OpenaiChat => chat_response(value),
        ApiFormat::OpenaiResponses => responses_response(value),
        ApiFormat::Anthropic => bail!("Anthropic response does not need translation"),
    }
}

pub(super) fn chat_response(value: &Value) -> Result<Value> {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .context("chat response has no choice")?;
    let message = choice
        .get("message")
        .context("chat choice has no message")?;
    let mut content = Vec::new();
    if let Some(reasoning) = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        content.push(json!({"type":"thinking","thinking":reasoning,"signature":"mux-openai"}));
    }
    if let Some(text) = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        content.push(json!({"type":"text","text":text}));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let function = call.get("function").context("tool call has no function")?;
            let args = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            content.push(json!({
                "type":"tool_use",
                "id":call.get("id").and_then(Value::as_str).unwrap_or("call_mux"),
                "name":function.get("name").and_then(Value::as_str).context("tool call has no name")?,
                "input":serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({"_raw":args}))
            }));
        }
    }
    Ok(anthropic_message(
        value.get("id").and_then(Value::as_str),
        value.get("model").and_then(Value::as_str),
        content,
        stop_reason(choice.get("finish_reason").and_then(Value::as_str)),
        value.get("usage"),
    ))
}

pub(super) fn responses_response(value: &Value) -> Result<Value> {
    let mut content = Vec::new();
    for item in value
        .get("output")
        .and_then(Value::as_array)
        .context("Responses output is missing")?
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .unwrap_or(&Vec::new())
                {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => content.push(json!({"type":"text","text":part.get("text").and_then(Value::as_str).unwrap_or("")})),
                        Some("refusal") => content.push(json!({"type":"text","text":part.get("refusal").and_then(Value::as_str).unwrap_or("")})),
                        Some(other) => bail!("unsupported Responses content: {other}"),
                        None => {}
                    }
                }
            }
            Some("function_call") => {
                let args = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                content.push(json!({
                    "type":"tool_use",
                    "id":item.get("call_id").or_else(|| item.get("id")).and_then(Value::as_str).unwrap_or("call_mux"),
                    "name":item.get("name").and_then(Value::as_str).context("function call has no name")?,
                    "input":serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({"_raw":args}))
                }));
            }
            Some("reasoning") => {
                if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                    let text = summary
                        .iter()
                        .filter_map(|row| row.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        content.push(
                            json!({"type":"thinking","thinking":text,"signature":"mux-openai"}),
                        );
                    }
                }
            }
            Some(other) => bail!("unsupported Responses output item: {other}"),
            None => {}
        }
    }
    let has_tool = content
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"));
    let incomplete = value.get("status").and_then(Value::as_str) == Some("incomplete");
    Ok(anthropic_message(
        value.get("id").and_then(Value::as_str),
        value.get("model").and_then(Value::as_str),
        content,
        if has_tool {
            "tool_use"
        } else if incomplete {
            "max_tokens"
        } else {
            "end_turn"
        },
        value.get("usage"),
    ))
}

pub(super) fn anthropic_message(
    id: Option<&str>,
    model: Option<&str>,
    content: Vec<Value>,
    stop: &str,
    usage: Option<&Value>,
) -> Value {
    let input_tokens = usage
        .and_then(|u| u.get("prompt_tokens").or_else(|| u.get("input_tokens")))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| {
            u.get("completion_tokens")
                .or_else(|| u.get("output_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "id":id.unwrap_or("msg_mux"),"type":"message","role":"assistant",
        "model":model.unwrap_or("openai-compatible"),"content":content,
        "stop_reason":stop,"stop_sequence":null,
        "usage":{"input_tokens":input_tokens,"output_tokens":output_tokens}
    })
}

pub(super) fn stop_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("length") => "max_tokens",
        Some("tool_calls" | "function_call") => "tool_use",
        Some("content_filter") => "refusal",
        _ => "end_turn",
    }
}

#[derive(Default)]
pub(super) struct StreamState {
    pub(super) started: bool,
    pub(super) text_block: Option<usize>,
    pub(super) thinking_block: Option<usize>,
    pub(super) tools: HashMap<usize, usize>,
    pub(super) next_block: usize,
    pub(super) finish_reason: Option<String>,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) id: String,
    pub(super) model: String,
}

pub(super) fn stream_response(response: reqwest::Response, format: ApiFormat) -> Response {
    stream_with_idle(response, format, IDLE_TIMEOUT)
}
pub(super) fn stream_with_idle(
    response: reqwest::Response,
    format: ApiFormat,
    idle: Duration,
) -> Response {
    let mut upstream = response.bytes_stream();
    let output = async_stream::stream! {
        let mut decoder = Decoder::default();
        let mut state = StreamState::default();
        loop {
            let bytes = match tokio::time::timeout(idle, upstream.next()).await {
                Ok(Some(Ok(bytes))) => bytes,
                Ok(None) => { yield Ok::<Bytes, std::convert::Infallible>(Bytes::from(error_sse("upstream stream ended without completion"))); return; },
                _ => { yield Ok(Bytes::from(error_sse("upstream stream failed or idle timeout"))); return; }
            };
            let frames = match decoder.push(&bytes) {
                Ok(frames) => frames,
                Err(error) => { yield Ok(Bytes::from(error_sse(&error.to_string()))); return; }
            };
            for data in frames {
                let mut completed = data == "[DONE]";
                if !completed {
                    let value: Value = match serde_json::from_str(&data) {
                        Ok(value) => value,
                        Err(error) => { yield Ok(Bytes::from(error_sse(&format!("invalid upstream SSE JSON: {error}")))); return; }
                    };
                    if value.get("error").is_some() || value["type"] == "response.failed" || value["type"] == "error" {
                        yield Ok(Bytes::from(error_sse("upstream reported a stream error"))); return;
                    }
                    completed = value["type"] == "response.completed" || value["type"] == "response.incomplete";
                    for event in translate_stream_event(&value, format, &mut state) { yield Ok(Bytes::from(event)); }
                }
                if completed {
                    for event in finalize_stream(&mut state) { yield Ok(Bytes::from(event)); }
                    return;
                }
            }
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(output))
        .expect("valid streaming response")
}

pub(super) fn translate_stream_event(
    value: &Value,
    format: ApiFormat,
    state: &mut StreamState,
) -> Vec<String> {
    match format {
        ApiFormat::OpenaiChat => chat_stream_event(value, state),
        ApiFormat::OpenaiResponses => responses_stream_event(value, state),
        ApiFormat::Anthropic => vec![error_sse("invalid proxy stream format")],
    }
}

pub(super) fn ensure_start(
    state: &mut StreamState,
    id: Option<&str>,
    model: Option<&str>,
) -> Vec<String> {
    if state.started {
        return Vec::new();
    }
    state.started = true;
    state.id = id.unwrap_or("msg_mux_stream").to_owned();
    state.model = model.unwrap_or("openai-compatible").to_owned();
    vec![sse(
        "message_start",
        json!({"type":"message_start","message":{
            "id":state.id,"type":"message","role":"assistant","model":state.model,
            "content":[],"stop_reason":null,"stop_sequence":null,
            "usage":{"input_tokens":0,"output_tokens":0}
        }}),
    )]
}

pub(super) fn chat_stream_event(value: &Value, state: &mut StreamState) -> Vec<String> {
    let mut out = ensure_start(
        state,
        value.get("id").and_then(Value::as_str),
        value.get("model").and_then(Value::as_str),
    );
    if let Some(usage) = value.get("usage") {
        state.input_tokens = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(state.input_tokens);
        state.output_tokens = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(state.output_tokens);
    }
    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    else {
        return out;
    };
    let delta = choice.get("delta").unwrap_or(&Value::Null);
    if let Some(reasoning) = delta
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        let index = ensure_block(state, &mut out, BlockKind::Thinking, None, None);
        out.push(sse("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":reasoning}})));
    }
    if let Some(text) = delta
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        let index = ensure_block(state, &mut out, BlockKind::Text, None, None);
        out.push(sse("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}})));
    }
    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let tool_index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let function = call.get("function").unwrap_or(&Value::Null);
            let index = if let Some(index) = state.tools.get(&tool_index) {
                *index
            } else {
                let index = ensure_block(
                    state,
                    &mut out,
                    BlockKind::Tool(tool_index),
                    call.get("id").and_then(Value::as_str),
                    function.get("name").and_then(Value::as_str),
                );
                state.tools.insert(tool_index, index);
                index
            };
            if let Some(arguments) = function
                .get("arguments")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                out.push(sse("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":arguments}})));
            }
        }
    }
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        state.finish_reason = Some(stop_reason(Some(reason)).into());
    }
    out
}

pub(super) enum BlockKind {
    Text,
    Thinking,
    Tool(usize),
}

pub(super) fn ensure_block(
    state: &mut StreamState,
    out: &mut Vec<String>,
    kind: BlockKind,
    id: Option<&str>,
    name: Option<&str>,
) -> usize {
    match kind {
        BlockKind::Text if state.text_block.is_some() => return state.text_block.unwrap_or(0),
        BlockKind::Thinking if state.thinking_block.is_some() => {
            return state.thinking_block.unwrap_or(0);
        }
        BlockKind::Tool(tool) if state.tools.contains_key(&tool) => return state.tools[&tool],
        _ => {}
    }
    let index = state.next_block;
    state.next_block += 1;
    let block = match kind {
        BlockKind::Text => {
            state.text_block = Some(index);
            json!({"type":"text","text":""})
        }
        BlockKind::Thinking => {
            state.thinking_block = Some(index);
            json!({"type":"thinking","thinking":"","signature":"mux-openai"})
        }
        BlockKind::Tool(tool) => {
            state.tools.insert(tool, index);
            json!({"type":"tool_use","id":id.unwrap_or("call_mux"),"name":name.unwrap_or("tool"),"input":{}})
        }
    };
    out.push(sse(
        "content_block_start",
        json!({"type":"content_block_start","index":index,"content_block":block}),
    ));
    index
}

pub(super) fn responses_stream_event(value: &Value, state: &mut StreamState) -> Vec<String> {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let response = value.get("response").unwrap_or(value);
    let mut out = ensure_start(
        state,
        response.get("id").and_then(Value::as_str),
        response.get("model").and_then(Value::as_str),
    );
    match kind {
        "response.output_text.delta" => {
            let index = ensure_block(state, &mut out, BlockKind::Text, None, None);
            let text = value.get("delta").and_then(Value::as_str).unwrap_or("");
            out.push(sse("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}})));
        }
        "response.reasoning_summary_text.delta" => {
            let index = ensure_block(state, &mut out, BlockKind::Thinking, None, None);
            let text = value.get("delta").and_then(Value::as_str).unwrap_or("");
            out.push(sse("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":text}})));
        }
        "response.output_item.added" => {
            if let Some(item) = value.get("item")
                && item.get("type").and_then(Value::as_str) == Some("function_call")
            {
                let key = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                ensure_block(
                    state,
                    &mut out,
                    BlockKind::Tool(key),
                    item.get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str),
                    item.get("name").and_then(Value::as_str),
                );
            }
        }
        "response.function_call_arguments.delta" => {
            let key = value
                .get("output_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let index = if let Some(index) = state.tools.get(&key) {
                *index
            } else {
                ensure_block(
                    state,
                    &mut out,
                    BlockKind::Tool(key),
                    value.get("item_id").and_then(Value::as_str),
                    Some("tool"),
                )
            };
            let args = value.get("delta").and_then(Value::as_str).unwrap_or("");
            out.push(sse("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":args}})));
        }
        "response.completed" | "response.incomplete" => {
            let usage = response.get("usage").unwrap_or(&Value::Null);
            state.input_tokens = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            state.output_tokens = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            state.finish_reason = Some(
                if !state.tools.is_empty() {
                    "tool_use"
                } else if kind == "response.incomplete" {
                    "max_tokens"
                } else {
                    "end_turn"
                }
                .into(),
            );
        }
        "response.failed" => out.push(error_sse(
            response
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("upstream response failed"),
        )),
        _ => {}
    }
    out
}

pub(super) fn finalize_stream(state: &mut StreamState) -> Vec<String> {
    if !state.started {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut blocks = state
        .text_block
        .into_iter()
        .chain(state.thinking_block)
        .chain(state.tools.values().copied())
        .collect::<Vec<_>>();
    blocks.sort_unstable();
    blocks.dedup();
    for index in blocks {
        out.push(sse(
            "content_block_stop",
            json!({"type":"content_block_stop","index":index}),
        ));
    }
    out.push(sse("message_delta", json!({"type":"message_delta","delta":{"stop_reason":state.finish_reason.as_deref().unwrap_or("end_turn"),"stop_sequence":null},"usage":{"input_tokens":state.input_tokens,"output_tokens":state.output_tokens}})));
    out.push(sse("message_stop", json!({"type":"message_stop"})));
    state.started = false;
    out
}

pub(super) fn sse(event: &str, value: Value) -> String {
    format!("event: {event}\ndata: {value}\n\n")
}

pub(super) fn error_sse(message: &str) -> String {
    sse(
        "error",
        json!({"type":"error","error":{"type":"api_error","message":message}}),
    )
}

pub(super) fn mapped_effort(input: &Value, maximum: &str) -> Result<Option<String>> {
    if maximum == "off" {
        return Ok(None);
    }
    let levels = ["low", "medium", "high", "xhigh"];
    let limit = levels
        .iter()
        .position(|v| *v == maximum)
        .context("invalid model reasoning maximum")?;
    let requested = input
        .pointer("/output_config/effort")
        .or_else(|| input.pointer("/reasoning/effort"));
    let requested = if let Some(value) = requested {
        value.as_str().context("effort must be a string")?
    } else if input
        .get("thinking")
        .is_some_and(|v| v["type"] != "disabled")
    {
        "high"
    } else {
        return Ok(None);
    };
    if requested == "auto" {
        return Ok(None);
    }
    let index = if requested == "max" {
        limit
    } else {
        levels
            .iter()
            .position(|v| *v == requested)
            .context("unsupported effort level")?
            .min(limit)
    };
    Ok(Some(levels[index].into()))
}

/// Client-executed search remains a function call. OpenAI-compatible servers
/// receive the request's full tool catalog instead of Anthropic deferred loading.
pub(super) fn normalize_client_tool_search(input: &Value) -> Result<Value> {
    let mut output = input.clone();
    let mut names = std::collections::BTreeSet::new();
    if let Some(tools) = output.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if tool
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "custom")
            {
                bail!("server-side Anthropic tools are not supported by this OpenAI route");
            }
            let name = tool["name"]
                .as_str()
                .context("tool has no name")?
                .to_owned();
            if !names.insert(name) {
                bail!("duplicate tool name in request");
            }
            tool.as_object_mut()
                .context("tool must be an object")?
                .remove("defer_loading");
        }
    }
    if let Some(messages) = output.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
                for block in blocks {
                    if block["type"] != "tool_result" {
                        continue;
                    }
                    if let Some(results) = block.get_mut("content").and_then(Value::as_array_mut) {
                        for result in results {
                            if result["type"] == "tool_reference" {
                                let name = result["tool_name"]
                                    .as_str()
                                    .context("tool_reference has no tool_name")?;
                                if !names.contains(name) {
                                    bail!(
                                        "tool_reference {name} is missing from this request's tool catalog"
                                    );
                                }
                                *result =
                                    json!({"type":"text","text":format!("Available tool: {name}")});
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(output)
}
