//! OpenAI Responses ingress and the Messages -> Responses return boundary.
//! Native Codex payloads stay native; only Claude/other Messages backends translate.
use std::collections::{BTreeMap, HashSet};

use axum::{body::Body, response::Response};
use bytes::Bytes;
use http::{header, StatusCode};
use serde_json::{json, Value};
use tokio_stream::StreamExt;

use super::sse::EventBuffer;

pub fn is_responses_path(path: &str) -> bool {
    matches!(path.split('?').next(), Some("/v1/responses" | "/responses"))
}

pub fn validate(body: &[u8]) -> Result<Value, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "body must be a JSON object")?;
    if !v.is_object() {
        return Err("body must be a JSON object".into());
    }
    if !v
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err("model must be a nonempty string".into());
    }
    if !v
        .get("input")
        .is_some_and(|x| x.is_string() || x.is_array())
    {
        return Err("input must be a string or array".into());
    }
    for field in ["previous_response_id", "conversation"] {
        if v.get(field).is_some_and(|x| !x.is_null()) {
            return Err(format!(
                "{field} is unavailable; send the full input history with store:false"
            ));
        }
    }
    if v.get("store").and_then(Value::as_bool) == Some(true)
        || v.get("background").and_then(Value::as_bool) == Some(true)
    {
        return Err("stored/background responses are unavailable; use store:false".into());
    }
    for field in ["stream", "store", "background", "parallel_tool_calls"] {
        if v.get(field)
            .is_some_and(|x| !x.is_boolean() && !x.is_null())
        {
            return Err(format!("{field} must be boolean"));
        }
    }
    if v.get("tools").is_some_and(|x| !x.is_array()) {
        return Err("tools must be an array".into());
    }
    if let Some(cap) = v.get("max_output_tokens") {
        if !cap.as_u64().is_some_and(|n| n > 0) {
            return Err("max_output_tokens must be a positive integer".into());
        }
    }
    if let Some(instructions) = v.get("instructions") {
        if !instructions.is_null() && !instructions.is_string() {
            return Err("instructions must be a string".into());
        }
    }
    if let Some(reasoning) = v.get("reasoning").filter(|v| !v.is_null()) {
        if !reasoning.is_object() {
            return Err("reasoning must be an object".into());
        }
        if let Some(effort) = reasoning.get("effort").filter(|v| !v.is_null()) {
            if !matches!(
                effort.as_str(),
                Some("none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra")
            ) {
                return Err("reasoning.effort is invalid".into());
            }
        }
    }
    if let Some(text) = v.get("text").filter(|v| !v.is_null()) {
        if !text.is_object() {
            return Err("text must be an object".into());
        }
        if let Some(format) = text.get("format") {
            match format["type"].as_str() {
                Some("text") => {}
                Some("json_schema") if format["schema"].is_object() => {}
                _ => {
                    return Err(
                        "text.format must be text or json_schema with an object schema".into(),
                    )
                }
            }
        }
    }
    fn validate_tools(tools: &Value) -> Result<(), String> {
        let tools = tools.as_array().ok_or("tools must be an array")?;
        for tool in tools {
            match tool["type"].as_str() {
                Some("namespace") => {
                    if !tool["name"].as_str().is_some_and(|s| !s.is_empty()) {
                        return Err("namespace.name must be nonempty".into());
                    }
                    validate_tools(&tool["tools"])?;
                }
                Some("function" | "custom") => {
                    if !tool["name"].as_str().is_some_and(|s| !s.is_empty()) {
                        return Err("tools.name must be nonempty".into());
                    }
                    if tool.get("parameters").is_some_and(|p| !p.is_object()) {
                        return Err("tools.parameters must be an object".into());
                    }
                }
                Some(_) => {}
                None => return Err("tools.type must be a string".into()),
            }
        }
        Ok(())
    }
    if let Some(tools) = v.get("tools") {
        validate_tools(tools)?;
    }
    for item in v["input"].as_array().into_iter().flatten() {
        if !item.is_object() {
            return Err("input items must be objects".into());
        }
        if item["type"] == "additional_tools" {
            validate_tools(&item["tools"])?;
        }
    }
    Ok(v)
}

