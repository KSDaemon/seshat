//! Implementation of the `seshat scan <path>` command.
//!
//! Runs the full scan pipeline: discovery -> parse -> detect -> aggregate -> store,
//! with uniform spinner-based progress display for all phases.

use std::path::Path;
use std::time::Instant;

use indicatif::{ProgressBar, ProgressStyle};
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, NodeId};
use seshat_detectors::{AggregatedConvention, aggregate_findings, run_all_detectors};
use seshat_graph::SOURCE_AUTO_DETECTED;
use seshat_scanner::{
    ScanProgress, detect_submodule_paths, get_submodule_commit_hash, scan_project_with_progress,
};
use seshat_storage::{
    Database, NodeRepository, RepoMetadataRepository, SqliteNodeRepository,
    SqliteRepoMetadataRepository, SqliteSubmoduleRepository, SubmoduleInput, SubmoduleRepository,
};

use crate::config::AppConfig;
use crate::error::CliError;
use crate::format::{self, Verbosity};

/// Run the scan command on the given project directory.
///
/// # Pipeline
///
/// 1. Validate path
/// 2. Load config from `seshat.toml` (or defaults)
/// 3. Open database in XDG data directory
/// 4. Run scan pipeline with progress reporting
/// 5. Run convention detectors
/// 6. Aggregate findings
/// 7. Print report (verbosity-aware)
pub fn run_scan(
    path: &Path,
    verbose: bool,
    quiet: bool,
    exclude_submodules: bool,
) -> Result<(), CliError> {
    let verbosity = Verbosity::from_flags(verbose, quiet);
    let color = format::color_enabled();

    // -- Validate path ------------------------------------------------
    if !path.exists() {
        return Err(CliError::InvalidPath {
            path: path.display().to_string(),
            reason: "path does not exist".to_owned(),
        });
    }
    if !path.is_dir() {
        return Err(CliError::InvalidPath {
            path: path.display().to_string(),
            reason: "path is not a directory".to_owned(),
        });
    }

    let root = path.canonicalize().map_err(|e| CliError::InvalidPath {
        path: path.display().to_string(),
        reason: format!("failed to canonicalize: {e}"),
    })?;

    // -- Version header -----------------------------------------------
    if verbosity.show_warnings() {
        eprintln!("seshat v{}", env!("CARGO_PKG_VERSION"));
    }

    // -- Load config --------------------------------------------------
    let mut config = AppConfig::load().map_err(|e| CliError::CommandFailed {
        command: "scan".to_owned(),
        reason: format!("failed to load config: {e}"),
    })?;

    // CLI flag overrides config file value.
    if exclude_submodules {
        config.scan.exclude_submodules = true;
    }

    // -- Open database ------------------------------------------------
    let db_path = resolve_db_path(&root)?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to create database directory: {e}"),
        })?;
    }
    let db = Database::open(&db_path).map_err(|e| CliError::CommandFailed {
        command: "scan".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    // -- Detect submodules early (before root scan) --------------------
    let submodule_paths = detect_submodule_paths(&root);
    let project_name = crate::db::project_name(&root);

    // -- Scan submodules first (each gets its own DB) -----------------
    let start = Instant::now();

    // Helper: create a spinner with the standard braille style.
    let make_spinner = |msg: &str, visible: bool| -> ProgressBar {
        let sp = ProgressBar::new_spinner();
        if visible {
            sp.set_style(
                ProgressStyle::with_template("  {spinner:.cyan} {msg}")
                    .expect("valid template")
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]),
            );
            sp.set_message(msg.to_owned());
            sp.enable_steady_tick(std::time::Duration::from_millis(80));
        } else {
            sp.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        sp
    };

    let show = verbosity.show_warnings();

    // Phase 1: Discovery spinner.
    let discovery_sp = make_spinner("Discovering files...", show);

    // Lazily-created spinners for phases that start inside the orchestrator callback.
    let git_sp: std::cell::RefCell<Option<ProgressBar>> = std::cell::RefCell::new(None);
    let scan_sp: std::cell::RefCell<Option<ProgressBar>> = std::cell::RefCell::new(None);
    let graph_sp: std::cell::RefCell<Option<ProgressBar>> = std::cell::RefCell::new(None);
    let project_sp: std::cell::RefCell<Option<ProgressBar>> = std::cell::RefCell::new(None);

    // Per-submodule spinners keyed by mount path.
    let submodule_sps: std::cell::RefCell<std::collections::HashMap<String, ProgressBar>> =
        std::cell::RefCell::new(std::collections::HashMap::new());

    // -- Submodule scan phase -----------------------------------------
    // Track scanned submodules for updating the root DB's submodules table.
    struct ScannedSubmodule {
        mount_path: String,
        name: String,
        db_path: String,
        commit_hash: Option<String>,
    }
    let mut scanned_submodules: Vec<ScannedSubmodule> = Vec::new();

    if !config.scan.exclude_submodules {
        for mount_path in &submodule_paths {
            let submodule_abs = root.join(mount_path);
            let name = mount_path
                .rsplit('/')
                .next()
                .unwrap_or(mount_path)
                .to_string();

            // Emit SubmoduleDetected for each discovered submodule.
            if show {
                eprintln!("  \u{2139} Submodule detected: {mount_path}");
            }

            // Check if initialized (non-empty dir with .git).
            if !submodule_abs.is_dir()
                || (!submodule_abs.join(".git").exists() && !submodule_abs.join(".git").is_file())
            {
                if show {
                    let reason = "not initialized (no .git)";
                    eprintln!("  \u{2298} Submodule {name} skipped: {reason}");
                }
                continue;
            }

            // Get the current commit hash for the submodule.
            let commit_hash = get_submodule_commit_hash(&submodule_abs);

            // Open/create the submodule's dedicated DB.
            let sub_db_path = crate::db::resolve_submodule_db_path(&project_name, mount_path)?;
            let sub_db = Database::open(&sub_db_path).map_err(|e| CliError::CommandFailed {
                command: "scan".to_owned(),
                reason: format!("failed to open submodule database for '{mount_path}': {e}"),
            })?;

            // Show scanning spinner.
            let sp = make_spinner(&format!("Scanning submodule {name}..."), show);

            // Run the full scan pipeline on the submodule directory.
            let sub_scan_result =
                scan_project_with_progress(&submodule_abs, &config.scan, &sub_db, |_event| {
                    // Submodule inner progress is silenced; the parent spinner covers it.
                })
                .map_err(|e| CliError::CommandFailed {
                    command: "scan".to_owned(),
                    reason: format!("submodule scan failed for '{mount_path}': {e}"),
                })?;

            sp.finish_with_message(format!("Scanning submodule {name}... done"));

            // Run convention detection on the submodule.
            let sub_detection_config = config.detection.clone();
            let sub_files = load_all_files_for_detection(&sub_db, &sub_detection_config)?;
            let sub_file_count = sub_files.len();
            let sub_progress_cb = |_done: usize, _total: usize| {};
            let sub_detector_results =
                run_all_detectors(&sub_files, &sub_detection_config, Some(&sub_progress_cb));

            let sub_findings: Vec<seshat_core::ConventionFinding> = sub_detector_results
                .into_iter()
                .flat_map(|dr| dr.findings)
                .collect();

            let sub_file_dates_map: std::collections::HashMap<String, Option<i64>> = sub_files
                .iter()
                .map(|f| {
                    let date = sub_scan_result.file_dates.get(f.path.as_path()).copied();
                    (f.path.to_string_lossy().to_string(), date)
                })
                .collect();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let sub_aggregated = aggregate_findings(
                &sub_findings,
                &sub_detection_config,
                &sub_file_dates_map,
                now,
            );
            let convention_count = sub_aggregated.len();

            persist_conventions(&sub_db, &sub_aggregated)?;
            update_compliance_counts(&sub_db, &sub_findings)?;
            rebuild_fts_index(&sub_db)?;

            // Write repo_metadata to submodule DB.
            let sub_meta_repo = SqliteRepoMetadataRepository::new(sub_db.connection().clone());
            sub_meta_repo
                .set("parent_project", &project_name)
                .map_err(|e| CliError::CommandFailed {
                    command: "scan".to_owned(),
                    reason: format!("failed to write submodule metadata: {e}"),
                })?;
            sub_meta_repo
                .set("mount_path", mount_path)
                .map_err(|e| CliError::CommandFailed {
                    command: "scan".to_owned(),
                    reason: format!("failed to write submodule metadata: {e}"),
                })?;
            sub_meta_repo
                .set("file_count", &sub_file_count.to_string())
                .map_err(|e| CliError::CommandFailed {
                    command: "scan".to_owned(),
                    reason: format!("failed to write submodule metadata: {e}"),
                })?;
            sub_meta_repo
                .set("convention_count", &convention_count.to_string())
                .map_err(|e| CliError::CommandFailed {
                    command: "scan".to_owned(),
                    reason: format!("failed to write submodule metadata: {e}"),
                })?;
            sub_meta_repo
                .set("last_scan_time", &now.to_string())
                .map_err(|e| CliError::CommandFailed {
                    command: "scan".to_owned(),
                    reason: format!("failed to write submodule metadata: {e}"),
                })?;

            scanned_submodules.push(ScannedSubmodule {
                mount_path: mount_path.clone(),
                name,
                db_path: sub_db_path.to_string_lossy().to_string(),
                commit_hash,
            });
        }
    }

    // -- Run root scan with progress ----------------------------------
    let scan_result = scan_project_with_progress(&root, &config.scan, &db, |event| match event {
        ScanProgress::Discovering { count } => {
            discovery_sp.set_message(format!("Discovering files... {count} found"));
        }
        ScanProgress::DiscoveryDone { total } => {
            discovery_sp.finish_with_message(format!("Discovering files... {total} found"));
        }
        ScanProgress::CollectingGitHistory => {
            *git_sp.borrow_mut() = Some(make_spinner("Collecting git history...", show));
        }
        ScanProgress::GitHistoryDone => {
            if let Some(ref sp) = *git_sp.borrow() {
                sp.finish_with_message("Collecting git history... done");
            }
        }
        ScanProgress::Scanning { done, total } => {
            let mut sp_opt = scan_sp.borrow_mut();
            if sp_opt.is_none() {
                *sp_opt = Some(make_spinner(&format!("Scanning files... 0/{total}"), show));
            }
            if let Some(ref sp) = *sp_opt {
                sp.set_message(format!("Scanning files... {done}/{total}"));
            }
        }
        ScanProgress::ScanningDone => {
            if let Some(ref sp) = *scan_sp.borrow() {
                sp.finish_with_message(sp.message().to_string());
            }
        }
        ScanProgress::BuildingModuleGraph => {
            *graph_sp.borrow_mut() = Some(make_spinner("Building module graph...", show));
        }
        ScanProgress::ModuleGraphDone => {
            if let Some(ref sp) = *graph_sp.borrow() {
                sp.finish_with_message("Building module graph... done");
            }
        }
        ScanProgress::AnalyzingProjectFiles => {
            *project_sp.borrow_mut() = Some(make_spinner("Analyzing manifests & docs...", show));
        }
        ScanProgress::ProjectFilesDone => {
            if let Some(ref sp) = *project_sp.borrow() {
                sp.finish_with_message("Analyzing manifests & docs... done");
            }
        }

        // -- Submodule progress events --------------------------------
        ScanProgress::SubmoduleDetected { path } => {
            if show {
                eprintln!("  ℹ Submodule detected: {path}");
            }
        }
        ScanProgress::ScanningSubmodule { path, name } => {
            let sp = make_spinner(&format!("Scanning submodule {name}..."), show);
            submodule_sps.borrow_mut().insert(path.clone(), sp);
        }
        ScanProgress::ScanningSubmoduleDone { path } => {
            if let Some(sp) = submodule_sps.borrow().get(path) {
                let name = path.rsplit('/').next().unwrap_or(path);
                sp.finish_with_message(format!("Scanning submodule {name}... done"));
            }
        }
        ScanProgress::SubmoduleUpToDate { path, hash } => {
            let short_hash = if hash.len() >= 7 { &hash[..7] } else { hash };
            let name = path.rsplit('/').next().unwrap_or(path);
            if show {
                eprintln!("  ✓ Submodule {name} up-to-date ({short_hash})");
            }
        }
        ScanProgress::SubmoduleSkipped { path, reason } => {
            let name = path.rsplit('/').next().unwrap_or(path);
            if show {
                eprintln!("  ⊘ Submodule {name} skipped: {reason}");
            }
        }
    })
    .map_err(|e| CliError::CommandFailed {
        command: "scan".to_owned(),
        reason: e.to_string(),
    })?;

    // -- Run convention detectors -------------------------------------
    let detection_config = config.detection.clone();

    // Start the detection spinner BEFORE the DB load so the user
    // never sees a blinking cursor with no context.
    let detect_sp = make_spinner("Analyzing conventions...", show);
    let all_files = load_all_files_for_detection(&db, &detection_config)?;

    let file_count = all_files.len();
    detect_sp.set_message(format!("Analyzing conventions... 0/{file_count}"));
    let progress_cb = |done: usize, _total: usize| {
        detect_sp.set_message(format!("Analyzing conventions... {done}/{file_count}"));
    };
    let detector_results = run_all_detectors(&all_files, &detection_config, Some(&progress_cb));
    detect_sp.finish_with_message(format!(
        "Analyzing conventions... {file_count}/{file_count}"
    ));

    // -- Aggregate findings -------------------------------------------
    let all_findings: Vec<seshat_core::ConventionFinding> = detector_results
        .into_iter()
        .flat_map(|dr| dr.findings)
        .collect();

    // Use file dates from scan result (no duplicate collect_git_file_dates call).
    let file_dates_map: std::collections::HashMap<String, Option<i64>> = all_files
        .iter()
        .map(|f| {
            let date = scan_result.file_dates.get(f.path.as_path()).copied();
            (f.path.to_string_lossy().to_string(), date)
        })
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let aggregated = aggregate_findings(&all_findings, &detection_config, &file_dates_map, now);

    // -- Persist conventions to nodes table ----------------------------
    persist_conventions(&db, &aggregated)?;

    // -- Compute and persist per-file convention compliance counts -----
    update_compliance_counts(&db, &all_findings)?;

    // -- Rebuild FTS5 index -------------------------------------------
    rebuild_fts_index(&db)?;

    // -- Update root DB with submodule info + repo_metadata -----------
    let root_sub_repo = SqliteSubmoduleRepository::new(db.connection().clone());
    let root_meta_repo = SqliteRepoMetadataRepository::new(db.connection().clone());

    // Update submodules table in root DB for each scanned submodule.
    for sub in &scanned_submodules {
        let input = SubmoduleInput {
            relative_path: sub.mount_path.clone(),
            name: sub.name.clone(),
            db_path: sub.db_path.clone(),
            commit_hash: sub.commit_hash.clone(),
        };

        // Try update first; if the submodule is new, insert it.
        match root_sub_repo.update(&input) {
            Ok(()) => {}
            Err(seshat_storage::StorageError::NotFound { .. }) => {
                root_sub_repo
                    .insert(&input)
                    .map_err(|e| CliError::CommandFailed {
                        command: "scan".to_owned(),
                        reason: format!(
                            "failed to insert submodule '{}' in root DB: {e}",
                            sub.mount_path
                        ),
                    })?;
            }
            Err(e) => {
                return Err(CliError::CommandFailed {
                    command: "scan".to_owned(),
                    reason: format!(
                        "failed to update submodule '{}' in root DB: {e}",
                        sub.mount_path
                    ),
                });
            }
        }
    }

    // Remove submodules from the root DB that are no longer in .gitmodules.
    if let Ok(stored_submodules) = root_sub_repo.list() {
        let active_paths: std::collections::HashSet<&str> =
            submodule_paths.iter().map(|s| s.as_str()).collect();
        for stored in &stored_submodules {
            if !active_paths.contains(stored.relative_path.as_str()) {
                // Submodule removed from .gitmodules — delete from table (leave DB on disk).
                let _ = root_sub_repo.delete(&stored.relative_path);
            }
        }
    }

    // Write repo_metadata to root DB.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    root_meta_repo
        .set("project_name", &project_name)
        .map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to write root metadata: {e}"),
        })?;
    root_meta_repo
        .set("file_count", &file_count.to_string())
        .map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to write root metadata: {e}"),
        })?;
    root_meta_repo
        .set("convention_count", &aggregated.len().to_string())
        .map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to write root metadata: {e}"),
        })?;
    root_meta_repo
        .set("last_scan_time", &now.to_string())
        .map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to write root metadata: {e}"),
        })?;

    let elapsed = start.elapsed();

    // -- Build report data and print ----------------------------------
    let report_data =
        crate::report::build_report_data(&scan_result, &all_files, aggregated, &db_path, elapsed);
    crate::report::print_report(&report_data, verbosity, color);

    Ok(())
}

