//! regulations.gov dockets + public comments. IMPLEMENTATION PENDING.
use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct CommentsRequest {
    pub docket: Option<String>,
    pub query: Option<String>,
    pub agency: Option<String>,
    pub posted_after: Option<String>,
    pub comment_id: Option<String>,
    pub with_text: bool,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CommentRecord {
    pub id: Option<String>,
    pub tracking_number: Option<String>,
    pub title: Option<String>,
    pub submitter: Option<String>,
    pub organization: Option<String>,
    pub posted_date: Option<String>,
    pub docket_id: Option<String>,
    pub agency_id: Option<String>,
    pub comment_text: Option<String>,
    pub attachment_urls: Vec<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommentsResponse {
    pub generated_at: DateTime<Utc>,
    pub docket: Option<String>,
    pub total_available: Option<u64>,
    pub comments: Vec<CommentRecord>,
    pub has_api_key: bool,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_comments(req: CommentsRequest) -> Result<CommentsResponse> {
    Ok(CommentsResponse {
        generated_at: Utc::now(),
        docket: req.docket,
        total_available: None,
        comments: Vec::new(),
        has_api_key: false,
        source_url: None,
        warnings: vec!["legal.comments not implemented yet".to_string()],
    })
}
