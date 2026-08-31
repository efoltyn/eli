//! Federal Register API. IMPLEMENTATION PENDING.
use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct FedregRequest {
    pub query: Option<String>,
    pub kind: Option<String>,
    pub agencies: Vec<String>,
    pub published_after: Option<String>,
    pub published_before: Option<String>,
    pub docket: Option<String>,
    pub cfr_title: Option<u32>,
    pub cfr_part: Option<String>,
    pub document_number: Option<String>,
    pub with_text: bool,
    pub public_inspection: bool,
    pub facet: Option<String>,
    pub max_chars: usize,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FedregDocument {
    pub document_number: Option<String>,
    pub title: Option<String>,
    pub doc_type: Option<String>,
    pub agencies: Vec<String>,
    pub publication_date: Option<String>,
    pub effective_on: Option<String>,
    pub comments_close_on: Option<String>,
    pub docket_ids: Vec<String>,
    pub regulation_id_numbers: Vec<String>,
    pub cfr_references: Vec<String>,
    pub significant: Option<bool>,
    pub abstract_text: Option<String>,
    pub html_url: Option<String>,
    pub raw_text_url: Option<String>,
    pub text: Option<String>,
    pub text_truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FedregFacet {
    pub key: String,
    pub name: Option<String>,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FedregResponse {
    pub generated_at: DateTime<Utc>,
    pub mode: String,
    pub query: Option<String>,
    pub total_available: Option<u64>,
    pub documents: Vec<FedregDocument>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<FedregFacet>,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_fedreg(req: FedregRequest) -> Result<FedregResponse> {
    Ok(FedregResponse {
        generated_at: Utc::now(),
        mode: "search".to_string(),
        query: req.query,
        total_available: None,
        documents: Vec::new(),
        facets: Vec::new(),
        source_url: None,
        warnings: vec!["legal.fedreg not implemented yet".to_string()],
    })
}
