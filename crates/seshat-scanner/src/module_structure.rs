//! Module structure detection and dependency graph construction.
//!
//! Analyzes parsed [`ProjectFile`]s to detect module boundaries (directories
//! containing source files) and build a dependency graph from import/export
//! relationships. Produces [`KnowledgeNode`]s (Fact nature) for each module
//! and [`Edge`]s for `DependsOn` (import relationships) and `PartOf`
//! (submodule hierarchy) relationships.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use seshat_core::{
    BranchId, Edge, EdgeId, EdgeType, KnowledgeNature, KnowledgeNode, KnowledgeWeight, Language,
    NodeId, ProjectFile,
};

/// A detected module in the project.
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    /// Relative path of the module directory from the project root.
    pub path: PathBuf,
    /// Files contained directly in this module directory.
    pub files: Vec<PathBuf>,
    /// Languages used in this module.
    pub languages: BTreeSet<String>,
}

/// The complete module structure analysis result.
#[derive(Debug)]
pub struct ModuleGraph {
    /// Knowledge nodes representing each module (Fact nature).
    pub nodes: Vec<KnowledgeNode>,
    /// Edges: DependsOn (import relationships) and PartOf (hierarchy).
    pub edges: Vec<Edge>,
    /// Module info indexed by module path (for querying).
    pub modules: HashMap<PathBuf, ModuleInfo>,
    /// Mapping from module path to assigned node ID (for query lookups).
    path_to_node_id: HashMap<PathBuf, NodeId>,
    /// Reverse mapping from node ID to module path.
    node_id_to_path: HashMap<NodeId, PathBuf>,
}

impl ModuleGraph {
    /// Find all modules that the given module depends on (outgoing DependsOn edges).
    pub fn dependencies_of(&self, module_path: &Path) -> Vec<&PathBuf> {
        let Some(&source_node_id) = self.path_to_node_id.get(module_path) else {
            return Vec::new();
        };

        self.edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn && e.source_id == source_node_id)
            .filter_map(|e| self.node_id_to_path.get(&e.target_id))
            .collect()
    }

    /// Find all modules that depend on the given module (incoming DependsOn edges).
    pub fn dependents_of(&self, module_path: &Path) -> Vec<&PathBuf> {
        let Some(&target_node_id) = self.path_to_node_id.get(module_path) else {
            return Vec::new();
        };

        self.edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn && e.target_id == target_node_id)
            .filter_map(|e| self.node_id_to_path.get(&e.source_id))
            .collect()
    }
}

