//! Implementation of the `seshat serve` command.
//!
//! Discovers the project database via smart resolution (explicit repo argument,
//! current working directory, git root walk-up, or single-DB fallback), displays
//! startup information, and starts the MCP server on stdio transport with
//! graceful Ctrl+C shutdown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use seshat_core::BranchId;
use seshat_mcp::{ProjectConnection, ScanState};
use seshat_scanner::scan_project;
use seshat_storage::{
    BranchRepository, Database, SqliteBranchRepository, SqliteSubmoduleRepository,
    SubmoduleRepository, SubmoduleRow,
};
use seshat_watcher::{WatcherParams, start_watcher};
use tokio::sync::oneshot;

use crate::config::AppConfig;
use crate::db::{ServeTarget, detect_branch, gc_branch_snapshots};
use crate::error::CliError;

/// Handle for the GC background task.
///
/// Call [`GcHandle::shutdown`] (or simply drop) to stop the periodic GC task.
pub struct GcHandle {
    shutdown_tx: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl GcHandle {
    /// Signal the GC task to stop and await its completion.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.task).await;
    }
}

/// Metadata about a discovered scanned project database.
struct RepoInfo {
    /// Human-readable project name (derived from DB filename).
    name: String,
    /// Path to the `.db` file.
    db_path: PathBuf,
    /// Current branch stored in the database.
    branch: BranchId,
    /// Number of indexed files.
    file_count: usize,
    /// Number of convention nodes.
    convention_count: usize,
}

/// Resolve the call log path from CLI flag and config.
///
/// Priority: CLI flag > config value > disabled.
/// - `Some("")` (bare `--call-log`) → default path `$XDG_DATA_HOME/seshat/call-log.jsonl`
/// - `Some("/path")` → explicit path
/// - `None` + `Some(config)` → config value
/// - `None` + `None` config → disabled
fn resolve_call_log_path(cli_flag: Option<PathBuf>, config_value: Option<&str>) -> Option<PathBuf> {
    match cli_flag {
        Some(p) if p.as_os_str().is_empty() => {
            // Bare --call-log with no value → use default path
            let data_dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
            Some(data_dir.join("seshat").join("call-log.jsonl"))
        }
        Some(p) => Some(p),
        None => config_value.map(PathBuf::from),
    }
}

/// Handle branch switching and snapshot logic for the serve flow.
///
/// For ExistingDb: if detected branch differs from DB's current branch,
/// switch to it (creating a snapshot from source if target has no data).
/// For AutoScan: if detected branch differs from "main" and "main" has data,
/// create a snapshot from "main" to the detected branch.
///
/// Returns the final branch ID after any switch.
fn handle_branch_switch(
    db: &Database,
    detected_branch: &str,
    current_branch: &BranchId,
    _is_auto_scan: bool,
) -> Result<BranchId, CliError> {
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());

    // Check if we need to switch branches.
    if detected_branch == current_branch.0 {
        return Ok(current_branch.clone());
    }

    let detected_id = BranchId::from(detected_branch);

    // Check if target branch already has data.
    let branches = branch_repo
        .list_branches()
        .map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("failed to list branches: {e}"),
        })?;

    let target_has_data = branches.iter().any(|b| b.0 == detected_branch);

    if !target_has_data {
        // Target branch has no data — check if source has data to snapshot.
        let source_branch = current_branch.clone();

        // Check source has actual data (not just registered).
        let source_branches = branch_repo
            .list_branches()
            .map_err(|e| CliError::CommandFailed {
                command: "serve".to_owned(),
                reason: format!("failed to list branches: {e}"),
            })?;
        let source_has_data = source_branches.iter().any(|b| b.0 == source_branch.0);

        if !source_has_data {
            tracing::info!(
                source_branch = %source_branch.0,
                target_branch = %detected_branch,
                "Source branch has no data — switching without snapshot"
            );
        } else {
            tracing::info!(
                source_branch = %source_branch.0,
                target_branch = %detected_branch,
                "Target branch has no data — creating snapshot from source"
            );
            branch_repo
                .create_snapshot(&source_branch, &detected_id)
                .map_err(|e| CliError::CommandFailed {
                    command: "serve".to_owned(),
                    reason: format!("failed to create snapshot: {e}"),
                })?;
        }
    }

    // Switch to the detected branch.
    tracing::info!(
        from = %current_branch.0,
        to = %detected_branch,
        "Switching branch"
    );
    branch_repo
        .switch_branch(&detected_id)
        .map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("failed to switch branch: {e}"),
        })?;

    Ok(detected_id)
}

