use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::domain::anthropic::{AnthropicMessagesRequest, AnthropicToolChoice};
use crate::domain::openai::ChatCompletionsRequest;

#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    schemas: HashMap<String, Value>,
    declared_tools: HashSet<String>,
    display_names: HashMap<String, String>,
    tool_choice_required: bool,
}

impl ToolRegistry {
    pub fn from_anthropic_request(
        req: &AnthropicMessagesRequest,
        aliases: Option<&HashMap<String, String>>,
    ) -> Option<Self> {
        let mut schemas = HashMap::new();
        let mut declared_tools = HashSet::new();
        let mut display_names = HashMap::new();
        if let Some(tools) = &req.tools {
            for tool in tools {
                if let Some(name) = infer_anthropic_tool_name(tool) {
                    let canonical_name = apply_tool_alias(&name, aliases);
                    declared_tools.insert(canonical_name.clone());
                    display_names.insert(canonical_name.clone(), name);
                    if let Some(schema) = tool.input_schema.clone() {
                        schemas.insert(canonical_name, schema);
                    }
                }
            }
        }

        let tool_choice_required = anthropic_tool_choice_requires_tool(req.tool_choice.as_ref());
        if declared_tools.is_empty() && !tool_choice_required {
            return None;
        }

        Some(Self {
            schemas,
            declared_tools,
            display_names,
            tool_choice_required,
        })
    }

    pub fn from_openai_request(req: &ChatCompletionsRequest) -> Option<Self> {
        let mut schemas = HashMap::new();
        let mut declared_tools = HashSet::new();

        if let Some(tools) = &req.tools {
            for tool in tools {
                declared_tools.insert(tool.function.name.clone());
                if let Some(schema) = tool.function.parameters.clone() {
                    schemas.insert(tool.function.name.clone(), schema);
                }
            }
        }

        if let Some(functions) = &req.functions {
            for function in functions {
                declared_tools.insert(function.name.clone());
                if let Some(schema) = function.parameters.clone() {
                    schemas.insert(function.name.clone(), schema);
                }
            }
        }

        let tool_choice_required = openai_tool_choice_requires_tool(req.tool_choice.as_ref());
        if declared_tools.is_empty() && !tool_choice_required {
            return None;
        }

        Some(Self {
            schemas,
            declared_tools,
            display_names: HashMap::new(),
            tool_choice_required,
        })
    }

    pub fn schema_for(&self, tool_name: &str) -> Option<&Value> {
        if let Some(schema) = self.schemas.get(tool_name) {
            return Some(schema);
        }

        if let Some((_, schema)) = self
            .schemas
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(tool_name))
        {
            return Some(schema);
        }

        let wanted = canonicalize_tool_name(tool_name);
        if wanted.is_empty() {
            return None;
        }

