//! US Code sections + bill text via govinfo. IMPLEMENTATION PENDING.
use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct StatuteRequest {
    pub title: Option<u32>,
    pub section: Option<String>,
    pub congress: Option<u32>,
    pub bill: Option<String>,
    pub max_chars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatuteResponse {
    pub generated_at: DateTime<Utc>,
    pub citation: Option<String>,
    pub heading: Option<String>,
    pub text: Option<String>,
    pub chars: usize,
    pub truncated: bool,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_statute(_req: StatuteRequest) -> Result<StatuteResponse> {
    Ok(StatuteResponse {
        generated_at: Utc::now(),
        citation: None,
        heading: None,
        text: None,
        chars: 0,
        truncated: false,
        source_url: None,
        warnings: vec!["legal.statute not implemented yet".to_string()],
    })
}
