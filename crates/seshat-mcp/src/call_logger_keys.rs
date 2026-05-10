pub mod project_context {
    pub const DATA_LANGUAGES: &str = "languages";
    pub const DATA_CONVENTIONS_COUNT: &str = "conventions_count";
    pub const DATA_GOLDEN_FILES: &str = "golden_files";
}

pub mod query_convention {
    pub const DATA_CONVENTIONS: &str = "conventions";
    pub const CONVENTION_SOURCE: &str = "source";
}

pub mod code_pattern {
    pub const DATA_PATTERNS: &str = "patterns";
    pub const DATA_RELATED_CONVENTIONS: &str = "related_conventions";
}

pub mod dependencies {
    pub const DATA_DEPENDENTS: &str = "dependents";
    pub const DATA_DEPENDENCIES: &str = "dependencies";
    pub const DATA_BLAST_RADIUS: &str = "blast_radius";
    pub const DATA_TRANSITIVE_DEPENDENT_COUNT: &str = "transitive_dependent_count";
    pub const DATA_REQUESTED_DEPTH: &str = "requested_depth";
}

pub mod validate_approach {
    pub const DATA_VERDICT: &str = "verdict";
    pub const DATA_RULES: &str = "rules";
    pub const DATA_DUPLICATES: &str = "duplicates";
    pub const DATA_CONVENTIONS: &str = "conventions";
    pub const DATA_READY: &str = "ready";
}
