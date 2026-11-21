//! Gemini API request → Anthropic Messages body, for every backend that does
//! not serve the genai wire natively (claude through the Agent SDK, codex,
//! grok, openrouter). The gemini group never comes here: it gets the body as
//! it arrived (`GeminiProvider::build_native_request`).
//!
//! Live ground truth (agy 1.5.13, research capture 2026-11-21): agy sends
//! `contents`, `systemInstruction`, `tools` (one `functionDeclarations` entry
//! per tool, OpenAPI-subset schemas with UPPERCASE types and int64 counts as
//! strings), `generationConfig` and `labels`; it echoes a model's
//! `functionCall` back WITHOUT its `id`, so tool ids are minted here.

use serde_json::{json, Map, Value};

use crate::routing::BackendGroup;

/// Schema keywords whose value is a count (int64): protojson renders them as
/// strings (`"minItems": "1"`), JSON Schema wants numbers.
const COUNT_KEYS: &[&str] = &[
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minProperties",
    "maxProperties",
];

/// Gemini-only schema keywords with no JSON Schema meaning.
const DROPPED_SCHEMA_KEYS: &[&str] = &["propertyOrdering", "nullable"];

/// One Messages block on its way into a role turn.
struct Pending {
    role: &'static str,
    block: Value,
}

/// The calls of the model turn a `functionResponse` answers: `(id, name,
/// answered)` in the order they were made.
#[derive(Default)]
struct OpenCalls(Vec<(String, String, bool)>);

impl OpenCalls {
    /// Pair a response with a call: its own `id` when it carries one that is
    /// open, else the first unanswered call of the same name.
    fn answer(&mut self, id: Option<&str>, name: &str) -> Option<String> {
        let slot = id
            .and_then(|id| {
                self.0
                    .iter()
                    .position(|(call, _, answered)| !answered && call == id)
            })
            .or_else(|| {
                self.0
                    .iter()
                    .position(|(_, call, answered)| !answered && call == name)
            })?;
        self.0[slot].2 = true;
        Some(self.0[slot].0.clone())
    }
}

