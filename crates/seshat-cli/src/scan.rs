//! Implementation of the `seshat scan <path>` command.
//!
//! Runs the full scan pipeline: discovery -> parse -> detect -> aggregate -> store,
//! with uniform spinner-based progress display for all phases.

use std::path::Path;
use std::time::Instant;

use indicatif::{ProgressBar, ProgressStyle};
use seshat_core::{BranchId, DetectionConfig};
use seshat_detectors::{aggregate_findings, run_all_detectors};
use seshat_scanner::{
    ScanProgress, ScanResult, detect_submodule_paths, get_submodule_commit_hash,
    scan_project_with_progress,
};
use seshat_storage::{
    Database, EmbeddingInput, EmbeddingRepository, RepoMetadataRepository,
    SqliteEmbeddingRepository, SqliteRepoMetadataRepository, SqliteSubmoduleRepository,
    SubmoduleInput, SubmoduleRepository,
};

use crate::config::AppConfig;
use crate::db::unix_now;
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
    let mut config =
        AppConfig::load().map_err(|e| CliError::scan(format!("failed to load config: {e}")))?;

    // CLI flag overrides config file value.
    if exclude_submodules {
        config.scan.exclude_submodules = true;
    }

    // -- Open database ------------------------------------------------
    let db_path = resolve_db_path(&root)?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CliError::scan(format!("failed to create database directory: {e}")))?;
    }
    let db = Database::open(&db_path)
        .map_err(|e| CliError::scan(format!("failed to open database: {e}")))?;

    // -- Detect submodules early (before root scan) --------------------
    let submodule_paths = detect_submodule_paths(&root);
    let project_name = crate::db::project_name(&root);

    // -- Scan submodules first (each gets its own DB) -----------------
    let start = Instant::now();

    let show = verbosity.show_warnings();

    // -- Submodule scan phase -----------------------------------------
    // Track scanned submodules for updating the root DB's submodules table.
    struct ScannedSubmodule {
        mount_path: String,
        name: String,
        db_path: String,
        commit_hash: Option<String>,
    }

    // Look up stored submodule records from the root DB for change detection.
    let root_sub_repo_for_detect = SqliteSubmoduleRepository::new(db.connection().clone());

    // Scan submodules in parallel using std::thread::scope.
    // Each submodule gets its own thread, DB connection, and spinner line.
    // The root scan runs after all submodule threads complete.
    let scanned_submodules: Vec<ScannedSubmodule> = if !config.scan.exclude_submodules
        && !submodule_paths.is_empty()
    {
        // Pre-filter submodules: detect, check initialization, run change detection.
        // This is done on the main thread since it's fast (no scanning).
        enum SubmoduleAction {
            Skip(ScannedSubmodule),
            Scan {
                mount_path: String,
                name: String,
                submodule_abs: std::path::PathBuf,
                commit_hash: Option<String>,
            },
        }

        let mut actions: Vec<SubmoduleAction> = Vec::new();

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

            // -- Change detection: compare current hash with stored hash ------
            let stored_record = root_sub_repo_for_detect
                .find_by_path(mount_path)
                .map_err(|e| {
                    CliError::scan(format!("failed to look up submodule '{mount_path}': {e}"))
                })?;

            if let Some(ref stored) = stored_record {
                // Both hashes must be Some and equal for an up-to-date match.
                if let (Some(current_hash), Some(stored_hash)) = (&commit_hash, &stored.commit_hash)
                {
                    if current_hash == stored_hash {
                        // Commit hash matches — check whether the IR schema
                        // version in the existing DB is still current.
                        // If it isn't (e.g. IR_SCHEMA_VERSION was bumped since
                        // the last scan), we must re-scan even though the files
                        // haven't changed, so that all rows are rewritten with
                        // the new schema version and become visible to queries.
                        //
                        // Use stored.db_path (already the resolved path written
                        // by the previous scan) to open the submodule DB.
                        let schema_ok =
                            seshat_storage::Database::open(std::path::Path::new(&stored.db_path))
                                .ok()
                                .map(|sub_db| {
                                    crate::db::submodule_ir_schema_is_current(&sub_db, "main")
                                })
                                .unwrap_or(false); // can't open DB → force rescan

                        if schema_ok {
                            // Submodule is fully up-to-date — skip the scan.
                            if show {
                                let short = if current_hash.len() >= 7 {
                                    &current_hash[..7]
                                } else {
                                    current_hash
                                };
                                eprintln!("  \u{2713} Submodule {name} up-to-date ({short})");
                            }

                            actions.push(SubmoduleAction::Skip(ScannedSubmodule {
                                mount_path: mount_path.clone(),
                                name,
                                db_path: stored.db_path.clone(),
                                commit_hash,
                            }));
                            continue;
                        }

                        // Schema is stale — fall through to schedule a rescan.
                        if show {
                            eprintln!(
                                "  \u{21bb} Submodule {name} IR schema outdated, re-scanning..."
                            );
                        }
                    }
                }
            }

            // Hash differs or submodule is new — schedule for parallel scan.
            actions.push(SubmoduleAction::Scan {
                mount_path: mount_path.clone(),
                name,
                submodule_abs,
                commit_hash,
            });
        }

        // Collect skipped submodules immediately, scan the rest in parallel.
        let mut results: Vec<ScannedSubmodule> = Vec::new();
        let mut to_scan: Vec<(String, String, std::path::PathBuf, Option<String>)> = Vec::new();

        for action in actions {
            match action {
                SubmoduleAction::Skip(sub) => results.push(sub),
                SubmoduleAction::Scan {
                    mount_path,
                    name,
                    submodule_abs,
                    commit_hash,
                } => to_scan.push((mount_path, name, submodule_abs, commit_hash)),
            }
        }

        if !to_scan.is_empty() {
            // References shared across threads (read-only or thread-safe).
            let scan_config = &config.scan;
            let detection_config = &config.detection;
            let project_name_ref = &project_name;

            // Parallel scan via std::thread::scope — all threads join before scope exits.
            let parallel_results: Vec<Result<ScannedSubmodule, CliError>> =
                std::thread::scope(|scope| {
                    let handles: Vec<_> = to_scan
                        .iter()
                        .map(|(mount_path, name, submodule_abs, commit_hash)| {
                            let sp =
                                make_manual_spinner(&format!("{name}: discovering files..."), show);

                            scope.spawn(move || -> Result<ScannedSubmodule, CliError> {
                                // Each thread opens its own DB connection.
                                let sub_db_path = crate::db::resolve_submodule_db_path(
                                    project_name_ref,
                                    mount_path,
                                )?;
                                let sub_db = Database::open(&sub_db_path).map_err(|e| {
                                    CliError::scan(format!(
                                        "failed to open submodule database for '{mount_path}': {e}"
                                    ))
                                })?;

                                // Run the full scan pipeline, updating the spinner
                                // with phase info so the user sees progress.
                                let scan_result = scan_project_with_progress(
                                    submodule_abs,
                                    scan_config,
                                    &sub_db,
                                    |event| {
                                        match event {
                                            ScanProgress::Discovering { count } => {
                                                sp.set_message(format!(
                                                    "{name}: discovering files... {count} found"
                                                ));
                                            }
                                            ScanProgress::DiscoveryDone { total } => {
                                                sp.set_message(format!(
                                                    "{name}: discovering files... {total} found"
                                                ));
                                            }
                                            ScanProgress::CollectingGitHistory => {
                                                sp.set_message(format!(
                                                    "{name}: collecting git history..."
                                                ));
                                            }
                                            ScanProgress::Scanning { done, total } => {
                                                sp.set_message(format!(
                                                    "{name}: scanning files... {done}/{total}"
                                                ));
                                            }
                                            ScanProgress::BuildingModuleGraph => {
                                                sp.set_message(format!(
                                                    "{name}: building module graph..."
                                                ));
                                            }
                                            ScanProgress::AnalyzingProjectFiles => {
                                                sp.set_message(format!(
                                                    "{name}: analyzing manifests & docs..."
                                                ));
                                            }
                                            _ => {}
                                        }
                                        sp.tick();
                                    },
                                )
                                .map_err(|e| {
                                    CliError::scan(format!(
                                        "submodule scan failed for '{mount_path}': {e}"
                                    ))
                                })?;

                                sp.set_message(format!("{name}: analyzing conventions..."));
                                sp.tick();

                                let report = detect_and_persist(
                                    &sub_db,
                                    &detection_config.clone(),
                                    &scan_result,
                                )?;

                                // Write repo_metadata to submodule DB.
                                let meta =
                                    SqliteRepoMetadataRepository::new(sub_db.connection().clone());
                                write_metadata(
                                    &meta,
                                    &[
                                        ("parent_project", project_name_ref),
                                        ("mount_path", mount_path),
                                        ("file_count", &report.file_count.to_string()),
                                        ("convention_count", &report.convention_count.to_string()),
                                        ("last_scan_time", &unix_now().to_string()),
                                    ],
                                )?;

                                sp.finish_with_message(format!(
                                    "{name}: done ({} files, {} conventions)",
                                    report.file_count, report.convention_count,
                                ));

                                Ok(ScannedSubmodule {
                                    mount_path: mount_path.clone(),
                                    name: name.clone(),
                                    db_path: sub_db_path.to_string_lossy().to_string(),
                                    commit_hash: commit_hash.clone(),
                                })
                            })
                        })
                        .collect();

                    // Collect results from all threads.
                    handles
                        .into_iter()
                        .map(|h| h.join().expect("submodule scan thread panicked"))
                        .collect()
                });

            // Propagate any errors from parallel scans.
            for result in parallel_results {
                results.push(result?);
            }
        }

        results
    } else {
        Vec::new()
    };

    // -- Run root scan with progress ----------------------------------
    // Root scan is sequential (all submodules are done), so plain spinners
    // are fine — no MultiProgress needed.
    let discovery_sp = make_spinner("Discovering files...", show);

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

        // Submodule progress events are not emitted by the root orchestrator
        // (submodules are scanned in a separate phase above), but the enum
        // is exhaustive so we need a catch-all.
        _ => {}
    })
    .map_err(CliError::scan)?;

    // -- Run convention detection + persistence on root ----------------
    let detection_config = config.detection.clone();

    let detect_sp = make_spinner("Analyzing conventions...", show);
    let all_files = {
        use seshat_storage::{FileIRRepository, SqliteFileIRRepository};
        SqliteFileIRRepository::new(db.connection().clone())
            .get_by_branch(&BranchId::from("main"))
            .map_err(|e| CliError::scan(format!("failed to load files for detection: {e}")))?
    };

    // scan_result.source_map now contains source for ALL files (unchanged and
    // changed alike) — the orchestrator keeps source in memory for every file
    // it reads, not just the ones it re-parses.  So we can pass it directly
    // to run_all_detectors and every file will go through detect_with_source,
    // producing real snippets in convention evidence.
    let file_count = all_files.len();
    detect_sp.set_message(format!("Analyzing conventions... 0/{file_count}"));
    let progress_cb = |done: usize, _total: usize| {
        detect_sp.set_message(format!("Analyzing conventions... {done}/{file_count}"));
    };
    let detector_results = run_all_detectors(
        &all_files,
        &scan_result.source_map,
        &detection_config,
        Some(&progress_cb),
    );
    detect_sp.finish_with_message(format!(
        "Analyzing conventions... {file_count}/{file_count}"
    ));

    let all_findings: Vec<seshat_core::ConventionFinding> = detector_results
        .into_iter()
        .flat_map(|dr| dr.findings)
        .collect();

    let file_dates_map: std::collections::HashMap<String, Option<i64>> = all_files
        .iter()
        .map(|f| {
            let date = scan_result.file_dates.get(f.path.as_path()).copied();
            (f.path.to_string_lossy().to_string(), date)
        })
        .collect();

    let aggregated = aggregate_findings(
        &all_findings,
        &detection_config,
        &file_dates_map,
        unix_now(),
    );

    seshat_graph::persist_and_index(
        db.connection(),
        &BranchId::from("main"),
        &aggregated,
        &all_findings,
    )
    .map_err(|e| CliError::scan(format!("persist conventions: {e}")))?;

    // -- Generate embeddings (optional) --------------------------------
    // Pass changed_paths (not the full source_map) so that only new/changed
    // files get re-embedded.  Unchanged files already have current embeddings
    // in the DB and don't need to consume embedding API quota.
    if let Some(ref embedding_config) = config.embedding {
        generate_embeddings(
            &db,
            embedding_config,
            &all_files,
            &scan_result.source_map,
            &scan_result.changed_paths,
            "main",
            show,
        )?;
    }

    // -- Update root DB with submodule info + repo_metadata -----------
    let root_sub_repo = SqliteSubmoduleRepository::new(db.connection().clone());

    for sub in &scanned_submodules {
        root_sub_repo
            .upsert(&SubmoduleInput {
                relative_path: sub.mount_path.clone(),
                name: sub.name.clone(),
                db_path: sub.db_path.clone(),
                commit_hash: sub.commit_hash.clone(),
            })
            .map_err(|e| {
                CliError::scan(format!(
                    "failed to upsert submodule '{}' in root DB: {e}",
                    sub.mount_path
                ))
            })?;
    }

    // Remove submodules from the root DB that are no longer in .gitmodules.
    if let Ok(stored_submodules) = root_sub_repo.list() {
        let active_paths: std::collections::HashSet<&str> =
            submodule_paths.iter().map(|s| s.as_str()).collect();
        for stored in &stored_submodules {
            if !active_paths.contains(stored.relative_path.as_str()) {
                let _ = root_sub_repo.delete(&stored.relative_path);
            }
        }
    }

    // Write repo_metadata to root DB.
    let root_meta = SqliteRepoMetadataRepository::new(db.connection().clone());
    write_metadata(
        &root_meta,
        &[
            ("project_name", &project_name),
            ("file_count", &file_count.to_string()),
            ("convention_count", &aggregated.len().to_string()),
            ("last_scan_time", &unix_now().to_string()),
        ],
    )?;

    let elapsed = start.elapsed();

    // -- Build report data and print ----------------------------------
    let report_data = crate::report::build_report_data(
        &scan_result,
        &all_files,
        aggregated,
        &db_path,
        elapsed,
        config.scan.exclude_submodules,
    );
    crate::report::print_report(&report_data, verbosity, color);

    Ok(())
}