/// Handle branch snapshot for AutoScan path.
///
/// If detected branch differs from "main" and "main" has data,
/// create a snapshot from "main" to the detected branch.
///
/// Returns the final branch ID after any switch.
fn handle_auto_scan_snapshot(db: &Database, detected_branch: &str) -> Result<BranchId, CliError> {
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());

    if detected_branch == "main" {
        return Ok(BranchId::from(detected_branch));
    }

    let detected_id = BranchId::from(detected_branch);

    // Check if "main" has data.
    let branches = branch_repo
        .list_branches()
        .map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("failed to list branches: {e}"),
        })?;

    let main_has_data = branches.iter().any(|b| b.0 == "main");

    if !main_has_data {
        return Ok(detected_id);
    }

    // Create snapshot from "main" to detected branch.
    let main_branch = BranchId::from("main");
    tracing::info!(
        source_branch = "main",
        target_branch = %detected_branch,
        "Auto-scan on non-main branch — creating snapshot from main"
    );
    branch_repo
        .create_snapshot(&main_branch, &detected_id)
        .map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("failed to create snapshot: {e}"),
        })?;

    // Switch to the detected branch.
    branch_repo
        .switch_branch(&detected_id)
        .map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("failed to switch branch: {e}"),
        })?;

    Ok(detected_id)
}

