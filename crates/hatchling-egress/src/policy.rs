//! The request policy for model APIs (WO-008).
//!
//! A model vendor's API can fetch on the caller's behalf: an image or a
//! document named by URL, a server-side search or fetch tool, a remote
//! tool server. A hostile cell allowed to reach a vendor could ask it to
//! fetch an attacker's URL, and carry data out to a host the allowlist
//! never named. So the proxy and the harness relay read a model request
//! before it leaves, and refuse it if, anywhere in its JSON body, it
//! carries:
//!
//! - **content by URL:** a block whose `type` is `url`, or an
//!   `image_url`, `file_url`, `audio_url`, `video_url` or `document_url`
//!   that names anything but a `data:` URI (content sent inline passes);
//! - **a tool the vendor runs:** any tool whose `type` isn't one the
//!   caller runs itself ([`CLIENT_TOOL_TYPES`], [`CLIENT_TOOL_FAMILIES`]);
//! - **a remote tool server or the vendor's own search:**
//!   `mcp_servers`, `web_search_options`, a `plugins` list, a stored
//!   prompt (`prompt` by `id`, which can carry tools set on the vendor's
//!   side), or a model that searches on its own (`:online`).
//!
//! Unknown is refused: a vendor's new server-side tool type stays
//! refused until it's reviewed and listed here. What a prompt says is
//! never filtered; text passes as text. A tool call's own `input` isn't
//! read either: it's what a tool the caller runs was asked, not something
//! the vendor acts on.
//!
//! The body is read strictly: it must be JSON, and an object may not
//! name a key twice, since a parser that kept the other copy could see a
//! different request than this one did.
//!
//! **Where it applies.** Every request to a model endpoint, a `POST` to
//! a path ending in one of [`MODEL_ENDPOINTS`], on any destination. A
//! destination the allowlist names that is a known model vendor
//! ([`MODEL_VENDORS`]) is open to those endpoints and its model list
//! only: a vendor's batch, file and assistant APIs would carry a request
//! this policy never reads. The harness relay already holds each vendor
//! to its own API; it checks every body it relays.

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// Why the policy refused a request: safe to log, and to answer with.
pub type Refusal = &'static str;

/// The endpoints a model request goes to, as the end of its path:
/// `OpenAI`'s chat completions and responses, Anthropic's messages and
/// their token count, and embeddings.
pub const MODEL_ENDPOINTS: [&str; 5] = [
    "/chat/completions",
    "/responses",
    "/messages",
    "/messages/count_tokens",
    "/embeddings",
];

/// Hosts of model vendors whose APIs fetch on request, or route to one
/// that does. A destination on one of them is open to
/// [`MODEL_ENDPOINTS`] and `GET` of its model list only.
pub const MODEL_VENDORS: [&str; 10] = [
    "api.openai.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
    "openrouter.ai",
    "api.mistral.ai",
    "api.groq.com",
    "api.x.ai",
    "api.deepseek.com",
    "api.together.xyz",
    "api.fireworks.ai",
];

/// Tool types the caller runs itself: a function, a free-form custom
/// tool, Codex's local shell.
pub const CLIENT_TOOL_TYPES: [&str; 3] = ["function", "custom", "local_shell"];

/// Families of versioned tool types the caller runs itself.
///
/// Anthropic's bash, text editor, computer use and memory tools, and
/// `OpenAI`'s computer use: the vendor defines their schema, and the
/// caller carries them out.
pub const CLIENT_TOOL_FAMILIES: [&str; 4] = ["bash_", "text_editor_", "computer_", "memory_"];

/// Keys that name content by URL.
const URL_KEYS: [&str; 5] = [
    "image_url",
    "file_url",
    "audio_url",
    "video_url",
    "document_url",
];

/// Keys that ask the vendor to reach a remote tool server or search the
/// web, whatever their value.
const REMOTE_KEYS: [&str; 2] = ["mcp_servers", "web_search_options"];

const NOT_JSON: Refusal = "a model request's body must be JSON, each key named once";
const URL_CONTENT: Refusal = "a model request may not give content by URL";
const SERVER_TOOL: Refusal = "a model request may not use a tool the vendor runs";
const REMOTE: Refusal = "a model request may not reach a remote tool server or search the web";
const ENDPOINT: Refusal =
    "a model vendor is open to its chat, responses, messages, embeddings and model list only";

