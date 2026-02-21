use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct ExtensionData {
    pub url: String,
    pub cookie: String,
    pub request_headers: HashMap<String, String>,
    method: String,
    pub user_agent: String,
    pub referer: Option<String>,
    pub mine_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoListItem {
    pub id: String,
    pub text: String,
    pub info: String,
    pub tab_id: String,
}
