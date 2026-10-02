// crates/agent-nexus/src/hooks/sentinel.rs
//! Sentinel lifecycle — phase-aware `pre_tool_use` guard for the reviewer role.
//!
//! Sentinel has no `status set` phases of its own: its "job" is *derived* from
//! the shared ticket's durable state (what FORGE is doing). Two review jobs:
//!   - `plan_gate` — FORGE is `planning` and the gate is not approved yet →
//!     review `PLAN.md`, then `gate approve`.
//!   - `pr_review` — FORGE is `submit` with a PR → review the diff, write
//!     the evaluation report, then `review submit`.
//!   - `idle` — nothing pending; no review action should be taken.
//!
//! Rules mirror FORGE's phase guard but for the reviewing direction:
//!   - Sentinel is read-only for source; only review artifacts (*-eval.md,
//!     final-review.md) may be written.
//!   - In `plan_gate` a `gate approve` requires a plan to have been uploaded.
//!   - In `pr_review` a `review submit` requires the evaluation report to have
//!     been written first (tracked on `post_tool_use`).

use super::context::{review_report_exists, TicketState};
use crate::hooks::types::HookDecision;
use pocketflow_core::SharedStore;
use serde_json::Value;

/// Sentinel's derived review job for the current shared ticket state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentinelJob {
    PlanGateReview,
    TestingReview,
    PrReview,
    Idle,
}

/// Derive what review job Sentinel should be doing from the ticket state.
pub fn sentinel_job(st: &TicketState) -> SentinelJob {
    match st.phase.as_deref() {
        Some("plan_ready") if !st.gate_approved => SentinelJob::PlanGateReview,
        Some("testing") => SentinelJob::TestingReview,
        Some("submit") if st.pr_recorded => SentinelJob::PrReview,
        _ => SentinelJob::Idle,
    }
}

/// Files Sentinel is allowed to write despite the read-only policy: these are
/// its evaluation/verdict artifacts, never source.
fn is_review_artifact(path: &str) -> bool {
    let p = path.to_lowercase();
    let file = p.rsplit(['/', '\\']).next().unwrap_or(p.as_str());
    file.ends_with("-eval.md")
        || file == "eval.md"
        || file == "final-review.md"
        || file == "review.md"
        || file == "review-report.md"
}

fn command_text(input: &Value) -> String {
    if let Some(command) = input.get("command").and_then(|c| c.as_str()) {
        return command.to_string();
    }
    if let Some(argv) = input.get("argv").and_then(|v| v.as_array()) {
        return argv
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }
    input.as_str().unwrap_or_default().to_string()
}

fn shell_writes_source(command: &str) -> bool {
    match shell_write_targets(command) {
        Some(targets) => targets.iter().any(|target| !is_review_artifact(target)),
        None => true,
    }
}

fn shell_write_targets(command: &str) -> Option<Vec<String>> {
    let words = shell_words(command);
    if words.is_empty() {
        return Some(Vec::new());
    }

    let mut targets = redirection_targets(&words);
    targets.extend(tee_targets(&words));
    targets.extend(command_operand_targets(&words));
    if !targets.is_empty() {
        return Some(targets);
    }

    let cmd = command.to_lowercase();
    if cmd.contains("git apply")
        || cmd.contains("apply_patch")
        || cmd.contains("sed -i")
        || cmd.contains("perl -i")
    {
        return None;
    }

    // Unknown-but-write-capable commands (dd of=, curl -o, wget -O, git
    // checkout/restore/..., sh -c ...) have filesystem effects the operand
    // parser does not capture. Fail closed so Sentinel cannot silently modify
    // the checkout despite its read-only role.
    if is_write_capable_command(command) {
        return None;
    }

    Some(Vec::new())
}

/// Detect commands whose own syntax can write files in a way the redirection /
/// operand parsers do not capture (`dd of=`, `curl -o`, `wget -O`,
/// `git checkout`/`git restore`, a nested `sh -c`, ...). Such commands must be
/// treated as write-capable, otherwise they could silently alter the checkout.
fn is_write_capable_command(command: &str) -> bool {
    let words = shell_words(command);
    if words.is_empty() {
        return false;
    }
    // A wrapper must not shift the token used to resolve the executable or its
    // subcommand: wrapper names *and* their arguments/options (`sudo -u root`,
    // `env SOME=1 git`) are part of the prefix (P1: "Wrapper Arguments Hide
    // Writes").
    let cmd_idx = resolve_executable_index(&words);
    let cmd = words.get(cmd_idx).map(|w| w.as_str()).unwrap_or_default();
    let next = words
        .get(cmd_idx + 1)
        .map(|w| w.as_str())
        .unwrap_or_default();
    match cmd {
        "dd" => true,
        "curl" | "wget" => words
            .iter()
            .any(|w| w == "-o" || w == "-O" || w.starts_with("--output")),
        "git" => matches!(
            next,
            "checkout"
                | "restore"
                | "reset"
                | "stash"
                | "rm"
                | "mv"
                | "apply"
                | "am"
                | "merge"
                | "rebase"
                | "cherry-pick"
                | "clean"
                | "pull"
        ),
        "sh" | "bash" | "zsh" | "ksh" | "dash" => words.iter().any(|w| w == "-c"),
        _ => false,
    }
}