/// Checks a request to `destination` (`host:port`): a model vendor's
/// endpoint, then, for a model request, its body.
///
/// # Errors
///
/// The [`Refusal`] that stopped it.
pub fn check(destination: &str, method: &str, path: &str, body: &[u8]) -> Result<(), Refusal> {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let model = method == "POST" && MODEL_ENDPOINTS.iter().any(|end| path.ends_with(end));
    let host = destination
        .rsplit_once(':')
        .map_or(destination, |(host, _)| host);
    if MODEL_VENDORS.contains(&host) && !model && !(method == "GET" && path.ends_with("/models")) {
        return Err(ENDPOINT);
    }
    if model { check_body(body) } else { Ok(()) }
}

/// Checks a model request's body (see the [module docs](self)). An empty
/// body passes: it asks for nothing.
///
/// # Errors
///
/// The [`Refusal`] that stopped it.
pub fn check_body(body: &[u8]) -> Result<(), Refusal> {
    if body.is_empty() {
        return Ok(());
    }
    let Strict(value) = serde_json::from_slice(body).map_err(|_| NOT_JSON)?;
    scan(&value)
}

/// Walks a request's JSON, refusing what the policy refuses.
fn scan(value: &Value) -> Result<(), Refusal> {
    match value {
        Value::Array(items) => items.iter().try_for_each(scan),
        Value::Object(object) => {
            check_object(object)?;
            let tool_call = object.get("type").and_then(Value::as_str) == Some("tool_use");
            object
                .iter()
                .filter(|(key, _)| !(tool_call && key.as_str() == "input"))
                .try_for_each(|(_, value)| scan(value))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
    }
}

/// The policy's checks on one object, before its values are walked.
fn check_object(object: &Map<String, Value>) -> Result<(), Refusal> {
    if object.get("type").and_then(Value::as_str) == Some("url")
        || URL_KEYS
            .iter()
            .filter_map(|key| object.get(*key))
            .any(|value| named_url(value).is_some_and(|url| !url.starts_with("data:")))
    {
        return Err(URL_CONTENT);
    }
    if let Some(Value::Array(tools)) = object.get("tools") {
        if tools.iter().any(is_server_tool) {
            return Err(SERVER_TOOL);
        }
    }
    let stored_prompt = object
        .get("prompt")
        .is_some_and(|prompt| prompt.get("id").is_some_and(Value::is_string));
    let searching_model = object
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|model| model.ends_with(":online"));
    if REMOTE_KEYS.iter().any(|key| object.contains_key(*key))
        || matches!(object.get("plugins"), Some(Value::Array(_)))
        || stored_prompt
        || searching_model
    {
        return Err(REMOTE);
    }
    Ok(())
}

/// The URL a content field names: the string itself, or an object's
/// `url`.
fn named_url(value: &Value) -> Option<&str> {
    match value {
        Value::String(url) => Some(url),
        Value::Object(object) => object.get("url").and_then(Value::as_str),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => None,
    }
}

/// Whether `tool` is one the vendor runs: an object whose `type` isn't a
/// client tool's. A tool with no `type` (Anthropic's own tools) is the
/// caller's.
fn is_server_tool(tool: &Value) -> bool {
    match tool.get("type") {
        None => false,
        Some(Value::String(kind)) => {
            !(CLIENT_TOOL_TYPES.contains(&kind.as_str())
                || CLIENT_TOOL_FAMILIES
                    .iter()
                    .any(|family| kind.starts_with(family)))
        }
        Some(_) => true,
    }
}

/// A JSON value read strictly: an object that names a key twice is
/// refused rather than resolved, so no other parser can read a
/// different request out of the same bytes.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