/// Run the serve command.
///
/// Discovers the project database (from explicit repo arg, cwd, git root, or
/// single-DB fallback), loads it, displays startup information, and starts the
/// MCP server on stdio transport.
pub fn run_serve(
    repo: Option<&Path>,
    host: Option<String>,
    port: Option<u16>,
    call_log: Option<PathBuf>,
) -> Result<(), CliError> {
    // -- Load config --------------------------------------------------
    let mut config = AppConfig::load().map_err(|e| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!("failed to load config: {e}"),
    })?;

    // CLI flags override config values.
    if let Some(h) = host {
        config.server.host = h;
    }
    if let Some(p) = port {
        config.server.port = p;
    }

    // -- Discover databases or project root --------------------------
    let target = crate::db::resolve_serve_db_or_project_root(repo)?;

    let (db_path, db, mut repo_info, scan_state, auto_scan_project_root, detected_branch) =
        match target {
            ServeTarget::ExistingDb {
                db_path,
                project_root,
            } => {
                let db = Database::open(&db_path).map_err(|e| CliError::CommandFailed {
                    command: "serve".to_owned(),
                    reason: format!("failed to open database: {e}"),
                })?;
                let detected = detect_branch(&project_root);
                let repo_info = load_repo_info(&db, &db_path)?;
                (
                    db_path,
                    db,
                    repo_info,
                    ScanState::not_needed(),
                    None,
                    detected,
                )
            }
            ServeTarget::AutoScan {
                project_root,
                db_path,
            } => {
                // Detect git branch before creating DB.
                let detected = detect_branch(&project_root);

                // Create empty DB (migrations auto-apply).
                let db = Database::open(&db_path).map_err(|e| CliError::CommandFailed {
                    command: "serve".to_owned(),
                    reason: format!("failed to create database: {e}"),
                })?;
                tracing::info!(
                    project_root = %project_root.display(),
                    db_path = %db_path.display(),
                    detected_branch = %detected,
                    "No existing DB found — starting auto-scan"
                );

                // Create scan state before the discovery check so that any early
                // error paths can still transition it to Failed.
                let scan_state = ScanState::in_progress();

                // File count pre-check: abort auto-scan if project is too large.
                let scan_config = config.scan.clone();
                let auto_scan_limit = scan_config.auto_scan_limit;
                match seshat_scanner::discover_files(&project_root, &scan_config) {
                    Ok(discovery_result) => {
                        let file_count = discovery_result.files.len();

                        if file_count > auto_scan_limit {
                            scan_state.mark_failed(format!(
                            "Project too large for auto-scan ({} files). Run: seshat scan --verbose",
                            file_count
                        ));
                            let repo_info = load_repo_info(&db, &db_path)?;
                            (db_path, db, repo_info, scan_state, None, detected)
                        } else {
                            let repo_info = load_repo_info(&db, &db_path)?;
                            (
                                db_path,
                                db,
                                repo_info,
                                scan_state,
                                Some(project_root),
                                detected,
                            )
                        }
                    }
                    Err(e) => {
                        // Discovery failed — continue with empty DB.
                        // MCP calls will get AUTO_SCAN_FAILED error.
                        scan_state.mark_failed(format!("auto-scan discovery failed: {e}"));
                        let repo_info = load_repo_info(&db, &db_path)?;
                        (db_path, db, repo_info, scan_state, None, detected)
                    }
                }
            }
        };

    // -- Handle branch switching / snapshots --------------------------
    let is_auto_scan = auto_scan_project_root.is_some();

    let final_branch = if is_auto_scan {
        handle_auto_scan_snapshot(&db, &detected_branch)?
    } else {
        handle_branch_switch(&db, &detected_branch, &repo_info.branch, is_auto_scan)?
    };

    // Update repo_info.branch to reflect the actual branch after any switch.
    repo_info.branch = final_branch.clone();

    // -- Run branch snapshot garbage collection -----------------------
    let gc_repo_path = match &auto_scan_project_root {
        Some(root) => root.clone(),
        None => crate::db::find_git_root(&std::env::current_dir().unwrap_or_default())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    };
    if let Ok(deleted) = gc_branch_snapshots(&db, &gc_repo_path) {
        if !deleted.is_empty() {
            tracing::info!(
                deleted_count = deleted.len(),
                deleted_branches = ?deleted,
                "Garbage collected orphan branch snapshots on startup"
            );
        }
    }

    // -- Load submodule connections -----------------------------------
    let submodule_rows = load_submodule_rows(&db);
    let submodules = open_submodule_connections(&submodule_rows, &repo_info.name);

    // -- Resolve call log path ----------------------------------------
    let call_log_path = resolve_call_log_path(call_log, config.server.call_log.as_deref());

    // -- Create embedding provider (optional) -------------------------
    let embedding_provider: Option<Arc<dyn seshat_embedding::EmbeddingProvider>> =
        config.embedding.as_ref().and_then(|emb_config| {
            match seshat_embedding::create_provider(emb_config) {
                Ok(provider) => {
                    tracing::info!("Embedding provider enabled: {emb_config}");
                    Some(Arc::from(provider))
                }
                Err(e) => {
                    tracing::warn!("Failed to create embedding provider: {e}");
                    eprintln!("  Warning: embedding provider unavailable: {e}");
                    None
                }
            }
        });

    // -- Start MCP server (async via tokio) ---------------------------
    let server_config = config.server.clone();
    let _start = Instant::now();

    let runtime = tokio::runtime::Runtime::new().map_err(|e| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!("failed to create tokio runtime: {e}"),
    })?;

    let root = ProjectConnection::new(
        db.connection().clone(),
        repo_info.name.clone(),
        detected_branch.clone(),
    );

    // Derive project root for the watcher: use the auto-scan root if available,
    // otherwise walk up from cwd to find the git root, or fall back to cwd itself.
    let project_root = match &auto_scan_project_root {
        Some(root) => root.clone(),
        None => crate::db::find_git_root(&std::env::current_dir().unwrap_or_default())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    };

    let watcher_enabled = config.watcher.enabled;
    let watcher_params = WatcherParams {
        enabled: watcher_enabled,
        debounce_ms: config.watcher.debounce_ms,
        ignore_patterns: config.watcher.ignore_patterns.clone(),
        warm_tier_interval_seconds: config.watcher.warm_tier_interval_seconds,
        bulk_change_threshold: config.watcher.bulk_change_threshold,
    };
    let watcher_scan_config = config.scan.clone();
    let watcher_detection_config = config.detection.clone();

    let has_auto_scan = auto_scan_project_root.is_some();
    let auto_scan_root = auto_scan_project_root.clone();

    runtime
        .block_on(async {
            let scan_state_clone = scan_state.clone();

            // -- Launch background scan (if auto-scan) ------------------
            if let Some(scan_root) = auto_scan_root.clone() {
                let scan_config = config.scan.clone();
                let scan_db = db.clone();
                let scan_branch = detected_branch.clone();
                tokio::spawn(async move {
                    let branch = seshat_core::BranchId::from(scan_branch);
                    let result = tokio::task::spawn_blocking(move || {
                        scan_project(&scan_root, &scan_config, &scan_db, branch)
                    })
                    .await;
                    match result {
                        Ok(Ok(_scan_result)) => {
                            tracing::info!("Auto-scan completed successfully");
                            scan_state_clone.mark_complete();
                        }
                        Ok(Err(scan_err)) => {
                            tracing::error!("Auto-scan failed: {scan_err}");
                            scan_state_clone.mark_failed(scan_err.to_string());
                        }
                        Err(join_err) => {
                            tracing::error!("Auto-scan task panicked: {join_err}");
                            scan_state_clone.mark_failed(join_err.to_string());
                        }
                    }
                });
            }

            // -- Launch periodic GC background task -------------------
            let gc_db = db.clone();
            let gc_repo_path = gc_repo_path.clone();
            let (gc_shutdown_tx, mut gc_shutdown_rx) = oneshot::channel();
            let gc_task = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let db_clone = gc_db.clone();
                            let path_clone = gc_repo_path.clone();
                            match tokio::task::spawn_blocking(move || {
                                gc_branch_snapshots(&db_clone, &path_clone)
                            })
                            .await
                            {
                                Ok(Ok(deleted_list)) => {
                                    if !deleted_list.is_empty() {
                                        tracing::info!(
                                            deleted_count = deleted_list.len(),
                                            deleted_branches = ?deleted_list,
                                            "Periodic branch snapshot garbage collection"
                                        );
                                    }
                                }
                                Ok(Err(e)) => {
                                    tracing::error!(error = %e, "Periodic GC failed");
                                }
                                Err(join_err) => {
                                    tracing::error!(error = %join_err, "Periodic GC task panicked");
                                }
                            }
                        }
                        _ = &mut gc_shutdown_rx => {
                            tracing::debug!("GC background task shutting down");
                            break;
                        }
                    }
                }
            });
            let gc_handle = GcHandle {
                shutdown_tx: gc_shutdown_tx,
                task: gc_task,
            };

            // -- Start watcher (delayed if auto-scan) ------------------
            // When auto-scan is in progress, watcher must wait for scan to
            // complete before starting (it needs a populated DB).
            let watcher_rx = if watcher_enabled {
                let (watcher_tx, watcher_rx) = tokio::sync::oneshot::channel();
                let params = watcher_params;
                let root = project_root.clone();
                let db_p = db_path.clone();
                let conn = db.connection().clone();
                let branch = BranchId::from(detected_branch.as_str());
                let scan_cfg = watcher_scan_config;
                let detect_cfg = watcher_detection_config;
                let wait_scan = scan_state.clone();

                let on_branch_switch: Arc<dyn Fn() + Send + Sync + 'static> = {
                    let root_clone = project_root.clone();
                    let db_path_clone = db_path.clone();
                    Arc::new(move || {
                        let root = root_clone.clone();
                        let db_path = db_path_clone.clone();
                        std::thread::spawn(move || {
                            let start = Instant::now();
                            let new_branch = detect_branch(&root);
                            let db = match Database::open(&db_path) {
                                Ok(d) => d,
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to open DB for branch switch");
                                    return;
                                }
                            };
                            let branch_repo = SqliteBranchRepository::new(db.connection().clone());
                            let current_branch = branch_repo
                                .get_current_branch()
                                .map(|b| b.0.clone())
                                .unwrap_or_else(|_| "main".to_string());

                            tracing::info!(
                                old_branch = %current_branch,
                                new_branch = %new_branch,
                                "Branch switch detected by watcher"
                            );
                            if new_branch == current_branch {
                                tracing::debug!("Branch unchanged, no switch needed");
                                return;
                            }
                            let new_id = BranchId::from(new_branch.as_str());
                            let old_id = BranchId::from(current_branch.as_str());

                            let branches = match branch_repo.list_branches() {
                                Ok(b) => b,
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to list branches for switch");
                                    return;
                                }
                            };
                            let snapshot_exists = branches.iter().any(|b| b.0 == new_branch);
                            if snapshot_exists {
                                match branch_repo.switch_branch(&new_id) {
                                    Ok(()) => {
                                        let elapsed = start.elapsed();
                                        tracing::info!(
                                            to = %new_branch,
                                            elapsed_ms = elapsed.as_millis(),
                                            "Branch switch completed (instant, snapshot existed)"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(error = %e, "Failed to switch branch");
                                    }
                                }
                            } else {
                                tracing::info!(
                                    source = %current_branch,
                                    target = %new_branch,
                                    "No snapshot for target — creating"
                                );
                                match branch_repo.create_snapshot(&old_id, &new_id) {
                                    Ok(()) => {
                                        match branch_repo.switch_branch(&new_id) {
                                            Ok(()) => {
                                                let elapsed = start.elapsed();
                                                tracing::info!(
                                                    to = %new_branch,
                                                    elapsed_ms = elapsed.as_millis(),
                                                    "Branch switch completed (snapshot created)"
                                                );
                                            }
                                            Err(e) => {
                                                tracing::error!(error = %e, "Failed to switch after snapshot");
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!(error = %e, "Failed to create snapshot");
                                    }
                                }
                            }
                        });
                    })
                };

                tokio::spawn(async move {
                    // If auto-scan is in progress, wait for it to complete
                    // before starting the watcher.
                    wait_scan.wait_for_scan();

                    let result =
                        start_watcher(params, root, db_p, conn, branch, scan_cfg, detect_cfg, on_branch_switch).await;
                    if let Err(ref e) = result {
                        tracing::warn!(
                            "File watcher failed to start: {e}. \
                             Serving without incremental updates."
                        );
                    }
                    let _ = watcher_tx.send(result);
                });
                Some(watcher_rx)
            } else {
                None
            };

            // -- Print startup banner ------------------------------------
            let watcher_status = if has_auto_scan && scan_state.error_message().is_some() {
                "disabled (auto-scan failed)"
            } else if has_auto_scan
                && scan_state.error_message().is_none()
                && !scan_state.auto_scanned()
            {
                "starting (after scan)"
            } else if watcher_enabled {
                "starting"
            } else {
                "disabled"
            };
            print_startup(
                &repo_info,
                &submodules,
                &config,
                call_log_path.as_deref(),
                watcher_status,
                is_auto_scan,
                &detected_branch,
            );

            // -- Run MCP server -----------------------------------------
            let shutdown = async {
                tokio::signal::ctrl_c()
                    .await
                    .expect("failed to listen for Ctrl+C");
                eprintln!();
                eprintln!("Shutting down...");
            };

            let result = seshat_mcp::start_stdio_with_shutdown(
                server_config,
                root,
                submodules,
                call_log_path,
                embedding_provider,
                scan_state,
                shutdown,
                std::time::Duration::from_secs(5),
            )
            .await;

            // -- Shutdown GC background task ------------------------------
            drop(gc_handle);

            // -- Shutdown watcher ---------------------------------------
            if let Some(mut rx) = watcher_rx {
                if let Ok(Ok(handle)) = rx.try_recv() {
                    handle.shutdown().await;
                }
            }

            result
        })
        .map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("MCP server error: {e}"),
        })
}

