//! The `Tool` trait and the fixed `ToolRegistry`.
//!
//! The model never defines tools. The host constructs the registry;
//! the model only names tools and supplies arguments. Arguments are
//! validated against the tool's JSON Schema before `execute` runs,
//! so tools can assume well-typed input.

use serde_json::Value;
use std::collections::HashMap;

use crate::Error;

/// A single tool call requested by the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// The tool's registry name.
    pub name: String,
    /// The arguments, validated against the tool's schema before
    /// execution.
    pub arguments: Value,
}

/// The result of executing a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutput {
    /// Execution succeeded; the value goes back to the model.
    Ok(Value),
    /// Execution failed; the reason goes back to the model as
    /// information, not as a loop-stopping error.
    Err(String),
}

/// A tool the loop can execute.
///
/// Implementations must be deterministic in their schema: the same
/// tool always reports the same `parameters_schema`.
pub trait Tool: Send + Sync {
    /// The registry name the model uses to call this tool.
    fn name(&self) -> &'static str;

    /// A human-readable description, shown to the model.
    fn description(&self) -> &'static str;

    /// The JSON Schema the model's arguments must satisfy. Validated
    /// before `execute` runs.
    fn parameters_schema(&self) -> Value;

    /// Execute the tool. `arguments` have already passed schema
    /// validation; implementations still handle semantic errors
    /// (unknown names, denied grants) by returning
    /// `ToolOutput::Err`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ToolFailed`] when execution itself fails
    /// outside the feedback path. Semantic failures (denied grants,
    /// missing secrets) are `ToolOutput::Err`, not `Error`.
    fn execute(&self, arguments: Value) -> Result<ToolOutput, Error>;
}

/// The fixed set of tools the loop may call.
///
/// Constructed by the host. The model cannot add, remove, or
/// redefine entries.
#[derive(Default)]
pub struct ToolRegistry<'a> {
    tools: HashMap<String, Box<dyn Tool + 'a>>,
}

impl<'a> ToolRegistry<'a> {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. A later registration under the same name
    /// replaces the earlier one; the host controls the final set.
    pub fn register(&mut self, tool: Box<dyn Tool + 'a>) {
        self.tools.insert(tool.name().to_owned(), tool);
    }

    /// Look up a tool by the name the model supplied.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&(dyn Tool + 'a)> {
        self.tools.get(name).map(AsRef::as_ref)
    }

    /// The tool descriptions shown to the model.
    #[must_use]
    pub fn descriptions(&self) -> Vec<ToolDescription> {
        self.tools
            .values()
            .map(|t| ToolDescription {
                name: t.name().to_owned(),
                description: t.description().to_owned(),
                parameters_schema: t.parameters_schema(),
            })
            .collect()
    }

    /// Checks a call without running it: its tool is registered, and its
    /// arguments fit the tool's schema.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownTool`] when the name is not registered,
    /// [`Error::InvalidArguments`] when validation fails.
    pub fn check(&self, call: &ToolCall) -> Result<(), Error> {
        let tool = self
            .get(&call.name)
            .ok_or_else(|| Error::UnknownTool(call.name.clone()))?;
        validate_arguments(tool, &call.arguments)
    }

    /// Validate a call's arguments against the named tool's schema,
    /// then execute it. Unknown names and schema violations stop
    /// before execution.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownTool`] when the name is not
    /// registered, [`Error::InvalidArguments`] when validation
    /// fails.
    pub fn call(&self, call: &ToolCall) -> Result<ToolOutput, Error> {
        let tool = self
            .get(&call.name)
            .ok_or_else(|| Error::UnknownTool(call.name.clone()))?;
        validate_arguments(tool, &call.arguments)?;
        tool.execute(call.arguments.clone())
    }
}

/// The subset of a tool's definition shown to the model.
#[derive(Debug, Clone)]
pub struct ToolDescription {
    /// Registry name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for arguments.
    pub parameters_schema: Value,
}

