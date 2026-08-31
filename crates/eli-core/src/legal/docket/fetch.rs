//! CourtListener RECAP federal docket sheets. IMPLEMENTATION PENDING.
use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct DocketRequest {
    pub court: Option<String>,
    pub docket_number: Option<String>,
    pub docket_id: Option<u64>,
    pub query: Option<String>,
    pub include_entries: bool,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RecapDocument {
    pub document_number: Option<String>,
    pub attachment_number: Option<u32>,
    pub description: Option<String>,
    pub page_count: Option<u32>,
    pub is_available: bool,
    pub filepath_local: Option<String>,
    pub pacer_url: Option<String>,
    pub plain_text_chars: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DocketEntry {
    pub entry_number: Option<u64>,
    pub date_filed: Option<String>,
    pub description: Option<String>,
    pub documents: Vec<RecapDocument>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocketResponse {
    pub generated_at: DateTime<Utc>,
    pub docket_id: Option<u64>,
    pub case_name: Option<String>,
    pub court: Option<String>,
    pub docket_number: Option<String>,
    pub date_filed: Option<String>,
    pub date_terminated: Option<String>,
    pub nature_of_suit: Option<String>,
    pub cause: Option<String>,
    pub assigned_to: Option<String>,
    pub jury_demand: Option<String>,
    pub entry_count: usize,
    pub entries: Vec<DocketEntry>,
    pub candidates: Vec<serde_json::Value>,
    pub url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_docket(req: DocketRequest) -> Result<DocketResponse> {
    Ok(DocketResponse {
        generated_at: Utc::now(),
        docket_id: req.docket_id,
        case_name: None,
        court: req.court,
        docket_number: req.docket_number,
        date_filed: None,
        date_terminated: None,
        nature_of_suit: None,
        cause: None,
        assigned_to: None,
        jury_demand: None,
        entry_count: 0,
        entries: Vec::new(),
        candidates: Vec::new(),
        url: None,
        warnings: vec!["legal.docket not implemented yet".to_string()],
    })
}