/// Analyze parsed files to detect module structure and build a dependency graph.
///
/// # Arguments
///
/// * `project_root` - The root directory of the project (used to compute relative paths).
/// * `parsed_files` - All parsed [`ProjectFile`]s from the scanning pipeline.
/// * `branch_id` - The branch identifier for the knowledge graph nodes and edges.
///
/// # Returns
///
/// A [`ModuleGraph`] containing:
/// - Knowledge nodes (Fact nature, Info weight) for each detected module.
/// - DependsOn edges between modules based on import relationships.
/// - PartOf edges from submodules to their parent modules.
pub fn build_module_graph(
    project_root: &Path,
    parsed_files: &[ProjectFile],
    branch_id: &BranchId,
) -> ModuleGraph {
    // Step 1: Detect modules — group files by their parent directory.
    let mut dir_files: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut dir_languages: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();

    for pf in parsed_files {
        let rel_path = make_relative(&pf.path, project_root);
        let dir = rel_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();

        dir_files
            .entry(dir.clone())
            .or_default()
            .push(rel_path.clone());
        dir_languages
            .entry(dir)
            .or_default()
            .insert(pf.language.as_str().to_owned());
    }

    // Build a map from relative path → parsed file for quick lookup.
    let file_map: HashMap<PathBuf, &ProjectFile> = parsed_files
        .iter()
        .map(|pf| (make_relative(&pf.path, project_root), pf))
        .collect();

    // Build ordered list of module paths for stable node ID assignment.
    let module_paths: Vec<PathBuf> = dir_files.keys().cloned().collect();
    let path_to_node_id: HashMap<&PathBuf, NodeId> = module_paths
        .iter()
        .enumerate()
        .map(|(i, p)| (p, NodeId((i + 1) as i64)))
        .collect();

    // Build ModuleInfo map.
    let mut modules: HashMap<PathBuf, ModuleInfo> = HashMap::new();
    for (dir, files) in &dir_files {
        modules.insert(
            dir.clone(),
            ModuleInfo {
                path: dir.clone(),
                files: files.clone(),
                languages: dir_languages.get(dir).cloned().unwrap_or_default(),
            },
        );
    }

    // Step 2: Create KnowledgeNode for each module.
    let nodes: Vec<KnowledgeNode> = module_paths
        .iter()
        .map(|dir| {
            let info = &modules[dir];
            let node_id = path_to_node_id[dir];

            // Compute human-readable purpose from doc-comments / symbols.
            let purpose = derive_module_purpose(&info.files, &file_map);

            let description = format!(
                "Module '{}' containing {} file(s) [{}]",
                if dir.as_os_str().is_empty() {
                    "(root)"
                } else {
                    dir.to_str().unwrap_or("(non-utf8)")
                },
                info.files.len(),
                info.languages
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            let mut ext = serde_json::json!({
                "source": "module_structure",
                "module_path": dir.to_str().unwrap_or(""),
                "file_count": info.files.len(),
                "languages": info.languages.iter().cloned().collect::<Vec<_>>(),
                "files": info.files.iter().map(|f| f.to_str().unwrap_or("").to_owned()).collect::<Vec<_>>(),
            });
            if let Some(ref p) = purpose {
                ext["purpose"] = serde_json::Value::String(p.clone());
            }
            let ext_data = ext;

            KnowledgeNode {
                id: node_id,
                branch_id: branch_id.clone(),
                nature: KnowledgeNature::Fact,
                weight: KnowledgeWeight::Info,
                confidence: 1.0,
                adoption_count: 1,
                total_count: 1,
                description,
                ext_data: Some(ext_data),
            }
        })
        .collect();

    // Step 3: Resolve import module paths to target module directories.
    // Build a map from potential import targets to their module directories.
    let import_target_map = build_import_target_map(project_root, parsed_files);

    // Step 5: Build DependsOn edges from imports.
    let mut edge_id_counter: i64 = 1;
    let mut depends_on_set: BTreeSet<(PathBuf, PathBuf)> = BTreeSet::new();

    for pf in parsed_files {
        let rel = make_relative(&pf.path, project_root);
        let source_dir = rel.parent().map(|p| p.to_path_buf()).unwrap_or_default();

        for import in &pf.imports {
            if let Some(target_dir) = resolve_import_to_module(
                &import.module,
                &source_dir,
                &import_target_map,
                &pf.language,
            ) {
                // Skip self-imports (same module).
                if target_dir != source_dir {
                    depends_on_set.insert((source_dir.clone(), target_dir));
                }
            }
        }
    }

    let mut edges: Vec<Edge> = Vec::new();

    for (source_dir, target_dir) in &depends_on_set {
        if let (Some(&source_id), Some(&target_id)) = (
            path_to_node_id.get(source_dir),
            path_to_node_id.get(target_dir),
        ) {
            edges.push(Edge {
                id: EdgeId(edge_id_counter),
                source_id,
                target_id,
                edge_type: EdgeType::DependsOn,
                branch_id: branch_id.clone(),
                weight: 1.0,
                metadata: Some(serde_json::json!({
                    "source_module": source_dir.to_str().unwrap_or(""),
                    "target_module": target_dir.to_str().unwrap_or(""),
                })),
            });
            edge_id_counter += 1;
        }
    }

    // Step 6: Build PartOf edges for module hierarchy.
    for dir in &module_paths {
        if dir.as_os_str().is_empty() {
            continue; // Root has no parent.
        }
        if let Some(parent) = dir.parent() {
            let parent_path = parent.to_path_buf();
            // Only add PartOf if the parent is itself a detected module.
            if let (Some(&child_id), Some(&parent_id)) =
                (path_to_node_id.get(dir), path_to_node_id.get(&parent_path))
            {
                edges.push(Edge {
                    id: EdgeId(edge_id_counter),
                    source_id: child_id,
                    target_id: parent_id,
                    edge_type: EdgeType::PartOf,
                    branch_id: branch_id.clone(),
                    weight: 1.0,
                    metadata: Some(serde_json::json!({
                        "child_module": dir.to_str().unwrap_or(""),
                        "parent_module": parent_path.to_str().unwrap_or(""),
                    })),
                });
                edge_id_counter += 1;
            }
        }
    }

    // Build lookup maps for queries.
    let path_to_node_id_owned: HashMap<PathBuf, NodeId> = path_to_node_id
        .iter()
        .map(|(p, &id)| ((*p).clone(), id))
        .collect();
    let node_id_to_path: HashMap<NodeId, PathBuf> = path_to_node_id_owned
        .iter()
        .map(|(p, &id)| (id, p.clone()))
        .collect();

    ModuleGraph {
        nodes,
        edges,
        modules,
        path_to_node_id: path_to_node_id_owned,
        node_id_to_path,
    }
}

/// Build a map from import target strings to the module directory containing
/// the target file. This handles:
/// - Relative file paths (e.g., "./utils", "../models/user")
/// - Module/package paths for each language
/// - Directory-level module paths (e.g., "src/models" → src/models directory)
fn build_import_target_map(
    project_root: &Path,
    parsed_files: &[ProjectFile],
) -> HashMap<String, PathBuf> {
    let mut map: HashMap<String, PathBuf> = HashMap::new();

    // First, collect all module directories so we can register them.
    let mut module_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for pf in parsed_files {
        let rel = make_relative(&pf.path, project_root);
        let dir = rel.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        module_dirs.insert(dir.clone());

        // Register by full relative path (without extension).
        if let Some(stem) = rel.file_stem().and_then(|s| s.to_str()) {
            let no_ext = if dir.as_os_str().is_empty() {
                stem.to_owned()
            } else {
                format!("{}/{stem}", dir.display())
            };
            map.entry(no_ext).or_insert_with(|| dir.clone());
        }

        // Register by full relative path (with extension).
        map.entry(rel.to_string_lossy().to_string())
            .or_insert_with(|| dir.clone());

        // For Python, register by dotted module path.
        if pf.language == Language::Python {
            let dotted = rel
                .with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(".");
            map.entry(dotted.clone()).or_insert_with(|| dir.clone());
            // Also register the directory itself as a package.
            if !dir.as_os_str().is_empty() {
                let dir_dotted = dir
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join(".");
                map.entry(dir_dotted).or_insert_with(|| dir.clone());
            }
        }

        // For Rust, register by crate-style path (:: separator).
        if pf.language == Language::Rust {
            let rust_path = rel
                .with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("::");
            map.entry(rust_path).or_insert_with(|| dir.clone());
        }

        // Register each export name in this file, mapped to this module.
        for export in &pf.exports {
            if !export.name.is_empty() {
                // Qualified: "dir/export_name" or just "export_name" at root.
                let qualified = if dir.as_os_str().is_empty() {
                    export.name.clone()
                } else {
                    format!("{}/{}", dir.display(), export.name)
                };
                map.entry(qualified).or_insert_with(|| dir.clone());
            }
        }
    }

    // Register directory-level module paths.
    // This handles imports like `crate::models` (Rust) or `models` (Python package).
    for dir in &module_dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }
        // Register by filesystem path (e.g., "src/models").
        let dir_str = dir.to_string_lossy().to_string();
        map.entry(dir_str).or_insert_with(|| dir.clone());

        // Register by Rust-style path (e.g., "src::models").
        let rust_dir_path = dir
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("::");
        map.entry(rust_dir_path).or_insert_with(|| dir.clone());

        // Register by dotted path for Python (e.g., "models").
        let dotted_dir = dir
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(".");
        map.entry(dotted_dir).or_insert_with(|| dir.clone());

        // Register by last component (directory name) for simple imports.
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            map.entry(name.to_owned()).or_insert_with(|| dir.clone());
        }
    }

    map
}

