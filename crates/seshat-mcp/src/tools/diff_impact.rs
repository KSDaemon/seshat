use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{
    ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata, map_graph_error,
    serialize_response,
};

#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct MapDiffImpactRequest {
    #[schemars(description = "If true, diff index vs HEAD (only staged changes)")]
    pub staged_only: Option<bool>,
    #[schemars(
        description = "Optional base commitish to diff against instead of HEAD. Mutually exclusive with staged_only"
    )]
    pub base: Option<String>,
    #[schemars(description = "Path to the git repository root on disk")]
    pub repo_path: Option<String>,
    pub repo: Option<String>,
    pub scope: Option<String>,
    pub file_path: Option<String>,
}

pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    req: MapDiffImpactRequest,
) -> String {
    let tool = "map_diff_impact";

    let staged_only = req.staged_only.unwrap_or(false);

    if staged_only && req.base.is_some() {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            "staged_only and base are mutually exclusive",
            "Use either staged_only=true or base=<commitish>, not both",
        );
        return serde_json::to_string(&err).unwrap_or_default();
    }

    let repo_path = match &req.repo_path {
        Some(p) if !p.trim().is_empty() => {
            let path = PathBuf::from(p.trim());
            if !path.join(".git").exists() && !path.join(".git").is_dir() {
                let err = ErrorEnvelope::new(
                    tool,
                    repo_name,
                    ErrorCode::InvalidInput,
                    format!("Not a git repository: {}", path.display()),
                    "Provide the path to the root of a git repository",
                );
                return serde_json::to_string(&err).unwrap_or_default();
            }
            path
        }
        _ => {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::InvalidInput,
                "repo_path is required",
                "Provide the path to the root of the git repository",
            );
            return serde_json::to_string(&err).unwrap_or_default();
        }
    };

    let graph_request = seshat_graph::DiffImpactRequest {
        staged_only,
        base: req.base.clone(),
        repo_path: repo_path.to_string_lossy().to_string(),
    };

    let result = seshat_graph::map_diff_impact(conn, branch, &repo_path, &graph_request);

    match result {
        Ok(data) => {
            let metadata = data.metadata.next_steps.clone();
            let meta = ResponseMetadata::new(metadata)
                .with_extra("changed_files_count", data.changed_files.len() as i64)
                .with_extra("affected_symbols_count", data.affected_symbols.len() as i64)
                .with_extra("convention_risks_count", data.convention_risks.len() as i64)
                .with_extra("risk", data.blast_radius_summary.risk.as_str());
            let envelope = ResponseEnvelope::success(tool, repo_name, data, meta);
            serialize_response(tool, repo_name, &envelope)
        }
        Err(e) => map_graph_error(tool, repo_name, e),
    }
}