/// Validate arguments against the tool's JSON Schema.
///
/// This is a minimal hand-rolled validator for the subset of JSON
/// Schema that tools actually use: `type: "object"`, `properties`
/// with scalar types, `required` fields, and `additionalProperties`.
/// The full `jsonschema` crate was rejected because its transitive
/// tree (reqwest, hyper, …) violates the workspace's cargo-deny bans.
///
/// # Errors
///
/// Returns [`Error::InvalidArguments`] when the schema itself is
/// malformed or the arguments violate it.
fn validate_arguments(tool: &dyn Tool, arguments: &Value) -> Result<(), Error> {
    let schema = tool.parameters_schema();
    validate_against_schema(tool.name(), &schema, arguments)
}

/// The subset of JSON Schema we support. Schemas outside this subset
/// are rejected at validation time — tools must stay within it.
fn validate_against_schema(tool: &str, schema: &Value, value: &Value) -> Result<(), Error> {
    let invalid = |reason: String| Error::InvalidArguments {
        tool: tool.to_owned(),
        reason,
    };

    // Only object schemas are supported at the top level.
    let schema_type = schema.get("type").and_then(Value::as_str);
    if schema_type != Some("object") {
        return Err(invalid(format!(
            "unsupported schema type: {schema_type:?} (only 'object' is supported)"
        )));
    }
    let obj = value
        .as_object()
        .ok_or_else(|| invalid(String::from("arguments must be a JSON object")))?;

    // Check required fields. A `required` entry that is not an
    // array of strings is outside the supported subset: reject
    // fail-closed rather than silently skipping the constraint.
    if let Some(required) = schema.get("required") {
        let required = required.as_array().ok_or_else(|| {
            invalid(String::from(
                "schema 'required' must be an array of strings",
            ))
        })?;
        for req in required {
            let name = req.as_str().ok_or_else(|| {
                invalid(String::from(
                    "schema 'required' must be an array of strings",
                ))
            })?;
            if !obj.contains_key(name) {
                return Err(invalid(format!("missing required field: '{name}'")));
            }
        }
    }

    // Check properties. A `properties` entry that is not an object is
    // outside the supported subset: reject fail-closed rather than
    // silently skipping validation.
    let properties = if let Some(props) = schema.get("properties") {
        Some(
            props
                .as_object()
                .ok_or_else(|| invalid(String::from("schema 'properties' must be an object")))?,
        )
    } else {
        None
    };
    let additional = schema.get("additionalProperties");

    for (key, val) in obj {
        if let Some(props) = properties {
            if let Some(prop_schema) = props.get(key) {
                validate_value_type(tool, key, prop_schema, val)?;
                continue;
            }
        }
        // Field not in properties: check additionalProperties.
        match additional {
            // Absent or explicitly true: extra fields are allowed.
            None | Some(Value::Bool(true)) => {}
            Some(Value::Bool(false)) => {
                return Err(invalid(format!("unexpected field: '{key}'")));
            }
            Some(schema) => {
                // A non-object additionalProperties is outside the
                // supported subset: reject fail-closed rather than
                // silently allowing the extra field.
                if !schema.is_object() {
                    return Err(invalid(format!(
                        "unsupported additionalProperties schema: {schema}"
                    )));
                }
                validate_value_type(tool, key, schema, val)?;
            }
        }
    }

    Ok(())
}

/// Validate a single value against its property schema.
fn validate_value_type(
    tool: &str,
    field: &str,
    schema: &Value,
    value: &Value,
) -> Result<(), Error> {
    let invalid = |reason: String| Error::InvalidArguments {
        tool: tool.to_owned(),
        reason,
    };

    let expected = schema.get("type").and_then(Value::as_str);
    let ok = match expected {
        Some("string") => value.is_string(),
        Some("number") => value.is_number(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("boolean") => value.is_boolean(),
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("null") => value.is_null(),
        // No type constraint: allow anything.
        None => true,
        Some(other) => {
            return Err(invalid(format!(
                "unsupported type '{other}' for field '{field}'"
            )));
        }
    };
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "field '{field}' must be of type '{}', got {}",
            expected.unwrap_or("any"),
            type_name(value),
        )))
    }
}

/// Human-readable JSON type name for error messages.
const fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

impl std::fmt::Debug for ToolRegistry<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}