/// Resolve an import module path to a target module directory.
fn resolve_import_to_module(
    import_module: &str,
    source_dir: &Path,
    target_map: &HashMap<String, PathBuf>,
    language: &Language,
) -> Option<PathBuf> {
    // 1. Direct lookup in the target map.
    if let Some(dir) = target_map.get(import_module) {
        return Some(dir.clone());
    }

    // 2. For relative imports (starting with . or ..), resolve relative to source dir.
    if import_module.starts_with('.') {
        let cleaned = import_module
            .trim_start_matches("./")
            .trim_start_matches("../");

        // Try resolving relative to source directory.
        let resolved = if import_module.starts_with("../") {
            source_dir
                .parent()
                .map(|p| p.join(cleaned))
                .unwrap_or_else(|| PathBuf::from(cleaned))
        } else if import_module.starts_with("./") {
            source_dir.join(cleaned)
        } else {
            // Just "." — refers to current directory (Python relative import).
            return Some(source_dir.to_path_buf());
        };

        let resolved_str = resolved.to_string_lossy().to_string();
        if let Some(dir) = target_map.get(&resolved_str) {
            return Some(dir.clone());
        }

        // Try the resolved path as a directory itself if it's a module.
        if target_map.values().any(|d| *d == resolved) {
            return Some(resolved);
        }
    }

    // 3. For Python dotted imports like "mypackage.models", try lookup.
    if *language == Language::Python && import_module.contains('.') {
        if let Some(dir) = target_map.get(import_module) {
            return Some(dir.clone());
        }
        // Try the base package.
        let base = import_module.split('.').next().unwrap_or(import_module);
        if let Some(dir) = target_map.get(base) {
            return Some(dir.clone());
        }
    }

    // 4. For Rust :: imports like "crate::config" or "super::models".
    if *language == Language::Rust {
        // Handle crate:: prefix.
        if let Some(rest) = import_module.strip_prefix("crate::") {
            // Map crate::X::Y to path X/Y or src/X/Y.
            let as_path = rest.replace("::", "/");
            if let Some(dir) = target_map.get(&as_path) {
                return Some(dir.clone());
            }
            let src_path = format!("src/{as_path}");
            if let Some(dir) = target_map.get(&src_path) {
                return Some(dir.clone());
            }
        }
        // Handle super:: prefix.
        if let Some(rest) = import_module.strip_prefix("super::") {
            if let Some(parent) = source_dir.parent() {
                let as_path = rest.replace("::", "/");
                let resolved = parent.join(&as_path);
                let resolved_str = resolved.to_string_lossy().to_string();
                if let Some(dir) = target_map.get(&resolved_str) {
                    return Some(dir.clone());
                }
            }
        }
        // Handle self:: prefix.
        if let Some(rest) = import_module.strip_prefix("self::") {
            let as_path = rest.replace("::", "/");
            let resolved = source_dir.join(&as_path);
            let resolved_str = resolved.to_string_lossy().to_string();
            if let Some(dir) = target_map.get(&resolved_str) {
                return Some(dir.clone());
            }
        }
    }

    // 5. For JS/TS, try with common file extensions.
    if matches!(language, Language::JavaScript | Language::TypeScript) {
        // "./foo" might resolve to "./foo.ts", "./foo.js", "./foo/index.ts", etc.
        if import_module.starts_with('.') {
            let cleaned = import_module
                .trim_start_matches("./")
                .trim_start_matches("../");
            let base = if import_module.starts_with("../") {
                source_dir
                    .parent()
                    .map(|p| p.join(cleaned))
                    .unwrap_or_else(|| PathBuf::from(cleaned))
            } else {
                source_dir.join(cleaned)
            };

            let base_str = base.to_string_lossy().to_string();

            // Try: base/index
            let index_path = format!("{base_str}/index");
            if let Some(dir) = target_map.get(&index_path) {
                return Some(dir.clone());
            }
        }
    }

    None
}

