//! Gemini Live Translation wire protocol.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::catalog;
use crate::usage::TurnUsage;

use super::protocol::{ParsedEvent, ServerEvent, SessionParams};

pub fn endpoint_url(api_key: &str) -> String {
    format!(
        "{}?key={}",
        catalog::GEMINI_API_BASE,
        percent_encode(api_key)
    )
}

pub fn setup_frame(params: &SessionParams) -> String {
    json!({
        "setup": {
            "model": format!("models/{}", catalog::GEMINI_MODEL_NAME),
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "translationConfig": {
                    "targetLanguageCode": params.wire_target(),
                    "echoTargetLanguage": true
                }
            },
            // These belong to BidiGenerateContentSetup, not GenerationConfig.
            // Nesting them below generationConfig makes Gemini close the socket
            // with INVALID_JSON (unknown field at setup.generation_config).
            "inputAudioTranscription": {},
            "outputAudioTranscription": {}
        }
    })
    .to_string()
}

pub fn audio_frame(pcm: &[u8]) -> String {
    json!({
        "realtimeInput": {
            "audio": {
                "data": base64::engine::general_purpose::STANDARD.encode(pcm),
                "mimeType": "audio/pcm;rate=16000"
            }
        }
    })
    .to_string()
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct Decoder {
    output_text: String,
    usage: TurnUsage,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending(&self) -> &str {
        &self.output_text
    }

    pub fn reset(&mut self) {
        self.output_text.clear();
        self.usage = TurnUsage::default();
    }

    pub fn decode(&mut self, message: &str) -> Vec<ParsedEvent> {
        let Ok(value) = serde_json::from_str::<Value>(message) else {
            return Vec::new();
        };
        let mut events = Vec::new();

        if value.get("setupComplete").is_some() {
            events.push(parsed("setupComplete", ServerEvent::SessionUpdated));
        }

        if let Some(error) = value.get("error") {
            let code = error
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| error.get("code").map(Value::to_string));
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Gemini 返回了错误，但没有说明原因")
                .to_string();
            events.push(parsed("error", ServerEvent::Error { code, message }));
        }

        if let Some(metadata) = value.get("usageMetadata") {
            self.usage = TurnUsage {
                input_tokens: u64_field(metadata, "promptTokenCount"),
                output_tokens: u64_field(metadata, "responseTokenCount"),
                total_tokens: u64_field(metadata, "totalTokenCount"),
            };
        }

        let Some(content) = value.get("serverContent") else {
            if events.is_empty() {
                events.push(parsed(
                    "unknown",
                    ServerEvent::Other {
                        event_type: "unknown".into(),
                    },
                ));
            }
            return events;
        };

        if let Some(language) = content
            .get("inputTranscription")
            .and_then(|t| t.get("languageCode"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            events.push(parsed(
                "serverContent.inputTranscription",
                ServerEvent::SourceDetected {
                    language: language.to_string(),
                },
            ));
        }

        let mut pcm = Vec::new();
        if let Some(parts) = content
            .get("modelTurn")
            .and_then(|turn| turn.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                let data = part
                    .get("inlineData")
                    .or_else(|| part.get("inline_data"))
                    .and_then(|inline| inline.get("data"))
                    .and_then(Value::as_str);
                if let Some(data) = data {
                    if let Ok(mut decoded) = base64::engine::general_purpose::STANDARD.decode(data)
                    {
                        pcm.append(&mut decoded);
                    }
                }
            }
        }
        if !pcm.is_empty() {
            events.push(parsed(
                "serverContent.modelTurn",
                ServerEvent::AudioDelta { pcm },
            ));
        }

        if let Some(text) = content
            .get("outputTranscription")
            .and_then(|t| t.get("text"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.output_text.push_str(text);
            events.push(parsed(
                "serverContent.outputTranscription",
                ServerEvent::TextDelta {
                    text: self.output_text.clone(),
                    confirmed: Some(self.output_text.clone()),
                },
            ));
        }

        if content
            .get("turnComplete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if !self.output_text.is_empty() {
                events.push(parsed(
                    "serverContent.turnComplete",
                    ServerEvent::TextDone {
                        text: std::mem::take(&mut self.output_text),
                    },
                ));
            }
            events.push(parsed(
                "serverContent.turnComplete",
                ServerEvent::TurnDone { usage: self.usage },
            ));
            self.usage = TurnUsage::default();
        }

        if events.is_empty() {
            events.push(parsed(
                "serverContent",
                ServerEvent::Other {
                    event_type: "serverContent".into(),
                },
            ));
        }
        events
    }
}

fn parsed(event_type: &str, event: ServerEvent) -> ParsedEvent {
    ParsedEvent {
        event_type: event_type.to_string(),
        event,
    }
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(audio: bool) -> SessionParams {
        SessionParams {
            model_name: catalog::GEMINI_MODEL_NAME.into(),
            target_language: "ja".into(),
            voice: audio.then(|| "provider-default".into()),
            clone_frequency: None,
            source_language: None,
        }
    }

    #[test]
    fn setup_uses_live_translation_config() {
        let value: Value = serde_json::from_str(&setup_frame(&params(true))).unwrap();
        assert_eq!(
            value["setup"]["model"],
            "models/gemini-3.5-live-translate-preview"
        );
        assert_eq!(
            value["setup"]["generationConfig"]["translationConfig"]["targetLanguageCode"],
            "ja"
        );
        assert_eq!(
            value["setup"]["generationConfig"]["responseModalities"],
            json!(["AUDIO"])
        );
        assert_eq!(value["setup"]["inputAudioTranscription"], json!({}));
        assert_eq!(value["setup"]["outputAudioTranscription"], json!({}));
        assert!(value["setup"]["generationConfig"]["inputAudioTranscription"].is_null());
        assert!(value["setup"]["generationConfig"]["outputAudioTranscription"].is_null());
    }

    #[test]
    fn audio_is_pcm16_base64() {
        let value: Value = serde_json::from_str(&audio_frame(&[1, 2, 3])).unwrap();
        assert_eq!(
            value["realtimeInput"]["audio"]["mimeType"],
            "audio/pcm;rate=16000"
        );
        assert_eq!(value["realtimeInput"]["audio"]["data"], "AQID");
    }

    #[test]
    fn one_server_frame_can_emit_audio_text_and_turn_done() {
        let mut decoder = Decoder::new();
        let events = decoder.decode(
            r#"{"usageMetadata":{"promptTokenCount":10,"responseTokenCount":4,"totalTokenCount":14},"serverContent":{"modelTurn":{"parts":[{"inlineData":{"data":"AQI="}}]},"outputTranscription":{"text":"hello"},"turnComplete":true}}"#,
        );
        assert!(events
            .iter()
            .any(|event| matches!(event.event, ServerEvent::AudioDelta { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event.event, ServerEvent::TextDelta { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event.event, ServerEvent::TextDone { .. })));
        assert!(events.iter().any(|event| matches!(event.event, ServerEvent::TurnDone { usage } if usage.total_tokens == 14)));
    }

    #[test]
    fn resource_exhausted_error_keeps_status_code() {
        let mut decoder = Decoder::new();
        let events = decoder.decode(
            r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"Quota exceeded"}}"#,
        );
        assert!(matches!(
            &events[0].event,
            ServerEvent::Error { code: Some(code), .. } if code == "RESOURCE_EXHAUSTED"
        ));
    }
}