pub fn error(status: StatusCode, message: &str) -> Response {
    let mut r = Response::new(Body::from(json!({"error":{"type":"invalid_request_error","message":message,"code":null,"param":null}}).to_string()));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    r
}

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("input.{key} must be a string"))
}

fn content(v: &Value) -> Result<Vec<Value>, String> {
    if let Some(s) = v.as_str() {
        return Ok(vec![json!({"type":"text","text":s})]);
    }
    v.as_array()
        .ok_or("input.content must be string or array")?
        .iter()
        .map(|part| match part["type"].as_str() {
            Some("input_text" | "output_text") => {
                Ok(json!({"type":"text","text": string(part,"text")?}))
            }
            Some("input_image") => {
                let url = string(part, "image_url")?;
                let source = if let Some(data) = url.strip_prefix("data:") {
                    let (media, data) = data
                        .split_once(";base64,")
                        .ok_or("input.image_url must be a base64 data URL")?;
                    json!({"type":"base64","media_type":media,"data":data})
                } else if url.starts_with("https://") || url.starts_with("http://") {
                    json!({"type":"url","url":url})
                } else {
                    return Err("input.image_url must be an HTTP or base64 URL".into());
                };
                Ok(json!({"type":"image","source":source}))
            }
            _ => Err("unsupported input.content type".into()),
        })
        .collect()
}

