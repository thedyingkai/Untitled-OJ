use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultFile {
    pub submission_id: i64,
    pub status: String,
    pub score: i32,
    pub time_ms: i32,
    pub memory_kb: i32,
    pub message: String,
    pub cases: Vec<ResultCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultCase {
    pub case_no: i32,
    pub status: String,
    pub score: i32,
    pub time_ms: i32,
    pub memory_kb: i32,
    pub stdout_path: String,
    pub stderr_path: String,
    pub checker_log_path: String,
    pub message: String,
}
