//! Implementation of the `seshat scan <path>` command.
//!
//! Runs the full scan pipeline: discovery -> parse -> detect -> aggregate -> store,
//! with uniform spinner-based progress display for all phases.

use std::path::Path;
use std::time::Instant;

use indicatif::{ProgressBar, ProgressStyle};
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, NodeId};
use seshat_detectors::{AggregatedConvention, aggregate_findings, run_all_detectors};
use seshat_scanner::{ScanProgress, scan_project_with_progress};
use seshat_storage::{Database, NodeRepository, SqliteNodeRepository};

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
    include_submodules: bool,
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
    if include_submodules {
        config.scan.include_submodules = true;
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

    // -- Run scan with progress ---------------------------------------
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
        serde_json::Value::String("auto_detected".to_owned()),
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