/// `model` + genai `body` → an Anthropic Messages body, shaped for the
/// backend `served` that will take it. `Err` is a client-facing detail for a
/// local 400.
///
/// - `systemInstruction.parts[].text` → `system`.
/// - Roles `user` → user, `model` → assistant, dispatched per PART: a
///   `functionResponse` always becomes a user `tool_result` (agy may put one
///   under `model`), a `functionCall` an assistant `tool_use`. Consecutive
///   same-role blocks merge; `tool_result` blocks lead their user turn.
/// - Thought parts (`thought: true`) and `thoughtSignature`s are dropped:
///   foreign reasoning is never replayed to another vendor.
/// - Tool ids: a call's own `id` when present, else `toolu_agy_<content>_<part>`
///   (deterministic, so a replayed history keeps its ids); each
///   `functionResponse` pairs with the preceding model turn's calls by id,
///   then by name in order.
/// - `generationConfig`: `maxOutputTokens` → `max_tokens`; sampling knobs and
///   `stopSequences` carried except where `served` refuses them (they are
///   dropped there instead — agy users cannot set them); `thinkingLevel` →
///   `output_config.effort`; `includeThoughts` → adaptive thinking;
///   `thinkingBudget: 0` → no thinking.
pub(crate) fn messages_request(
    model: &str,
    body: &Value,
    served: BackendGroup,
) -> Result<Value, String> {
    let contents = body["contents"]
        .as_array()
        .ok_or("contents must be an array")?;
    let mut pending: Vec<Pending> = Vec::new();
    let mut open = OpenCalls::default();
    for (content_index, content) in contents.iter().enumerate() {
        let role = match content["role"].as_str().unwrap_or("user") {
            "user" => "user",
            "model" | "assistant" => "assistant",
            other => {
                return Err(format!(
                    "contents[{content_index}].role `{other}` is not user or model"
                ))
            }
        };
        let parts = content["parts"]
            .as_array()
            .ok_or_else(|| format!("contents[{content_index}].parts must be an array"))?;
        for (part_index, part) in parts.iter().enumerate() {
            let path = format!("contents[{content_index}].parts[{part_index}]");
            if let Some(call) = part.get("functionCall") {
                let name = call["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| {
                        format!("{path}.functionCall.name must be a non-empty string")
                    })?;
                let args = match call.get("args") {
                    None | Some(Value::Null) => json!({}),
                    Some(args) if args.is_object() => args.clone(),
                    Some(_) => return Err(format!("{path}.functionCall.args must be an object")),
                };
                let id = call["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("toolu_agy_{content_index}_{part_index}"));
                // A call opens a new model step unless it continues one.
                if pending.last().is_none_or(|p| p.role != "assistant") {
                    open = OpenCalls::default();
                }
                open.0.push((id.clone(), name.to_string(), false));
                pending.push(Pending {
                    role: "assistant",
                    block: json!({"type": "tool_use", "id": id, "name": name, "input": args}),
                });
            } else if let Some(response) = part.get("functionResponse") {
                let name = response["name"]
                    .as_str()
                    .ok_or_else(|| format!("{path}.functionResponse.name must be a string"))?;
                let id = open
                    .answer(response["id"].as_str(), name)
                    .ok_or_else(|| {
                        format!("{path}: functionResponse `{name}` answers no functionCall of the preceding model turn")
                    })?;
                pending.push(Pending {
                    role: "user",
                    block: tool_result(&id, &response["response"]),
                });
            } else if part["thought"] == true {
                // Foreign reasoning never goes to another vendor.
            } else if let Some(text) = part["text"].as_str() {
                if !text.is_empty() {
                    pending.push(Pending {
                        role,
                        block: json!({"type": "text", "text": text}),
                    });
                }
            } else if let Some(data) = part.get("inlineData") {
                pending.push(Pending {
                    role,
                    block: inline_data(role, data),
                });
            } else if let Some(file) = part.get("fileData") {
                let uri = file["fileUri"].as_str().unwrap_or("");
                let mime = file["mimeType"].as_str().unwrap_or("unknown type");
                pending.push(Pending {
                    role,
                    block: json!({"type": "text", "text": format!("[file reference not carried: {uri} ({mime})]")}),
                });
            } else if part.get("thoughtSignature").is_some() {
                // A bare signature part (empty text) carries nothing here.
            } else {
                tracing::debug!(path = %path, "genai part with no Messages form dropped");
            }
        }
    }
    let messages = merge(pending);
    if messages.is_empty() {
        return Err("contents carry no message the backend can take".into());
    }

    let mut result = Map::new();
    result.insert("model".into(), json!(model));
    result.insert("messages".into(), Value::Array(messages));
    result.insert("stream".into(), json!(true));
    if let Some(system) = system_text(body.get("systemInstruction")) {
        result.insert("system".into(), json!(system));
    }
    let tools = tools(body.get("tools"))?;
    if !tools.is_empty() {
        result.insert("tools".into(), Value::Array(tools));
        if let Some(choice) = tool_choice(body.pointer("/toolConfig/functionCallingConfig"))? {
            result.insert("tool_choice".into(), choice);
        }
    }
    generation_config(body.get("generationConfig"), &mut result)?;
    if let Some(effort) = super::effort(body) {
        result.insert("output_config".into(), json!({"effort": effort}));
    }
    drop_unsupported(served, &mut result);
    Ok(Value::Object(result))
}

/// Merge consecutive same-role blocks into turns; within a user turn the
/// `tool_result` blocks go first (Anthropic requires them to lead).
fn merge(pending: Vec<Pending>) -> Vec<Value> {
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for Pending { role, block } in pending {
        match turns.last_mut() {
            Some((last, blocks)) if *last == role => blocks.push(block),
            _ => turns.push((role, vec![block])),
        }
    }
    turns
        .into_iter()
        .map(|(role, mut blocks)| {
            if role == "user" {
                blocks.sort_by_key(|block| block["type"] != "tool_result");
            }
            json!({"role": role, "content": blocks})
        })
        .collect()
}