/// Make a path relative to the project root. If already relative, return as-is.
fn make_relative(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Returns `true` if the file_doc string is a technical directive rather than
/// a human-readable description and should be excluded from `purpose`.
///
/// Covers TypeScript/JavaScript (`@ts-nocheck`, `@type`, `eslint-disable`),
/// Python lint directives (`noqa`, `type: ignore`), shebangs, and strings that
/// are too short to carry meaning.
/// Minimum byte length for a file-doc string to be considered meaningful.
const MIN_DOC_LEN: usize = 8;

fn is_noise_file_doc(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("@ts-")              // @ts-nocheck, @ts-ignore
        || s.starts_with("@type")      // @type {import('...')} JSDoc annotations
        || s.starts_with("@jest-")
        || s.starts_with("@flow")
        || s.starts_with("@noinspection")
        // eslint directives always start the line — use starts_with to avoid
        // false positives on doc comments that *mention* eslint-disable.
        || s.starts_with("eslint-disable")
        || s.starts_with("// eslint-disable")
        || s.starts_with("/* eslint-disable")
        || s.starts_with("noqa")
        || s.contains("type: ignore")
        || s.contains("type:ignore")
        || s.starts_with("#!")         // shebang
        || s.len() < MIN_DOC_LEN // too short to be meaningful
}

/// Strip Markdown heading markers (`# `, `## `, …) from each line and return
/// at most `max_lines` non-empty lines joined with `\n`.
fn clean_doc_text(s: &str, max_lines: usize) -> String {
    s.lines()
        .map(|line| line.trim_start_matches('#').trim())
        .filter(|line| !line.is_empty())
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Derive a human-readable purpose string for a module.
///
/// Priority:
/// 1. `file_doc` from the canonical entry-point file (`lib.rs`, `mod.rs`,
///    `__init__.py`, `index.ts`, `index.js`, `main.rs`) — up to 5 lines.
/// 2. Up to `MAX_DOCS` `file_doc` values from other files in the module,
///    each truncated to `MAX_LINES_PER_DOC` lines, joined with ` | `.
/// 3. Deduplicated public symbol names (functions + types) — up to
///    `MAX_SYMBOLS`, with `+N more` for the remainder.
/// 4. `None` if nothing useful is found.
fn derive_module_purpose(
    files: &[PathBuf],
    file_map: &HashMap<PathBuf, &ProjectFile>,
) -> Option<String> {
    const ENTRY_POINT_NAMES: &[&str] = &[
        "lib.rs",
        "mod.rs",
        "main.rs",
        "__init__.py",
        "index.ts",
        "index.js",
        "index.mjs",
    ];
    /// Lines taken from the entry-point doc.
    const ENTRY_POINT_MAX_LINES: usize = 5;
    /// Maximum number of file_docs collected for Priority 2.
    const MAX_DOCS: usize = 10;
    /// Lines taken per file_doc in Priority 2.
    const MAX_LINES_PER_DOC: usize = 3;
    /// Maximum distinct public symbol names shown in the fallback.
    const MAX_SYMBOLS: usize = 8;

    // Priority 1: file_doc from entry-point file.
    for file_path in files {
        let file_name = file_path.file_name().and_then(|f| f.to_str()).unwrap_or("");
        if ENTRY_POINT_NAMES.contains(&file_name) {
            if let Some(pf) = file_map.get(file_path) {
                if let Some(ref doc) = pf.file_doc {
                    let raw = doc.trim();
                    if !raw.is_empty() && !is_noise_file_doc(raw) {
                        let cleaned = clean_doc_text(raw, ENTRY_POINT_MAX_LINES);
                        if !cleaned.is_empty() {
                            return Some(cleaned);
                        }
                    }
                }
            }
        }
    }

    // Priority 2: collect file_doc from any file in the module.
    // Each doc is truncated to MAX_LINES_PER_DOC lines; noise is filtered out.
    let file_docs: Vec<String> = files
        .iter()
        .filter_map(|fp| {
            let pf = file_map.get(fp)?;
            let raw = pf.file_doc.as_deref()?.trim();
            if raw.is_empty() || is_noise_file_doc(raw) {
                return None;
            }
            let cleaned = clean_doc_text(raw, MAX_LINES_PER_DOC);
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned)
            }
        })
        .take(MAX_DOCS)
        .collect();

    if !file_docs.is_empty() {
        return Some(file_docs.join(" | "));
    }

    // Priority 3: deduplicated public symbol names.
    let mut seen = std::collections::HashSet::new();
    let mut symbols: Vec<String> = Vec::new();
    for file_path in files {
        if let Some(pf) = file_map.get(file_path) {
            for f in &pf.functions {
                if f.is_public && seen.insert(f.name.clone()) {
                    symbols.push(f.name.clone());
                }
            }
            for t in &pf.types {
                if t.is_public && seen.insert(t.name.clone()) {
                    symbols.push(t.name.clone());
                }
            }
        }
    }

    if symbols.is_empty() {
        return None;
    }

    let total = symbols.len();
    let shown = symbols.into_iter().take(MAX_SYMBOLS).collect::<Vec<_>>();
    let mut result = shown.join(", ");
    if total > MAX_SYMBOLS {
        result.push_str(&format!(" +{} more", total - MAX_SYMBOLS));
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::{
        Export, Import, JavaScriptIR, Language, LanguageIR, PythonIR, RustIR, TypeScriptIR,
    };
    use std::path::PathBuf;

    /// Helper: create a minimal ProjectFile with the given path, language, imports, and exports.
    fn make_project_file(
        path: &str,
        language: Language,
        imports: Vec<Import>,
        exports: Vec<Export>,
    ) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language,
            content_hash: "test_hash".to_owned(),
            imports,
            exports,
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: match language {
                Language::Rust => LanguageIR::Rust(RustIR::default()),
                Language::TypeScript => LanguageIR::TypeScript(TypeScriptIR::default()),
                Language::JavaScript => LanguageIR::JavaScript(JavaScriptIR::default()),
                Language::Python => LanguageIR::Python(PythonIR::default()),
            },
            file_doc: None,
        }
    }

    fn import(module: &str) -> Import {
        Import {
            module: module.to_owned(),
            names: Vec::new(),
            is_type_only: false,
            line: 1,
        }
    }

    fn import_with_names(module: &str, names: &[&str]) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|n| n.to_string()).collect(),
            is_type_only: false,
            line: 1,
        }
    }

    fn export(name: &str) -> Export {
        Export {
            name: name.to_owned(),
            is_default: false,
            is_type_only: false,
            line: 1,
        }
    }

    // -----------------------------------------------------------------------
    // Module detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn detects_modules_from_directories() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file("/project/src/main.rs", Language::Rust, vec![], vec![]),
            make_project_file("/project/src/lib.rs", Language::Rust, vec![], vec![]),
            make_project_file(
                "/project/tests/test_main.rs",
                Language::Rust,
                vec![],
                vec![],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        assert_eq!(graph.modules.len(), 2);
        assert!(graph.modules.contains_key(&PathBuf::from("src")));
        assert!(graph.modules.contains_key(&PathBuf::from("tests")));
    }

    #[test]
    fn root_directory_detected_as_module() {
        let root = Path::new("/project");
        let files = vec![make_project_file(
            "/project/main.py",
            Language::Python,
            vec![],
            vec![],
        )];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        // Root directory is PathBuf::from("")
        assert_eq!(graph.modules.len(), 1);
        assert!(graph.modules.contains_key(&PathBuf::from("")));
    }

    #[test]
    fn nested_modules_detected() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file("/project/src/main.rs", Language::Rust, vec![], vec![]),
            make_project_file(
                "/project/src/handlers/api.rs",
                Language::Rust,
                vec![],
                vec![],
            ),
            make_project_file(
                "/project/src/handlers/web.rs",
                Language::Rust,
                vec![],
                vec![],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        assert_eq!(graph.modules.len(), 2);
        assert!(graph.modules.contains_key(&PathBuf::from("src")));
        assert!(graph.modules.contains_key(&PathBuf::from("src/handlers")));
    }

    #[test]
    fn module_tracks_languages() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/index.ts",
                Language::TypeScript,
                vec![],
                vec![],
            ),
            make_project_file(
                "/project/src/utils.js",
                Language::JavaScript,
                vec![],
                vec![],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        let src = &graph.modules[&PathBuf::from("src")];
        assert!(src.languages.contains("typescript"));
        assert!(src.languages.contains("javascript"));
    }

    #[test]
    fn module_tracks_files() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file("/project/src/main.rs", Language::Rust, vec![], vec![]),
            make_project_file("/project/src/config.rs", Language::Rust, vec![], vec![]),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        let src = &graph.modules[&PathBuf::from("src")];
        assert_eq!(src.files.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Knowledge node tests
    // -----------------------------------------------------------------------

    #[test]
    fn creates_fact_nodes_for_modules() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file("/project/src/main.rs", Language::Rust, vec![], vec![]),
            make_project_file("/project/tests/test.rs", Language::Rust, vec![], vec![]),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        assert_eq!(graph.nodes.len(), 2);
        for node in &graph.nodes {
            assert_eq!(node.nature, KnowledgeNature::Fact);
            assert_eq!(node.weight, KnowledgeWeight::Info);
            assert_eq!(node.confidence, 1.0);
            assert_eq!(node.branch_id, BranchId::from("main"));
            assert!(node.ext_data.is_some());
        }
    }

    #[test]
    fn node_ext_data_contains_module_info() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file("/project/src/main.rs", Language::Rust, vec![], vec![]),
            make_project_file("/project/src/lib.rs", Language::Rust, vec![], vec![]),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        let node = graph
            .nodes
            .iter()
            .find(|n| n.description.contains("'src'"))
            .expect("should have src module node");

        let ext = node.ext_data.as_ref().unwrap();
        assert_eq!(ext["source"], "module_structure");
        assert_eq!(ext["module_path"], "src");
        assert_eq!(ext["file_count"], 2);
    }

    // -----------------------------------------------------------------------
    // DependsOn edge tests
    // -----------------------------------------------------------------------

    #[test]
    fn creates_depends_on_edges_for_relative_imports_ts() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/index.ts",
                Language::TypeScript,
                vec![import("./utils")],
                vec![],
            ),
            make_project_file(
                "/project/src/utils.ts",
                Language::TypeScript,
                vec![],
                vec![export("formatDate")],
            ),
        ];

        // Both in "src" — same module, so no DependsOn edge.
        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();
        assert_eq!(
            depends_on.len(),
            0,
            "Same-module imports should not produce DependsOn edges"
        );
    }

    #[test]
    fn creates_depends_on_edges_cross_directory_ts() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/pages/home.ts",
                Language::TypeScript,
                vec![import("../utils/format")],
                vec![],
            ),
            make_project_file(
                "/project/src/utils/format.ts",
                Language::TypeScript,
                vec![],
                vec![export("formatDate")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();

        assert_eq!(depends_on.len(), 1);
        assert_eq!(depends_on[0].edge_type, EdgeType::DependsOn);
    }

    #[test]
    fn creates_depends_on_edges_rust_crate_imports() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/main.rs",
                Language::Rust,
                vec![import_with_names("crate::config", &["Config"])],
                vec![],
            ),
            make_project_file(
                "/project/src/config.rs",
                Language::Rust,
                vec![],
                vec![export("Config")],
            ),
        ];

        // Both in "src" — same module, no DependsOn edge.
        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();
        assert_eq!(
            depends_on.len(),
            0,
            "Same-module crate:: imports should not produce edges"
        );
    }

    #[test]
    fn creates_depends_on_edges_rust_cross_module() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/handlers/api.rs",
                Language::Rust,
                vec![import_with_names("crate::models", &["User"])],
                vec![],
            ),
            make_project_file(
                "/project/src/models/user.rs",
                Language::Rust,
                vec![],
                vec![export("User")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();

        assert_eq!(depends_on.len(), 1);
    }

    #[test]
    fn creates_depends_on_edges_python_dotted_imports() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/mypackage/services.py",
                Language::Python,
                vec![import("mypackage.models")],
                vec![],
            ),
            make_project_file(
                "/project/mypackage/models.py",
                Language::Python,
                vec![],
                vec![export("User")],
            ),
        ];

        // Both in "mypackage" — same module, no DependsOn edge.
        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();
        assert_eq!(depends_on.len(), 0);
    }

    #[test]
    fn creates_depends_on_edges_python_cross_directory() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/app/views.py",
                Language::Python,
                vec![import("models.user")],
                vec![],
            ),
            make_project_file(
                "/project/models/user.py",
                Language::Python,
                vec![],
                vec![export("User")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();

        assert_eq!(depends_on.len(), 1);
    }

    #[test]
    fn no_duplicate_depends_on_edges() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/pages/home.ts",
                Language::TypeScript,
                vec![
                    import("../utils/format"),
                    import("../utils/validate"), // Two imports to same target module.
                ],
                vec![],
            ),
            make_project_file(
                "/project/src/utils/format.ts",
                Language::TypeScript,
                vec![],
                vec![export("formatDate")],
            ),
            make_project_file(
                "/project/src/utils/validate.ts",
                Language::TypeScript,
                vec![],
                vec![export("isValid")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();

        // pages -> utils (only one edge, even though two imports resolve there).
        assert_eq!(depends_on.len(), 1);
    }

    #[test]
    fn self_imports_not_edges() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/a.ts",
                Language::TypeScript,
                vec![import("./b")],
                vec![],
            ),
            make_project_file(
                "/project/src/b.ts",
                Language::TypeScript,
                vec![],
                vec![export("B")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();

        assert_eq!(
            depends_on.len(),
            0,
            "Same-directory imports should not produce edges"
        );
    }

    // -----------------------------------------------------------------------
    // PartOf edge tests
    // -----------------------------------------------------------------------

    #[test]
    fn creates_part_of_edges_for_nested_modules() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file("/project/src/main.rs", Language::Rust, vec![], vec![]),
            make_project_file(
                "/project/src/handlers/api.rs",
                Language::Rust,
                vec![],
                vec![],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let part_of: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::PartOf)
            .collect();

        // src/handlers PartOf src
        assert_eq!(part_of.len(), 1);
    }

    #[test]
    fn no_part_of_for_root_module() {
        let root = Path::new("/project");
        let files = vec![make_project_file(
            "/project/main.py",
            Language::Python,
            vec![],
            vec![],
        )];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let part_of: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::PartOf)
            .collect();

        assert_eq!(part_of.len(), 0);
    }

    #[test]
    fn part_of_only_when_parent_is_module() {
        let root = Path::new("/project");
        // "src/deep/nested/" exists but "src/deep/" has no files — not a module.
        let files = vec![make_project_file(
            "/project/src/deep/nested/file.rs",
            Language::Rust,
            vec![],
            vec![],
        )];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let part_of: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::PartOf)
            .collect();

        // "src/deep/nested" has no parent module (src/deep has no files), so no PartOf.
        assert_eq!(part_of.len(), 0);
    }

    #[test]
    fn deep_hierarchy_part_of_chain() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file("/project/src/main.rs", Language::Rust, vec![], vec![]),
            make_project_file(
                "/project/src/api/handler.rs",
                Language::Rust,
                vec![],
                vec![],
            ),
            make_project_file(
                "/project/src/api/v2/handler.rs",
                Language::Rust,
                vec![],
                vec![],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));
        let part_of: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::PartOf)
            .collect();

        // src/api PartOf src
        // src/api/v2 PartOf src/api
        assert_eq!(part_of.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Queryability tests
    // -----------------------------------------------------------------------

    #[test]
    fn query_dependencies_of() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/pages/home.ts",
                Language::TypeScript,
                vec![import("../utils/format")],
                vec![],
            ),
            make_project_file(
                "/project/src/utils/format.ts",
                Language::TypeScript,
                vec![],
                vec![export("formatDate")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        let deps = graph.dependencies_of(Path::new("src/pages"));
        assert_eq!(deps.len(), 1);
        assert_eq!(*deps[0], PathBuf::from("src/utils"));
    }

    #[test]
    fn query_dependents_of() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/pages/home.ts",
                Language::TypeScript,
                vec![import("../utils/format")],
                vec![],
            ),
            make_project_file(
                "/project/src/utils/format.ts",
                Language::TypeScript,
                vec![],
                vec![export("formatDate")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        let dependents = graph.dependents_of(Path::new("src/utils"));
        assert_eq!(dependents.len(), 1);
        assert_eq!(*dependents[0], PathBuf::from("src/pages"));
    }

    #[test]
    fn query_nonexistent_module_returns_empty() {
        let root = Path::new("/project");
        let files = vec![make_project_file(
            "/project/src/main.rs",
            Language::Rust,
            vec![],
            vec![],
        )];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        assert!(graph.dependencies_of(Path::new("nonexistent")).is_empty());
        assert!(graph.dependents_of(Path::new("nonexistent")).is_empty());
    }

    // -----------------------------------------------------------------------
    // Empty input tests
    // -----------------------------------------------------------------------

    #[test]
    fn empty_files_produces_empty_graph() {
        let root = Path::new("/project");
        let graph = build_module_graph(root, &[], &BranchId::from("main"));

        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
        assert!(graph.modules.is_empty());
    }

    // -----------------------------------------------------------------------
    // Mixed language tests
    // -----------------------------------------------------------------------

    #[test]
    fn mixed_language_project() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/frontend/src/App.tsx",
                Language::TypeScript,
                vec![import("../shared/types")],
                vec![],
            ),
            make_project_file(
                "/project/frontend/shared/types.ts",
                Language::TypeScript,
                vec![],
                vec![export("AppConfig")],
            ),
            make_project_file(
                "/project/backend/src/main.rs",
                Language::Rust,
                vec![],
                vec![],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        // 3 modules: frontend/src, frontend/shared, backend/src
        assert_eq!(graph.modules.len(), 3);
        assert_eq!(graph.nodes.len(), 3);

        // frontend/src depends on frontend/shared
        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();
        assert_eq!(depends_on.len(), 1);
    }

    // -----------------------------------------------------------------------
    // JS index barrel imports
    // -----------------------------------------------------------------------

    #[test]
    fn js_index_barrel_import() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/app.ts",
                Language::TypeScript,
                vec![import("./components")], // barrel import to directory
                vec![],
            ),
            make_project_file(
                "/project/src/components/index.ts",
                Language::TypeScript,
                vec![],
                vec![export("Button"), export("Input")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        let depends_on: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::DependsOn)
            .collect();

        // src -> src/components
        assert_eq!(depends_on.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Edge metadata tests
    // -----------------------------------------------------------------------

    #[test]
    fn depends_on_edge_has_metadata() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file(
                "/project/src/pages/home.ts",
                Language::TypeScript,
                vec![import("../utils/format")],
                vec![],
            ),
            make_project_file(
                "/project/src/utils/format.ts",
                Language::TypeScript,
                vec![],
                vec![export("formatDate")],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        let depends_on = graph
            .edges
            .iter()
            .find(|e| e.edge_type == EdgeType::DependsOn)
            .expect("should have DependsOn edge");

        let metadata = depends_on.metadata.as_ref().expect("should have metadata");
        assert!(metadata.get("source_module").is_some());
        assert!(metadata.get("target_module").is_some());
    }

    #[test]
    fn part_of_edge_has_metadata() {
        let root = Path::new("/project");
        let files = vec![
            make_project_file("/project/src/main.rs", Language::Rust, vec![], vec![]),
            make_project_file(
                "/project/src/handlers/api.rs",
                Language::Rust,
                vec![],
                vec![],
            ),
        ];

        let graph = build_module_graph(root, &files, &BranchId::from("main"));

        let part_of = graph
            .edges
            .iter()
            .find(|e| e.edge_type == EdgeType::PartOf)
            .expect("should have PartOf edge");

        let metadata = part_of.metadata.as_ref().expect("should have metadata");
        assert!(metadata.get("child_module").is_some());
        assert!(metadata.get("parent_module").is_some());
    }

    // -----------------------------------------------------------------------
    // derive_module_purpose tests
    // -----------------------------------------------------------------------

    fn make_file_with_doc(path: &str, file_doc: Option<&str>) -> ProjectFile {
        let mut pf = make_project_file(path, Language::Rust, vec![], vec![]);
        pf.file_doc = file_doc.map(str::to_owned);
        pf
    }

    fn make_file_with_pub_fn(path: &str, fn_name: &str) -> ProjectFile {
        let pf_base = make_project_file(path, Language::Rust, vec![], vec![]);
        ProjectFile {
            functions: vec![seshat_core::Function {
                name: fn_name.to_owned(),
                is_public: true,
                is_async: false,
                line: 1,
                end_line: 5,
                parameters: vec![],
                doc_comment: None,
            }],
            ..pf_base
        }
    }

    #[test]
    fn purpose_from_entry_point_file_doc() {
        let lib_rs = make_file_with_doc("/project/src/lib.rs", Some("Authentication module."));
        let other = make_file_with_doc("/project/src/handler.rs", Some("Handles requests."));

        let file_map: HashMap<PathBuf, &ProjectFile> = [
            (PathBuf::from("src/lib.rs"), &lib_rs),
            (PathBuf::from("src/handler.rs"), &other),
        ]
        .into_iter()
        .collect();

        let files = vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/handler.rs")];
        let purpose = derive_module_purpose(&files, &file_map);
        assert_eq!(purpose.as_deref(), Some("Authentication module."));
    }

    #[test]
    fn purpose_falls_back_to_file_docs_when_no_entry_point() {
        let handler = make_file_with_doc("/project/src/handler.rs", Some("Handles HTTP."));
        let service = make_file_with_doc("/project/src/service.rs", Some("Business logic."));

        let file_map: HashMap<PathBuf, &ProjectFile> = [
            (PathBuf::from("src/handler.rs"), &handler),
            (PathBuf::from("src/service.rs"), &service),
        ]
        .into_iter()
        .collect();

        let files = vec![
            PathBuf::from("src/handler.rs"),
            PathBuf::from("src/service.rs"),
        ];
        let purpose = derive_module_purpose(&files, &file_map);
        let p = purpose.unwrap();
        assert!(p.contains("Handles HTTP."), "got: {p}");
        assert!(p.contains("Business logic."), "got: {p}");
    }

    #[test]
    fn purpose_falls_back_to_symbols_when_no_docs() {
        let pf = make_file_with_pub_fn("/project/src/handler.rs", "handle_request");
        let file_map: HashMap<PathBuf, &ProjectFile> = [(PathBuf::from("src/handler.rs"), &pf)]
            .into_iter()
            .collect();
        let files = vec![PathBuf::from("src/handler.rs")];

        let purpose = derive_module_purpose(&files, &file_map);
        let p = purpose.unwrap();
        assert!(p.contains("handle_request"), "got: {p}");
    }

    #[test]
    fn purpose_is_none_when_no_docs_no_symbols() {
        let pf = make_file_with_doc("/project/src/empty.rs", None);
        let file_map: HashMap<PathBuf, &ProjectFile> =
            [(PathBuf::from("src/empty.rs"), &pf)].into_iter().collect();
        let files = vec![PathBuf::from("src/empty.rs")];

        let purpose = derive_module_purpose(&files, &file_map);
        assert!(purpose.is_none());
    }

    // -----------------------------------------------------------------------
    // noise filter tests
    // -----------------------------------------------------------------------

    #[test]
    fn noise_filter_rejects_ts_nocheck() {
        assert!(is_noise_file_doc("@ts-nocheck"));
        assert!(is_noise_file_doc("@ts-ignore"));
    }

    #[test]
    fn noise_filter_rejects_type_annotation() {
        assert!(is_noise_file_doc("@type {import('next').NextConfig}"));
    }

    #[test]
    fn noise_filter_rejects_eslint_disable() {
        assert!(is_noise_file_doc("eslint-disable no-console"));
        assert!(is_noise_file_doc("// eslint-disable-next-line"));
        // But only when the whole string is the directive, not when it appears
        // mid-sentence — check via the `contains` rule.
        assert!(is_noise_file_doc(
            "eslint-disable @typescript-eslint/no-explicit-any"
        ));
    }

    #[test]
    fn noise_filter_rejects_python_noqa() {
        assert!(is_noise_file_doc("noqa: E501"));
        assert!(is_noise_file_doc("noqa"));
    }

    #[test]
    fn noise_filter_rejects_type_ignore() {
        assert!(is_noise_file_doc("type: ignore"));
        assert!(is_noise_file_doc("type:ignore"));
    }

    #[test]
    fn noise_filter_rejects_short_strings() {
        assert!(is_noise_file_doc("ok"));
        assert!(is_noise_file_doc("   hi   "));
    }

    #[test]
    fn noise_filter_accepts_real_doc() {
        assert!(!is_noise_file_doc(
            "Handles authentication and session management."
        ));
        assert!(!is_noise_file_doc(
            "# Auth Module\n\nProvides JWT-based login."
        ));
    }

    #[test]
    fn noise_docs_excluded_from_purpose() {
        // entry-point has noise, other file has real doc
        let index_ts = make_file_with_doc("/project/src/index.ts", Some("@ts-nocheck\n// barrel"));
        let service =
            make_file_with_doc("/project/src/service.ts", Some("Handles user operations."));

        let file_map: HashMap<PathBuf, &ProjectFile> = [
            (PathBuf::from("src/index.ts"), &index_ts),
            (PathBuf::from("src/service.ts"), &service),
        ]
        .into_iter()
        .collect();

        let files = vec![
            PathBuf::from("src/index.ts"),
            PathBuf::from("src/service.ts"),
        ];
        let purpose = derive_module_purpose(&files, &file_map);
        let p = purpose.as_deref().unwrap_or("");
        assert!(!p.contains("@ts-nocheck"), "noise must be filtered: {p}");
        assert!(
            p.contains("Handles user operations."),
            "real doc missing: {p}"
        );
    }

    #[test]
    fn markdown_headings_stripped_from_purpose() {
        let lib_rs = make_file_with_doc(
            "/project/src/lib.rs",
            Some("# Auth Module\n\nProvides JWT-based login."),
        );
        let file_map: HashMap<PathBuf, &ProjectFile> = [(PathBuf::from("src/lib.rs"), &lib_rs)]
            .into_iter()
            .collect();
        let files = vec![PathBuf::from("src/lib.rs")];

        let purpose = derive_module_purpose(&files, &file_map);
        let p = purpose.as_deref().unwrap_or("");
        assert!(
            !p.starts_with('#'),
            "markdown heading must be stripped: {p}"
        );
        assert!(p.contains("Auth Module"), "heading text should remain: {p}");
        assert!(
            p.contains("Provides JWT-based login."),
            "body must be kept: {p}"
        );
    }

    #[test]
    fn symbols_are_deduplicated() {
        // Two files both export a function called `new` (common in Rust).
        let f1 = {
            let mut pf = make_file_with_pub_fn("/project/src/a.rs", "new");
            // add a second unique symbol
            pf.functions.push(seshat_core::Function {
                name: "run".to_owned(),
                is_public: true,
                is_async: false,
                line: 10,
                end_line: 20,
                parameters: vec![],
                doc_comment: None,
            });
            pf
        };
        let f2 = make_file_with_pub_fn("/project/src/b.rs", "new"); // duplicate

        let file_map: HashMap<PathBuf, &ProjectFile> = [
            (PathBuf::from("src/a.rs"), &f1),
            (PathBuf::from("src/b.rs"), &f2),
        ]
        .into_iter()
        .collect();

        let files = vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")];
        let purpose = derive_module_purpose(&files, &file_map);
        let p = purpose.unwrap();
        // `new` should appear exactly once
        assert_eq!(
            p.matches("new").count(),
            1,
            "duplicate symbol in purpose: {p}"
        );
        assert!(p.contains("run"), "unique symbol missing: {p}");
    }

    #[test]
    fn file_doc_truncated_to_max_lines() {
        // A doc with many lines — only first 5 should appear for entry-point.
        let doc = "Line1\nLine2\nLine3\nLine4\nLine5\nLine6\nLine7\nLine8";
        let lib_rs = make_file_with_doc("/project/src/lib.rs", Some(doc));
        let file_map: HashMap<PathBuf, &ProjectFile> = [(PathBuf::from("src/lib.rs"), &lib_rs)]
            .into_iter()
            .collect();
        let files = vec![PathBuf::from("src/lib.rs")];

        let purpose = derive_module_purpose(&files, &file_map).unwrap();
        let line_count = purpose.lines().count();
        assert!(
            line_count <= 5,
            "entry-point doc should be ≤5 lines, got {line_count}: {purpose}"
        );
        assert!(
            !purpose.contains("Line6"),
            "line 6 must be truncated: {purpose}"
        );
    }
}
