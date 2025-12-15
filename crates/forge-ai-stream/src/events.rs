//! AI streaming events
//!
//! Defines all event types that flow between the application and AI services.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// AI event type
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AIEventType {
    /// Connection established
    Connected,
    /// Connection closed
    Disconnected,
    /// Session started
    SessionStarted,
    /// Session ended
    SessionEnded,
    /// Audio response from AI
    AudioResponse,
    /// Transcript from user or AI
    Transcript,
    /// Function/tool call from AI
    FunctionCall,
    /// VAD state change
    VadStateChange,
    /// Barge-in detected
    BargeIn,
    /// Error occurred
    Error,
    /// Custom event
    Custom(String),
}

/// AI event
#[derive(Debug, Clone)]
pub enum AIEvent {
    /// Connection established
    Connected {
        session_id: String,
    },

    /// Connection closed
    Disconnected {
        reason: String,
    },

    /// Session started
    SessionStarted {
        session_id: String,
        config: SessionConfig,
    },

    /// Session ended
    SessionEnded {
        session_id: String,
        duration_ms: u64,
    },

    /// Audio response from AI (PCM16, 24kHz mono)
    AudioResponse {
        audio_data: Vec<i16>,
        sample_rate: u32,
        item_id: Option<String>,
    },

    /// Transcript segment
    Transcript {
        segment: TranscriptSegment,
    },

    /// Function call request from AI
    FunctionCall {
        call: FunctionCall,
    },

    /// Function call response sent to AI
    FunctionResponse {
        call_id: String,
        output: String,
    },

    /// VAD state changed
    VadStateChange {
        is_speech: bool,
        confidence: f32,
    },

    /// Barge-in detected (user interrupted AI)
    BargeIn {
        response_id: String,
    },

    /// Error occurred
    Error {
        message: String,
        code: Option<String>,
    },

    /// Custom event
    Custom {
        event_type: String,
        data: serde_json::Value,
    },
}

impl AIEvent {
    /// Get the event type
    pub fn event_type(&self) -> AIEventType {
        match self {
            AIEvent::Connected { .. } => AIEventType::Connected,
            AIEvent::Disconnected { .. } => AIEventType::Disconnected,
            AIEvent::SessionStarted { .. } => AIEventType::SessionStarted,
            AIEvent::SessionEnded { .. } => AIEventType::SessionEnded,
            AIEvent::AudioResponse { .. } => AIEventType::AudioResponse,
            AIEvent::Transcript { .. } => AIEventType::Transcript,
            AIEvent::FunctionCall { .. } => AIEventType::FunctionCall,
            AIEvent::FunctionResponse { .. } => AIEventType::Custom("function_response".to_string()),
            AIEvent::VadStateChange { .. } => AIEventType::VadStateChange,
            AIEvent::BargeIn { .. } => AIEventType::BargeIn,
            AIEvent::Error { .. } => AIEventType::Error,
            AIEvent::Custom { event_type, .. } => AIEventType::Custom(event_type.clone()),
        }
    }
}

/// Session configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Model identifier
    pub model: String,

    /// Voice identifier (if applicable)
    pub voice: Option<String>,

    /// Temperature for response generation
    pub temperature: Option<f32>,

    /// Maximum response tokens
    pub max_tokens: Option<u32>,

    /// System instructions
    pub instructions: Option<String>,

    /// Turn detection mode
    pub turn_detection: Option<TurnDetectionMode>,

    /// Available tools/functions
    pub tools: Vec<ToolDefinition>,
}

/// Turn detection mode
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TurnDetectionMode {
    /// Server-side VAD
    #[serde(rename = "server_vad")]
    ServerVad {
        threshold: f32,
        prefix_padding_ms: u32,
        silence_duration_ms: u32,
    },

    /// No turn detection (manual control)
    #[serde(rename = "none")]
    None,
}

/// Transcript segment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    /// Segment ID
    pub id: String,

    /// Source (user or assistant)
    pub role: TranscriptRole,

    /// Transcript text
    pub text: String,

    /// Start timestamp (milliseconds)
    pub start_ms: Option<u64>,

    /// End timestamp (milliseconds)
    pub end_ms: Option<u64>,

    /// Confidence score (0.0-1.0)
    pub confidence: Option<f32>,

    /// Whether this is a final transcript
    pub is_final: bool,
}

/// Transcript role
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptRole {
    /// User speaking
    User,
    /// AI assistant speaking
    Assistant,
    /// System message
    System,
}

