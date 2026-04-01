//! Implementation of the `seshat scan <path>` command.
//!
//! Runs the full scan pipeline: discovery -> parse -> detect -> aggregate -> store,
//! with uniform spinner-based progress display for all phases.

use std::path::Path;
use std::time::Instant;

use indicatif::{ProgressBar, ProgressStyle};
use seshat_core::DetectionConfig;
use seshat_detectors::{aggregate_findings, run_all_detectors};
use seshat_scanner::{ScanProgress, scan_project_with_progress};
use seshat_storage::Database;

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

    let elapsed = start.elapsed();

    // -- Build report data and print ----------------------------------
    let report_data =
        crate::report::build_report_data(&scan_result, &all_files, aggregated, &db_path, elapsed);
    crate::report::print_report(&report_data, verbosity, color);

    Ok(())
}

/// Resolve the database path for a project.
///
/// Uses XDG data directory: `$XDG_DATA_HOME/seshat/repos/{project_name}.db`
/// Falls back to `~/.local/share/seshat/repos/{project_name}.db` on Linux/macOS.
fn resolve_db_path(root: &Path) -> Result<std::path::PathBuf, CliError> {
    let project_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_owned());

    let data_dir = dirs::data_dir().ok_or_else(|| CliError::CommandFailed {
        command: "scan".to_owned(),
        reason: "could not determine XDG data directory".to_owned(),
    })?;

    Ok(data_dir
        .join("seshat")
        .join("repos")
        .join(format!("{project_name}.db")))
}

/// Load all parsed files from the database for detection.
///
/// After the scan pipeline has stored file IR, we reload all files
/// from the database to run convention detectors on the complete set.
fn load_all_files_for_detection(
    db: &Database,
    _config: &DetectionConfig,
) -> Result<Vec<seshat_core::ProjectFile>, CliError> {
    use seshat_core::BranchId;
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