/// Resolve the database path for a project.
///
/// Delegates to shared `crate::db::resolve_db_path()` which uses
/// `$XDG_DATA_HOME/seshat/repos/{project_name}.db`.
fn resolve_db_path(root: &Path) -> Result<std::path::PathBuf, CliError> {
    crate::db::resolve_db_path(root)
}

/// Persist aggregated conventions to the nodes table.
///
/// On re-scan, this replaces all auto-detected convention nodes while
/// preserving user-recorded decisions (`ext_data.source = "user"`).
fn persist_conventions(db: &Database, aggregated: &[AggregatedConvention]) -> Result<(), CliError> {
    let conn = db.connection().clone();
    let node_repo = SqliteNodeRepository::new(conn);
    let branch_id = BranchId::from("main");

    // Delete previous auto-detected convention nodes (preserves user decisions).
    node_repo
        .delete_auto_detected_by_branch(&branch_id)
        .map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to clear old conventions: {e}"),
        })?;

    // Insert each aggregated convention as a KnowledgeNode.
    for convention in aggregated {
        let node = convention_to_node(convention, &branch_id);
        node_repo
            .insert(&node)
            .map_err(|e| CliError::CommandFailed {
                command: "scan".to_owned(),
                reason: format!("failed to persist convention: {e}"),
            })?;
    }

    tracing::info!(
        count = aggregated.len(),
        "Persisted convention nodes to database"
    );

    Ok(())
}