/// Function/tool call from AI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Unique call identifier
    pub call_id: String,

    /// Function name
    pub name: String,

    /// Function arguments (JSON)
    pub arguments: serde_json::Value,

    /// Item ID (for tracking)
    pub item_id: Option<String>,
}

/// Function/tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool type (currently only "function")
    #[serde(rename = "type")]
    pub tool_type: String,

    /// Function name
    pub name: String,

    /// Function description
    pub description: String,

    /// Parameters schema (JSON Schema)
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    /// Create a new function tool
    pub fn function(name: impl Into<String>, description: impl Into<String>) -> ToolDefinitionBuilder {
        ToolDefinitionBuilder {
            name: name.into(),
            description: description.into(),
            parameters: HashMap::new(),
            required: Vec::new(),
        }
    }
}

/// Builder for tool definitions
pub struct ToolDefinitionBuilder {
    name: String,
    description: String,
    parameters: HashMap<String, ParameterSchema>,
    required: Vec<String>,
}

impl ToolDefinitionBuilder {
    /// Add a string parameter
    pub fn string_param(mut self, name: impl Into<String>, description: impl Into<String>, required: bool) -> Self {
        let name = name.into();
        self.parameters.insert(
            name.clone(),
            ParameterSchema {
                param_type: "string".to_string(),
                description: description.into(),
                enum_values: None,
            },
        );
        if required {
            self.required.push(name);
        }
        self
    }

    /// Add a number parameter
    pub fn number_param(mut self, name: impl Into<String>, description: impl Into<String>, required: bool) -> Self {
        let name = name.into();
        self.parameters.insert(
            name.clone(),
            ParameterSchema {
                param_type: "number".to_string(),
                description: description.into(),
                enum_values: None,
            },
        );
        if required {
            self.required.push(name);
        }
        self
    }

    /// Build the tool definition
    pub fn build(self) -> ToolDefinition {
        let mut properties = serde_json::Map::new();
        for (name, schema) in self.parameters {
            let mut prop = serde_json::Map::new();
            prop.insert("type".to_string(), serde_json::Value::String(schema.param_type));
            prop.insert("description".to_string(), serde_json::Value::String(schema.description));
            if let Some(enum_values) = schema.enum_values {
                prop.insert("enum".to_string(), serde_json::Value::Array(enum_values));
            }
            properties.insert(name, serde_json::Value::Object(prop));
        }

        let mut parameters = serde_json::Map::new();
        parameters.insert("type".to_string(), serde_json::Value::String("object".to_string()));
        parameters.insert("properties".to_string(), serde_json::Value::Object(properties));
        if !self.required.is_empty() {
            parameters.insert(
                "required".to_string(),
                serde_json::Value::Array(
                    self.required.into_iter().map(serde_json::Value::String).collect(),
                ),
            );
        }

        ToolDefinition {
            tool_type: "function".to_string(),
            name: self.name,
            description: self.description,
            parameters: serde_json::Value::Object(parameters),
        }
    }
}

#[derive(Debug, Clone)]
struct ParameterSchema {
    param_type: String,
    description: String,
    enum_values: Option<Vec<serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definition_builder() {
        let tool = ToolDefinition::function("get_weather", "Get current weather for a location")
            .string_param("location", "City name", true)
            .string_param("units", "Temperature units", false)
            .build();

        assert_eq!(tool.name, "get_weather");
        assert_eq!(tool.tool_type, "function");
        assert!(tool.parameters.is_object());
    }

    #[test]
    fn test_event_type() {
        let event = AIEvent::Connected {
            session_id: "test-123".to_string(),
        };
        assert_eq!(event.event_type(), AIEventType::Connected);

        let event = AIEvent::AudioResponse {
            audio_data: vec![0i16; 100],
            sample_rate: 24000,
            item_id: None,
        };
        assert_eq!(event.event_type(), AIEventType::AudioResponse);
    }

    #[test]
    fn test_transcript_segment() {
        let segment = TranscriptSegment {
            id: "seg-1".to_string(),
            role: TranscriptRole::User,
            text: "Hello, how are you?".to_string(),
            start_ms: Some(0),
            end_ms: Some(2000),
            confidence: Some(0.95),
            is_final: true,
        };

        assert_eq!(segment.role, TranscriptRole::User);
        assert!(segment.is_final);
    }
}
