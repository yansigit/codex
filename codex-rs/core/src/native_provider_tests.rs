use super::{call_item, messages, tool_result, tools};
use crate::client_common::Prompt;
use codex_protocol::models::{FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem};
use codex_provider_api as api;
use codex_tools::{
    FreeformTool, FreeformToolFormat, JsonSchema, ResponsesApiNamespace, ResponsesApiNamespaceTool,
    ResponsesApiTool, ToolSpec,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn item(value: Value) -> ResponseItem {
    serde_json::from_value(value).expect("valid response item")
}

#[test]
fn messages_map_roles_images_and_adjacent_assistant_items() {
    let input = vec![
        item(
            json!({"type":"message","role":"system","content":[{"type":"input_text","text":"rules"}]}),
        ),
        item(json!({"type":"message","role":"user","content":[
            {"type":"input_text","text":"look"},
            {"type":"input_image","image_url":"data:image/png;base64,AA=="}
        ]})),
        item(
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"checking"}]}),
        ),
        item(
            json!({"type":"reasoning","summary":[{"type":"summary_text","text":"thought"}],"encrypted_content":null}),
        ),
        item(
            json!({"type":"function_call","call_id":"c1","name":"read","arguments":"{\"path\":\"a\"}"}),
        ),
    ];

    let mapped = messages(&input).expect("messages convert");
    assert_eq!(mapped.len(), 3);
    assert_eq!(mapped[0].role, api::Role::System);
    assert_eq!(
        mapped[0].content,
        vec![api::Content::Text {
            text: "rules".into()
        }]
    );
    assert_eq!(mapped[1].role, api::Role::User);
    assert_eq!(
        mapped[1].content,
        vec![
            api::Content::Text {
                text: "look".into()
            },
            api::Content::Image {
                url: "data:image/png;base64,AA==".into()
            },
        ]
    );
    assert_eq!(mapped[2].role, api::Role::Assistant);
    assert_eq!(
        mapped[2].content,
        vec![
            api::Content::Text {
                text: "checking".into()
            },
            api::Content::Thinking {
                text: "thought".into(),
                signature: None
            },
            api::Content::ToolCall(api::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: json!({"path":"a"}),
            }),
        ]
    );
}

#[test]
fn tools_map_namespaced_functions_and_custom_tools() {
    let function = ResponsesApiTool {
        name: "lookup".into(),
        description: "find a thing".into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::default(),
        output_schema: None,
    };
    let custom = FreeformTool {
        name: "script".into(),
        description: "run script".into(),
        defer_loading: None,
        format: FreeformToolFormat {
            r#type: "text".into(),
            syntax: "plain".into(),
            definition: "...".into(),
        },
    };
    let mut prompt = Prompt::default();
    prompt.tools = Arc::from([ToolSpec::Namespace(ResponsesApiNamespace {
        name: "local".into(),
        description: String::new(),
        tools: vec![
            ResponsesApiNamespaceTool::Function(function),
            ResponsesApiNamespaceTool::Custom(custom),
        ],
    })]);

    let (definitions, targets) = tools(&prompt).expect("tools convert");
    assert_eq!(
        definitions
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["local__lookup", "local__script"]
    );
    assert_eq!(targets["local__lookup"].namespace.as_deref(), Some("local"));
    assert!(!targets["local__lookup"].custom);
    assert!(targets["local__script"].custom);
    assert_eq!(
        definitions[1].parameters,
        json!({
            "type":"object", "properties":{"input":{"type":"string"}},
            "required":["input"], "additionalProperties":false
        })
    );

    let custom_call = call_item(
        api::ToolCall {
            id: "c2".into(),
            name: "local__script".into(),
            arguments: json!({"input":"print(1)"}),
        },
        &targets,
    )
    .expect("custom call maps back to the Responses item");
    assert_eq!(
        serde_json::to_value(custom_call).expect("serialize mapped call"),
        json!({
            "type":"custom_tool_call", "call_id":"c2", "name":"script",
            "namespace":"local", "input":"print(1)"
        })
    );
}

#[test]
fn failed_tool_outputs_remain_marked_as_errors() {
    let failed = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some("c1".into()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("denied".into()),
            success: Some(false),
        },
        internal_chat_message_metadata_passthrough: None,
    };
    let converted = tool_result(&failed).expect("tool output converts");
    assert_eq!(converted.call_id, "c1");
    assert!(converted.is_error);
    assert_eq!(converted.output, json!("denied"));

    let mapped = messages(&[failed]).expect("history output converts");
    assert_eq!(mapped[0].role, api::Role::Tool);
    assert!(matches!(&mapped[0].content[0], api::Content::ToolResult(result) if result.is_error));
}

#[test]
fn unsupported_history_and_content_formats_are_rejected() {
    let audio = item(
        json!({"type":"message","role":"user","content":[{"type":"input_audio","audio_url":"data:audio/wav;base64,AA=="}]}),
    );
    assert!(messages(&[audio]).is_err());

    let agent_message = item(
        json!({"type":"agent_message","author":"a","recipient":"b","content":[{"type":"input_text","text":"opaque"}]}),
    );
    assert!(messages(&[agent_message]).is_err());

    let malformed_call =
        item(json!({"type":"function_call","call_id":"c1","name":"read","arguments":"not json"}));
    assert!(messages(&[malformed_call]).is_err());
}