/// Load repository metadata from the database for startup display.
fn load_repo_info(db: &Database, db_path: &Path) -> Result<RepoInfo, CliError> {
    let name = db_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_owned());

    let info = crate::db::load_project_info(db);

    Ok(RepoInfo {
        name,
        db_path: db_path.to_path_buf(),
        branch: info.branch,
        file_count: info.file_count,
        convention_count: info.convention_count,
    })
}

/// Load the list of submodule rows from the root database.
///
/// Returns an empty `Vec` if the query fails (e.g. empty DB, no submodules
/// table data).
fn load_submodule_rows(db: &Database) -> Vec<SubmoduleRow> {
    let sub_repo = SqliteSubmoduleRepository::new(db.connection().clone());
    match sub_repo.list() {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!(
                "  Warning: could not read submodules table: {e}. Continuing without submodules."
            );
            Vec::new()
        }
    }
}

/// Open database connections for each submodule and build the `ProjectConnection` map.
///
/// For each submodule row, resolves the DB path, opens the database, reads its
/// branch, and wraps it in a `ProjectConnection`. If a submodule DB is missing
/// or fails to open, a warning is logged and that submodule is skipped.
fn open_submodule_connections(
    rows: &[SubmoduleRow],
    root_project_name: &str,
) -> HashMap<String, ProjectConnection> {
    let mut submodules = HashMap::new();

    for row in rows {
        let db_path =
            match crate::db::resolve_submodule_db_path(root_project_name, &row.relative_path) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!(
                        "  Warning: could not resolve DB path for submodule '{}': {e}. Skipping.",
                        row.relative_path
                    );
                    continue;
                }
            };

        if !db_path.exists() {
            eprintln!(
                "  Warning: submodule DB not found at '{}'. Skipping '{}'.",
                db_path.display(),
                row.relative_path
            );
            continue;
        }

        let db = match Database::open(&db_path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "  Warning: failed to open submodule DB '{}': {e}. Skipping '{}'.",
                    db_path.display(),
                    row.relative_path
                );
                continue;
            }
        };

        // Read the submodule's branch (default to "main" if not set).
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let branch = branch_repo.get_current_branch().unwrap_or_else(|_| {
            tracing::debug!("Could not detect submodule branch from DB, defaulting to 'main'");
            BranchId::from("main")
        });

        let pc = ProjectConnection::new(
            db.connection().clone(),
            row.relative_path.clone(),
            branch.to_string(),
        );

        submodules.insert(row.relative_path.clone(), pc);
    }

    submodules
}