/// Flatten namespaced tool declarations into SDK-safe, reversible names.
fn tool_definitions(request: &Value) -> Vec<Value> {
    fn append(out: &mut Vec<Value>, tools: &[Value], namespace: Option<&str>) {
        for tool in tools {
            if tool["type"] == "namespace" {
                if let (Some(name), Some(children)) =
                    (tool["name"].as_str(), tool["tools"].as_array())
                {
                    append(out, children, Some(name));
                }
            } else {
                let mut tool = tool.clone();
                if let Some(namespace) = namespace {
                    if let Some(name) = tool["name"].as_str().map(str::to_string) {
                        tool["_original_name"] = json!(name);
                        tool["_namespace"] = json!(namespace);
                        tool["name"] = json!(format!("{namespace}__{name}"));
                    }
                }
                out.push(tool);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(tools) = request["tools"].as_array() {
        append(&mut out, tools, None);
    }
    for item in request["input"].as_array().into_iter().flatten() {
        if item["type"] == "additional_tools" {
            if let Some(tools) = item["tools"].as_array() {
                append(&mut out, tools, None);
            }
        }
    }
    out
}
fn call_name(item: &Value) -> Result<String, String> {
    let name = string(item, "name")?;
    Ok(match item["namespace"].as_str() {
        Some(ns) => format!("{ns}__{name}"),
        None => name.to_string(),
    })
}

fn push_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if let Some(last) = messages.last_mut().filter(|m| m["role"] == role) {
        if let Some(content) = last["content"].as_array_mut() {
            content.extend(blocks);
        }
    } else {
        messages.push(json!({"role":role,"content":blocks}));
    }
}

/// Role-preserving full conversation. Custom tools use an explicit string
/// input schema; the return adapter restores native custom_tool_call items.
pub fn messages_request(v: &Value) -> Result<Value, String> {
    let definitions = tool_definitions(v);
    let mut messages = Vec::new();
    let mut system = Vec::new();
    if let Some(instructions) = v.get("instructions").filter(|x| !x.is_null()) {
        let s = instructions
            .as_str()
            .ok_or("instructions must be a string")?;
        if !s.is_empty() {
            system.push(json!({"type":"text","text":s}));
        }
    }
    let items = if let Some(s) = v["input"].as_str() {
        vec![json!({"role":"user","content":s})]
    } else {
        v["input"]
            .as_array()
            .cloned()
            .ok_or("input must be array or string")?
    };
    for item in items {
        match item["type"].as_str().unwrap_or("message") {
            "additional_tools" => {}
            "message" => {
                let role = string(&item, "role")?;
                let blocks = content(&item["content"])?;
                match role {
                    "system" | "developer" => system.extend(blocks),
                    "user" | "assistant" => push_message(&mut messages, role, blocks),
                    _ => return Err("unsupported input.role".into()),
                }
            }
            "function_call" | "custom_tool_call" | "tool_search_call" => {
                let kind = item["type"].as_str().unwrap_or_default();
                let (name, args) = if kind == "custom_tool_call" {
                    (call_name(&item)?, json!({"input":string(&item,"input")?}))
                } else if kind == "tool_search_call" {
                    (
                        "tool_search".to_string(),
                        if item["arguments"].is_object() {
                            item["arguments"].clone()
                        } else {
                            serde_json::from_str(string(&item, "arguments")?)
                                .map_err(|_| "input.arguments must be valid JSON")?
                        },
                    )
                } else {
                    (
                        call_name(&item)?,
                        serde_json::from_str::<Value>(string(&item, "arguments")?)
                            .map_err(|_| "input.arguments must be valid JSON")?,
                    )
                };
                if !args.is_object() {
                    return Err("input.arguments must be a JSON object".into());
                }
                push_message(
                    &mut messages,
                    "assistant",
                    vec![
                        json!({"type":"tool_use","id":string(&item,"call_id")?,"name":name,"input":args}),
                    ],
                );
            }
            "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
                let out = if item["type"] == "tool_search_output" {
                    Value::String(item["tools"].to_string())
                } else if item["output"].is_string() {
                    item["output"].clone()
                } else {
                    Value::Array(content(&item["output"])?)
                };
                push_message(
                    &mut messages,
                    "user",
                    vec![
                        json!({"type":"tool_result","tool_use_id":string(&item,"call_id")?,"content":out}),
                    ],
                );
            }
            "reasoning" => { /* foreign encrypted reasoning cannot enter Claude */ }
            "web_search_call" => { /* the accompanying assistant text carries citations */ }
            _ => return Err("unsupported input item type for this model".into()),
        }
    }
    if messages.is_empty() {
        return Err("input must contain a user conversation".into());
    }
    let mut tools = Vec::new();
    for tool in &definitions {
        let kind = tool["type"].as_str().ok_or("tools.type must be a string")?;
        let (name, schema) = match kind {
            "function" => (
                string(tool, "name")?,
                tool.get("parameters")
                    .cloned()
                    .unwrap_or(json!({"type":"object","properties":{}})),
            ),
            "custom" => (
                string(tool, "name")?,
                json!({"type":"object","properties":{"input":{"type":"string","description":tool["format"].to_string()}},"required":["input"],"additionalProperties":false}),
            ),
            "tool_search" => ("tool_search", tool["parameters"].clone()),
            "web_search" | "web_search_preview" => {
                continue;
            }
            _ => return Err("unsupported tools.type for this model".into()),
        };
        tools.push(json!({"name":name,"description":tool["description"].as_str().unwrap_or(""),"input_schema":schema}));
    }
    let mut result =
        json!({"model":v["model"],"messages":messages,"system":system,"tools":tools,"stream":true});
    if let Some(cap) = v.get("max_output_tokens") {
        if !cap.as_u64().is_some_and(|n| n > 0) {
            return Err("max_output_tokens must be a positive integer".into());
        }
        result["max_tokens"] = cap.clone();
    }
    if let Some(format) = v.pointer("/text/format").filter(|v| v["type"] != "text") {
        result["_llmux_output_format"] = format.clone();
    }
    if let Some(effort) = v.pointer("/reasoning/effort") {
        result["output_config"] = json!({"effort":effort});
    }
    if definitions.iter().any(|t| {
        matches!(
            t["type"].as_str(),
            Some("web_search" | "web_search_preview")
        )
    }) {
        result["_llmux_web_search"] = json!(true);
    }
    if let Some(choice) = v.get("tool_choice") {
        result["tool_choice"] = match choice.as_str() {
            Some("auto") => json!({"type":"auto"}),
            Some("required") => json!({"type":"any"}),
            Some("none") => json!({"type":"none"}),
            _ if choice["type"] == "function" => json!({"type":"tool","name":choice["name"]}),
            _ => return Err("unsupported tool_choice".into()),
        };
    }
    for key in ["temperature", "top_p"] {
        if let Some(value) = v.get(key) {
            result[key] = value.clone();
        }
    }
    Ok(result)
}

/// Keep only metadata used by the shared scheduler/activity, without touching
/// native Responses items (encrypted reasoning, custom tools, future events).
pub fn metadata_request(v: &Value) -> Bytes {
    let text = if let Some(text) = v["input"].as_str() {
        text.to_string()
    } else {
        v["input"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .find(|i| i["role"] == "user")
            .map(|i| {
                if let Some(text) = i["content"].as_str() {
                    text.to_string()
                } else {
                    i["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            })
            .unwrap_or_default()
    };
    Bytes::from(json!({"model":v["model"],"stream":true,"_llmux_native_responses":true,"service_tier":v["service_tier"],"system":v["instructions"],"output_config":{"effort":v.pointer("/reasoning/effort")},"metadata":v["metadata"],"messages":[{"role":"user","content":text}]}).to_string())
}

struct Converter {
    id: String,
    model: Value,
    sequence: u64,
    output: BTreeMap<u64, Value>,
    custom: HashSet<String>,
    names: BTreeMap<String, (String, String)>,
    terminal: Option<Value>,
    created_at: u64,
    tool_search: bool,
    usage: Value,
    done: bool,
    incomplete: bool,
}
impl Converter {
    fn new(request: &Value) -> Self {
        let definitions = tool_definitions(request);
        Self {
            names: definitions
                .iter()
                .filter_map(|t| {
                    Some((
                        t["name"].as_str()?.to_string(),
                        (
                            t["_namespace"].as_str()?.to_string(),
                            t["_original_name"].as_str()?.to_string(),
                        ),
                    ))
                })
                .collect(),
            terminal: None,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            id: format!("resp_{}", ulid::Ulid::new()),
            model: request["model"].clone(),
            sequence: 0,
            output: BTreeMap::new(),
            custom: definitions
                .iter()
                .filter(|t| t["type"] == "custom")
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect(),
            tool_search: request["tools"]
                .as_array()
                .is_some_and(|t| t.iter().any(|t| t["type"] == "tool_search")),
            usage: json!({"input_tokens":0,"output_tokens":0,"total_tokens":0}),
            done: false,
            incomplete: false,
        }
    }
    fn response(&self, status: &str) -> Value {
        json!({"id":self.id,"object":"response","created_at":self.created_at,"model":self.model,"status":status,"output":self.output.values().map(|item|{let mut item=item.clone();if let Some((namespace,name))=item["name"].as_str().and_then(|n|self.names.get(n)){item["name"]=json!(name);item["namespace"]=json!(namespace);}item}).collect::<Vec<_>>(),"usage":self.usage,"error":null,"incomplete_details":if self.incomplete {json!({"reason":"max_output_tokens"})} else {Value::Null}})
    }
    fn emit(&mut self, out: &mut Vec<u8>, kind: &str, mut data: Value) {
        if let Some(item) = data.get_mut("item") {
            if let Some((namespace, name)) = item["name"].as_str().and_then(|n| self.names.get(n)) {
                item["name"] = json!(name);
                item["namespace"] = json!(namespace);
            }
        }
        data["type"] = json!(kind);
        data["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        out.extend_from_slice(format!("event: {kind}\ndata: {data}\n\n").as_bytes());
    }
    fn fail(&mut self, out: &mut Vec<u8>, message: &str) {
        if self.done {
            return;
        }
        self.done = true;
        let mut response = self.response("failed");
        response["error"] = json!({"code":"server_error","message":message});
        self.terminal = Some(response.clone());
        self.emit(out, "response.failed", json!({"response":response}));
    }
    fn event(&mut self, v: &Value, out: &mut Vec<u8>) {
        if self.done {
            return;
        }
        let index = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &v["message"]["usage"];
                let fresh = u["input_tokens"].as_u64().unwrap_or(0);
                let cache = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                let creation = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                self.usage["input_tokens"] = json!(fresh + cache + creation);
                self.usage["input_tokens_details"] = json!({"cached_tokens":cache});
                self.emit(
                    out,
                    "response.created",
                    json!({"response":self.response("in_progress")}),
                );
            }
            "content_block_start" => {
                let b = &v["content_block"];
                let id = format!("item_{}_{}", self.id, index);
                let item = match b["type"].as_str() {
                    Some("tool_use") => {
                        json!({"id":id,"type":"function_call","call_id":b["id"],"name":b["name"],"arguments":"","status":"in_progress"})
                    }
                    Some("thinking") => {
                        json!({"id":id,"type":"reasoning","summary":[],"status":"in_progress"})
                    }
                    Some("text") => {
                        json!({"id":id,"type":"message","role":"assistant","content":[{"type":"output_text","text":b["text"].as_str().unwrap_or(""),"annotations":[]}],"status":"in_progress"})
                    }
                    _ => return,
                };
                self.output.insert(index, item.clone());
                // Custom/search inputs are JSON-wrapped internally. Defer the
                // item until done so no executable partial wrapper escapes.
                if !self.special(&item) {
                    self.emit(
                        out,
                        "response.output_item.added",
                        json!({"output_index":index,"item":item}),
                    );
                }
            }
            "content_block_delta" => {
                let Some(item) = self.output.get_mut(&index) else {
                    return;
                };
                let id = item["id"].clone();
                let (kind, field, delta) = match v["delta"]["type"].as_str() {
                    Some("text_delta") => (
                        "response.output_text.delta",
                        "text",
                        v["delta"]["text"].as_str().unwrap_or(""),
                    ),
                    Some("input_json_delta") => (
                        "response.function_call_arguments.delta",
                        "arguments",
                        v["delta"]["partial_json"].as_str().unwrap_or(""),
                    ),
                    Some("thinking_delta") => (
                        "response.reasoning_summary_text.delta",
                        "summary",
                        v["delta"]["thinking"].as_str().unwrap_or(""),
                    ),
                    _ => return,
                };
                if field == "text" {
                    let old = item["content"][0]["text"].as_str().unwrap_or("");
                    item["content"][0]["text"] = json!(format!("{old}{delta}"));
                } else if field == "summary" {
                    let old = item["summary"][0]["text"].as_str().unwrap_or("");
                    item["summary"] =
                        json!([{"type":"summary_text","text":format!("{old}{delta}")}]);
                } else {
                    let old = item[field].as_str().unwrap_or("");
                    item[field] = json!(format!("{old}{delta}"));
                }
                let special = self.special(&self.output[&index]);
                if !special {
                    self.emit(out,kind,json!({"item_id":id,"output_index":index,"content_index":0,"summary_index":0,"delta":delta}));
                }
            }
            "content_block_stop" => {
                let Some(mut item) = self.output.remove(&index) else {
                    return;
                };
                item["status"] = json!("completed");
                if self.special(&item) {
                    let args = match serde_json::from_str::<Value>(
                        item["arguments"].as_str().unwrap_or(""),
                    ) {
                        Ok(a) => a,
                        Err(_) => {
                            self.fail(out, "malformed tool arguments from Claude SDK");
                            return;
                        }
                    };
                    if item["name"] == "tool_search" && self.tool_search {
                        item["type"] = json!("tool_search_call");
                        item["arguments"] = args;
                        item["execution"] = json!("client");
                    } else {
                        let Some(input) = args["input"].as_str() else {
                            self.fail(out, "malformed custom tool input from Claude SDK");
                            return;
                        };
                        item["type"] = json!("custom_tool_call");
                        item["input"] = json!(input);
                        item.as_object_mut().map(|o| o.remove("arguments"));
                    }
                    self.emit(
                        out,
                        "response.output_item.added",
                        json!({"output_index":index,"item":item}),
                    );
                } else if item["type"] == "function_call" {
                    if !serde_json::from_str::<Value>(item["arguments"].as_str().unwrap_or(""))
                        .is_ok_and(|x| x.is_object())
                    {
                        self.fail(out, "malformed function arguments from Claude SDK");
                        return;
                    }
                    self.emit(out,"response.function_call_arguments.done",json!({"item_id":item["id"],"output_index":index,"arguments":item["arguments"]}));
                } else if item["type"] == "message" {
                    self.emit(out,"response.output_text.done",json!({"item_id":item["id"],"output_index":index,"content_index":0,"text":item["content"][0]["text"]}));
                }
                self.emit(
                    out,
                    "response.output_item.done",
                    json!({"output_index":index,"item":item}),
                );
                self.output.insert(index, item);
            }
            "message_delta" => {
                if let Some(input) = v["usage"]["input_tokens"].as_u64() {
                    let cache = v["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    let creation = v["usage"]["cache_creation_input_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    self.usage["input_tokens"] = json!(input + cache + creation);
                    self.usage["input_tokens_details"] = json!({"cached_tokens":cache});
                }
                let output = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
                self.usage["output_tokens"] = json!(output);
                self.usage["total_tokens"] =
                    json!(self.usage["input_tokens"].as_u64().unwrap_or(0) + output);
                self.incomplete = v["delta"]["stop_reason"] == "max_tokens";
            }
            "message_stop" => {
                let status = if self.incomplete {
                    "incomplete"
                } else {
                    "completed"
                };
                let kind = if self.incomplete {
                    "response.incomplete"
                } else {
                    "response.completed"
                };
                let response = self.response(status);
                self.terminal = Some(response.clone());
                self.emit(out, kind, json!({"response":response}));
                self.done = true;
            }
            "error" => self.fail(
                out,
                v["error"]["message"].as_str().unwrap_or("upstream error"),
            ),
            _ => {}
        }
    }
    fn special(&self, item: &Value) -> bool {
        item["name"]
            .as_str()
            .is_some_and(|n| self.custom.contains(n) || (n == "tool_search" && self.tool_search))
    }
}

/// Per-request capture at the actual client protocol boundary. The upstream
/// leg is the SDK transport (Messages SSE) or native provider Responses SSE.
pub struct Capture {
    pub path: std::path::PathBuf,
    pub id: u64,
    pub group: Option<String>,
    pub model: Option<String>,
    pub account: Option<String>,
    pub started: std::time::Instant,
    pub request: Bytes,
    pub request_headers: Vec<(String, String)>,
    pub max_body: usize,
    pub upstream: Option<super::raw_io::UpstreamRaw>,
}
impl Capture {
    fn finish(
        self,
        status: StatusCode,
        upstream: &super::sse::RawCapture,
        output: &super::sse::RawCapture,
        headers: &http::HeaderMap,
    ) {
        let mut upstream_meta = self.upstream;
        if let Some(meta) = &mut upstream_meta {
            meta.response_body = Some(super::raw_io::bounded_body_streamed(
                upstream.bytes(),
                upstream.total(),
            ));
        }
        super::raw_io::capture_streamed(
            Some(&self.path),
            self.id,
            self.group,
            self.model,
            self.account,
            Some(status.as_u16()),
            Some(u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            &self.request,
            output.bytes(),
            output.total(),
            self.max_body,
            Some(self.request_headers),
            Some(super::forward::redacted_header_pairs(headers)),
            upstream_meta,
        );
    }
}

/// Return conversion is outside shared accounting: the existing relay sees
/// the provider's true usage/terminal events and retains the account lease.
pub async fn adapt(mut response: Response, request: Value, capture: Option<Capture>) -> Response {
    let native = response
        .headers_mut()
        .remove("x-llmux-native-responses")
        .is_some();
    if !response.status().is_success() {
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap_or_default();
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let detail = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| v["detail"].as_str())
            .or_else(|| v["message"].as_str())
            .unwrap_or("llmux upstream failure");
        let detail = super::logging::mask_credentials(detail);
        let mut out = error(status, &detail);
        if let Some(capture) = capture {
            let mut raw = super::sse::RawCapture::new(capture.max_body);
            raw.push(&bytes);
            // Error envelope remains structured; content differs only in wrapper shape.
            let rendered=json!({"error":{"type":"invalid_request_error","message":detail,"code":null,"param":null}}).to_string();
            let mut output = super::sse::RawCapture::new(capture.max_body);
            output.push(rendered.as_bytes());
            capture.finish(status, &raw, &output, out.headers());
        }
        if let Some(retry) = headers.get(header::RETRY_AFTER) {
            out.headers_mut().insert(header::RETRY_AFTER, retry.clone());
        }
        return out;
    }
    let mut headers = response.headers().clone();
    headers.remove(header::CONTENT_LENGTH);
    headers.remove("x-llmux-native-responses");
    let streaming = request["stream"].as_bool().unwrap_or(false);
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let mut stream = response.into_body().into_data_stream();
    let mut capture_headers = headers.clone();
    capture_headers.insert(
        header::CONTENT_TYPE,
        http::HeaderValue::from_static(if streaming {
            "text/event-stream"
        } else {
            "application/json"
        }),
    );
    let task = tokio::spawn(async move {
        let capture_limit = capture.as_ref().map(|c| c.max_body).unwrap_or(0);
        let mut upstream_capture = super::sse::RawCapture::new(capture_limit);
        let mut output_capture = super::sse::RawCapture::new(capture_limit);
        let mut buffer = EventBuffer::default();
        let mut converter = Converter::new(&request);
        let mut terminal = None;
        let mut native_items = BTreeMap::<u64, Value>::new();
        loop {
            let next = tokio::select! { _=tx.closed()=>break, next=stream.next()=>next };
            let Some(chunk) = next else {
                break;
            };
            let Ok(chunk) = chunk else {
                break;
            };
            upstream_capture.push(&chunk);
            let mut out = Vec::new();
            for event in buffer.push(&chunk) {
                let Some(data) = event
                    .lines()
                    .find_map(|l| l.strip_prefix("data:").map(str::trim))
                else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if native {
                    if v["type"] == "response.output_item.done" && v["item"].is_object() {
                        let index = v["output_index"]
                            .as_u64()
                            .unwrap_or(native_items.len() as u64);
                        native_items.insert(index, v["item"].clone());
                    }
                    if matches!(
                        v["type"].as_str(),
                        Some("response.completed" | "response.incomplete" | "response.failed")
                    ) {
                        let mut response = v["response"].clone();
                        if response
                            .get("output")
                            .is_none_or(|o| o.as_array().is_none_or(Vec::is_empty))
                            && !native_items.is_empty()
                        {
                            response["output"] = json!(native_items.values().collect::<Vec<_>>());
                        }
                        terminal = Some(response);
                    }
                } else {
                    converter.event(&v, &mut out);
                    if converter.done {
                        terminal = converter.terminal.clone();
                    }
                }
            }
            if streaming {
                let data = if native { chunk } else { Bytes::from(out) };
                output_capture.push(&data);
                if !data.is_empty() && tx.send(Ok(data)).await.is_err() {
                    break;
                }
            }
        }
        let failed = terminal.as_ref().is_none_or(|v| {
            v["status"] == "failed" || v.get("error").is_some_and(|e| !e.is_null())
        });
        if streaming {
            if terminal.is_none() {
                let mut out = Vec::new();
                converter.fail(&mut out, "upstream stream ended before a terminal response");
                output_capture.push(&out);
                let _ = tx.send(Ok(Bytes::from(out))).await;
            }
        } else {
            let result=terminal.unwrap_or_else(||json!({"error":{"type":"server_error","message":"upstream stream ended before a terminal response"}}));
            let data = result.to_string();
            output_capture.push(data.as_bytes());
            let _ = tx.send(Ok(Bytes::from(data))).await;
        }
        if let Some(capture) = capture {
            capture.finish(
                if !streaming && failed {
                    StatusCode::BAD_GATEWAY
                } else {
                    StatusCode::OK
                },
                &upstream_capture,
                &output_capture,
                &capture_headers,
            );
        }
    });
    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    if streaming {
        let mut out = Response::new(body);
        *out.headers_mut() = headers;
        out.headers_mut().insert(
            header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/event-stream"),
        );
        out
    } else {
        let data = axum::body::to_bytes(body, 64 * 1024 * 1024)
            .await
            .unwrap_or_default();
        let _ = task.await;
        let value: Value = serde_json::from_slice(&data).unwrap_or(Value::Null);
        let mut out = Response::new(Body::from(data));
        *out.headers_mut() = headers;
        out.headers_mut().insert(
            header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        if value.get("error").is_some_and(|e| !e.is_null()) || value["status"] == "failed" {
            *out.status_mut() = StatusCode::BAD_GATEWAY;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_tool_history_preserves_roles_and_custom_input() {
        let v = json!({"model":"opus","instructions":"system","input":[{"role":"developer","content":"dev"},{"role":"user","content":"change"},{"type":"custom_tool_call","call_id":"c1","name":"apply_patch","input":"*** patch"},{"type":"custom_tool_call_output","call_id":"c1","output":"ok"}],"tools":[{"type":"custom","name":"apply_patch"}]});
        let m = messages_request(&v).unwrap();
        assert_eq!(m["system"].as_array().unwrap().len(), 2);
        assert_eq!(
            m["messages"][1]["content"][0]["input"]["input"],
            "*** patch"
        );
        assert_eq!(m["messages"][2]["content"][0]["tool_use_id"], "c1");
    }
    #[test]
    fn stored_history_is_never_silently_lost() {
        assert!(validate(br#"{"model":"sol","input":"hi","previous_response_id":"r1"}"#).is_err());
    }
    fn messages_sse(events: &[Value]) -> Response {
        let body = events
            .iter()
            .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
            .collect::<String>();
        let mut response = Response::new(Body::from(body));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/event-stream"),
        );
        response
    }
    #[tokio::test]
    async fn failures_never_aggregate_to_completed() {
        for events in [
            vec![json!({"type":"error","error":{"message":"failed"}})],
            vec![
                json!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"f"}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_stop"}),
            ],
            vec![json!({"type":"message_start","message":{"usage":{"input_tokens":1}}})],
        ] {
            let out = adapt(
                messages_sse(&events),
                json!({"model":"opus","stream":false}),
                None,
            )
            .await;
            assert_eq!(out.status(), 502);
            let value: Value = serde_json::from_slice(
                &axum::body::to_bytes(out.into_body(), 10000).await.unwrap(),
            )
            .unwrap();
            assert_ne!(value["status"], "completed");
            assert!(value["error"].is_object());
        }
    }
    #[test]
    fn namespace_custom_roundtrip_and_usage_are_preserved() {
        let request = json!({"model":"opus","input":[{"type":"additional_tools","tools":[{"type":"namespace","name":"functions","tools":[{"type":"custom","name":"exec"}]}]},{"role":"user","content":"hi"}]});
        let messages = messages_request(&request).unwrap();
        assert_eq!(messages["tools"][0]["name"], "functions__exec");
        let mut c = Converter::new(&request);
        let mut out = Vec::new();
        for e in [
            json!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","name":"functions__exec","id":"c1"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"text(1)\"}"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"input_tokens":20,"cache_read_input_tokens":5,"output_tokens":3}}),
            json!({"type":"message_stop"}),
        ] {
            c.event(&e, &mut out);
        }
        let terminal = c.terminal.unwrap();
        assert_eq!(terminal["output"][0]["name"], "exec");
        assert_eq!(terminal["output"][0]["namespace"], "functions");
        assert_eq!(terminal["output"][0]["type"], "custom_tool_call");
        assert_eq!(terminal["output"][0]["input"], "text(1)");
        assert_eq!(terminal["usage"]["input_tokens"], 25);
    }
}
