mod analyzer;
#[cfg(test)]
mod analyzer_test;

use std::fs;
use zed_extension_api::{self as zed, Result};
use analyzer::Analyzer;

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
}

impl CrystalliZedExtension {
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
