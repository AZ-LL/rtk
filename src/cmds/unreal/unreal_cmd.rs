//! Filters Unreal Engine build, UAT, commandlet, and automation-test output.

use crate::core::runner::{self, RunOptions};
use crate::core::truncate::{CAP_ERRORS, CAP_LIST, CAP_WARNINGS};
use crate::core::utils::resolved_command;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnrealMode {
    Build,
    Uat,
    Cook,
    Package,
    Commandlet,
    Automation,
}

impl UnrealMode {
    fn label(self) -> &'static str {
        match self {
            UnrealMode::Build => "build",
            UnrealMode::Uat => "uat",
            UnrealMode::Cook => "cook",
            UnrealMode::Package => "package",
            UnrealMode::Commandlet => "commandlet",
            UnrealMode::Automation => "automation",
        }
    }
}

lazy_static! {
    static ref ANSI_RE: Regex = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    static ref GCC_CLANG_DIAG_RE: Regex =
        Regex::new(r"^[^:\s].*:\d+:\d+:\s+(?:error|warning|note|fatal error):").unwrap();
    static ref MSVC_DIAG_RE: Regex =
        Regex::new(r"^.+\(\d+\):\s+(?:error|warning|fatal error)\s+[A-Z]+\d+:").unwrap();
    static ref UNREAL_ERROR_RE: Regex = Regex::new(
        r"(?i)(\berror:|fatal error|assertion failed|ensure condition failed|automation test failed|packagingresults:\s*error|automationtool:\s*error|runuat error|unrealbuildtool failed|build failed|result=\{fail\}|unknown cook failure|undefined reference|linker command failed|lnk\d+)"
    )
    .unwrap();
    static ref UNREAL_WARNING_RE: Regex =
        Regex::new(r"(?i)(\bwarning:|packagingresults:\s*warning|log\w+:\s*warning:)").unwrap();
    static ref SUMMARY_RE: Regex = Regex::new(
        r"(?i)(^result:\s|^total execution time:|^total time in|automationtool exiting|build successful|build failed|\*{5,} .+ command (completed|failed) \*{5,}|tests? completed|execution of commandlet took|exiting staticshutdownaftererror)"
    )
    .unwrap();
    static ref PROGRESS_RE: Regex = Regex::new(
        r"(?i)(^\[\d+/\d+\]\s+(compile|link|write|copy)|^creating makefile|^parsing headers|^reflection code generated|^determining max actions|^executing up to|cooked packages \d+ packages remain|finished savepackage|resaving package|worker \d+: \d+ shaders|asset registry cache written|^initializing script modules|^using bundled dotnet|^running automationtool|^automationtool executed|^parsing command line:|^loginit:\s*display:|^logassetregistry:\s*display:|^logsourcecontrol:\s*display:|^logstreaming:\s*display:|^logautomationcontroller:\s*display:\s*running test|result=\{success\})"
    )
    .unwrap();
    static ref ASSET_PATH_RE: Regex =
        Regex::new(r"(/Game/[A-Za-z0-9_./-]+|/[A-Za-z0-9_./-]+\.uasset|/[A-Za-z0-9_./-]+\.umap)")
            .unwrap();
    static ref SOURCE_CONTEXT_RE: Regex =
        Regex::new(r"^\s*(\d+\s*\||\||\^|~|\w|UnrealEditor-|Module\.|/|\[File:)").unwrap();
    static ref STACK_RE: Regex = Regex::new(r"(?i)^(stack:|\s+unrealeditor-|.+\.so!|.+\.dll!)").unwrap();
}

