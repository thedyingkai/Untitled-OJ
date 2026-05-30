use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Event {
    pub id: String,

    #[serde(rename = "type")]
    pub event_type: String,

    pub producer: String,
    pub timestamp: String,
    pub payload: serde_json::Value,
}

impl Event {
    pub fn submission_id(&self) -> Option<i64> {
        self.payload.get("submission_id").and_then(|v| v.as_i64())
    }
}