/// Shared spinner style for the standard braille animation.
fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.cyan} {msg}")
        .expect("valid template")
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"])
}

/// Create a spinner with automatic steady tick (80ms).
///
/// Use for main-thread spinners (root scan phases) where a background
/// tick thread is safe and keeps the animation smooth.
/// If `visible` is `false`, the spinner draws to a hidden target (silent mode).
fn make_spinner(msg: &str, visible: bool) -> ProgressBar {
    let sp = ProgressBar::new_spinner();
    if visible {
        sp.set_style(spinner_style());
        sp.set_message(msg.to_owned());
        sp.enable_steady_tick(std::time::Duration::from_millis(80));
    } else {
        sp.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    sp
}

/// Create a spinner driven manually via `tick()` + `set_message()`.
///
/// Use for worker-thread spinners (submodule scans) where the caller
/// drives updates from progress callbacks. No background tick thread —
/// avoids cursor-position races between the tick thread and the worker.
fn make_manual_spinner(msg: &str, visible: bool) -> ProgressBar {
    let sp = ProgressBar::new_spinner();
    if visible {
        sp.set_style(spinner_style());
        sp.set_message(msg.to_owned());
        sp.tick(); // draw initial frame
    } else {
        sp.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    sp
}

/// Resolve the database path for a project.
///
/// Delegates to shared `crate::db::resolve_db_path()` which uses
/// `$XDG_DATA_HOME/seshat/repos/{project_name}.db`.
fn resolve_db_path(root: &Path) -> Result<std::path::PathBuf, CliError> {
    crate::db::resolve_db_path(root)
}

// ── Shared scan pipeline helpers ─────────────────────────────

/// Result of [`detect_and_persist`] — counts for metadata writes.
struct DetectionReport {
    file_count: usize,
    convention_count: usize,
}

/// Run convention detection, aggregation, and persistence on an already-scanned DB.
///
/// Delegates to [`seshat_graph::run_detection_cycle`] — the single authoritative
/// implementation shared with the warm-tier watcher.
fn detect_and_persist(
    db: &Database,
    detection_config: &DetectionConfig,
    scan_result: &ScanResult,
) -> Result<DetectionReport, CliError> {
    // Build file-date map from the scan result so trend computation has git dates.
    let file_dates_map: std::collections::HashMap<String, Option<i64>> = scan_result
        .file_dates
        .iter()
        .map(|(p, &ts)| (p.to_string_lossy().to_string(), Some(ts)))
        .collect();

    let report = seshat_graph::run_detection_cycle(
        db.connection(),
        &BranchId::from("main"),
        detection_config,
        &file_dates_map,
    )
    .map_err(|e| CliError::scan(format!("detection pipeline failed: {e}")))?;

    Ok(DetectionReport {
        file_count: report.file_count,
        convention_count: report.convention_count,
    })
}

/// Write multiple key-value pairs to a [`SqliteRepoMetadataRepository`].
fn write_metadata(
    repo: &SqliteRepoMetadataRepository,
    pairs: &[(&str, &str)],
) -> Result<(), CliError> {
    for (key, value) in pairs {
        repo.set(key, value)
            .map_err(|e| CliError::scan(format!("failed to write metadata '{key}': {e}")))?;
    }
    Ok(())
}

/// Generate embeddings for all code items (functions, types, exports) in the project.
///
/// When an embedding provider is configured, this function:
/// 1. Creates the provider from config
/// 2. Collects all (function, type, export) items from all parsed files
/// 3. Batches texts and calls the provider
/// 4. Stores embeddings in the `code_embeddings` table
///
/// On failure (e.g., provider timeout, connection error), logs a warning and
/// continues — embedding is optional and should never break the scan pipeline.
fn generate_embeddings(
    db: &Database,
    embedding_config: &seshat_embedding::EmbeddingConfig,
    all_files: &[seshat_core::ProjectFile],
    source_map: &std::collections::HashMap<std::path::PathBuf, String>,
    changed_paths: &std::collections::HashSet<std::path::PathBuf>,
    branch_id: &str,
    show: bool,
) -> Result<(), CliError> {
    let provider = match seshat_embedding::create_provider(embedding_config) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to create embedding provider: {e}");
            if show {
                eprintln!("  \u{26a0} Embedding provider unavailable: {e}");
            }
            return Ok(());
        }
    };

    // Collect items to embed: (file_path, item_name, item_kind, text_to_embed)
    let mut items: Vec<(String, String, String, String)> = Vec::new();
    for file in all_files {
        // Skip files that haven't changed — their embeddings are already
        // current in the DB from the previous scan.  Only new/changed files
        // (tracked in changed_paths) need fresh embeddings.
        if !changed_paths.contains(&file.path) {
            continue;
        }
        // Source is always present in source_map for changed files.
        let source = match source_map.get(&file.path) {
            Some(s) => s,
            None => continue,
        };

        let file_path = file.path.to_string_lossy().to_string();

        // Use source already in memory — no disk read needed.
        let source_lines: Option<Vec<String>> = Some(source.lines().map(str::to_owned).collect());

        // Build import context string: module names imported in this file.
        // Filter empty module names (e.g. side-effect imports like `import './foo'`).
        // Cap at 20 modules to avoid consuming the model's token budget with boilerplate.
        let import_context = {
            let modules: Vec<&str> = file
                .imports
                .iter()
                .map(|i| i.module.as_str())
                .filter(|m| !m.is_empty())
                .take(20)
                .collect();
            if modules.is_empty() {
                String::new()
            } else {
                format!("\nuses: {}", modules.join(", "))
            }
        };

        for func in &file.functions {
            let vis = if func.is_public { "pub " } else { "" };
            let asyncness = if func.is_async { "async " } else { "" };
            let params = func.parameters.join(", ");
            let body_snippet =
                extract_body_snippet(source_lines.as_deref(), func.line, func.end_line);
            let text = format!(
                "{vis}{asyncness}fn {}({params}) in {file_path}{body_snippet}{import_context}",
                func.name
            );
            items.push((
                file_path.clone(),
                func.name.clone(),
                "function".to_string(),
                text,
            ));
        }
        for ty in &file.types {
            let vis = if ty.is_public { "pub " } else { "" };
            // Use explicit match instead of Debug format to get human-readable labels
            // (e.g. "type_alias" not "TypeAlias", "class" not "Class").
            let kind = match ty.kind {
                seshat_core::TypeDefKind::Struct => "struct",
                seshat_core::TypeDefKind::Enum => "enum",
                seshat_core::TypeDefKind::Trait => "trait",
                seshat_core::TypeDefKind::Interface => "interface",
                seshat_core::TypeDefKind::Class => "class",
                seshat_core::TypeDefKind::TypeAlias => "type_alias",
            };
            let text = format!("{vis}{kind} {} in {file_path}{import_context}", ty.name);
            items.push((file_path.clone(), ty.name.clone(), "type".to_string(), text));
        }
        for exp in &file.exports {
            let default = if exp.is_default { "default " } else { "" };
            let text = format!(
                "export {default}{} in {file_path}{import_context}",
                exp.name
            );
            items.push((
                file_path.clone(),
                exp.name.clone(),
                "export".to_string(),
                text,
            ));
        }
    }

    if items.is_empty() {
        tracing::info!("No code items to embed");
        return Ok(());
    }

    let total = items.len();
    let batch_size = embedding_config.batch_size.max(1);
    let embed_sp = make_spinner(&format!("Generating embeddings... 0/{total}"), show);

    let conn = db.connection().clone();
    let embedding_repo = SqliteEmbeddingRepository::new(conn);

    // NOTE: We intentionally do NOT delete_by_branch here. If embedding
    // generation fails mid-way (provider timeout, rate limit), we'd lose
    // the previously complete embedding set with nothing to replace it.
    // Instead we rely on upsert (ON CONFLICT DO UPDATE) — stale rows from
    // deleted/renamed files may remain, but that's less harmful than data loss.
    // A future improvement could diff current items vs stored and prune stale.

    let mut embedded_count: usize = 0;

    for chunk in items.chunks(batch_size) {
        let texts: Vec<String> = chunk.iter().map(|(_, _, _, text)| text.clone()).collect();

        match provider.embed(&texts) {
            Ok(embeddings) => {
                let inputs: Vec<EmbeddingInput> = chunk
                    .iter()
                    .zip(embeddings)
                    .map(
                        |((file_path, item_name, item_kind, _), emb)| EmbeddingInput {
                            file_path: file_path.clone(),
                            item_name: item_name.clone(),
                            item_kind: item_kind.clone(),
                            embedding: emb,
                        },
                    )
                    .collect();

                if let Err(e) = embedding_repo.upsert_batch(branch_id, &inputs) {
                    tracing::warn!("Failed to store embedding batch: {e}");
                    embed_sp.finish_with_message(
                        "Generating embeddings... failed (storage error)".to_string(),
                    );
                    return Ok(());
                }

                embedded_count += chunk.len();
                embed_sp.set_message(format!("Generating embeddings... {embedded_count}/{total}"));
            }
            Err(e) => {
                tracing::warn!(
                    embedded = embedded_count,
                    total = total,
                    remaining = total - embedded_count,
                    "Embedding provider error mid-batch; {embedded_count}/{total} items stored, \
                     {} items skipped. Database contains partial embeddings: {e}",
                    total - embedded_count,
                );
                embed_sp.finish_with_message(format!(
                    "Generating embeddings... failed ({embedded_count}/{total})"
                ));
                if show {
                    eprintln!(
                        "  \u{26a0} Embedding generation failed after {embedded_count}/{total} items \
                         ({} skipped, partial state): {e}",
                        total - embedded_count,
                    );
                }
                return Ok(());
            }
        }
    }

    embed_sp.finish_with_message(format!("Generating embeddings... {embedded_count}/{total}"));

    tracing::info!(
        count = embedded_count,
        total = total,
        "Generated code embeddings"
    );

    Ok(())
}