/// Rebuild the FTS5 full-text search index after convention persistence.
///
/// Clears the existing index and repopulates from convention nodes in the
/// `nodes` table. This ensures the FTS5 index stays in sync after every scan.
fn rebuild_fts_index(db: &Database) -> Result<(), CliError> {
    seshat_graph::rebuild_fts_index(db.connection()).map_err(|e| CliError::CommandFailed {
        command: "scan".to_owned(),
        reason: format!("failed to rebuild FTS5 index: {e}"),
    })?;
    Ok(())
}

/// Compute per-file convention compliance counts and update the `files_ir` table.
///
/// Counts how many findings have `follows_convention == true` for each file
/// and writes those counts to the `convention_compliance_count` column.
fn update_compliance_counts(
    db: &Database,
    findings: &[seshat_core::ConventionFinding],
) -> Result<(), CliError> {
    use seshat_storage::{FileIRRepository, SqliteFileIRRepository};

    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for finding in findings {
        if finding.follows_convention {
            let file_key = finding.file_path.to_string_lossy().to_string();
            *counts.entry(file_key).or_insert(0) += 1;
        }
    }

    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch_id = BranchId::from("main");

    file_ir_repo
        .update_convention_compliance_counts(&branch_id, &counts)
        .map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to update convention compliance counts: {e}"),
        })?;

    tracing::info!(
        files_with_conventions = counts.len(),
        "Updated per-file convention compliance counts"
    );

    Ok(())
}