/// `functionResponse.response` → a `tool_result`: `output` when it is a
/// string, else the JSON text; an `error` member marks it `is_error`.
fn tool_result(id: &str, response: &Value) -> Value {
    let (content, is_error) = match response.get("error").filter(|e| !e.is_null()) {
        Some(Value::String(error)) => (error.clone(), true),
        Some(error) => (error.to_string(), true),
        None => match response.get("output") {
            Some(Value::String(output)) => (output.clone(), false),
            _ if response.is_null() => (String::new(), false),
            _ => (response.to_string(), false),
        },
    };
    let mut block = json!({"type": "tool_result", "tool_use_id": id, "content": content});
    if is_error {
        block["is_error"] = json!(true);
    }
    block
}

/// `inlineData` → an image (`image/*`) or a PDF document (base64) on a user
/// turn; anything else, or media in a model turn, becomes a text note.
fn inline_data(role: &str, data: &Value) -> Value {
    let mime = data["mimeType"].as_str().unwrap_or("");
    let payload = data["data"].as_str().unwrap_or("");
    if role == "user" && !payload.is_empty() {
        let source = json!({"type": "base64", "media_type": mime, "data": payload});
        if mime.starts_with("image/") {
            return json!({"type": "image", "source": source});
        }
        if mime == "application/pdf" {
            return json!({"type": "document", "source": source});
        }
    }
    json!({"type": "text", "text": format!("[inline {mime} data not carried]")})
}

/// `systemInstruction` (a Content, or a plain string) → its text.
fn system_text(system: Option<&Value>) -> Option<String> {
    let text = match system? {
        Value::String(text) => text.clone(),
        content => content["parts"]
            .as_array()?
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    (!text.is_empty()).then_some(text)
}

/// `tools[].functionDeclarations[]` → Anthropic tools. `parametersJsonSchema`
/// passes through; `parameters` (the OpenAPI subset) is converted
/// ([`json_schema`]). Non-function tools (`googleSearch`, `codeExecution`, …)
/// have no client-side executor here and are dropped (debug log).
fn tools(tools: Option<&Value>) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for (index, tool) in tools
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(declarations) = tool.get("functionDeclarations") else {
            let kinds: Vec<&str> = tool
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            tracing::debug!(tool = index, ?kinds, "genai non-function tool dropped");
            continue;
        };
        let declarations = declarations
            .as_array()
            .ok_or_else(|| format!("tools[{index}].functionDeclarations must be an array"))?;
        for (inner, declaration) in declarations.iter().enumerate() {
            let path = format!("tools[{index}].functionDeclarations[{inner}]");
            let name = declaration["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| format!("{path}.name must be a non-empty string"))?;
            let schema = match (
                declaration
                    .get("parametersJsonSchema")
                    .filter(|s| !s.is_null()),
                declaration.get("parameters").filter(|s| !s.is_null()),
            ) {
                (Some(schema), _) => schema.clone(),
                (None, Some(schema)) => json_schema(schema),
                (None, None) => json!({"type": "object", "properties": {}}),
            };
            out.push(json!({
                "name": name,
                "description": declaration["description"].as_str().unwrap_or(""),
                "input_schema": schema,
            }));
        }
    }
    Ok(out)
}

