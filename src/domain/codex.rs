use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct CodexResponsesRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<CodexInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<CodexToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<CodexToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    pub store: bool,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<CodexTextFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodexInputItem {
    Message {
        role: String,
        content: CodexMessageContent,
    },
    FunctionCall {
        #[serde(rename = "type")]
        item_type: String,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        item_type: String,
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodexMessageContent {
    Text(String),
    Parts(Vec<CodexContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodexContentPart {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodexToolChoice {
    Strategy(String),
    Function {
        #[serde(rename = "type")]
        choice_type: String,
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTextFormat {
    pub format: CodexTextFormatType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTextFormatType {
    #[serde(rename = "type")]
    pub format_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodexUsage {
    #[serde(default)]
    pub input_tokens: i32,
    #[serde(default)]
    pub output_tokens: i32,
    #[serde(default)]
    pub cached_tokens: Option<i32>,
    #[serde(default)]
    pub reasoning_tokens: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct FunctionCallStart {
    pub call_id: String,
    pub name: String,
    pub _output_index: usize,
}

#[derive(Debug, Clone)]
pub struct FunctionCallDelta {
    pub call_id: String,
    pub delta: String,
}

#[derive(Debug, Clone)]
pub struct FunctionCallDone {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ExtractedCodexEvent {
    pub _event_type: String,
    pub response_id: Option<String>,
    pub text_delta: Option<String>,
    pub reasoning_delta: Option<String>,
    pub usage: Option<CodexUsage>,
    pub function_call_start: Option<FunctionCallStart>,
    pub function_call_delta: Option<FunctionCallDelta>,
    pub function_call_done: Option<FunctionCallDone>,
    pub error: Option<CodexErrorEvent>,
    pub is_done: bool,
}

impl ExtractedCodexEvent {
    pub fn empty(event_type: String) -> Self {
        Self {
            _event_type: event_type,
            response_id: None,
            text_delta: None,
            reasoning_delta: None,
            usage: None,
            function_call_start: None,
            function_call_delta: None,
            function_call_done: None,
            error: None,
            is_done: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodexErrorEvent {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum ParsedSseEvent {
    Json {
        event: Option<String>,
        payload: Value,
    },
    Done,
}