        self.schemas
            .iter()
            .find(|(name, _)| canonicalize_tool_name(name) == wanted)
            .map(|(_, schema)| schema)
    }

    pub fn has_declared_tools(&self) -> bool {
        !self.declared_tools.is_empty()
    }

    pub fn knows_tool(&self, tool_name: &str) -> bool {
        if self.declared_tools.contains(tool_name) {
            return true;
        }

        if self
            .declared_tools
            .iter()
            .any(|name| name.eq_ignore_ascii_case(tool_name))
        {
            return true;
        }

        let wanted = canonicalize_tool_name(tool_name);
        if wanted.is_empty() {
            return false;
        }

        self.declared_tools
            .iter()
            .any(|name| canonicalize_tool_name(name) == wanted)
    }

    pub fn tool_choice_required(&self) -> bool {
        self.tool_choice_required
    }

    pub fn display_name_for(&self, tool_name: &str) -> Option<String> {
        if let Some(name) = self.display_names.get(tool_name) {
            return Some(name.clone());
        }

        if let Some((_, name)) = self
            .display_names
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(tool_name))
        {
            return Some(name.clone());
        }

        let wanted = canonicalize_tool_name(tool_name);
        if wanted.is_empty() {
            return None;
        }

        self.display_names
            .iter()
            .find(|(candidate, _)| canonicalize_tool_name(candidate) == wanted)
            .map(|(_, display)| display.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolArgumentSource {
    Delta,
    Done,
    None,
}

impl ToolArgumentSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Delta => "delta",
            Self::Done => "done",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolCallDecision {
    pub call_id: String,
    pub tool_name: String,
    pub display_tool_name: String,
    pub input_json: String,
    pub input_value: Option<Value>,
    pub source: ToolArgumentSource,
    pub json_valid: bool,
    pub schema_valid: bool,
    pub emit: bool,
    pub reason: String,
}

#[derive(Debug, Clone)]
struct ToolCallState {
    call_id: String,
    tool_name: String,
    delta_buffer: String,
    done_arguments: Option<String>,
    finalized: bool,
}

#[derive(Debug, Clone)]
pub struct ToolCallAssembler {
    registry: Option<ToolRegistry>,
    states: HashMap<String, ToolCallState>,
    insertion_order: Vec<String>,
}

impl ToolCallAssembler {
    pub fn new(registry: Option<ToolRegistry>) -> Self {
        Self {
            registry,
            states: HashMap::new(),
            insertion_order: Vec::new(),
        }
    }

    pub fn on_start(&mut self, call_id: String, tool_name: String) {
        let state = self.ensure_state(call_id.clone());
        if !tool_name.is_empty() {
            state.tool_name = tool_name;
        }
    }

    pub fn on_delta(&mut self, call_id: String, delta: String) {
        let state = self.ensure_state(call_id);
        state.delta_buffer.push_str(&delta);
    }

    pub fn on_done(&mut self, call_id: String, tool_name: String, arguments: String) {
        let state = self.ensure_state(call_id);
        if !tool_name.is_empty() {
            state.tool_name = tool_name;
        }
        state.done_arguments = Some(arguments);
    }

    pub fn finalize_call(&mut self, call_id: &str) -> Option<ToolCallDecision> {
        self.finalize_state(call_id, false)
    }

    pub fn finalize_all(&mut self) -> Vec<ToolCallDecision> {
        let mut out = Vec::new();
        let order = self.insertion_order.clone();
        for call_id in order {
            if let Some(decision) = self.finalize_state(&call_id, true) {
                out.push(decision);
            }
        }
        out
    }

    fn ensure_state(&mut self, call_id: String) -> &mut ToolCallState {
        if !self.states.contains_key(&call_id) {
            self.insertion_order.push(call_id.clone());
            self.states.insert(
                call_id.clone(),
                ToolCallState {
                    call_id: call_id.clone(),
                    tool_name: "unknown".to_string(),
                    delta_buffer: String::new(),
                    done_arguments: None,
                    finalized: false,
                },
            );
        }
        self.states.get_mut(&call_id).expect("state")
    }

    fn finalize_state(&mut self, call_id: &str, force: bool) -> Option<ToolCallDecision> {
        let state = self.states.get_mut(call_id)?;
        if state.finalized {
            return None;
        }
        if !force && state.done_arguments.is_none() {
            return None;
        }
        state.finalized = true;

        let mut source = ToolArgumentSource::None;
        let mut parsed_input: Option<Value> = None;
        let mut selected_json = String::new();
        let mut json_valid = false;
        let mut reason = "missing_tool_arguments".to_string();

        if !state.delta_buffer.trim().is_empty() {
            source = ToolArgumentSource::Delta;
            selected_json = state.delta_buffer.clone();
            match serde_json::from_str::<Value>(&state.delta_buffer) {
                Ok(value) => {
                    parsed_input = Some(value);
                    json_valid = true;
                }
                Err(err) => {
                    reason = format!("delta_invalid_json:{err}");
                }
            }
        }

        if parsed_input.is_none() {
            if let Some(done_args) = state.done_arguments.as_ref() {
                source = ToolArgumentSource::Done;
                selected_json = done_args.clone();
                match serde_json::from_str::<Value>(done_args) {
                    Ok(value) => {
                        parsed_input = Some(value);
                        json_valid = true;
                        reason = String::new();
                    }
                    Err(err) => {
                        reason = format!("done_invalid_json:{err}");
                    }
                }
            }
        }

        let mut schema_valid = true;
        if let Some(input_value) = parsed_input.as_ref() {
            if let Some(registry) = self.registry.as_ref() {
                if let Some(schema) = registry.schema_for(&state.tool_name) {
                    if let Err(err) = validate_against_schema(schema, input_value, "$") {
                        schema_valid = false;
                        append_reason(&mut reason, &format!("schema_validation_failed:{err}"));
                    }
                } else if registry.knows_tool(&state.tool_name) || registry.has_declared_tools() {
                    schema_valid = false;
                    append_reason(&mut reason, "schema_not_found");
                } else if registry.tool_choice_required() {
                    schema_valid = false;
                    append_reason(&mut reason, "schema_required_but_missing");
                }
            }
        } else {
            schema_valid = false;
        }

        if !schema_valid && reason.is_empty() {
            reason = "invalid_tool_parameters".to_string();
        }

        let emit = parsed_input.is_some() && schema_valid;
        let display_tool_name = self
            .registry
            .as_ref()
            .and_then(|registry| registry.display_name_for(&state.tool_name))
            .unwrap_or_else(|| state.tool_name.clone());
        let input_json = if emit {
            serde_json::to_string(parsed_input.as_ref().expect("input"))
                .unwrap_or_else(|_| "{}".to_string())
        } else if selected_json.is_empty() {
            "{}".to_string()
        } else {
            selected_json
        };

        if emit && reason.is_empty() {
            reason = "ok".to_string();
        }

        Some(ToolCallDecision {
            call_id: state.call_id.clone(),
            tool_name: state.tool_name.clone(),
            display_tool_name,
            input_json,
            input_value: parsed_input,
            source,
            json_valid,
            schema_valid,
            emit,
            reason,
        })
    }
}

fn apply_tool_alias(name: &str, aliases: Option<&HashMap<String, String>>) -> String {
    let Some(aliases) = aliases else {
        return name.to_string();
    };

    aliases
        .get(name)
        .cloned()
        .or_else(|| {
            aliases
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        })
        .unwrap_or_else(|| name.to_string())
}

fn append_reason(reason: &mut String, value: &str) {
    if value.is_empty() {
        return;
    }
    if reason.is_empty() {
        *reason = value.to_string();
        return;
    }
    reason.push('|');
    reason.push_str(value);
}

fn canonicalize_tool_name(name: &str) -> String {
    let mut normalized = name.trim().to_ascii_lowercase();
    if let Some((prefix, suffix)) = normalized.rsplit_once('_') {
        if suffix.chars().all(|c| c.is_ascii_digit()) {
            normalized = prefix.to_string();
        }
    }

    normalized
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect()
}

pub fn protocol_debug_enabled() -> bool {
    std::env::var("LOG_PROTOCOL_DEBUG")
        .ok()
        .map(|v| {
            let v = v.to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
}

pub fn log_tool_decision(
    trace_id: &str,
    request_id: &str,
    response_id: Option<&str>,
    phase: &str,
    decision: &ToolCallDecision,
) {
    if !protocol_debug_enabled() {
        return;
    }

    let response_id = response_id.unwrap_or("-");
    log::info!(
        "[protocol-debug] trace_id={trace_id} request_id={request_id} response_id={response_id} phase={phase} call_id={} tool_name={} source={} json_valid={} schema_valid={} emit={} reason={} args_len={}",
        decision.call_id,
        decision.tool_name,
        decision.source.as_str(),
        decision.json_valid,
        decision.schema_valid,
        decision.emit,
        truncate_debug(&decision.reason, 120),
        decision.input_json.len(),
    );
}

fn truncate_debug(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    format!("{}...", &value[..max])
}

fn infer_anthropic_tool_name(tool: &crate::domain::anthropic::AnthropicTool) -> Option<String> {
    if let Some(name) = tool.name.as_ref().filter(|name| !name.trim().is_empty()) {
        return Some(name.clone());
    }

    let raw_type = tool.tool_type.as_ref()?.trim();
    if raw_type.is_empty() {
        return None;
    }

    let inferred = raw_type
        .rsplit_once('_')
        .and_then(|(prefix, suffix)| {
            if suffix.chars().all(|c| c.is_ascii_digit()) {
                Some(prefix.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| raw_type.to_string());

    if inferred.is_empty() {
        None
    } else {
        Some(inferred)
    }
}

fn anthropic_tool_choice_requires_tool(choice: Option<&AnthropicToolChoice>) -> bool {
    match choice {
        Some(AnthropicToolChoice::Simple(v)) => {
            let v = v.to_ascii_lowercase();
            v == "any" || v == "tool" || v == "required"
        }
        Some(AnthropicToolChoice::Object(v)) => {
            let t = v.choice_type.to_ascii_lowercase();
            t == "any" || t == "tool" || t == "required"
        }
        None => false,
    }
}

fn openai_tool_choice_requires_tool(choice: Option<&Value>) -> bool {
    let Some(choice) = choice else {
        return false;
    };

    if let Some(strategy) = choice.as_str() {
        let strategy = strategy.to_ascii_lowercase();
        return strategy == "required" || strategy == "any";
    }

    let Some(obj) = choice.as_object() else {
        return false;
    };

    obj.get("type")
        .and_then(Value::as_str)
        .map(|v| v.eq_ignore_ascii_case("function"))
        .unwrap_or(false)
}

fn validate_against_schema(schema: &Value, input: &Value, path: &str) -> Result<(), String> {
    validate_against_schema_with_root(schema, schema, input, path, 0)
}

fn validate_against_schema_with_root(
    root_schema: &Value,
    schema: &Value,
    input: &Value,
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if depth > 64 {
        return Err(format!("{path}: schema recursion depth exceeded"));
    }

    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let resolved = resolve_local_ref(root_schema, reference)
            .ok_or_else(|| format!("{path}: unresolved_ref:{reference}"))?;
        validate_against_schema_with_root(root_schema, resolved, input, path, depth + 1)?;
    }

    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        if any_of.iter().any(|entry| {
            validate_against_schema_with_root(root_schema, entry, input, path, depth + 1).is_ok()
        }) {
            return Ok(());
        }
        return Err(format!("{path}: anyOf validation failed"));
    }

    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        let valid_count = one_of
            .iter()
            .filter(|entry| {
                validate_against_schema_with_root(root_schema, entry, input, path, depth + 1)
                    .is_ok()
            })
            .count();
        if valid_count == 1 {
            return Ok(());
        }
        return Err(format!("{path}: oneOf validation failed"));
    }

    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        for entry in all_of {
            validate_against_schema_with_root(root_schema, entry, input, path, depth + 1)?;
        }
    }

    if let Some(const_value) = schema.get("const") {
        if const_value != input {
            return Err(format!("{path}: const mismatch"));
        }
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|entry| entry == input) {
            return Err(format!("{path}: enum mismatch"));
        }
    }

    if let Some(schema_type) = schema.get("type") {
        validate_type(schema_type, input, path)?;
    }

    if schema
        .get("type")
        .and_then(Value::as_str)
        .map(|t| t == "object")
        .unwrap_or(false)
        || schema.get("properties").is_some()
        || schema.get("required").is_some()
    {
        validate_object(root_schema, schema, input, path, depth + 1)?;
    }

    if schema
        .get("type")
        .and_then(Value::as_str)
        .map(|t| t == "array")
        .unwrap_or(false)
        || schema.get("items").is_some()
    {
        validate_array(root_schema, schema, input, path, depth + 1)?;
    }

    Ok(())
}

fn validate_type(schema_type: &Value, input: &Value, path: &str) -> Result<(), String> {
    let is_match = match schema_type {
        Value::String(kind) => type_matches(kind, input),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .any(|kind| type_matches(kind, input)),
        _ => true,
    };

    if is_match {
        Ok(())
    } else {
        Err(format!("{path}: type mismatch"))
    }
}

fn validate_object(
    root_schema: &Value,
    schema: &Value,
    input: &Value,
    path: &str,
    depth: usize,
) -> Result<(), String> {
    let Some(input_object) = input.as_object() else {
        return Err(format!("{path}: expected object"));
    };

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !input_object.contains_key(key) {
                return Err(format!("{path}.{key}: required field missing"));
            }
        }
    }

    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(properties) = properties {
        for (key, value) in input_object {
            if let Some(property_schema) = properties.get(key) {
                validate_against_schema_with_root(
                    root_schema,
                    property_schema,
                    value,
                    &format!("{path}.{key}"),
                    depth + 1,
                )?;
            }
        }

        let allow_additional = schema
            .get("additionalProperties")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !allow_additional {
            for key in input_object.keys() {
                if !properties.contains_key(key) {
                    return Err(format!("{path}.{key}: additional property not allowed"));
                }
            }
        }
    }

    Ok(())
}