/// A Gemini OpenAPI-subset Schema → JSON Schema: UPPERCASE `type` lowercased
/// (`TYPE_UNSPECIFIED` dropped), `nullable: true` → a `"null"` type member,
/// int64 counts given as strings → numbers, `format: "enum"` and
/// `propertyOrdering` dropped; `properties`, `items` and `anyOf` converted
/// recursively; every other keyword (`enum`, `format`, `description`,
/// `required`, `minimum`, …) kept.
pub(crate) fn json_schema(schema: &Value) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };
    let mut out = Map::new();
    for (key, value) in map {
        let key = key.as_str();
        if DROPPED_SCHEMA_KEYS.contains(&key) {
            continue;
        }
        let value = match key {
            "type" => match value.as_str().map(str::to_ascii_lowercase) {
                Some(kind) if kind == "type_unspecified" => continue,
                Some(kind) => json!(kind),
                None => value.clone(),
            },
            "format" if value == "enum" => continue,
            "properties" => match value {
                Value::Object(properties) => Value::Object(
                    properties
                        .iter()
                        .map(|(name, sub)| (name.clone(), json_schema(sub)))
                        .collect(),
                ),
                other => other.clone(),
            },
            "items" => json_schema(value),
            "anyOf" | "oneOf" | "allOf" => match value {
                Value::Array(schemas) => Value::Array(schemas.iter().map(json_schema).collect()),
                other => other.clone(),
            },
            key if COUNT_KEYS.contains(&key) => {
                match value.as_str().and_then(|s| s.parse::<u64>().ok()) {
                    Some(count) => json!(count),
                    None => value.clone(),
                }
            }
            _ => value.clone(),
        };
        out.insert(key.to_string(), value);
    }
    if map.get("nullable").and_then(Value::as_bool) == Some(true) {
        if let Some(kind) = out.get("type").and_then(Value::as_str).map(str::to_string) {
            out.insert("type".into(), json!([kind, "null"]));
        }
    }
    Value::Object(out)
}

/// `toolConfig.functionCallingConfig` → `tool_choice`: AUTO / VALIDATED →
/// auto, ANY → any (one `allowedFunctionNames` entry → that tool), NONE → none.
fn tool_choice(config: Option<&Value>) -> Result<Option<Value>, String> {
    let Some(config) = config.filter(|c| !c.is_null()) else {
        return Ok(None);
    };
    let allowed: Vec<&str> = config["allowedFunctionNames"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    match config["mode"]
        .as_str()
        .map(str::to_ascii_uppercase)
        .as_deref()
    {
        None | Some("MODE_UNSPECIFIED" | "AUTO" | "VALIDATED") => Ok(Some(json!({"type": "auto"}))),
        Some("ANY") if allowed.len() == 1 => Ok(Some(json!({"type": "tool", "name": allowed[0]}))),
        Some("ANY") => Ok(Some(json!({"type": "any"}))),
        Some("NONE") => Ok(Some(json!({"type": "none"}))),
        Some(other) => Err(format!(
            "toolConfig.functionCallingConfig.mode `{other}` is not AUTO, ANY or NONE"
        )),
    }
}

/// `generationConfig` → `max_tokens`, sampling knobs, `stop_sequences`, and
/// the thinking switch (`thinking: adaptive` when thoughts are asked for, no
/// thinking at all when the budget is 0).
fn generation_config(config: Option<&Value>, out: &mut Map<String, Value>) -> Result<(), String> {
    let Some(config) = config.filter(|c| !c.is_null()) else {
        return Ok(());
    };
    if let Some(cap) = config.get("maxOutputTokens").filter(|v| !v.is_null()) {
        let cap = cap
            .as_u64()
            .or_else(|| cap.as_str().and_then(|s| s.parse().ok()))
            .filter(|cap| *cap > 0)
            .ok_or("generationConfig.maxOutputTokens must be a positive integer")?;
        out.insert("max_tokens".into(), json!(cap));
    }
    for (from, to) in [
        ("temperature", "temperature"),
        ("topP", "top_p"),
        ("topK", "top_k"),
    ] {
        match config.get(from) {
            None | Some(Value::Null) => {}
            Some(value) if value.is_number() => {
                out.insert(to.into(), value.clone());
            }
            Some(_) => return Err(format!("generationConfig.{from} must be a number")),
        }
    }
    match config.get("stopSequences") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            if !items.is_empty() {
                out.insert("stop_sequences".into(), Value::Array(items.clone()));
            }
        }
        Some(_) => return Err("generationConfig.stopSequences must be an array of strings".into()),
    }
    let thinking = config.get("thinkingConfig").filter(|c| !c.is_null());
    let budget_off = thinking
        .and_then(|t| t.get("thinkingBudget"))
        .and_then(Value::as_i64)
        == Some(0);
    let wants_thoughts = thinking
        .and_then(|t| t.get("includeThoughts"))
        .and_then(Value::as_bool)
        == Some(true);
    if wants_thoughts && !budget_off {
        out.insert("thinking".into(), json!({"type": "adaptive"}));
    }
    Ok(())
}