/// Extract a body snippet from source lines for use in embedding text.
///
/// Returns the first `HEAD_LINES` lines and last `TAIL_LINES` lines of the
/// function body (1-indexed, inclusive). If the function is short enough to
/// fit in HEAD_LINES + TAIL_LINES, returns all lines without duplication.
///
/// Returns an empty string if source lines are not available or line range
/// is out of bounds.
fn extract_body_snippet(
    source_lines: Option<&[String]>,
    start_line: usize,
    end_line: usize,
) -> String {
    const HEAD_LINES: usize = 5;
    const TAIL_LINES: usize = 3;

    let lines = match source_lines {
        Some(l) if !l.is_empty() && start_line > 0 => l,
        _ => return String::new(),
    };

    // Convert to 0-indexed, clamp to available lines.
    let start = (start_line - 1).min(lines.len());
    let end = end_line.min(lines.len());

    if start >= end {
        return String::new();
    }

    let body = &lines[start..end];

    // If the body fits within HEAD + TAIL lines (no gap between them), return all
    // lines — using ... only when there are lines that would be skipped.
    let snippet = if body.len() <= HEAD_LINES + TAIL_LINES {
        body.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        let head: Vec<&str> = body.iter().take(HEAD_LINES).map(String::as_str).collect();
        let tail: Vec<&str> = body
            .iter()
            .rev()
            .take(TAIL_LINES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(String::as_str)
            .collect();
        format!("{}\n...\n{}", head.join("\n"), tail.join("\n"))
    };

    format!("\n{}", snippet.trim())
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

    // -- US-005: Change detection unit tests --------------------------

    /// Helper: determine if a submodule should be skipped based on stored vs current hash.
    /// Returns true if the scan should be skipped (hashes match).
    fn should_skip_submodule(stored_hash: Option<&str>, current_hash: Option<&str>) -> bool {
        match (current_hash, stored_hash) {
            (Some(current), Some(stored)) => current == stored,
            _ => false,
        }
    }

    #[test]
    fn change_detection_skip_when_hashes_match() {
        // Both hashes are Some and equal → skip.
        assert!(should_skip_submodule(
            Some("abc123def456abc123def456abc123def456abc123"),
            Some("abc123def456abc123def456abc123def456abc123"),
        ));
    }

    #[test]
    fn change_detection_rescan_when_hashes_differ() {
        // Both hashes are Some but different → rescan.
        assert!(!should_skip_submodule(
            Some("abc123def456abc123def456abc123def456abc123"),
            Some("000000def456abc123def456abc123def456abc123"),
        ));
    }

    #[test]
    fn change_detection_rescan_when_no_stored_hash() {
        // Stored hash is None (first scan or no commits at previous scan) → rescan.
        assert!(!should_skip_submodule(
            None,
            Some("abc123def456abc123def456abc123def456abc123"),
        ));
    }

    #[test]
    fn change_detection_rescan_when_no_current_hash() {
        // Current hash is None (submodule has no commits now) → rescan.
        assert!(!should_skip_submodule(
            Some("abc123def456abc123def456abc123def456abc123"),
            None,
        ));
    }

    #[test]
    fn change_detection_rescan_when_both_hashes_none() {
        // Both hashes are None → rescan (can't confirm up-to-date).
        assert!(!should_skip_submodule(None, None));
    }

    #[test]
    fn change_detection_new_submodule_triggers_full_scan() {
        // New submodule: not in the stored table at all → no stored record.
        let root_db = Database::open(":memory:").expect("open DB");
        let sub_repo = SqliteSubmoduleRepository::new(root_db.connection().clone());

        // Submodule "frontend" not in the table yet.
        let stored = sub_repo.find_by_path("frontend").unwrap();
        assert!(stored.is_none(), "new submodule should not be in table");

        // Since there's no stored record, the change detection logic
        // will fall through to full scan (no match possible).
    }

    #[test]
    fn change_detection_updated_hash_stored_after_rescan() {
        let root_db = Database::open(":memory:").expect("open DB");
        let sub_repo = SqliteSubmoduleRepository::new(root_db.connection().clone());

        // Insert a submodule with an old hash.
        let old_hash = "aaaa".repeat(10);
        sub_repo
            .insert(&SubmoduleInput {
                relative_path: "frontend".to_string(),
                name: "frontend".to_string(),
                db_path: "/data/repos/project/frontend.db".to_string(),
                commit_hash: Some(old_hash.clone()),
            })
            .unwrap();

        // Simulate: current hash differs → rescan happened → update stored hash.
        let new_hash = "bbbb".repeat(10);
        sub_repo
            .update(&SubmoduleInput {
                relative_path: "frontend".to_string(),
                name: "frontend".to_string(),
                db_path: "/data/repos/project/frontend.db".to_string(),
                commit_hash: Some(new_hash.clone()),
            })
            .unwrap();

        let stored = sub_repo.find_by_path("frontend").unwrap().unwrap();
        assert_eq!(
            stored.commit_hash.as_deref(),
            Some(new_hash.as_str()),
            "stored hash should be updated after rescan"
        );

        // On the next scan, the hashes will match → skip.
        assert!(should_skip_submodule(
            stored.commit_hash.as_deref(),
            Some(&new_hash),
        ));
    }

    #[test]
    fn change_detection_skipped_submodule_not_deleted_from_table() {
        let root_db = Database::open(":memory:").expect("open DB");
        let sub_repo = SqliteSubmoduleRepository::new(root_db.connection().clone());

        let hash = "abcd".repeat(10);
        sub_repo
            .insert(&SubmoduleInput {
                relative_path: "frontend".to_string(),
                name: "frontend".to_string(),
                db_path: "/data/repos/project/frontend.db".to_string(),
                commit_hash: Some(hash.clone()),
            })
            .unwrap();

        // Simulate: submodule was skipped (up-to-date) but still tracked in
        // the scanned_submodules list, so cleanup won't delete it.
        let active_paths: std::collections::HashSet<&str> = ["frontend"].iter().copied().collect();

        let stored = sub_repo.list().unwrap();
        for stored_sub in &stored {
            if !active_paths.contains(stored_sub.relative_path.as_str()) {
                let _ = sub_repo.delete(&stored_sub.relative_path);
            }
        }

        let remaining = sub_repo.list().unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "skipped submodule should remain in table"
        );
        assert_eq!(remaining[0].relative_path, "frontend");
    }

    // ── extract_body_snippet tests ────────────────────────────────────────────

    fn make_lines(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("line_{i}")).collect()
    }

    #[test]
    fn body_snippet_none_source_returns_empty() {
        assert_eq!(extract_body_snippet(None, 1, 5), "");
    }

    #[test]
    fn body_snippet_start_zero_returns_empty() {
        let lines = make_lines(10);
        // start_line=0 is invalid (IR lines are 1-indexed)
        assert_eq!(extract_body_snippet(Some(&lines), 0, 5), "");
    }

    #[test]
    fn body_snippet_single_line_function() {
        let lines = make_lines(20);
        // Function at line 5, single line
        let result = extract_body_snippet(Some(&lines), 5, 5);
        assert!(!result.is_empty());
        assert!(result.contains("line_5"));
    }

    #[test]
    fn body_snippet_short_function_returns_all_lines() {
        let lines = make_lines(20);
        // Function lines 3-7 (5 lines) — fits in HEAD (5) without truncation
        let result = extract_body_snippet(Some(&lines), 3, 7);
        assert!(result.contains("line_3"));
        assert!(result.contains("line_7"));
        assert!(!result.contains("...")); // no truncation marker
    }

    #[test]
    fn body_snippet_long_function_has_head_and_tail() {
        let lines = make_lines(50);
        // Function lines 1-50 — should produce head...tail
        let result = extract_body_snippet(Some(&lines), 1, 50);
        assert!(result.contains("line_1")); // head
        assert!(result.contains("line_5")); // head last
        assert!(result.contains("...")); // truncation marker
        assert!(result.contains("line_50")); // tail last
        assert!(result.contains("line_48")); // tail first
        // middle lines should NOT appear
        assert!(!result.contains("line_25"));
    }

    #[test]
    fn body_snippet_exactly_boundary_no_overlap() {
        let lines = make_lines(20);
        // HEAD_LINES=5 + TAIL_LINES=3 = 8. Function with exactly 8 lines
        // should NOT produce ... (fits entirely)
        let result = extract_body_snippet(Some(&lines), 1, 8);
        assert!(
            !result.contains("..."),
            "8-line function should not be truncated"
        );
        assert!(result.contains("line_1"));
        assert!(result.contains("line_8")); // all 8 lines present
    }

    #[test]
    fn body_snippet_trim_applied() {
        let lines = vec![
            "  fn foo() {".to_owned(),
            "    let x = 1;".to_owned(),
            "  }".to_owned(),
        ];
        let result = extract_body_snippet(Some(&lines), 1, 3);
        // Should start with \n then trimmed content
        assert!(result.starts_with('\n'));
        assert!(!result.starts_with("\n  ")); // leading whitespace trimmed
    }
}