/// Convert an [`AggregatedConvention`] to a [`KnowledgeNode`] for storage.
///
/// The `ext_data` JSON includes:
/// - `source`: always `"auto_detected"` (distinguishes from user decisions)
/// - `detector_name`: which detector produced this convention
/// - `trend`: rising/stable/declining/unknown
/// - `adoption_rate`: confidence as a float
/// - `evidence`: array of `{file, line, end_line, snippet}` objects
fn convention_to_node(convention: &AggregatedConvention, branch_id: &BranchId) -> KnowledgeNode {
    // Build evidence array for ext_data.
    let evidence_json: Vec<serde_json::Value> = convention
        .evidence
        .iter()
        .map(|e| {
            serde_json::json!({
                "file": e.snippet.lines().next().unwrap_or(""),
                "line": e.line,
                "end_line": e.end_line,
                "snippet": e.snippet,
            })
        })
        .collect();

    // Start with trend + adoption_rate from the existing ext_data helper.
    let mut ext_data = convention
        .ext_data(None)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    ext_data.insert(
        "source".to_owned(),
        serde_json::Value::String(SOURCE_AUTO_DETECTED.to_owned()),
    );
    ext_data.insert(
        "detector_name".to_owned(),
        serde_json::Value::String(convention.detector_name.clone()),
    );
    ext_data.insert(
        "evidence".to_owned(),
        serde_json::Value::Array(evidence_json),
    );

    KnowledgeNode {
        id: NodeId(0), // Auto-assigned by DB
        branch_id: branch_id.clone(),
        nature: convention.nature,
        weight: convention.weight,
        confidence: convention.confidence,
        adoption_count: convention.adoption_count,
        total_count: convention.total_count,
        description: convention.description.clone(),
        ext_data: Some(serde_json::Value::Object(ext_data)),
    }
}

