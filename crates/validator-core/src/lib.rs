use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
}

pub fn require(condition: bool, code: &str, message: &str) -> Vec<ValidationIssue> {
    if condition {
        vec![]
    } else {
        vec![ValidationIssue {
            code: code.to_string(),
            message: message.to_string(),
        }]
    }
}