pub fn run(mode: UnrealMode, args: &[String], verbose: u8) -> Result<i32> {
    let Some((executable, native_args)) = args.split_first() else {
        anyhow::bail!(
            "rtk unreal {}: expected native Unreal command path followed by arguments",
            mode.label()
        );
    };

    let mut cmd = new_command(executable);
    cmd.args(native_args);

    if verbose > 0 {
        eprintln!("Running: {} {}", executable, native_args.join(" "));
    }

    let args_owned = args.to_vec();
    runner::run_filtered_with_exit_code(
        cmd,
        executable,
        &native_args.join(" "),
        move |raw, exit_code| filter_output_with_exit_code(raw, mode, &args_owned, exit_code),
        RunOptions::with_tee(match mode {
            UnrealMode::Build => "unreal_build",
            UnrealMode::Uat => "unreal_uat",
            UnrealMode::Cook => "unreal_cook",
            UnrealMode::Package => "unreal_package",
            UnrealMode::Commandlet => "unreal_commandlet",
            UnrealMode::Automation => "unreal_automation",
        }),
    )
}

fn new_command(executable: &str) -> Command {
    if executable.contains('/') || executable.contains('\\') || executable.starts_with('.') {
        Command::new(executable)
    } else {
        resolved_command(executable)
    }
}

#[derive(Debug)]
struct CappedLines {
    label: &'static str,
    cap: usize,
    total: usize,
    lines: Vec<String>,
    seen: HashSet<String>,
}