/// The known forwarding wrappers that merely pass control to the real command.
const FORWARDING_WRAPPERS: &[&str] = &["nohup", "nice", "sudo", "env", "setsid", "time", "command"];

/// Wrapper options that consume a *separate* argument token (`sudo -u root`).
/// These must be skipped together with their value so the value is not mistaken
/// for the executable.
const WRAPPER_OPT_WITH_ARG: &[&str] = &[
    "-u",
    "--user",
    "-g",
    "--group",
    "-C",
    "--chroot",
    "-p",
    "--prompt",
    "-D",
    "--chdir",
    "-n",
    "--adjustment",
    "-S",
    "--set-home",
    "-o",
    "--output",
];

/// Is `word` a wrapper option (leading dash, not `-` alone)?
fn is_option_token(word: &str) -> bool {
    word.len() > 1 && word.starts_with('-')
}

/// Is `word` an environment assignment (`VAR=value`)?
fn is_env_assignment(word: &str) -> bool {
    let Some(eq) = word.find('=') else {
        return false;
    };
    eq > 0 && !word[..eq].contains(['-', '/'])
}

/// Resolve the index of the real executable after a forwarding-wrapper prefix.
///
/// A wrapper is not simply "the first token that is not a wrapper name":
/// wrapper *arguments* (`sudo -u root`, `env SOME=1 git`, `nice -n 10 git`)
/// would otherwise be mistaken for the command and let a write-capable
/// subcommand (e.g. `git checkout`) slip past detection (P1: "Wrapper Arguments
/// Hide Writes"). We skip wrapper names, their value-consuming options and the
/// option values, leading `--opt=value` options, and `VAR=value` assignments
/// before settling on the executable.
fn resolve_executable_index(segment: &[String]) -> usize {
    let mut i = 0;
    while i < segment.len() {
        let w = segment[i].as_str();
        if FORWARDING_WRAPPERS.contains(&w) {
            i += 1;
            continue;
        }
        if WRAPPER_OPT_WITH_ARG.contains(&w) {
            i += 2; // skip the option and its separate value token
            continue;
        }
        if is_option_token(w) || is_env_assignment(w) {
            i += 1;
            continue;
        }
        return i;
    }
    segment.len().saturating_sub(1)
}

fn redirection_targets(words: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    for (idx, word) in words.iter().enumerate() {
        let op = word.as_str();
        if matches!(op, ">" | ">>" | "1>" | "1>>" | "2>" | "2>>") {
            if let Some(target) = words.get(idx + 1) {
                targets.push(clean_shell_word(target));
            }
        } else if let Some(target) = op.strip_prefix(">>").or_else(|| op.strip_prefix('>')) {
            if !target.is_empty() {
                targets.push(clean_shell_word(target));
            }
        } else if let Some(target) = op.strip_prefix("1>").or_else(|| op.strip_prefix("2>")) {
            if !target.is_empty() {
                targets.push(clean_shell_word(target));
            }
        }
    }
    targets
}

fn tee_targets(words: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    for (idx, word) in words.iter().enumerate() {
        if word != "tee" {
            continue;
        }
        for arg in &words[idx + 1..] {
            if arg.starts_with('-') {
                continue;
            }
            if is_shell_separator(arg) {
                break;
            }
            targets.push(clean_shell_word(arg));
        }
    }
    targets
}

fn command_operand_targets(words: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    let mut idx = 0;
    while idx < words.len() {
        let cmd = words[idx].as_str();
        if matches!(cmd, "cp" | "mv" | "install") {
            if let Some(target) = words[idx + 1..]
                .iter()
                .rev()
                .find(|arg| !arg.starts_with('-') && !is_shell_separator(arg))
            {
                targets.push(clean_shell_word(target));
            }
        } else if matches!(cmd, "touch" | "mkdir" | "rm" | "truncate") {
            targets.extend(
                words[idx + 1..]
                    .iter()
                    .take_while(|arg| !is_shell_separator(arg))
                    .filter(|arg| !arg.starts_with('-'))
                    .map(|arg| clean_shell_word(arg)),
            );
        }
        idx += 1;
    }
    targets
}

fn shell_words(command: &str) -> Vec<String> {
    command
        .replace(">>", " __OPENFLOWS_REDIR__ ")
        .replace('>', " __OPENFLOWS_REDIR__ ")
        .replace('|', " | ")
        .replace(';', " ; ")
        .split_whitespace()
        .map(|word| {
            if word == "__OPENFLOWS_REDIR__" {
                ">".to_string()
            } else {
                clean_shell_word(word)
            }
        })
        .filter(|word| !word.is_empty())
        .collect()
}