/// Print the startup information block to stderr.
fn print_startup(
    info: &RepoInfo,
    submodules: &HashMap<String, ProjectConnection>,
    config: &AppConfig,
    call_log_path: Option<&Path>,
    watcher_status: &str,
    auto_scanning: bool,
    detected_branch: &str,
) {
    eprintln!("seshat v{}", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("  Repo:         {}", info.name);
    eprintln!("  Branch:       {}", detected_branch);
    if auto_scanning {
        eprintln!("  Files:        0 (auto-scanning...)");
    } else {
        eprintln!("  Files:        {}", info.file_count);
    }
    eprintln!("  Conventions:  {}", info.convention_count);
    eprintln!("  Database:     {}", info.db_path.display());
    eprintln!("  Watcher:      {watcher_status}");

    if submodules.is_empty() {
        eprintln!("  Submodules:   none");
    } else {
        eprintln!("  Submodules:   {}", submodules.len());
        let mut names: Vec<&String> = submodules.keys().collect();
        names.sort();
        for name in names {
            eprintln!("    - {name}");
        }
    }

    if let Some(path) = call_log_path {
        eprintln!("  Call log:     {}", path.display());
    }

    eprintln!();
    eprintln!(
        "  Transport:    stdio ({}:{})",
        config.server.host, config.server.port
    );
    eprintln!();
    eprintln!("Ready. Waiting for MCP client connection...");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_repo_info_empty_db() {
        // Verify that load_repo_info works with an empty in-memory DB.
        let db = Database::open(":memory:").expect("in-memory db");
        let path = PathBuf::from("/tmp/test-seshat-project.db");
        let info = load_repo_info(&db, &path).expect("should succeed with empty db");
        assert_eq!(info.name, "test-seshat-project");
        assert_eq!(info.file_count, 0);
        assert_eq!(info.convention_count, 0);
        assert_eq!(info.branch, BranchId::from("main"));
    }

    #[test]
    fn load_submodule_rows_empty_db() {
        let db = Database::open(":memory:").expect("in-memory db");
        let rows = load_submodule_rows(&db);
        assert!(rows.is_empty());
    }

    #[test]
    fn load_submodule_rows_with_data() {
        use seshat_storage::{SqliteSubmoduleRepository, SubmoduleInput, SubmoduleRepository};

        let db = Database::open(":memory:").expect("in-memory db");
        let sub_repo = SqliteSubmoduleRepository::new(db.connection().clone());
        sub_repo
            .insert(&SubmoduleInput {
                relative_path: "vendor/libfoo".to_string(),
                name: "libfoo".to_string(),
                db_path: "/data/seshat/repos/proj/vendor/libfoo.db".to_string(),
                commit_hash: Some("abc123".to_string()),
            })
            .expect("insert");
        sub_repo
            .insert(&SubmoduleInput {
                relative_path: "libs/core".to_string(),
                name: "core".to_string(),
                db_path: "/data/seshat/repos/proj/libs/core.db".to_string(),
                commit_hash: Some("def456".to_string()),
            })
            .expect("insert");

        let rows = load_submodule_rows(&db);
        assert_eq!(rows.len(), 2);
        // list() returns sorted by relative_path
        assert_eq!(rows[0].relative_path, "libs/core");
        assert_eq!(rows[1].relative_path, "vendor/libfoo");
    }

    #[test]
    fn open_submodule_connections_empty_rows() {
        let submodules = open_submodule_connections(&[], "test-project");
        assert!(submodules.is_empty());
    }

    #[test]
    fn open_submodule_connections_missing_db_skipped() {
        let project_name = "serve-test-missing-db";

        let row = SubmoduleRow {
            id: 1,
            relative_path: "vendor/nonexistent".to_string(),
            name: "nonexistent".to_string(),
            db_path: "/no/such/path.db".to_string(),
            commit_hash: Some("abc123".to_string()),
            created_at: "2026-04-03T00:00:00".to_string(),
            updated_at: "2026-04-03T00:00:00".to_string(),
        };

        let submodules = open_submodule_connections(&[row], project_name);
        // Should be empty since the DB file doesn't exist.
        assert!(submodules.is_empty());

        // Clean up directories created as side effect of resolve_submodule_db_path.
        if let Ok(repos) = crate::db::xdg_repos_dir() {
            let _ = std::fs::remove_dir_all(repos.join(project_name));
        }
    }

    #[test]
    fn resolve_call_log_bare_flag_uses_default_path() {
        // --call-log with no value → default_missing_value="" → empty PathBuf
        let result = resolve_call_log_path(Some(PathBuf::from("")), None);
        let path = result.expect("should resolve to default path");
        assert!(path.to_string_lossy().ends_with("seshat/call-log.jsonl"));
    }

    #[test]
    fn resolve_call_log_explicit_path() {
        let result = resolve_call_log_path(Some(PathBuf::from("/tmp/my-log.jsonl")), None);
        assert_eq!(result, Some(PathBuf::from("/tmp/my-log.jsonl")));
    }

    #[test]
    fn resolve_call_log_from_config() {
        let result = resolve_call_log_path(None, Some("/config/path.jsonl"));
        assert_eq!(result, Some(PathBuf::from("/config/path.jsonl")));
    }

    #[test]
    fn resolve_call_log_cli_overrides_config() {
        let result = resolve_call_log_path(
            Some(PathBuf::from("/cli/path.jsonl")),
            Some("/config/path.jsonl"),
        );
        assert_eq!(result, Some(PathBuf::from("/cli/path.jsonl")));
    }

    #[test]
    fn resolve_call_log_disabled_when_no_flag_and_no_config() {
        let result = resolve_call_log_path(None, None);
        assert!(result.is_none());
    }

    #[test]
    fn open_submodule_connections_with_real_dbs() {
        use std::fs;

        let project_name = "serve-test-submod";
        let mount_path = "vendor/testlib";

        // resolve_submodule_db_path creates the DB in the real XDG data dir
        // (required because open_submodule_connections resolves paths itself).
        let db_path =
            crate::db::resolve_submodule_db_path(project_name, mount_path).expect("resolve path");

        // RAII guard: clean up the XDG directory on drop (even on panic).
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let repos_dir = crate::db::xdg_repos_dir().expect("xdg repos dir");
        let _guard = Cleanup(repos_dir.join(project_name));

        let db = Database::open(&db_path).expect("create submodule DB");
        drop(db);

        let row = SubmoduleRow {
            id: 1,
            relative_path: mount_path.to_string(),
            name: "testlib".to_string(),
            db_path: db_path.to_string_lossy().to_string(),
            commit_hash: Some("abc123".to_string()),
            created_at: "2026-04-03T00:00:00".to_string(),
            updated_at: "2026-04-03T00:00:00".to_string(),
        };

        let submodules = open_submodule_connections(&[row], project_name);
        assert_eq!(submodules.len(), 1);
        assert!(submodules.contains_key(mount_path));

        let pc = &submodules[mount_path];
        assert_eq!(pc.name, mount_path);
        assert_eq!(pc.branch, "main"); // default branch for empty DB
        // _guard drops here, cleaning up the project dir.
    }
}
