#[cfg(test)]
mod analyzer;
#[allow(dead_code)]
mod semantic;
mod spec_runner;
#[cfg(test)]
mod analyzer_test;

use zed_extension_api::{self as zed, Result};

struct ZirconExtension;

impl zed::Extension for ZirconExtension {
    fn new() -> Self {
        Self
    }

    fn run_slash_command(
        &self,
        command: zed::SlashCommand,
        args: Vec<String>,
        worktree: Option<&zed::Worktree>,
    ) -> Result<zed::SlashCommandOutput, String> {
        match command.name.as_str() {
            "crystal-expand" => self.run_macro_expand(args, worktree),
            "crystal-check" => self.run_crystal_check(args, worktree),
            "crystal-spec" => self.run_crystal_spec(args, worktree),
            _ => Err(format!("unknown command: {}", command.name)),
        }
    }

    fn complete_slash_command_argument(
        &self,
        command: zed::SlashCommand,
        _args: Vec<String>,
    ) -> Result<Vec<zed::SlashCommandArgumentCompletion>, String> {
        match command.name.as_str() {
            "crystal-expand" => Ok(vec![
                zed::SlashCommandArgumentCompletion {
                    label: "file.cr".to_string(),
                    new_text: "file.cr".to_string(),
                    run_command: false,
                },
                zed::SlashCommandArgumentCompletion {
                    label: "file.cr line:col".to_string(),
                    new_text: "file.cr 1:1".to_string(),
                    run_command: false,
                },
            ]),
            "crystal-check" => Ok(vec![zed::SlashCommandArgumentCompletion {
                label: "file.cr".to_string(),
                new_text: "file.cr".to_string(),
                run_command: true,
            }]),
            "crystal-spec" => Ok(vec![
                zed::SlashCommandArgumentCompletion {
                    label: "spec/file_spec.cr".to_string(),
                    new_text: "spec/file_spec.cr".to_string(),
                    run_command: true,
                },
                zed::SlashCommandArgumentCompletion {
                    label: "spec/ (run all)".to_string(),
                    new_text: "spec/".to_string(),
                    run_command: true,
                },
            ]),
            _ => Ok(Vec::new()),
        }
    }
}

impl ZirconExtension {
    fn run_macro_expand(
        &self,
        args: Vec<String>,
        _worktree: Option<&zed::Worktree>,
    ) -> Result<zed::SlashCommandOutput, String> {
        let file_path = args
            .first()
            .ok_or("usage: /crystal-expand <file> [line:col]")?;

        let mut cmd = zed::process::Command::new("crystal");
        cmd = cmd.arg("tool").arg("expand");

        if let Some(cursor) = args.get(1) {
            cmd = cmd.arg("-c").arg(cursor.as_str());
        }

        cmd = cmd.arg(file_path.as_str());

        let output = cmd
            .output()
            .map_err(|e| format!("failed to run crystal tool expand: {e}"))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.stderr.is_empty() && output.stdout.is_empty() {
            return Err(format!("crystal tool expand failed:\n{stderr}"));
        }

        let expanded = String::from_utf8_lossy(&output.stdout);
        if expanded.is_empty() {
            return Err("crystal tool expand produced no output".to_string());
        }

        let text = format!("```crystal\n{}\n```", expanded.trim());
        let label = if args.get(1).is_some() {
            format!("Macro expansion: {}:{}", file_path, args[1])
        } else {
            format!("Macro expansion: {}", file_path)
        };

        Ok(zed::SlashCommandOutput {
            text: text.clone(),
            sections: vec![zed::SlashCommandOutputSection {
                range: zed::Range {
                    start: 0,
                    end: text.len() as u32,
                },
                label,
            }],
        })
    }

    fn run_crystal_check(
        &self,
        args: Vec<String>,
        _worktree: Option<&zed::Worktree>,
    ) -> Result<zed::SlashCommandOutput, String> {
        let file_path = args
            .first()
            .ok_or("usage: /crystal-check <file>")?;

        let output = zed::process::Command::new("crystal")
            .arg("build")
            .arg("--no-codegen")
            .arg("-f")
            .arg("json")
            .arg(file_path.as_str())
            .output()
            .map_err(|e| format!("failed to run crystal build: {e}"))?;

        let json_src = if !output.stdout.is_empty() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).to_string()
        };

        let diagnostics = semantic::parse_diagnostics(&json_src);

        if diagnostics.is_empty() {
            let text = format!("No type issues found in `{}`.", file_path);
            return Ok(zed::SlashCommandOutput {
                text: text.clone(),
                sections: vec![zed::SlashCommandOutputSection {
                    range: zed::Range {
                        start: 0,
                        end: text.len() as u32,
                    },
                    label: format!("Crystal check: {}", file_path),
                }],
            });
        }

        let mut text = String::new();
        for d in &diagnostics {
            let nil_tag = if semantic::is_nil_risk(&d.message) {
                " [nil-risk]"
            } else {
                ""
            };
            text.push_str(&format!(
                "{}:{}:{} [{}]{}\n  {}\n\n",
                d.file, d.line, d.column, d.severity, nil_tag, d.message
            ));
        }

        Ok(zed::SlashCommandOutput {
            text: text.clone(),
            sections: vec![zed::SlashCommandOutputSection {
                range: zed::Range {
                    start: 0,
                    end: text.len() as u32,
                },
                label: format!(
                    "Crystal check: {} ({} issues)",
                    file_path,
                    diagnostics.len()
                ),
            }],
        })
    }

    fn run_crystal_spec(
        &self,
        args: Vec<String>,
        _worktree: Option<&zed::Worktree>,
    ) -> Result<zed::SlashCommandOutput, String> {
        let spec_path = args.first().map(|s| s.as_str()).unwrap_or("spec/");

        let output = zed::process::Command::new("crystal")
            .arg("spec")
            .arg(spec_path)
            .arg("--no-color")
            .output()
            .map_err(|e| format!("failed to run crystal spec: {e}"))?;

        let combined = {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stdout.is_empty() {
                stdout.to_string()
            } else {
                stderr.to_string()
            }
        };

        let result = spec_runner::parse_spec_output(&combined);
        let text = spec_runner::format_inline_annotations(&result);

        let label = if result.failures.is_empty() {
            format!("Specs passed: {}", spec_path)
        } else {
            format!(
                "Spec failures: {} ({}/{})",
                spec_path, result.failed, result.total
            )
        };

        Ok(zed::SlashCommandOutput {
            text: text.clone(),
            sections: vec![zed::SlashCommandOutputSection {
                range: zed::Range {
                    start: 0,
                    end: text.len() as u32,
                },
                label,
            }],
        })
    }

}

zed::register_extension!(ZirconExtension);
