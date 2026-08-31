//! CourtListener case-law search and opinion text. IMPLEMENTATION PENDING.
use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct CaseSearchRequest {
    pub query: String,
    pub kind: String,
    pub courts: Vec<String>,
    pub filed_after: Option<String>,
    pub filed_before: Option<String>,
    pub judge: Option<String>,
    pub cited_gt: Option<u32>,
    pub status: Option<String>,
    pub order_by: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CaseHit {
    pub case_name: Option<String>,
    pub citation: Option<String>,
    pub court: Option<String>,
    pub court_id: Option<String>,
    pub date_filed: Option<String>,
    pub docket_number: Option<String>,
    pub judge: Option<String>,
    pub status: Option<String>,
    pub cite_count: Option<u32>,
    pub snippet: Option<String>,
    pub opinion_id: Option<u64>,
    pub cluster_id: Option<u64>,
    pub docket_id: Option<u64>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseSearchResponse {
    pub generated_at: DateTime<Utc>,
    pub query: String,
    pub kind: String,
    pub total_available: Option<u64>,
    pub results: Vec<CaseHit>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct OpinionRequest {
    pub id: Option<u64>,
    pub cite: Option<String>,
    pub max_chars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpinionResponse {
    pub generated_at: DateTime<Utc>,
    pub opinion_id: Option<u64>,
    pub cluster_id: Option<u64>,
    pub case_name: Option<String>,
    pub citations: Vec<String>,
    pub court: Option<String>,
    pub date_filed: Option<String>,
    pub author: Option<String>,
    pub status: Option<String>,
    pub cite_count: Option<u32>,
    pub text: Option<String>,
    pub chars: usize,
    pub truncated: bool,
    pub url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_case_search(req: CaseSearchRequest) -> Result<CaseSearchResponse> {
    Ok(CaseSearchResponse {
        generated_at: Utc::now(),
        query: req.query,
        kind: req.kind,
        total_available: None,
        results: Vec::new(),
        warnings: vec!["legal.search not implemented yet".to_string()],
    })
}

pub async fn fetch_opinion(req: OpinionRequest) -> Result<OpinionResponse> {
    Ok(OpinionResponse {
        generated_at: Utc::now(),
        opinion_id: req.id,
        cluster_id: None,
        case_name: None,
        citations: Vec::new(),
        court: None,
        date_filed: None,
        author: None,
        status: None,
        cite_count: None,
        text: None,
        chars: 0,
        truncated: false,
        url: None,
        warnings: vec!["legal.opinion not implemented yet".to_string()],
    })
}
