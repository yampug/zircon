mod analyzer;
mod semantic;
mod spec_runner;
#[cfg(test)]
mod analyzer_test;

use std::fs;
use zed_extension_api::{self as zed, Result};

struct CrystalliZedExtension {
    cached_binary_path: Option<String>,
}

impl zed::Extension for CrystalliZedExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        // TODO: Future Implementation (Option 2 - Custom WASM LSP)
        // In the future, this is where we could intercept the start request
        // and either spawn an internal WASM-based analyzer or fallback to the external one.
        // For now, we utilize `crystalline` as the default language server.

        let path = self.language_server_binary_path(language_server_id, worktree)?;

        Ok(zed::Command {
            command: path,
            args: vec![],
            env: Default::default(),
        })
    }

    fn label_for_completion(
        &self,
        _language_server_id: &zed::LanguageServerId,
        completion: zed::lsp::Completion,
    ) -> Option<zed::CodeLabel> {
        let kind = completion.kind.as_ref()?;
        let label = &completion.label;
        let name_highlight = semantic::highlight_for_completion_kind(kind);

        let mut spans = vec![zed::CodeLabelSpan::literal(
            label,
            Some(name_highlight.to_string()),
        )];

        if let Some(detail) = &completion.detail {
            if !detail.is_empty() {
                let type_highlight = semantic::highlight_for_type(detail);
                spans.push(zed::CodeLabelSpan::literal(
                    " : ",
                    Some("punctuation.delimiter".to_string()),
                ));
                spans.push(zed::CodeLabelSpan::literal(
                    detail,
                    Some(type_highlight.to_string()),
                ));
            }
        }

        Some(zed::CodeLabel {
            code: label.clone(),
            spans,
            filter_range: zed::Range {
                start: 0,
                end: label.len() as u32,
            },
        })
    }

    fn label_for_symbol(
        &self,
        _language_server_id: &zed::LanguageServerId,
        symbol: zed::lsp::Symbol,
    ) -> Option<zed::CodeLabel> {
        let name = &symbol.name;
        let highlight = semantic::highlight_for_symbol_kind(&symbol.kind);

        Some(zed::CodeLabel {
            code: name.clone(),
            spans: vec![zed::CodeLabelSpan::literal(
                name,
                Some(highlight.to_string()),
            )],
            filter_range: zed::Range {
                start: 0,
                end: name.len() as u32,
            },
        })
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

impl CrystalliZedExtension {
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

    fn language_server_binary_path(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).map_or(false, |stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        if let Some(path) = worktree.which("crystalline") {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = zed::latest_github_release(
            "elbywan/crystalline",
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (platform, arch) = zed::current_platform();
        
        let asset_name = format!(
            "crystalline_{}-{}.gz",
            match arch {
                zed::Architecture::Aarch64 => "arm64",
                zed::Architecture::X86 => "x86_64",
                zed::Architecture::X8664 => "x86_64",
            },
            match platform {
                zed::Os::Mac => "apple-darwin",
                zed::Os::Linux => "unknown-linux-musl",
                zed::Os::Windows => return Err("Windows is currently not supported by crystalline".to_string()),
            }
        );

        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no asset found matching {:?}", asset_name))?;

        let version_dir = format!("crystalline-{}", release.version);
        fs::create_dir_all(&version_dir)
            .map_err(|e| format!("failed to create directory: {e}"))?;
            
        let binary_path = format!("{version_dir}/crystalline");

        if !fs::metadata(&binary_path).map_or(false, |stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            zed::download_file(
                &asset.download_url,
                &binary_path,
                zed::DownloadedFileType::Gzip,
            )
            .map_err(|e| format!("failed to download file: {e}"))?;

            zed::make_file_executable(&binary_path)
                .map_err(|e| format!("failed to make file executable: {e}"))?;
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

zed::register_extension!(CrystalliZedExtension);