/// Drop what `served` refuses but an agy user cannot have chosen: the Agent
/// SDK (claude) and the Responses backends (codex, grok) refuse sampling
/// knobs and stop sequences; the SDK takes only `auto` / `none` tool choice.
fn drop_unsupported(served: BackendGroup, body: &mut Map<String, Value>) {
    let refuses_sampling = matches!(
        served,
        BackendGroup::Claude | BackendGroup::Codex | BackendGroup::Grok
    );
    if refuses_sampling {
        let dropped: Vec<&str> = ["temperature", "top_p", "top_k", "stop_sequences"]
            .into_iter()
            .filter(|key| body.remove(*key).is_some())
            .collect();
        if !dropped.is_empty() {
            tracing::debug!(%served, ?dropped, "genai sampling controls the backend refuses were dropped");
        }
    }
    if served == BackendGroup::Claude
        && body
            .get("tool_choice")
            .is_some_and(|choice| !matches!(choice["type"].as_str(), Some("auto" | "none")))
    {
        tracing::debug!("genai forced tool choice dropped on the Agent SDK path (auto/none only)");
        body.remove("tool_choice");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(body: Value) -> Value {
        messages_request("claude-opus-5-5", &body, BackendGroup::OpenRouter).unwrap()
    }

    #[test]
    fn text_system_and_roles_translate() {
        let m = translate(json!({
            "systemInstruction": {"role": "user", "parts": [{"text": "be brief"}, {"text": "and kind"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{"text": "thinking…", "thought": true}, {"text": "hello"}, {"text": "", "thoughtSignature": "c2ln"}]},
                {"role": "user", "parts": [{"text": "again"}]},
                {"role": "user", "parts": [{"text": "merged"}]},
            ],
            "labels": {"cascade_id": "x"},
        }));
        assert_eq!(m["system"], "be brief\nand kind");
        assert_eq!(m["model"], "claude-opus-5-5");
        assert_eq!(m["stream"], true);
        assert_eq!(
            m["messages"],
            json!([
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
                {"role": "user", "content": [{"type": "text", "text": "again"}, {"type": "text", "text": "merged"}]},
            ])
        );
        assert!(m.get("labels").is_none());
    }

    /// The captured agy tool round trip: the call comes back WITHOUT its id,
    /// the response under `user` without one either — ids are minted and the
    /// pair still matches.
    #[test]
    fn tool_round_trip_without_ids_pairs_by_order_and_name() {
        let m = translate(json!({"contents": [
            {"role": "user", "parts": [{"text": "run two"}]},
            {"role": "model", "parts": [
                {"functionCall": {"name": "run_command", "args": {"CommandLine": "a"}}, "thoughtSignature": "c2ln"},
                {"functionCall": {"name": "read_file", "args": {"Path": "x"}}},
                {"functionCall": {"name": "run_command", "args": {"CommandLine": "b"}}},
            ]},
            {"role": "user", "parts": [
                {"functionResponse": {"name": "read_file", "response": {"output": "file"}}},
                {"functionResponse": {"name": "run_command", "response": {"output": "ran a"}}},
                {"functionResponse": {"name": "run_command", "response": {"error": "boom"}}},
                {"text": "and then?"},
            ]},
        ]}));
        let assistant = &m["messages"][1]["content"];
        assert_eq!(assistant[0]["id"], "toolu_agy_1_0");
        assert_eq!(assistant[1]["id"], "toolu_agy_1_1");
        assert_eq!(assistant[2]["id"], "toolu_agy_1_2");
        assert_eq!(assistant[0]["input"], json!({"CommandLine": "a"}));
        let user = &m["messages"][2]["content"];
        assert_eq!(user[0]["tool_use_id"], "toolu_agy_1_1");
        assert_eq!(user[0]["content"], "file");
        assert_eq!(user[1]["tool_use_id"], "toolu_agy_1_0");
        assert_eq!(user[1]["content"], "ran a");
        assert_eq!(user[2]["tool_use_id"], "toolu_agy_1_2");
        assert_eq!(user[2]["is_error"], true);
        assert_eq!(user[2]["content"], "boom");
        assert_eq!(user[3], json!({"type": "text", "text": "and then?"}));
        assert_eq!(m["messages"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn present_ids_win_and_a_function_response_under_model_moves_to_user() {
        let m = translate(json!({"contents": [
            {"role": "user", "parts": [{"text": "go"}]},
            {"role": "model", "parts": [
                {"functionCall": {"name": "f", "args": {}, "id": "call_9"}},
                {"functionResponse": {"name": "f", "id": "call_9", "response": {"output": {"k": 1}}}},
            ]},
            {"role": "user", "parts": [{"text": "next"}]},
        ]}));
        assert_eq!(m["messages"][1]["content"][0]["id"], "call_9");
        assert_eq!(m["messages"][2]["role"], "user");
        let result = &m["messages"][2]["content"][0];
        assert_eq!(result["tool_use_id"], "call_9");
        assert_eq!(result["content"], r#"{"output":{"k":1}}"#);
        assert_eq!(m["messages"][2]["content"][1]["text"], "next");
    }

    #[test]
    fn an_unanswerable_function_response_is_a_400() {
        let err = messages_request(
            "m",
            &json!({"contents": [
                {"role": "user", "parts": [{"functionResponse": {"name": "f", "response": {}}}]},
            ]}),
            BackendGroup::Codex,
        )
        .unwrap_err();
        assert!(err.contains("answers no functionCall"), "{err}");
    }

    #[test]
    fn inline_and_file_data_become_media_or_notes() {
        let m = translate(json!({"contents": [
            {"role": "user", "parts": [
                {"inlineData": {"mimeType": "image/png", "data": "iVBOR"}},
                {"inlineData": {"mimeType": "application/pdf", "data": "JVBER"}},
                {"inlineData": {"mimeType": "audio/wav", "data": "UklGR"}},
                {"fileData": {"mimeType": "video/mp4", "fileUri": "gs://b/v.mp4"}},
            ]},
        ]}));
        let blocks = &m["messages"][0]["content"];
        assert_eq!(
            blocks[0],
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBOR"}})
        );
        assert_eq!(blocks[1]["type"], "document");
        assert_eq!(blocks[1]["source"]["media_type"], "application/pdf");
        assert_eq!(blocks[2]["text"], "[inline audio/wav data not carried]");
        assert!(blocks[3]["text"].as_str().unwrap().contains("gs://b/v.mp4"));
    }

    /// The captured agy declaration shape: UPPERCASE types, counts as strings.
    #[test]
    fn openapi_schemas_convert_to_json_schema() {
        let m = translate(json!({
            "contents": [{"role": "user", "parts": [{"text": "x"}]}],
            "tools": [
                {"functionDeclarations": [{"name": "run_command", "description": "Run", "parameters": {
                    "type": "OBJECT",
                    "properties": {
                        "CommandLine": {"type": "STRING", "description": "cmd", "minLength": "1"},
                        "Mode": {"type": "STRING", "format": "enum", "enum": ["a", "b"]},
                        "Paths": {"type": "ARRAY", "minItems": "2", "items": {"type": "STRING"}},
                        "Wait": {"type": "INTEGER", "minimum": 1, "nullable": true},
                        "Either": {"anyOf": [{"type": "NUMBER"}, {"type": "BOOLEAN"}]},
                    },
                    "required": ["CommandLine"],
                    "propertyOrdering": ["CommandLine"],
                }}]},
                {"functionDeclarations": [{"name": "raw", "parametersJsonSchema": {"type": "object", "properties": {"q": {"type": "string"}}}}]},
                {"functionDeclarations": [{"name": "bare"}]},
                {"googleSearch": {}},
            ],
        }));
        let tools = m["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3, "googleSearch dropped");
        assert_eq!(
            tools[0]["input_schema"],
            json!({
                "type": "object",
                "properties": {
                    "CommandLine": {"type": "string", "description": "cmd", "minLength": 1},
                    "Mode": {"type": "string", "enum": ["a", "b"]},
                    "Paths": {"type": "array", "minItems": 2, "items": {"type": "string"}},
                    "Wait": {"type": ["integer", "null"], "minimum": 1},
                    "Either": {"anyOf": [{"type": "number"}, {"type": "boolean"}]},
                },
                "required": ["CommandLine"],
            })
        );
        assert_eq!(tools[0]["description"], "Run");
        assert_eq!(
            tools[1]["input_schema"]["properties"]["q"]["type"],
            "string"
        );
        assert_eq!(
            tools[2]["input_schema"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn tool_config_maps_to_tool_choice() {
        let body = |config: Value| {
            json!({
                "contents": [{"role": "user", "parts": [{"text": "x"}]}],
                "tools": [{"functionDeclarations": [{"name": "f"}]}],
                "toolConfig": {"functionCallingConfig": config},
            })
        };
        for (config, choice) in [
            (json!({"mode": "AUTO"}), json!({"type": "auto"})),
            (json!({"mode": "ANY"}), json!({"type": "any"})),
            (
                json!({"mode": "ANY", "allowedFunctionNames": ["f"]}),
                json!({"type": "tool", "name": "f"}),
            ),
            (json!({"mode": "NONE"}), json!({"type": "none"})),
        ] {
            assert_eq!(translate(body(config))["tool_choice"], choice);
        }
        // The Agent SDK takes auto / none only: a forced choice is dropped there.
        let sdk =
            messages_request("m", &body(json!({"mode": "ANY"})), BackendGroup::Claude).unwrap();
        assert!(sdk.get("tool_choice").is_none());
        assert!(
            messages_request("m", &body(json!({"mode": "SOMETIMES"})), BackendGroup::Grok).is_err()
        );
    }

    #[test]
    fn generation_config_maps_caps_sampling_and_thinking() {
        let body = |thinking: Value| {
            json!({
                "contents": [{"role": "user", "parts": [{"text": "x"}]}],
                "generationConfig": {
                    "maxOutputTokens": 64000, "temperature": 0.2, "topP": 0.9, "topK": 40,
                    "stopSequences": ["END"], "thinkingConfig": thinking,
                },
            })
        };
        let m = translate(body(
            json!({"includeThoughts": true, "thinkingLevel": "LOW"}),
        ));
        assert_eq!(m["max_tokens"], 64000);
        assert_eq!(m["temperature"], 0.2);
        assert_eq!(m["top_p"], 0.9);
        assert_eq!(m["top_k"], 40);
        assert_eq!(m["stop_sequences"], json!(["END"]));
        assert_eq!(m["output_config"], json!({"effort": "low"}));
        assert_eq!(m["thinking"], json!({"type": "adaptive"}));

        // agy's default for a thinking row: thoughts on, no effort.
        let m = translate(body(
            json!({"includeThoughts": true, "thinkingBudget": 1024}),
        ));
        assert_eq!(m["thinking"], json!({"type": "adaptive"}));
        assert!(m.get("output_config").is_none());
        // agy's form for a row without thinking: none at all.
        let m = translate(body(json!({"includeThoughts": false, "thinkingBudget": 0})));
        assert!(m.get("thinking").is_none());
        assert!(m.get("output_config").is_none());

        // The SDK and the Responses backends refuse sampling knobs: dropped.
        for served in [
            BackendGroup::Claude,
            BackendGroup::Codex,
            BackendGroup::Grok,
        ] {
            let m = messages_request("m", &body(json!({})), served).unwrap();
            for key in ["temperature", "top_p", "top_k", "stop_sequences"] {
                assert!(m.get(key).is_none(), "{served}: {key}");
            }
            assert_eq!(m["max_tokens"], 64000);
        }
        let bad = json!({"contents": [{"parts": [{"text": "x"}]}], "generationConfig": {"maxOutputTokens": 0}});
        assert!(messages_request("m", &bad, BackendGroup::Codex).is_err());
    }
}