/// Builds a [`Strict`] value.
struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Strict;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON with each key named once")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Strict, E> {
        Ok(Strict(Value::Bool(value)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Strict, E> {
        Ok(Strict(Value::from(value)))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Strict, E> {
        Ok(Strict(Value::from(value)))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Strict, E> {
        Ok(Strict(Value::from(value)))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Strict, E> {
        Ok(Strict(Value::from(value)))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Strict(Value::Array(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom("a key named twice"));
            }
            let Strict(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(Strict(Value::Object(object)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    /// An ordinary chat-completions request with tools and an inline
    /// image, as the agent's model client and OpenAI-compatible servers
    /// send them.
    fn chat() -> Value {
        json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": [
                    {"type": "text", "text": "Read https://example.com and describe this."},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}}
                ]},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call-1", "type": "function",
                    "function": {"name": "fetch", "arguments": "{\"url\":\"https://example.com\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call-1", "content": "{\"image_url\":\"https://example.com/a.png\"}"}
            ],
            "tools": [{"type": "function", "function": {
                "name": "describe",
                "parameters": {"type": "object", "properties": {
                    "image_url": {"type": "string", "description": "an image's URL"},
                    "url": {"type": "string", "format": "uri"}
                }}
            }}],
            "tool_choice": "auto",
            "response_format": {"type": "json_schema", "json_schema": {"name": "r", "schema": {
                "type": "object", "properties": {"file_url": {"type": "string"}}
            }}},
            "temperature": 0.2,
            "stream": false
        })
    }

    /// An ordinary Anthropic Messages request, as Claude Code sends one:
    /// its own tools, a past call whose input names a URL, thinking,
    /// caching, an inline image and a tool result.
    fn messages() -> Value {
        json!({
            "model": "claude-sonnet",
            "max_tokens": 1024,
            "system": [{"type": "text", "text": "You are a coding agent.", "cache_control": {"type": "ephemeral"}}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Fetch the docs."},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}}
                ]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "Use the fetch tool.", "signature": "sig"},
                    {"type": "tool_use", "id": "toolu_1", "name": "WebFetch", "input": {
                        "url": "https://docs.example.com", "type": "url", "image_url": "https://x.example/a.png"
                    }}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "the docs"}]}
                ]}
            ],
            "tools": [
                {"name": "WebFetch", "description": "Fetches a URL.", "input_schema": {
                    "type": "object", "properties": {"url": {"type": "string", "format": "uri"}}
                }},
                {"type": "custom", "name": "Read", "input_schema": {"type": "object"}},
                {"type": "bash_20250124", "name": "bash"},
                {"type": "text_editor_20250728", "name": "str_replace_based_edit_tool"}
            ],
            "metadata": {"user_id": "u"},
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "stream": true
        })
    }

    /// An ordinary Responses request, as Codex sends one.
    fn responses() -> Value {
        json!({
            "model": "gpt-5-codex",
            "instructions": "You are Codex.",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "Fix the bug."},
                    {"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo="}
                ]},
                {"type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{\"command\":[\"curl\",\"https://x.example\"]}"},
                {"type": "function_call_output", "call_id": "c1", "output": "done"},
                {"type": "reasoning", "summary": [], "encrypted_content": "abc"}
            ],
            "tools": [
                {"type": "function", "name": "shell", "parameters": {"type": "object"}},
                {"type": "custom", "name": "apply_patch", "format": {"type": "grammar", "syntax": "lark", "definition": "start: x"}},
                {"type": "local_shell"}
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "reasoning": {"effort": "medium", "summary": "auto"},
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "k",
            "text": {"verbosity": "medium"}
        })
    }

    #[test]
    fn ordinary_chat_messages_and_responses_requests_pass() {
        for request in [chat(), messages(), responses()] {
            assert_eq!(check_body(&body(&request)), Ok(()), "{request}");
        }
        assert_eq!(check_body(b""), Ok(()));
        assert_eq!(
            check_body(br#"{"model":"text-embedding-3-small","input":["a","b"]}"#),
            Ok(())
        );
    }

    /// Puts `value` at `pointer` in `request`.
    fn with(mut request: Value, pointer: &str, value: Value) -> Value {
        *request.pointer_mut(pointer).unwrap() = value;
        request
    }

    #[test]
    fn content_given_by_url_is_refused_wherever_it_is() {
        let refused = [
            // Chat completions: an image by URL.
            with(
                chat(),
                "/messages/1/content/1/image_url",
                json!({"url": "https://attacker.example/x?d=secret"}),
            ),
            with(
                chat(),
                "/messages/1/content/1/image_url",
                json!("http://attacker.example/x"),
            ),
            // Anthropic: an image, a document, and an image inside a tool result.
            with(
                messages(),
                "/messages/0/content/1/source",
                json!({"type": "url", "url": "https://attacker.example/a.png"}),
            ),
            with(
                messages(),
                "/messages/0/content/1",
                json!({"type": "document", "source": {"type": "url", "url": "https://attacker.example/a.pdf"}}),
            ),
            with(
                messages(),
                "/messages/2/content/0/content/0",
                json!({"type": "image", "source": {"type": "url", "url": "https://attacker.example/b.png"}}),
            ),
            // Responses: an image and a file by URL.
            with(
                responses(),
                "/input/0/content/1/image_url",
                json!("https://attacker.example/c.png"),
            ),
            with(
                responses(),
                "/input/0/content/1",
                json!({"type": "input_file", "file_url": "https://attacker.example/d.pdf"}),
            ),
            // OpenAI-compatible servers' audio and video.
            with(
                chat(),
                "/messages/1/content/1",
                json!({"type": "video_url", "video_url": {"url": "https://attacker.example/v.mp4"}}),
            ),
            with(
                chat(),
                "/messages/1/content/1",
                json!({"type": "audio_url", "audio_url": {"url": "ftp://attacker.example/a.wav"}}),
            ),
            with(
                chat(),
                "/messages/1/content/1",
                json!({"type": "document_url", "document_url": "https://attacker.example/e.pdf"}),
            ),
        ];
        for request in refused {
            assert_eq!(check_body(&body(&request)), Err(URL_CONTENT), "{request}");
        }
    }

    #[test]
    fn a_tool_the_vendor_runs_is_refused() {
        for kind in [
            "web_search_20250305",
            "web_fetch_20250910",
            "code_execution_20250825",
            "web_search",
            "web_search_preview",
            "file_search",
            "mcp",
            "image_generation",
            "code_interpreter",
            "a_tool_not_yet_invented",
        ] {
            for request in [messages(), responses()] {
                let tool = json!({"type": kind, "name": "t"});
                let request = with(request, "/tools/1", tool);
                assert_eq!(check_body(&body(&request)), Err(SERVER_TOOL), "{kind}");
            }
        }
        // A `type` that isn't a string is no client tool either.
        let request = with(messages(), "/tools/1", json!({"type": 7, "name": "t"}));
        assert_eq!(check_body(&body(&request)), Err(SERVER_TOOL));
        // Batched requests are read too.
        let batch = json!({"requests": [{"custom_id": "a", "params": with(messages(), "/tools/0", json!({"type": "web_fetch_20250910", "name": "f"}))}]});
        assert_eq!(check_body(&body(&batch)), Err(SERVER_TOOL));
        // Client tools, by every listed type and family, pass.
        for kind in CLIENT_TOOL_TYPES.into_iter().chain([
            "bash_1",
            "text_editor_1",
            "computer_use_preview",
            "memory_1",
        ]) {
            let request = with(responses(), "/tools/0", json!({"type": kind, "name": "t"}));
            assert_eq!(check_body(&body(&request)), Ok(()), "{kind}");
        }
        assert_eq!(
            CLIENT_TOOL_FAMILIES,
            ["bash_", "text_editor_", "computer_", "memory_"]
        );
    }

    #[test]
    fn remote_tool_servers_and_the_vendors_own_search_are_refused() {
        let mut refused = Vec::new();
        for (key, value) in [
            (
                "mcp_servers",
                json!([{"type": "url", "url": "https://attacker.example/mcp", "name": "m"}]),
            ),
            ("mcp_servers", json!([])),
            ("web_search_options", json!({})),
            ("plugins", json!([{"id": "web"}])),
            ("prompt", json!({"id": "pmpt_123", "version": "2"})),
            ("model", json!("openai/gpt-4o:online")),
        ] {
            let mut request = chat();
            request
                .as_object_mut()
                .unwrap()
                .insert(String::from(key), value);
            refused.push(request);
        }
        for request in refused {
            let checked = check_body(&body(&request));
            assert!(matches!(checked, Err(REMOTE | URL_CONTENT)), "{request}");
            assert_ne!(checked, Ok(()));
        }
        let mut request = chat();
        request
            .as_object_mut()
            .unwrap()
            .insert(String::from("web_search_options"), json!({}));
        assert_eq!(check_body(&body(&request)), Err(REMOTE));
        // What only looks like them passes: a prompt as text, plugins as a
        // schema, a model that only mentions the word.
        let mut request = chat();
        let object = request.as_object_mut().unwrap();
        object.insert(String::from("prompt"), json!("Say hi"));
        object.insert(String::from("plugins"), json!({"type": "object"}));
        object.insert(String::from("model"), json!("online-model:latest"));
        assert_eq!(check_body(&body(&request)), Ok(()));
    }

    #[test]
    fn a_body_that_is_not_json_or_names_a_key_twice_is_refused() {
        assert_eq!(check_body(b"not json"), Err(NOT_JSON));
        assert_eq!(check_body(&[0x1f, 0x8b, 0x08, 0x00]), Err(NOT_JSON));
        // The second `tools` would be the one another parser kept.
        assert_eq!(
            check_body(br#"{"tools":[],"tools":[{"type":"web_fetch_20250910"}]}"#),
            Err(NOT_JSON)
        );
        assert_eq!(check_body(br#"{"a":{"b":1,"b":2}}"#), Err(NOT_JSON));
        assert_eq!(check_body(br#"[{"x":1},{"x":2}]"#), Ok(()));
    }

    #[test]
    fn the_strict_reading_is_the_ordinary_one_when_no_key_repeats() {
        let text = br#"{"t":true,"f":false,"n":null,"i":-3,"u":18446744073709551615,"x":1.5,"s":"text","e":"","a":[1,[2,{"k":"v"}],[]],"o":{}}"#;
        let Strict(strict) = serde_json::from_slice(text).unwrap();
        let ordinary: Value = serde_json::from_slice(text).unwrap();
        assert_eq!(strict, ordinary);
        // An escaped key is the key it spells.
        assert_eq!(
            check_body(br#"{"tools":[{"type":"web_fetch_20250910"}]}"#),
            Err(SERVER_TOOL)
        );
        let error = serde_json::from_slice::<Strict>(b"{").err().unwrap();
        assert!(!error.to_string().is_empty());
        let expected = serde_json::from_slice::<Strict>(b"{\"a\":1,\"a\":1}")
            .err()
            .unwrap();
        assert!(
            expected.to_string().contains("a key named twice"),
            "{expected}"
        );
        // What the strict reading expects, as an error names it.
        let unexpected = StrictVisitor
            .visit_bytes::<serde_json::Error>(b"x")
            .err()
            .unwrap();
        assert!(
            unexpected.to_string().contains("each key named once"),
            "{unexpected}"
        );
    }

    #[test]
    fn the_policys_parts_answer_on_their_own() {
        // named_url: a string, or an object's `url`; nothing else.
        assert_eq!(named_url(&json!("https://x")), Some("https://x"));
        assert_eq!(named_url(&json!({"url": "data:,a"})), Some("data:,a"));
        for none in [
            json!({"file_id": "f"}),
            json!(null),
            json!(true),
            json!(1),
            json!(["u"]),
        ] {
            assert_eq!(named_url(&none), None, "{none}");
        }
        // is_server_tool: by its `type`.
        assert!(!is_server_tool(&json!({"name": "t"})));
        assert!(!is_server_tool(&json!({"type": "function"})));
        assert!(!is_server_tool(&json!({"type": "memory_20250818"})));
        assert!(is_server_tool(&json!({"type": "web_search"})));
        assert!(is_server_tool(&json!({"type": ["function"]})));
        // check_object: one object, not what it holds.
        let object = |value: Value| value.as_object().cloned().unwrap();
        assert_eq!(
            check_object(&object(json!({"type": "url"}))),
            Err(URL_CONTENT)
        );
        assert_eq!(
            check_object(&object(json!({"file_url": "https://x"}))),
            Err(URL_CONTENT)
        );
        assert_eq!(
            check_object(&object(json!({"image_url": {"url": "data:,a"}}))),
            Ok(())
        );
        assert_eq!(
            check_object(&object(json!({"tools": [{"type": "mcp"}]}))),
            Err(SERVER_TOOL)
        );
        assert_eq!(
            check_object(&object(json!({"tools": {"type": "mcp"}}))),
            Ok(())
        );
        assert_eq!(
            check_object(&object(json!({"mcp_servers": null}))),
            Err(REMOTE)
        );
        assert_eq!(
            check_object(&object(json!({"inner": {"type": "url"}}))),
            Ok(())
        );
        // scan: everything it holds, but a tool call's input.
        assert_eq!(
            scan(&json!([[{"inner": {"type": "url"}}]])),
            Err(URL_CONTENT)
        );
        assert_eq!(
            scan(&json!({"type": "tool_use", "input": {"type": "url"}})),
            Ok(())
        );
        assert_eq!(
            scan(&json!({"type": "tool_result", "input": {"type": "url"}})),
            Err(URL_CONTENT)
        );
        assert_eq!(scan(&json!("text")), Ok(()));
    }

    #[test]
    fn the_strict_visitor_builds_each_kind_of_value() {
        /// Shows what the strict visitor expects.
        struct Expecting;
        impl std::fmt::Display for Expecting {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                StrictVisitor.expecting(formatter)
            }
        }
        use serde::de::value::{Error as ValueError, MapDeserializer, SeqDeserializer};
        let value = |built: Result<Strict, ValueError>| built.map(|Strict(value)| value).ok();
        assert_eq!(value(StrictVisitor.visit_bool(true)), Some(json!(true)));
        assert_eq!(value(StrictVisitor.visit_i64(-2)), Some(json!(-2)));
        assert_eq!(value(StrictVisitor.visit_u64(7)), Some(json!(7)));
        assert_eq!(value(StrictVisitor.visit_f64(0.5)), Some(json!(0.5)));
        assert_eq!(value(StrictVisitor.visit_str("s")), Some(json!("s")));
        assert_eq!(value(StrictVisitor.visit_unit()), Some(json!(null)));
        let seq = SeqDeserializer::<_, ValueError>::new([1_u64, 2].into_iter());
        assert_eq!(value(StrictVisitor.visit_seq(seq)), Some(json!([1, 2])));
        let map = MapDeserializer::<_, ValueError>::new([("a", 1_u64), ("b", 2)].into_iter());
        assert_eq!(
            value(StrictVisitor.visit_map(map)),
            Some(json!({"a": 1, "b": 2}))
        );
        let twice = MapDeserializer::<_, ValueError>::new([("a", 1_u64), ("a", 2)].into_iter());
        assert_eq!(value(StrictVisitor.visit_map(twice)), None);
        assert_eq!(Expecting.to_string(), "JSON with each key named once");
    }

    #[test]
    fn a_model_vendor_is_open_to_its_model_endpoints_only() {
        let chat_body = body(&chat());
        for (destination, method, path) in [
            ("api.openai.com:443", "POST", "/v1/chat/completions"),
            ("api.openai.com:443", "POST", "/v1/responses"),
            ("api.openai.com:443", "POST", "/v1/embeddings"),
            ("api.openai.com:443", "GET", "/v1/models"),
            ("api.anthropic.com:443", "POST", "/v1/messages?beta=true"),
            ("api.anthropic.com:443", "POST", "/v1/messages/count_tokens"),
            ("api.groq.com:443", "POST", "/openai/v1/chat/completions"),
            ("openrouter.ai:443", "POST", "/api/v1/chat/completions"),
        ] {
            assert_eq!(
                check(destination, method, path, &chat_body),
                Ok(()),
                "{destination}{path}"
            );
        }
        for (destination, method, path) in [
            ("api.openai.com:443", "POST", "/v1/batches"),
            ("api.openai.com:443", "POST", "/v1/files"),
            ("api.openai.com:443", "POST", "/v1/threads/runs"),
            ("api.openai.com:443", "GET", "/v1/files/f/content"),
            ("api.openai.com:443", "GET", "/v1/chat/completions"),
            ("api.anthropic.com:443", "POST", "/v1/messages/batches"),
            ("api.anthropic.com:443", "DELETE", "/v1/models"),
            ("api.anthropic.com:443", "POST", "/v1/xmessages"),
            (
                "generativelanguage.googleapis.com:443",
                "POST",
                "/v1beta/models/gemini:generateContent",
            ),
        ] {
            assert_eq!(
                check(destination, method, path, &chat_body),
                Err(ENDPOINT),
                "{destination}{path}"
            );
        }
        assert_eq!(MODEL_VENDORS.len(), 10);
        assert!(
            MODEL_VENDORS.iter().all(|vendor| check(
                &format!("{vendor}:443"),
                "POST",
                "/v1/files",
                b""
            )
            .is_err())
        );
    }

    #[test]
    fn a_model_request_is_read_on_any_destination_and_nothing_else_is() {
        let fetching = body(&with(chat(), "/tools/0", json!({"type": "web_search"})));
        // A gateway the policy has never heard of, a local server, a
        // vendor: a model request is read wherever it goes.
        for destination in [
            "llm-gateway.example:443",
            "localhost:11434",
            "api.openai.com:443",
        ] {
            assert_eq!(
                check(destination, "POST", "/v1/chat/completions", &fetching),
                Err(SERVER_TOOL)
            );
        }
        // Not a model request: another API, another method, another path.
        assert_eq!(
            check("api.github.com:443", "POST", "/repos/o/r/issues", &fetching),
            Ok(())
        );
        assert_eq!(
            check(
                "llm-gateway.example:443",
                "GET",
                "/v1/chat/completions",
                &fetching
            ),
            Ok(())
        );
        assert_eq!(
            check(
                "llm-gateway.example:443",
                "POST",
                "/v1/chat/completionsx",
                &fetching
            ),
            Ok(())
        );
        assert_eq!(
            MODEL_ENDPOINTS,
            [
                "/chat/completions",
                "/responses",
                "/messages",
                "/messages/count_tokens",
                "/embeddings"
            ]
        );
    }
}
