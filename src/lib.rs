#[cfg(test)]
mod analyzer;
#[allow(dead_code)]
mod semantic;
mod spec_runner;
#[cfg(test)]
mod analyzer_test;
#[cfg(test)]
mod tags_test;

use zed_extension_api::{self as zed, Result};

/// Manages downloading, caching, and versioning of the zircon-server binary.
struct ServerBinaryManager {
    cached_binary_path: Option<String>,
}

impl ServerBinaryManager {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    /// Return the path to a cached zircon-server binary, downloading it if needed.
    fn server_binary_path(
        &mut self,
        language_server_id: &zed::LanguageServerId,
    ) -> Result<String, String> {
        // Return in-memory cached path if the file still exists on disk.
        if let Some(ref path) = self.cached_binary_path {
            if std::fs::metadata(path).is_ok() {
                return Ok(path.clone());
            }
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = zed::latest_github_release(
            "yampug/zircon",
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let version = &release.version;
        let binary_path = format!("zircon-server-{version}/zircon-server");

        // Already have this version on disk — reuse it.
        if std::fs::metadata(&binary_path).is_ok() {
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(binary_path);
        }

        // Pick the platform-appropriate gzipped asset.
        let (os, arch) = zed::current_platform();
        let asset_name = match (os, arch) {
            (zed::Os::Mac, zed::Architecture::Aarch64) => {
                "zircon-server_arm64-apple-darwin.gz"
            }
            (zed::Os::Mac, zed::Architecture::X8664) => {
                "zircon-server_x86_64-apple-darwin.gz"
            }
            (zed::Os::Linux, zed::Architecture::X8664) => {
                "zircon-server_x86_64-unknown-linux-musl.gz"
            }
            (zed::Os::Linux, zed::Architecture::Aarch64) => {
                "zircon-server_arm64-unknown-linux-musl.gz"
            }
            _ => return Err("unsupported platform for zircon-server".to_string()),
        };

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| format!("no release asset found: {asset_name}"))?;

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::Downloading,
        );

        zed::download_file(
            &asset.download_url,
            &binary_path,
            zed::DownloadedFileType::Gzip,
        )?;

        zed::make_file_executable(&binary_path)?;

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

struct ZirconExtension {
    binary_manager: ServerBinaryManager,
}

impl zed::Extension for ZirconExtension {
    fn new() -> Self {
        Self {
            binary_manager: ServerBinaryManager::new(),
        }
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
