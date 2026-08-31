//! Agency enforcement actions (SEC / CFPB / DOJ). IMPLEMENTATION PENDING.
use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct EnforcementRequest {
    pub source: String,
    pub query: Option<String>,
    pub after: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct EnforcementAction {
    pub source: String,
    pub title: Option<String>,
    pub date: Option<String>,
    pub release_number: Option<String>,
    pub respondents: Vec<String>,
    pub summary: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnforcementResponse {
    pub generated_at: DateTime<Utc>,
    pub sources: Vec<String>,
    pub actions: Vec<EnforcementAction>,
    pub warnings: Vec<String>,
}

pub async fn fetch_enforcement(req: EnforcementRequest) -> Result<EnforcementResponse> {
    Ok(EnforcementResponse {
        generated_at: Utc::now(),
        sources: vec![req.source],
        actions: Vec::new(),
        warnings: vec!["legal.enforcement not implemented yet".to_string()],
    })
}
