//! Domain: the MCP messages of `idlemax mcp` (D12), JSON-RPC 2.0 with one object per line.
//! Pure: `handle` answers the handshake, `ping` and `tools/list` itself and names the tool a
//! `tools/call` asks for; the delivery layer runs the tool and builds the reply with
//! `tool_result`. Written by hand: five methods and six tools need no SDK.

use serde_json::{json, Map, Value};

use crate::VERSION;

/// The revisions this server speaks, newest first. `2025-03-26` is left out: it has a server
/// accept a batch, which this parser does not read.
pub const PROTOCOL_VERSIONS: [&str; 2] = ["2025-06-18", "2024-11-05"];

pub const PARSE_ERROR: i64 = -32700;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    Status,
    Start,
    Stop,
    ProjectOn(String),
    ProjectOff(String),
    FinishedWork,
}

/// What one input line asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// A reply to print.
    Reply(String),
    /// A notification: nothing to print.
    Quiet,
    /// Run the tool, then print `tool_result` with this id.
    Call { id: Value, tool: Tool },
}

pub fn handle(line: &str) -> Step {
    let Ok(Value::Object(msg)) = serde_json::from_str::<Value>(line) else {
        return Step::Reply(error(
            &Value::Null,
            PARSE_ERROR,
            "parse error: not a JSON object",
        ));
    };
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(id) = msg.get("id").cloned() else {
        return Step::Quiet;
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => Step::Reply(result(&id, initialize(&params))),
        "ping" => Step::Reply(result(&id, json!({}))),
        "tools/list" => Step::Reply(result(&id, json!({"tools": tools()}))),
        "tools/call" => match tool(&params) {
            Ok(tool) => Step::Call { id, tool },
            Err(why) => Step::Reply(error(&id, INVALID_PARAMS, &why)),
        },
        other => {
            // The name is echoed, so a huge one is cut.
            let name: String = other.chars().take(64).collect();
            Step::Reply(error(
                &id,
                METHOD_NOT_FOUND,
                &format!("unknown method {name}"),
            ))
        }
    }
}

/// The reply to a `tools/call`: the tool's text, flagged when the tool failed.
pub fn tool_result(id: &Value, text: &str, is_error: bool) -> String {
    result(
        id,
        json!({"content": [{"type": "text", "text": text}], "isError": is_error}),
    )
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "idlemax", "version": VERSION},
    })
}

/// The six tools (D12), each with an object schema that takes no other properties.
pub fn tools() -> Value {
    let none = json!({"type": "object", "properties": {}, "additionalProperties": false});
    let named = json!({
        "type": "object",
        "properties": {"name": {"type": "string", "description": "the project's name"}},
        "required": ["name"],
        "additionalProperties": false,
    });
    let tool = |name: &str, description: &str, schema: &Value| json!({"name": name, "description": description, "inputSchema": schema});
    json!([
        tool(
            "status",
            "What idlemax is doing now: devices, the dispatcher and its workers",
            &none
        ),
        tool(
            "start",
            "Start the dispatcher's workers again after a stop",
            &none
        ),
        tool(
            "stop",
            "Stop every dispatcher worker; each keeps its state",
            &none
        ),
        tool(
            "project_on",
            "Let the dispatcher plan work in a project",
            &named
        ),
        tool(
            "project_off",
            "Stop the dispatcher planning work in a project",
            &named
        ),
        tool(
            "finished_work",
            "Finished tasks waiting for the owner's review",
            &none
        ),
    ])
}

fn tool(params: &Value) -> Result<Tool, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let empty = Map::new();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err("arguments must be an object".into()),
    };
    let project = |make: fn(String) -> Tool| match (args.len(), args.get("name")) {
        (1, Some(Value::String(n))) if !n.trim().is_empty() => Ok(make(n.clone())),
        _ => Err(format!(
            "{name} takes exactly one argument: name, a project name"
        )),
    };
    let bare = |tool: Tool| match args.is_empty() {
        true => Ok(tool),
        false => Err(format!("{name} takes no arguments")),
    };
    match name {
        "status" => bare(Tool::Status),
        "start" => bare(Tool::Start),
        "stop" => bare(Tool::Stop),
        "finished_work" => bare(Tool::FinishedWork),
        "project_on" => project(Tool::ProjectOn),
        "project_off" => project(Tool::ProjectOff),
        other => Err(format!("unknown tool {other}")),
    }
}

fn result(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(line: &str) -> Value {
        match handle(line) {
            Step::Reply(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_handshake_and_the_list_need_nothing_but_the_line() {
        let init = |v: &str| {
            let line = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                              "params": {"protocolVersion": v}});
            reply(&line.to_string())["result"]["protocolVersion"].clone()
        };
        assert_eq!(init("2024-11-05"), "2024-11-05");
        assert_eq!(init("2025-03-26"), "2025-06-18");
        let quiet = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(handle(quiet), Step::Quiet);
        let list = reply(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let six = [
            "status",
            "start",
            "stop",
            "project_on",
            "project_off",
            "finished_work",
        ];
        assert_eq!(names, six);
        assert_eq!(reply("not json")["error"]["code"], PARSE_ERROR);
        assert_eq!(reply("not json")["id"], Value::Null);
        let unknown = reply(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#);
        assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);
        let huge = json!({"jsonrpc": "2.0", "id": 4, "method": "m".repeat(100_000)});
        let echoed = reply(&huge.to_string())["error"]["message"].to_string();
        assert!(echoed.len() < 100, "{}", echoed.len());
    }

    #[test]
    fn a_call_names_its_tool_or_is_refused_for_bad_arguments() {
        let call = |params: Value| {
            handle(
                &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": params})
                    .to_string(),
            )
        };
        assert_eq!(
            call(json!({"name": "project_on", "arguments": {"name": "app"}})),
            Step::Call {
                id: json!(4),
                tool: Tool::ProjectOn("app".into())
            }
        );
        assert_eq!(
            call(json!({"name": "status"})),
            Step::Call {
                id: json!(4),
                tool: Tool::Status
            }
        );
        for bad in [
            json!({"name": "project_on", "arguments": {}}),
            json!({"name": "project_off", "arguments": {"name": "app", "x": 1}}),
            json!({"name": "stop", "arguments": {"all": true}}),
            json!({"name": "rm", "arguments": {}}),
        ] {
            let Step::Reply(text) = call(bad.clone()) else {
                panic!("{bad}");
            };
            let v: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["error"]["code"], INVALID_PARAMS, "{bad}");
        }
        let done: Value = serde_json::from_str(&tool_result(&json!(4), "ok", true)).unwrap();
        assert_eq!(done["result"]["isError"], true);
        assert_eq!(done["result"]["content"][0]["text"], "ok");
    }
}
