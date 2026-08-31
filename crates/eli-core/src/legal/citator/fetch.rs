//! Citation verification + citation network. IMPLEMENTATION PENDING.
use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct CitationRequest {
    pub text: Option<String>,
    pub cite: Option<String>,
    pub opinion_id: Option<u64>,
    pub cited_by: bool,
    pub authorities: bool,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CitationVerdict {
    pub citation: String,
    pub normalized: Option<String>,
    pub exists: bool,
    pub status: u16,
    pub case_name: Option<String>,
    pub court: Option<String>,
    pub date_filed: Option<String>,
    pub cluster_id: Option<u64>,
    pub url: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct NetworkCase {
    pub case_name: Option<String>,
    pub citation: Option<String>,
    pub court_id: Option<String>,
    pub date_filed: Option<String>,
    pub depth: Option<u32>,
    pub opinion_id: Option<u64>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CitationResponse {
    pub generated_at: DateTime<Utc>,
    pub checked: usize,
    pub verified: usize,
    pub unverified: usize,
    pub citations: Vec<CitationVerdict>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cited_by: Vec<NetworkCase>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authorities: Vec<NetworkCase>,
    pub cited_by_total: Option<u64>,
    pub warnings: Vec<String>,
}

pub async fn fetch_citations(_req: CitationRequest) -> Result<CitationResponse> {
    Ok(CitationResponse {
        generated_at: Utc::now(),
        checked: 0,
        verified: 0,
        unverified: 0,
        citations: Vec::new(),
        cited_by: Vec::new(),
        authorities: Vec::new(),
        cited_by_total: None,
        warnings: vec!["legal.cite not implemented yet".to_string()],
    })
}