impl CappedLines {
    fn new(label: &'static str, cap: usize) -> Self {
        Self {
            label,
            cap,
            total: 0,
            lines: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn push(&mut self, line: &str) {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || !self.seen.insert(trimmed.to_string()) {
            return;
        }

        self.total += 1;
        if self.lines.len() < self.cap {
            self.lines.push(trimmed.to_string());
        }
    }

    fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn append_to(&self, out: &mut Vec<String>) {
        out.extend(self.lines.iter().cloned());
        if self.total > self.lines.len() {
            out.push(format!(
                "... +{} more {}",
                self.total - self.lines.len(),
                self.label
            ));
        }
    }
}

#[derive(Default)]
struct FilterState {
    failure_seen: bool,
    process_exit_code: Option<i32>,
    progress_total: usize,
    packages_seen: Option<String>,
}

#[cfg(test)]
pub(crate) fn filter_output(raw: &str, mode: UnrealMode, args: &[String]) -> String {
    filter_output_with_exit_code(raw, mode, args, 0)
}

pub(crate) fn filter_output_with_exit_code(
    raw: &str,
    mode: UnrealMode,
    args: &[String],
    exit_code: i32,
) -> String {
    let cleaned = ANSI_RE.replace_all(raw, "");
    let mut diagnostics = CappedLines::new("diagnostics", CAP_ERRORS);
    let mut warnings = CappedLines::new("warnings", CAP_WARNINGS);
    let mut summaries = CappedLines::new("summary lines", CAP_LIST);
    let mut state = FilterState::default();
    let mut context_remaining = 0usize;
    let mut previous_context: Option<String> = None;

    for line in cleaned.lines() {
        let trimmed = line.trim_end();
        let compact = trimmed.trim();
        if compact.is_empty() {
            continue;
        }

        if is_progress_noise(compact) {
            state.progress_total += 1;
            if compact.to_ascii_lowercase().contains("cooked packages") {
                state.packages_seen = Some(compact.to_string());
            }
            continue;
        }

        if is_error_line(compact) {
            state.failure_seen = true;
            if let Some(prev) = previous_context.take() {
                if is_context_line(&prev) {
                    diagnostics.push(&prev);
                }
            }
            diagnostics.push(trimmed);
            context_remaining = 4;
            continue;
        }

        if is_warning_line(compact) {
            warnings.push(trimmed);
            previous_context = Some(trimmed.to_string());
            context_remaining = context_remaining.max(1);
            continue;
        }

        if is_summary_line(compact) {
            if compact.eq_ignore_ascii_case("build failed") {
                state.failure_seen = true;
            }
            summaries.push(trimmed);
            continue;
        }

        if context_remaining > 0 && is_context_line(trimmed) {
            diagnostics.push(trimmed);
            context_remaining -= 1;
            continue;
        }

        if is_asset_or_source_context(trimmed) {
            previous_context = Some(trimmed.to_string());
        }
    }

    if exit_code != 0 {
        state.process_exit_code = Some(exit_code);
        state.failure_seen = true;
    }

    if state.failure_seen || !diagnostics.is_empty() {
        return format_failure_output(
            mode,
            &diagnostics,
            &warnings,
            &summaries,
            &state,
            cleaned.as_ref(),
        );
    }

    format_success_output(mode, args, &warnings, &summaries, &state)
}

fn format_failure_output(
    mode: UnrealMode,
    diagnostics: &CappedLines,
    warnings: &CappedLines,
    summaries: &CappedLines,
    state: &FilterState,
    raw: &str,
) -> String {
    let mut out = Vec::new();
    let summaries_before_tail = diagnostics.is_empty();

    if diagnostics.is_empty() {
        out.push(format!("unreal {}: failed", mode.label()));
        append_process_exit(&mut out, state);
        summaries.append_to(&mut out);
        out.extend(fallback_tail(raw, 12));
    } else {
        diagnostics.append_to(&mut out);
        append_process_exit(&mut out, state);
    }

    warnings.append_to(&mut out);
    if !summaries_before_tail {
        summaries.append_to(&mut out);
    }

    if state.progress_total > 0 {
        out.push(format!(
            "... omitted {} Unreal progress/noise lines",
            state.progress_total
        ));
    }

    compact_lines(out).join("\n")
}

fn append_process_exit(out: &mut Vec<String>, state: &FilterState) {
    if let Some(exit_code) = state.process_exit_code {
        out.push(format!("process exited with code {exit_code}"));
    }
}

fn format_success_output(
    mode: UnrealMode,
    args: &[String],
    warnings: &CappedLines,
    summaries: &CappedLines,
    state: &FilterState,
) -> String {
    let mut out = vec![success_header(mode, args, summaries, state)];

    if !warnings.is_empty() {
        out.push(format!(
            "warnings: {} total (showing up to {})",
            warnings.total,
            warnings.lines.len()
        ));
        warnings.append_to(&mut out);
    }

    if summaries.lines.len() > 1 {
        for line in &summaries.lines {
            if !out.iter().any(|existing| existing.contains(line)) {
                out.push(line.clone());
            }
        }
    }

    if state.progress_total > 0 {
        out.push(format!(
            "... omitted {} Unreal progress/noise lines",
            state.progress_total
        ));
    }

    compact_lines(out).join("\n")
}

fn success_header(
    mode: UnrealMode,
    args: &[String],
    summaries: &CappedLines,
    state: &FilterState,
) -> String {
    match mode {
        UnrealMode::Build => {
            let detail = build_arg_summary(args);
            format_with_detail("unreal build: ok", &detail)
        }
        UnrealMode::Cook => {
            let detail = uat_arg_summary(args);
            if let Some(packages) = state.packages_seen.as_deref() {
                format_with_detail("unreal cook: ok", &format!("{}  {}", detail, packages))
            } else {
                format_with_detail("unreal cook: ok", &detail)
            }
        }
        UnrealMode::Package => {
            let detail = uat_arg_summary(args);
            format_with_detail("unreal package: ok", &detail)
        }
        UnrealMode::Uat => {
            let detail = uat_arg_summary(args);
            format_with_detail("unreal uat: ok", &detail)
        }
        UnrealMode::Commandlet => {
            let detail = arg_value(args, "-run=").unwrap_or_else(|| "commandlet".to_string());
            format_with_detail("unreal commandlet: ok", &detail)
        }
        UnrealMode::Automation => {
            let summary = summaries
                .lines
                .iter()
                .find(|line| line.to_ascii_lowercase().contains("tests completed"))
                .cloned()
                .unwrap_or_else(|| "tests passed".to_string());
            format!("unreal automation: ok  {}", summary)
        }
    }
}

fn format_with_detail(prefix: &str, detail: &str) -> String {
    if detail.trim().is_empty() {
        prefix.to_string()
    } else {
        format!("{}  {}", prefix, detail.trim())
    }
}

fn build_arg_summary(args: &[String]) -> String {
    args.iter()
        .skip(1)
        .filter(|arg| {
            !arg.starts_with('-')
                && !arg.ends_with(".uproject")
                && !arg.contains('/')
                && !arg.contains('\\')
        })
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

fn uat_arg_summary(args: &[String]) -> String {
    let project = arg_value(args, "-project=")
        .or_else(|| arg_value(args, "-Project="))
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
    let platform = arg_value(args, "-targetplatform=")
        .or_else(|| arg_value(args, "-TargetPlatform="))
        .unwrap_or_default();
    let config = arg_value(args, "-clientconfig=")
        .or_else(|| arg_value(args, "-ClientConfig="))
        .or_else(|| arg_value(args, "-configuration="))
        .unwrap_or_default();

    [project.unwrap_or_default(), platform, config]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn arg_value(args: &[String], prefix: &str) -> Option<String> {
    args.iter()
        .find_map(|arg| arg.strip_prefix(prefix).map(str::to_string))
}

fn is_error_line(line: &str) -> bool {
    GCC_CLANG_DIAG_RE.is_match(line) || MSVC_DIAG_RE.is_match(line) || UNREAL_ERROR_RE.is_match(line)
}

fn is_warning_line(line: &str) -> bool {
    UNREAL_WARNING_RE.is_match(line)
}

fn is_summary_line(line: &str) -> bool {
    SUMMARY_RE.is_match(line)
}

fn is_progress_noise(line: &str) -> bool {
    PROGRESS_RE.is_match(line)
}

fn is_context_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    SOURCE_CONTEXT_RE.is_match(line)
        || STACK_RE.is_match(trimmed)
        || ASSET_PATH_RE.is_match(line)
        || trimmed.starts_with("clang++:")
        || trimmed.starts_with("ld:")
        || trimmed.starts_with("Module.")
}

fn is_asset_or_source_context(line: &str) -> bool {
    ASSET_PATH_RE.is_match(line)
        || line.contains(".cpp")
        || line.contains(".h")
        || line.contains(".cs")
        || line.contains(".uasset")
        || line.contains(".umap")
}

fn compact_lines(mut lines: Vec<String>) -> Vec<String> {
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    let mut out = Vec::with_capacity(lines.len());
    let mut seen = HashSet::new();
    for line in lines.drain(..) {
        if line.trim().is_empty() {
            continue;
        }
        if seen.insert(line.clone()) {
            out.push(line);
        }
    }
    out
}

fn fallback_tail(raw: &str, count: usize) -> Vec<String> {
    let meaningful: Vec<&str> = raw
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty() && !is_progress_noise(line.trim()))
        .collect();
    let start = meaningful.len().saturating_sub(count);
    meaningful[start..]
        .iter()
        .map(|line| (*line).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::utils::count_tokens;

    fn savings(raw: &str, filtered: &str) -> f64 {
        100.0 - (count_tokens(filtered) as f64 / count_tokens(raw) as f64 * 100.0)
    }

    #[test]
    fn build_success_compacts_progress() {
        let raw = include_str!("../../../tests/fixtures/unreal/build_success.txt");
        let args = vec![
            "Build.sh".to_string(),
            "LyraEditor".to_string(),
            "Linux".to_string(),
            "Development".to_string(),
        ];
        let out = filter_output(raw, UnrealMode::Build, &args);
        assert!(out.contains("unreal build: ok"));
        assert!(out.contains("LyraEditor Linux Development"));
        assert!(!out.contains("[1/18] Compile"));
        assert!(savings(raw, &out) >= 70.0);
    }

    #[test]
    fn build_nonzero_exit_overrides_success_shaped_log() {
        let raw = include_str!("../../../tests/fixtures/unreal/build_success.txt");
        let args = vec![
            "Build.sh".to_string(),
            "LyraEditor".to_string(),
            "Linux".to_string(),
            "Development".to_string(),
        ];
        let out = filter_output_with_exit_code(raw, UnrealMode::Build, &args, 134);
        assert!(out.contains("unreal build: failed"));
        assert!(out.contains("process exited with code 134"));
        assert!(!out.contains("unreal build: ok"));
    }

    #[test]
    fn build_compile_failure_keeps_source_diagnostics() {
        let raw = include_str!("../../../tests/fixtures/unreal/build_compile_failure.txt");
        let out = filter_output(raw, UnrealMode::Build, &["Build.sh".to_string()]);
        assert!(out.contains("GetItemCount"));
        assert!(out.contains("LyraInventoryComponent.cpp"));
        assert!(out.contains("unused variable"));
        assert!(out.contains("Result: Failed"));
        assert!(!out.contains("[1/11] Compile"));
    }

    #[test]
    fn build_link_failure_keeps_linker_details() {
        let raw = include_str!("../../../tests/fixtures/unreal/build_link_failure.txt");
        let out = filter_output(raw, UnrealMode::Build, &["Build.sh".to_string()]);
        assert!(out.contains("undefined reference"));
        assert!(out.contains("GrantStartupEffects"));
        assert!(out.contains("linker command failed"));
    }

    #[test]
    fn build_windows_msvc_failure_keeps_diagnostics() {
        let raw = include_str!("../../../tests/fixtures/unreal/build_windows_msvc_failure.txt");
        let out = filter_output(raw, UnrealMode::Build, &["Build.bat".to_string()]);
        assert!(out.contains("C2065"));
        assert!(out.contains("MissingAbility"));
        assert!(out.contains("LyraAbilityComponent.cpp"));
        assert!(out.contains("UnrealBuildTool failed"));
        assert!(!out.contains("[1/10] Compile"));
    }

    #[test]
    fn build_windows_link_failure_keeps_linker_details() {
        let raw = include_str!("../../../tests/fixtures/unreal/build_windows_link_failure.txt");
        let out = filter_output(raw, UnrealMode::Build, &["Build.bat".to_string()]);
        assert!(out.contains("LNK2019"));
        assert!(out.contains("GrantStartupEffects"));
        assert!(out.contains("LNK1120"));
        assert!(out.contains("UnrealBuildTool failed"));
    }

    #[test]
    fn build_macos_clang_failure_keeps_diagnostics() {
        let raw = include_str!("../../../tests/fixtures/unreal/build_macos_clang_failure.txt");
        let out = filter_output(raw, UnrealMode::Build, &["Build.sh".to_string()]);
        assert!(out.contains("MissingSlot"));
        assert!(out.contains("LyraInventoryComponent.cpp"));
        assert!(out.contains("unused variable"));
        assert!(out.contains("UnrealBuildTool failed"));
        assert!(!out.contains("[1/9] Compile"));
    }

    #[test]
    fn cook_failure_keeps_asset_context_and_summary() {
        let raw = include_str!("../../../tests/fixtures/unreal/uat_cook_failure.txt");
        let out = filter_output(raw, UnrealMode::Cook, &["RunUAT.sh".to_string()]);
        assert!(out.contains("/Game/Characters/Heroes/BP_Hero_Mage"));
        assert!(out.contains("GA_MissingSpell"));
        assert!(out.contains("ExitCode=25"));
        assert!(!out.contains("Cooked packages 125"));
    }

    #[test]
    fn package_success_compacts_stage_and_pak_noise() {
        let raw = include_str!("../../../tests/fixtures/unreal/uat_package_success.txt");
        let args = vec![
            "RunUAT.sh".to_string(),
            "BuildCookRun".to_string(),
            "-project=/workspace/Lyra/Lyra.uproject".to_string(),
            "-targetplatform=Linux".to_string(),
            "-clientconfig=Shipping".to_string(),
        ];
        let out = filter_output(raw, UnrealMode::Package, &args);
        assert!(out.contains("unreal package: ok"));
        assert!(out.contains("Lyra.uproject Linux Shipping"));
        assert!(!out.contains("Cooked packages 300"));
        assert!(savings(raw, &out) >= 60.0);
    }

    #[test]
    fn package_failure_keeps_uat_errors() {
        let raw = include_str!("../../../tests/fixtures/unreal/uat_package_failure.txt");
        let out = filter_output(raw, UnrealMode::Package, &["RunUAT.sh".to_string()]);
        assert!(out.contains("Failed to copy"));
        assert!(out.contains("Missing receipt"));
        assert!(out.contains("RunUAT ERROR"));
        assert!(out.contains("ExitCode=1"));
    }

    #[test]
    fn commandlet_success_compacts() {
        let raw = include_str!("../../../tests/fixtures/unreal/commandlet_success.txt");
        let args = vec![
            "UnrealEditor-Cmd".to_string(),
            "-run=ResavePackages".to_string(),
        ];
        let out = filter_output(raw, UnrealMode::Commandlet, &args);
        assert!(out.contains("unreal commandlet: ok  ResavePackages"));
        assert!(!out.contains("Resaving package"));
    }

    #[test]
    fn commandlet_failure_keeps_ensure_stack() {
        let raw = include_str!("../../../tests/fixtures/unreal/commandlet_failure.txt");
        let out = filter_output(raw, UnrealMode::Commandlet, &["UnrealEditor-Cmd".to_string()]);
        assert!(out.contains("WBP_MainMenu"));
        assert!(out.contains("Ensure condition failed"));
        assert!(out.contains("ULyraMenuFactory::CreateMenu"));
    }

    #[test]
    fn commandlet_macos_dylib_failure_keeps_module_stack() {
        let raw = include_str!("../../../tests/fixtures/unreal/commandlet_macos_dylib_failure.txt");
        let out = filter_output(raw, UnrealMode::Commandlet, &["UnrealEditor-Cmd".to_string()]);
        assert!(out.contains("UnrealEditor-LyraRuntime.dylib"));
        assert!(out.contains("FLyraRuntimeModule::StartupModule"));
        assert!(out.contains("Failed to load module LyraRuntime"));
        assert!(!out.contains("FAssetRegistry took"));
    }

    #[test]
    fn automation_success_compacts_pass_noise() {
        let raw = include_str!("../../../tests/fixtures/unreal/automation_success.txt");
        let out = filter_output(raw, UnrealMode::Automation, &["UnrealEditor-Cmd".to_string()]);
        assert!(out.contains("unreal automation: ok"));
        assert!(out.contains("3 tests completed"));
        assert!(!out.contains("CanAddItem"));
        assert!(savings(raw, &out) >= 60.0);
    }

    #[test]
    fn automation_success_lingering_handle_fixture_compacts_summary() {
        let raw = include_str!("../../../tests/fixtures/unreal/automation_success_lingering_handle.txt");
        let out = filter_output(raw, UnrealMode::Automation, &["UnrealEditor-Cmd".to_string()]);
        assert!(out.contains("unreal automation: ok"));
        assert!(out.contains("3 tests completed, 0 failed"));
        assert!(!out.contains("CanAddItem"));
    }

    #[test]
    fn automation_nonzero_exit_overrides_success_summary() {
        let raw = include_str!("../../../tests/fixtures/unreal/automation_success.txt");
        let out =
            filter_output_with_exit_code(raw, UnrealMode::Automation, &["UnrealEditor-Cmd".to_string()], 42);
        assert!(out.contains("unreal automation: failed"));
        assert!(out.contains("process exited with code 42"));
        assert!(out.contains("3 tests completed"));
        assert!(!out.contains("unreal automation: ok"));
    }

    #[test]
    fn automation_failure_keeps_failed_test_details() {
        let raw = include_str!("../../../tests/fixtures/unreal/automation_failure.txt");
        let out = filter_output(raw, UnrealMode::Automation, &["UnrealEditor-Cmd".to_string()]);
        assert!(out.contains("Lyra.Inventory.ReplicatesInventory"));
        assert!(out.contains("Expected replicated count"));
        assert!(out.contains("InventoryReplicationTest.cpp"));
        assert!(out.contains("1 failed"));
        assert!(!out.contains("CanAddItem"));
    }

    #[test]
    fn large_failure_cascade_gets_omitted_count() {
        let mut raw = String::new();
        for i in 0..30 {
            raw.push_str(&format!(
                "LogCook: Error: Unable to cook package /Game/Broken/Asset{}\n",
                i
            ));
        }
        raw.push_str("BUILD FAILED\n");
        let out = filter_output(&raw, UnrealMode::Cook, &["RunUAT.sh".to_string()]);
        assert!(out.contains("... +"));
        assert!(out.contains("more diagnostics"));
    }
}