fn clean_shell_word(word: &str) -> String {
    word.trim_matches(|c| matches!(c, '"' | '\'' | ';' | '(' | ')'))
        .to_string()
}

fn is_shell_separator(word: &str) -> bool {
    matches!(word, "|" | ";" | "&&" | "||")
}

/// Sentinel-level guidance injected so the model knows what job it is on.
pub fn sentinel_guidance(st: &TicketState) -> String {
    match sentinel_job(st) {
        SentinelJob::PlanGateReview => {
            "SENTINEL plan-gate review: FORGE is in `planning` and awaits approval. \
             Read PLAN.md, evaluate it against the ticket, then run \
             `openflows-harness gate decide --phase plan_ready --revision <N> --round <R> --verdict approve --report review.md`."
                .to_string()
        }
        SentinelJob::TestingReview => "Review implementation against the approved plan; run A2A verification and gate decide --phase testing with exact revision/head and a report. Map the approved plan criteria to commands and observed results in the report. For infrastructure failure or unavailable sandbox/toolchain prerequisites, record the blocker in review.md, run `openflows-harness status set blocked`, and stop instead of rejecting into building. Unfamiliar test tools are allowed in the mandatory verification sandbox. Known destructive/control-plane operations are denied. A successful echo is only a transport probe, not acceptance evidence. Human testing review is TODO; successful A2A verification and SENTINEL approval permit submit.".to_string(),
        SentinelJob::PrReview => {
            "SENTINEL PR review: FORGE is `submit` with a PR to review. Read the \
             ticket + diff, write your evaluation report (*-eval.md / final-review.md), \
             then `openflows-harness review submit --verdict approve|reject`."
                .to_string()
        }
        SentinelJob::Idle => {
            "SENTINEL: no review is pending right now. Do not submit a gate approval or \
             review verdict until FORGE signals `planning` or `submit`."
                .to_string()
        }
    }
}

/// Sentinel-only phase-aware decision for a `pre_tool_use` dispatch. Sentinel is
/// read-only (denies source writes) and must follow its review job.
pub async fn sentinel_phase_guard(
    store: &SharedStore,
    ticket_id: &str,
    st: &TicketState,
    tool_name: &str,
    input: &Value,
    base: HookDecision,
) -> HookDecision {
    let lower = tool_name.to_lowercase();
    let guidance = sentinel_guidance(st);

    // Source writes are always denied for Sentinel; review artifacts are allowed.
    let is_write = matches!(lower.as_str(), "write" | "edit" | "create" | "patch");
    if is_write {
        let path = input
            .get("path")
            .or_else(|| input.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if is_review_artifact(path) {
            return base.with_model_context(guidance);
        }
        return HookDecision::deny(
            "openflows policy: SENTINEL is a readonly reviewer — source writes are blocked \
             (only *-eval.md / final-review.md review reports may be written)",
        )
        .with_model_context(guidance);
    }

    if matches!(lower.as_str(), "bash" | "sh" | "shell" | "exec" | "execute")
        && super::guard::is_verification_request(&command_text(input))
    {
        if sentinel_job(st) != SentinelJob::TestingReview {
            return HookDecision::deny("A2A verification requires the testing phase");
        }
        return base.with_model_context(guidance);
    }

    if matches!(lower.as_str(), "bash" | "sh" | "shell" | "exec")
        && shell_writes_source(&command_text(input))
    {
        return HookDecision::deny(
            "openflows policy: SENTINEL is a readonly reviewer — shell source writes are blocked \
             (only review report artifacts may be written)",
        )
        .with_model_context(guidance);
    }

    // Only coordinate on review/harness tools.
    if !lower.contains("harness") && !matches!(lower.as_str(), "bash" | "sh" | "shell" | "exec") {
        return base;
    }
    let command = input
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_lowercase();

    // Gate approval requires an uploaded plan.
    if command.contains("gate approve")
        && sentinel_job(st) == SentinelJob::PlanGateReview
        && !st.plan_exists
    {
        return HookDecision::deny(
            "openflows policy: cannot approve the planning gate — no plan exists yet. \
             Ask FORGE to run `/plan` and upload PLAN.md first.",
        )
        .with_model_context(guidance);
    }

    // A review verdict requires the evaluation report to be written first.
    if command.contains("review submit")
        && sentinel_job(st) == SentinelJob::PrReview
        && !review_report_exists(store, ticket_id).await
    {
        return HookDecision::deny(
            "openflows policy: cannot submit a review yet — write your evaluation \
             report (*-eval.md / final-review.md) first, then `review submit`.",
        )
        .with_model_context(guidance);
    }

    // A verdict submitted outside the PR-review job is out of order.
    if command.contains("review submit")
        && sentinel_job(st) != SentinelJob::PrReview
        && !(command.contains("gate approve") || command.contains("gate status"))
    {
        return HookDecision::deny(
            "openflows policy: no PR review pending — do not submit a review verdict now.",
        )
        .with_model_context(guidance);
    }

    base.with_model_context(guidance)
}
