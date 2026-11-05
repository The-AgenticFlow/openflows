//! Docs branch and path classification for LORE's post-merge documentation.
//!
//! What a submission changes (`files`, `base`) is reported by the LORE
//! workspace and is therefore only advisory. The controller re-derives the
//! changed paths from GitHub and checks them with [`non_docs_paths`] before it
//! opens or merges a docs PR.

/// Synthetic ticket id VESSEL assigns to docs PRs. LORE never documents it.
pub const DOCS_TICKET: &str = "T-DOCS";
/// Every LORE docs branch starts with this prefix; VESSEL keys docs PRs on it.
pub const DOCS_BRANCH_PREFIX: &str = "lore/";
/// Separator for the docs cycle suffix (`lore/T-9--c2`). A single `/` cannot
/// be used: git refuses `lore/T-9` and `lore/T-9/c2` side by side.
const CYCLE_SEPARATOR: &str = "--c";
/// Transitions kept in a record's history; older ones are dropped.
pub const MAX_DOCS_HISTORY: usize = 100;

/// The branch LORE must use for `ticket` in docs cycle `cycle`. The first
/// cycle uses `lore/{ticket}`; each reopen moves to a fresh branch so the new
/// docs PR never collides with an earlier one.
pub fn docs_branch(ticket: &str, cycle: u64) -> String {
    if cycle == 0 {
        format!("{DOCS_BRANCH_PREFIX}{ticket}")
    } else {
        format!("{DOCS_BRANCH_PREFIX}{ticket}{CYCLE_SEPARATOR}{}", cycle + 1)
    }
}

/// The ticket a LORE docs branch documents, or `None` for other branches.
///
/// Assumes ticket ids never end in `--c<digits>` themselves; such an id would
/// be read as a cycle suffix.
pub fn ticket_from_docs_branch(branch: &str) -> Option<&str> {
    let rest = branch.strip_prefix(DOCS_BRANCH_PREFIX)?;
    let ticket = match rest.rsplit_once(CYCLE_SEPARATOR) {
        Some((ticket, cycle)) if !cycle.is_empty() && cycle.bytes().all(|b| b.is_ascii_digit()) => {
            ticket
        }
        _ => rest,
    };
    (!ticket.is_empty() && !ticket.contains('/')).then_some(ticket)
}

/// Paths LORE may never change, even when they look like documentation:
/// agent personas, skills and instructions steer other agents, and legal
/// files are not LORE's to rewrite. Every dot-directory and dotfile
/// (`.github/`, `.claude/`, `.cursor/`, `.changeset/`, `.cursorrules`, ...) is
/// refused too, since tooling reads those as configuration.
const PROTECTED_PREFIXES: &[&str] = &["orchestration/"];
const PROTECTED_NAMES: &[&str] = &[
    "claude.md",
    "agents.md",
    "gemini.md",
    "copilot-instructions.md",
    "codeowners",
];
/// Legal files, matched as name prefixes so suffixed variants count too
/// (`LICENSE`, `LICENSE-MIT.md`, `LICENSES/`, `COPYING-LESSER.md`, `NOTICE.txt`).
const PROTECTED_NAME_STEMS: &[&str] = &["license", "licence", "copying", "notice"];
/// Prose formats that are documentation wherever they live. MDX is not one:
/// it is JSX that docs sites execute at build time.
const DOC_EXTENSIONS: &[&str] = &[".md", ".rst", ".adoc"];
/// Allowed only under `docs/`: plain text and diagrams or screenshots.
const DOCS_DIR_EXTENSIONS: &[&str] = &[".txt", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp"];
/// Text files that are build or site inputs, not prose (`docs/requirements.txt`
/// installs the docs build's dependencies, for example).
const BUILD_INPUT_STEMS: &[&str] = &["requirements", "constraints", "cmakelists", "robots"];

/// Whether a repository path is documentation LORE may change: Markdown,
/// reStructuredText and AsciiDoc files, changelogs, and text files and images
/// under `docs/`. Executable, configuration and build-input files never
/// qualify (even under `docs/`), nor do dot-directories, agent instructions,
/// `orchestration/` and legal files.
pub fn is_docs_path(path: &str) -> bool {
    let path = path.trim();
    if path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part.starts_with('.'))
    {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let stem = name.split('.').next().unwrap_or(name);
    if PROTECTED_PREFIXES.iter().any(|p| lower.starts_with(p))
        || PROTECTED_NAMES.contains(&name)
        || lower
            .split('/')
            .any(|part| PROTECTED_NAME_STEMS.iter().any(|s| part.starts_with(s)))
        || BUILD_INPUT_STEMS.iter().any(|s| stem.starts_with(s))
        || name.ends_with(".agent.md")
        || name == "skill.md"
    {
        return false;
    }
    let has = |extensions: &[&str]| extensions.iter().any(|e| name.ends_with(e));
    let is_changelog = stem.starts_with("changelog")
        && (!name.contains('.') || has(DOC_EXTENSIONS) || name.ends_with(".txt"));
    is_changelog || has(DOC_EXTENSIONS) || (lower.starts_with("docs/") && has(DOCS_DIR_EXTENSIONS))
}

/// The paths in `paths` that are not documentation ([`is_docs_path`]).
pub fn non_docs_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| !is_docs_path(path))
        .map(str::to_string)
        .collect()
}