/// Load all parsed files from the database for detection.
///
/// After the scan pipeline has stored file IR, we reload all files
/// from the database to run convention detectors on the complete set.
fn load_all_files_for_detection(
    db: &Database,
    _config: &DetectionConfig,
) -> Result<Vec<seshat_core::ProjectFile>, CliError> {
    use seshat_storage::{FileIRRepository, SqliteFileIRRepository};

    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch_id = BranchId::from("main");

    file_ir_repo
        .get_by_branch(&branch_id)
        .map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to load files for detection: {e}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_scanner::scan_project;
    use seshat_storage::{
        Database, RepoMetadataRepository, SqliteRepoMetadataRepository, SqliteSubmoduleRepository,
        SubmoduleInput, SubmoduleRepository,
    };
    use std::fs;
    use tempfile::tempdir;

    /// Helper: create a root project with a mock submodule directory.
    ///
    /// Layout:
    /// ```text
    /// root/
    ///   .git/
    ///   .gitmodules          (declares "frontend" submodule)
    ///   src/main.rs
    ///   frontend/
    ///     .git/              (marks it as an initialized submodule)
    ///     src/app.ts
    /// ```
    fn create_project_with_submodule() -> tempfile::TempDir {
        let dir = tempdir().expect("create tempdir");
        let root = dir.path();

        // Root project
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/main.rs"),
            "pub fn main() { println!(\"hello\"); }\n",
        )
        .unwrap();

        // .gitmodules declaring the submodule
        fs::write(
            root.join(".gitmodules"),
            "[submodule \"frontend\"]\n\tpath = frontend\n\turl = https://example.com/fe.git\n",
        )
        .unwrap();

        // Submodule directory (initialized with .git)
        fs::create_dir_all(root.join("frontend/.git")).unwrap();
        fs::create_dir_all(root.join("frontend/src")).unwrap();
        fs::write(
            root.join("frontend/src/app.ts"),
            "export function app(): string { return 'hello'; }\n",
        )
        .unwrap();

        dir
    }

    #[test]
    fn submodule_scan_creates_separate_dbs_with_correct_structure() {
        let dir = create_project_with_submodule();
        let root = dir.path();
        let config = seshat_core::ScanConfig::default();

        // Create root DB and submodule DB (both in-memory for testing).
        let root_db = Database::open(":memory:").expect("open root DB");
        let sub_db = Database::open(":memory:").expect("open submodule DB");

        // Scan root project (submodule dirs are excluded from root discovery).
        let root_result = scan_project(root, &config, &root_db).expect("root scan should succeed");
        assert!(
            !root_result.excluded_submodules.is_empty(),
            "should detect submodule in .gitmodules"
        );
        assert_eq!(root_result.excluded_submodules, vec!["frontend"]);

        // Root should only find main.rs (frontend is excluded).
        assert_eq!(
            root_result.files_discovered, 1,
            "root should discover 1 file (main.rs)"
        );

        // Scan submodule directory into its own DB.
        let sub_root = root.join("frontend");
        let sub_result =
            scan_project(&sub_root, &config, &sub_db).expect("submodule scan should succeed");
        assert_eq!(
            sub_result.files_discovered, 1,
            "submodule should discover 1 file (app.ts)"
        );

        // Verify both DBs have IR records.
        use seshat_storage::{FileIRRepository, SqliteFileIRRepository};
        let branch = BranchId::from("main");

        let root_files = SqliteFileIRRepository::new(root_db.connection().clone())
            .get_by_branch(&branch)
            .unwrap();
        assert_eq!(root_files.len(), 1, "root DB should have 1 file IR");

        let sub_files = SqliteFileIRRepository::new(sub_db.connection().clone())
            .get_by_branch(&branch)
            .unwrap();
        assert_eq!(sub_files.len(), 1, "submodule DB should have 1 file IR");

        // Write repo_metadata to submodule DB (as run_scan does).
        let sub_meta = SqliteRepoMetadataRepository::new(sub_db.connection().clone());
        sub_meta.set("parent_project", "my-project").unwrap();
        sub_meta.set("mount_path", "frontend").unwrap();
        sub_meta
            .set("file_count", &sub_result.files_discovered.to_string())
            .unwrap();
        sub_meta.set("convention_count", "0").unwrap();
        sub_meta.set("last_scan_time", "1700000000").unwrap();

        assert_eq!(
            sub_meta.get("parent_project").unwrap().unwrap(),
            "my-project"
        );
        assert_eq!(sub_meta.get("mount_path").unwrap().unwrap(), "frontend");
        assert_eq!(sub_meta.get("file_count").unwrap().unwrap(), "1");

        // Write submodule record to root DB (as run_scan does).
        let root_sub_repo = SqliteSubmoduleRepository::new(root_db.connection().clone());
        root_sub_repo
            .insert(&SubmoduleInput {
                relative_path: "frontend".to_string(),
                name: "frontend".to_string(),
                db_path: "/data/seshat/repos/my-project/frontend.db".to_string(),
                commit_hash: None, // mock submodule has no real commits
            })
            .unwrap();

        let stored = root_sub_repo.list().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].relative_path, "frontend");
        assert_eq!(stored[0].name, "frontend");

        // Write repo_metadata to root DB.
        let root_meta = SqliteRepoMetadataRepository::new(root_db.connection().clone());
        root_meta.set("project_name", "my-project").unwrap();
        root_meta
            .set("file_count", &root_result.files_discovered.to_string())
            .unwrap();
        root_meta.set("convention_count", "0").unwrap();
        root_meta.set("last_scan_time", "1700000000").unwrap();

        assert_eq!(
            root_meta.get("project_name").unwrap().unwrap(),
            "my-project"
        );
        assert_eq!(root_meta.get("file_count").unwrap().unwrap(), "1");
    }

    #[test]
    fn uninitialised_submodule_is_skipped() {
        let dir = tempdir().expect("create tempdir");
        let root = dir.path();

        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "pub fn main() {}\n").unwrap();

        // .gitmodules declares a submodule that exists as a directory but has no .git
        fs::write(
            root.join(".gitmodules"),
            "[submodule \"libs/shared\"]\n\tpath = libs/shared\n\turl = https://example.com\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("libs/shared")).unwrap();
        // No .git in libs/shared — it's not initialized

        let config = seshat_core::ScanConfig::default();
        let db = Database::open(":memory:").expect("open DB");

        let result = scan_project(root, &config, &db).expect("scan should succeed");

        // Submodule dirs are always excluded from root discovery.
        assert_eq!(result.excluded_submodules, vec!["libs/shared"]);
        // Root only finds main.rs.
        assert_eq!(result.files_discovered, 1);
    }

    #[test]
    fn submodule_removed_from_gitmodules_gets_deleted_from_table() {
        let root_db = Database::open(":memory:").expect("open DB");
        let sub_repo = SqliteSubmoduleRepository::new(root_db.connection().clone());

        // Simulate a previously scanned submodule in the table.
        sub_repo
            .insert(&SubmoduleInput {
                relative_path: "old-module".to_string(),
                name: "old-module".to_string(),
                db_path: "/data/repos/project/old-module.db".to_string(),
                commit_hash: Some("abc123".to_string()),
            })
            .unwrap();

        // Current .gitmodules no longer includes "old-module".
        let active_paths: std::collections::HashSet<&str> = ["frontend"].iter().copied().collect();

        let stored = sub_repo.list().unwrap();
        for stored_sub in &stored {
            if !active_paths.contains(stored_sub.relative_path.as_str()) {
                let _ = sub_repo.delete(&stored_sub.relative_path);
            }
        }

        let remaining = sub_repo.list().unwrap();
        assert!(
            remaining.is_empty(),
            "old-module should have been removed from submodules table"
        );
    }
}