fn validate_array(
    root_schema: &Value,
    schema: &Value,
    input: &Value,
    path: &str,
    depth: usize,
) -> Result<(), String> {
    let Some(items) = schema.get("items") else {
        return Ok(());
    };
    let Some(values) = input.as_array() else {
        return Err(format!("{path}: expected array"));
    };

    for (idx, value) in values.iter().enumerate() {
        validate_against_schema_with_root(
            root_schema,
            items,
            value,
            &format!("{path}[{idx}]"),
            depth + 1,
        )?;
    }

    Ok(())
}

fn resolve_local_ref<'a>(root_schema: &'a Value, reference: &str) -> Option<&'a Value> {
    if reference == "#" {
        return Some(root_schema);
    }

    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root_schema);
    }

    root_schema.pointer(pointer)
}

fn type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::anthropic::{AnthropicMessagesRequest, AnthropicTool, AnthropicToolChoice};
    use crate::domain::openai::{
        ChatCompletionsRequest, OpenAIMessage, OpenAITool, OpenAIToolFunction,
    };

    fn registry_with_required_path() -> ToolRegistry {
        ToolRegistry {
            schemas: HashMap::from([(
                "read_file".to_string(),
                json!({
                    "type":"object",
                    "properties": {
                        "path": { "type":"string" }
                    },
                    "required": ["path"]
                }),
            )]),
            declared_tools: HashSet::from(["read_file".to_string()]),
            display_names: HashMap::new(),
            tool_choice_required: true,
        }
    }

    #[test]
    fn assembler_prefers_delta_when_valid() {
        let mut assembler = ToolCallAssembler::new(Some(registry_with_required_path()));
        assembler.on_start("c1".to_string(), "read_file".to_string());
        assembler.on_delta("c1".to_string(), "{\"path\":\"README.md\"}".to_string());
        assembler.on_done(
            "c1".to_string(),
            "read_file".to_string(),
            "{\"path\":\"X\"}".to_string(),
        );

        let result = assembler.finalize_call("c1").expect("decision");
        assert!(result.emit);
        assert_eq!(result.source, ToolArgumentSource::Delta);
        assert_eq!(result.input_value, Some(json!({"path":"README.md"})));
    }

    #[test]
    fn assembler_falls_back_to_done_when_delta_invalid() {
        let mut assembler = ToolCallAssembler::new(Some(registry_with_required_path()));
        assembler.on_start("c1".to_string(), "read_file".to_string());
        assembler.on_delta("c1".to_string(), "{\"path\":".to_string());
        assembler.on_done(
            "c1".to_string(),
            "read_file".to_string(),
            "{\"path\":\"README.md\"}".to_string(),
        );

        let result = assembler.finalize_call("c1").expect("decision");
        assert!(result.emit);
        assert_eq!(result.source, ToolArgumentSource::Done);
    }

    #[test]
    fn assembler_supports_done_only_function_call() {
        let mut assembler = ToolCallAssembler::new(Some(registry_with_required_path()));
        assembler.on_start("c1".to_string(), "read_file".to_string());
        assembler.on_done(
            "c1".to_string(),
            "read_file".to_string(),
            "{\"path\":\"README.md\"}".to_string(),
        );

        let result = assembler.finalize_call("c1").expect("decision");
        assert!(result.emit);
        assert_eq!(result.source, ToolArgumentSource::Done);
        assert_eq!(result.input_value, Some(json!({"path":"README.md"})));
    }

    #[test]
    fn assembler_skips_when_both_delta_and_done_are_invalid() {
        let mut assembler = ToolCallAssembler::new(Some(registry_with_required_path()));
        assembler.on_start("c1".to_string(), "read_file".to_string());
        assembler.on_delta("c1".to_string(), "{\"path\":".to_string());
        assembler.on_done("c1".to_string(), "read_file".to_string(), "{".to_string());

        let result = assembler.finalize_call("c1").expect("decision");
        assert!(!result.emit);
        assert!(!result.json_valid);
    }

    #[test]
    fn assembler_skips_when_schema_validation_fails() {
        let mut assembler = ToolCallAssembler::new(Some(registry_with_required_path()));
        assembler.on_start("c1".to_string(), "read_file".to_string());
        assembler.on_done(
            "c1".to_string(),
            "read_file".to_string(),
            "{\"x\":1}".to_string(),
        );

        let result = assembler.finalize_call("c1").expect("decision");
        assert!(!result.emit);
        assert!(result.json_valid);
        assert!(!result.schema_valid);
    }

    #[test]
    fn assembler_handles_interleaved_calls() {
        let mut assembler = ToolCallAssembler::new(Some(registry_with_required_path()));
        assembler.on_start("c1".to_string(), "read_file".to_string());
        assembler.on_start("c2".to_string(), "read_file".to_string());
        assembler.on_delta("c1".to_string(), "{\"path\":\"A\"}".to_string());
        assembler.on_done(
            "c2".to_string(),
            "read_file".to_string(),
            "{\"path\":\"B\"}".to_string(),
        );
        assembler.on_done(
            "c1".to_string(),
            "read_file".to_string(),
            "{\"path\":\"C\"}".to_string(),
        );

        let r2 = assembler.finalize_call("c2").expect("c2");
        let r1 = assembler.finalize_call("c1").expect("c1");
        assert!(r1.emit && r2.emit);
    }

    #[test]
    fn assembler_force_finalize_when_stream_completes_before_done() {
        let mut assembler = ToolCallAssembler::new(Some(registry_with_required_path()));
        assembler.on_start("c1".to_string(), "read_file".to_string());
        assembler.on_delta("c1".to_string(), "{\"path\":\"README.md\"}".to_string());

        let all = assembler.finalize_all();
        assert_eq!(all.len(), 1);
        assert!(all[0].emit);
    }

    #[test]
    fn builds_registry_from_anthropic_request() {
        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![],
            system: None,
            tools: Some(vec![AnthropicTool {
                name: Some("read_file".to_string()),
                description: None,
                input_schema: Some(json!({"type":"object"})),
                tool_type: None,
            }]),
            tool_choice: Some(AnthropicToolChoice::Simple("any".to_string())),
            stream: Some(true),
            thinking: None,
        };

        let registry = ToolRegistry::from_anthropic_request(&request, None).expect("registry");
        assert!(registry.schema_for("read_file").is_some());
        assert!(registry.tool_choice_required());
    }

    #[test]
    fn builds_registry_from_openai_request() {
        let request = ChatCompletionsRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: Some(crate::domain::openai::OpenAIContent::Text("hi".to_string())),
                tool_calls: None,
                function_call: None,
                tool_call_id: None,
                name: None,
            }],
            stream: Some(true),
            tools: Some(vec![OpenAITool {
                _tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: "read_file".to_string(),
                    description: None,
                    parameters: Some(
                        json!({"type":"object","properties":{"path":{"type":"string"}}}),
                    ),
                },
            }]),
            tool_choice: Some(json!("required")),
            functions: None,
            reasoning_effort: None,
            response_format: None,
        };

        let registry = ToolRegistry::from_openai_request(&request).expect("registry");
        assert!(registry.schema_for("read_file").is_some());
        assert!(registry.tool_choice_required());
    }

    #[test]
    fn registry_schema_lookup_is_case_insensitive_and_normalized() {
        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![],
            system: None,
            tools: Some(vec![AnthropicTool {
                name: Some("Grep_20250124".to_string()),
                description: None,
                input_schema: Some(json!({
                    "type":"object",
                    "required":["pattern"],
                    "properties": {"pattern": {"type":"string"}}
                })),
                tool_type: None,
            }]),
            tool_choice: Some(AnthropicToolChoice::Simple("any".to_string())),
            stream: Some(true),
            thinking: None,
        };

        let registry = ToolRegistry::from_anthropic_request(&request, None).expect("registry");
        assert!(registry.schema_for("grep").is_some());
        assert!(registry.schema_for("GREP").is_some());
        assert!(registry.schema_for("grep_20250124").is_some());
    }

    #[test]
    fn assembler_skips_when_registry_has_no_matching_schema() {
        let mut assembler = ToolCallAssembler::new(Some(registry_with_required_path()));
        assembler.on_start("c1".to_string(), "grep".to_string());
        assembler.on_done(
            "c1".to_string(),
            "grep".to_string(),
            "{\"pattern\":\"todo\"}".to_string(),
        );

        let result = assembler.finalize_call("c1").expect("decision");
        assert!(!result.emit);
        assert!(!result.schema_valid);
        assert!(result.reason.contains("schema_not_found"));
    }

    #[test]
    fn assembler_validates_schema_with_local_ref() {
        let registry = ToolRegistry {
            schemas: HashMap::from([(
                "grep".to_string(),
                json!({
                    "$ref": "#/$defs/GrepInput",
                    "$defs": {
                        "GrepInput": {
                            "type":"object",
                            "required":["pattern"],
                            "properties": {
                                "pattern": {"type":"string"},
                                "path": {"type":"string"}
                            }
                        }
                    }
                }),
            )]),
            declared_tools: HashSet::from(["grep".to_string()]),
            display_names: HashMap::new(),
            tool_choice_required: true,
        };

        let mut assembler = ToolCallAssembler::new(Some(registry));
        assembler.on_start("c1".to_string(), "grep".to_string());
        assembler.on_done(
            "c1".to_string(),
            "grep".to_string(),
            "{\"path\":\".\"}".to_string(),
        );

        let result = assembler.finalize_call("c1").expect("decision");
        assert!(!result.emit);
        assert!(!result.schema_valid);
        assert!(result.reason.contains("schema_validation_failed"));
    }

    #[test]
    fn registry_keeps_display_name_for_aliased_anthropic_tools() {
        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![],
            system: None,
            tools: Some(vec![AnthropicTool {
                name: Some("ReadFile".to_string()),
                description: None,
                input_schema: Some(json!({"type":"object"})),
                tool_type: None,
            }]),
            tool_choice: Some(AnthropicToolChoice::Simple("any".to_string())),
            stream: Some(true),
            thinking: None,
        };
        let aliases = HashMap::from([("ReadFile".to_string(), "read_file".to_string())]);

        let registry =
            ToolRegistry::from_anthropic_request(&request, Some(&aliases)).expect("registry");
        assert_eq!(
            registry.display_name_for("read_file").as_deref(),
            Some("ReadFile")
        );
    }
}
