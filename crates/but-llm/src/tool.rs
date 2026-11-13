//! Tool-calling abstractions used by the providers' tool-calling loops.
//!
//! A [`Tool`] is a named function with a JSON-schema description that an LLM may call.
//! A [`Toolset`] is a registry of tools that dispatches calls by name.
//! Neither depends on any application context: tools capture whatever state they need themselves.
use std::{collections::BTreeMap, sync::Arc};

use serde_json::json;

/// A registry of [`Tool`]s which the LLM may call.
pub trait Toolset {
    /// Register `tool` under its [`name()`](Tool::name), replacing any previous tool of that name.
    fn register_tool<T: Tool>(&mut self, tool: T);
    /// Return the tool called `name`, if any.
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;
    /// Return all registered tools.
    fn list(&self) -> Vec<Arc<dyn Tool>>;
    /// Call the tool `name` with the JSON-encoded `parameters`.
    ///
    /// Failures are reported as a JSON object with an `error` field, so they can be handed back to the LLM.
    fn call_tool(&mut self, name: &str, parameters: &str) -> serde_json::Value;
}

/// A [`Toolset`] that holds its tools in a sorted map.
#[derive(Default)]
pub struct BasicToolset {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl BasicToolset {
    /// Create an empty toolset.
    pub fn new() -> Self {
        Self::default()
    }

    fn call_tool_inner(&self, name: &str, parameters: &str) -> anyhow::Result<serde_json::Value> {
        let tool = self
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Tool '{name}' not found"))?;
        let params: serde_json::Value = serde_json::from_str(parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse parameters: {e}"))?;
        tool.call(params)
    }
}

impl Toolset for BasicToolset {
    fn register_tool<T: Tool>(&mut self, tool: T) {
        self.tools.insert(tool.name(), Arc::new(tool));
    }

    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    fn list(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.values().cloned().collect()
    }

    fn call_tool(&mut self, name: &str, parameters: &str) -> serde_json::Value {
        self.call_tool_inner(name, parameters).unwrap_or_else(|e| {
            json!({
                "error": format!("Failed to call tool '{name}': {e}")
            })
        })
    }
}

/// A function the LLM can call.
pub trait Tool: 'static + Send + Sync {
    /// The unique name the LLM refers to the tool by.
    fn name(&self) -> String;
    /// A description of what the tool does, shown to the LLM.
    fn description(&self) -> String;
    /// The JSON schema of the tool's parameters.
    fn parameters(&self) -> serde_json::Value;
    /// Run the tool with the already-parsed `parameters`.
    fn call(self: Arc<Self>, parameters: serde_json::Value) -> anyhow::Result<serde_json::Value>;
}

/// Convert `error` to the JSON shape returned to the LLM when `action_identifier` failed.
pub fn error_to_json(error: &anyhow::Error, action_identifier: &str) -> serde_json::Value {
    json!({
        "error": format!("Failed to {action_identifier}: {error}")
    })
}

/// Convert a string `result` to JSON, or an error object mentioning `action_identifier`.
pub fn string_result_to_json(
    result: &Result<String, &anyhow::Error>,
    action_identifier: &str,
) -> serde_json::Value {
    match result {
        Ok(value) => json!({ "result": value }),
        Err(e) => error_to_json(e, action_identifier),
    }
}

/// Convert a list-of-strings `result` to JSON, or an error object mentioning `action_identifier`.
pub fn string_vec_result_to_json(
    result: &Result<Vec<String>, &anyhow::Error>,
    action_identifier: &str,
) -> serde_json::Value {
    match result {
        Ok(values) => json!({ "result": values }),
        Err(e) => error_to_json(e, action_identifier),
    }
}

/// Convert any serializable `result` to JSON, or an error object mentioning `action_identifier`.
/// `data_identifier` names the data in case it can't be serialized.
pub fn result_to_json<T: serde::Serialize>(
    result: &Result<T, anyhow::Error>,
    action_identifier: &str,
    data_identifier: &str,
) -> serde_json::Value {
    match result {
        Ok(entry) => json!({ "result": serde_json::to_value(entry).unwrap_or_else(
            |e| json!({ "error": format!("Failed to serialize {data_identifier}: {e}")}),
        )}),
        Err(e) => error_to_json(e, action_identifier),
    }
}

/// Something that can be reported back to the LLM as the result of an action.
pub trait ToolResult: 'static + Send + Sync {
    /// Convert `self` to the JSON handed back to the LLM.
    fn to_json(&self, action_identifier: &str) -> serde_json::Value;
}

impl ToolResult for Result<String, anyhow::Error> {
    fn to_json(&self, action_identifier: &str) -> serde_json::Value {
        let result = match self {
            Ok(value) => Ok(value.clone()),
            Err(e) => Err(e),
        };
        string_result_to_json(&result, action_identifier)
    }
}

impl ToolResult for Result<Vec<String>, anyhow::Error> {
    fn to_json(&self, action_identifier: &str) -> serde_json::Value {
        let result = match self {
            Ok(values) => Ok(values.clone()),
            Err(e) => Err(e),
        };
        string_vec_result_to_json(&result, action_identifier)
    }
}

impl TryFrom<&dyn Tool> for async_openai::types::chat::ChatCompletionTools {
    type Error = anyhow::Error;

    fn try_from(tool: &dyn Tool) -> Result<Self, Self::Error> {
        use async_openai::types::chat::{ChatCompletionTool, FunctionObject};
        Ok(Self::Function(ChatCompletionTool {
            function: FunctionObject {
                name: tool.name(),
                description: Some(tool.description()),
                parameters: Some(tool.parameters()),
                strict: Some(false),
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    struct Echo;

    impl Tool for Echo {
        fn name(&self) -> String {
            "echo".into()
        }
        fn description(&self) -> String {
            "Echo the `text` parameter".into()
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        fn call(
            self: Arc<Self>,
            parameters: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            Ok(json!({ "result": parameters["text"] }))
        }
    }

    #[test]
    fn basic_toolset_dispatches_by_name_without_any_context() {
        let mut tools = BasicToolset::new();
        tools.register_tool(Echo);
        assert_eq!(tools.list().len(), 1);
        assert_eq!(
            tools.call_tool("echo", r#"{"text":"hi"}"#),
            json!({"result": "hi"})
        );
    }

    #[test]
    fn basic_toolset_reports_failures_as_json_errors() {
        let mut tools = BasicToolset::new();
        tools.register_tool(Echo);
        let unknown = tools.call_tool("nope", "{}");
        assert!(unknown["error"].as_str().unwrap().contains("not found"));
        let invalid = tools.call_tool("echo", "{");
        assert!(
            invalid["error"]
                .as_str()
                .unwrap()
                .contains("Failed to parse parameters")
        );
    }

    #[test]
    fn tools_convert_to_openai_function_tools() {
        let tool: &dyn Tool = &Echo;
        let converted = async_openai::types::chat::ChatCompletionTools::try_from(tool).unwrap();
        assert!(matches!(
            converted,
            async_openai::types::chat::ChatCompletionTools::Function(_)
        ));
    }
}
