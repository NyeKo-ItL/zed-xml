use zed_extension_api as zed;

const LANGUAGE_SERVER_ID: &str = "xml-lsp";
const XML_LSP_PATH_ENV: &str = "XML_LSP_PATH";
const XML_LSP_DOWNLOAD_URL_ENV: &str = "XML_LSP_DOWNLOAD_URL";
const RELEASE_REPOSITORY: &str = "NyeKo-ItL/zed-xml";

struct XmlExtension;

impl XmlExtension {
    fn environment(worktree: &zed::Worktree, name: &str) -> Option<String> {
        worktree
            .shell_env()
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn native_command(
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        if let Some(path) = Self::environment(worktree, XML_LSP_PATH_ENV) {
            return Ok(Self::command(path));
        }

        if let Some(binary) = worktree.which(LANGUAGE_SERVER_ID) {
            return Ok(Self::command(binary));
        }

        // Keep local development convenient when the opened worktree is this repository.
        if worktree.read_text_file("Cargo.toml").is_ok()
            && let Some(cargo) = worktree.which("cargo")
        {
            return Ok(zed::Command {
                command: cargo,
                args: vec![
                    "run".to_owned(),
                    "--quiet".to_owned(),
                    "-p".to_owned(),
                    LANGUAGE_SERVER_ID.to_owned(),
                    "--".to_owned(),
                    "--stdio".to_owned(),
                ],
                env: Vec::new(),
            });
        }

        Self::downloaded_command(language_server_id, worktree)
    }

    fn command(path: String) -> zed::Command {
        zed::Command {
            command: path,
            args: vec!["--stdio".to_owned()],
            env: Vec::new(),
        }
    }

    fn release_asset(
        os: zed::Os,
        architecture: zed::Architecture,
    ) -> zed::Result<(&'static str, &'static str)> {
        match (os, architecture) {
            (zed::Os::Windows, zed::Architecture::X8664) => {
                Ok(("xml-lsp-x86_64-pc-windows-msvc.exe", "xml-lsp.exe"))
            }
            (zed::Os::Linux, zed::Architecture::X8664) => {
                Ok(("xml-lsp-x86_64-unknown-linux-gnu", "xml-lsp"))
            }
            (zed::Os::Mac, zed::Architecture::Aarch64) => {
                Ok(("xml-lsp-aarch64-apple-darwin", "xml-lsp"))
            }
            _ => Err(
                "Unsupported platform for xml-lsp. Set XML_LSP_PATH to a native binary.".to_owned(),
            ),
        }
    }

    fn downloaded_command(
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let (os, architecture) = zed::current_platform();
        let (asset_name, executable) = Self::release_asset(os, architecture)?;
        let override_url = Self::environment(worktree, XML_LSP_DOWNLOAD_URL_ENV);

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::Downloading,
        );

        let result: zed::Result<String> = (|| {
            let (download_url, executable_path) = if let Some(url) = override_url {
                // The extension host does not create parent directories for downloads.
                (url, executable.to_owned())
            } else {
                let release = zed::latest_github_release(
                    RELEASE_REPOSITORY,
                    zed::GithubReleaseOptions {
                        require_assets: true,
                        pre_release: false,
                    },
                )?;
                let asset = release
                    .assets
                    .into_iter()
                    .find(|asset| asset.name == asset_name)
                    .ok_or_else(|| {
                        format!(
                            "Could not find asset {asset_name} in the latest {RELEASE_REPOSITORY} release"
                        )
                    })?;
                (
                    asset.download_url,
                    format!("{LANGUAGE_SERVER_ID}-{}-{executable}", release.version),
                )
            };

            if let Err(error) = zed::download_file(
                &download_url,
                &executable_path,
                zed::DownloadedFileType::Uncompressed,
            ) {
                // A second Zed window may try to refresh the same binary while the
                // first LSP process has it open. Windows reports that as os error 32.
                if !error.contains("os error 32") {
                    return Err(error);
                }
            }
            if !matches!(os, zed::Os::Windows) {
                zed::make_file_executable(&executable_path)?;
            }
            Ok(executable_path)
        })();

        match result {
            Ok(executable_path) => {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::None,
                );
                Ok(Self::command(executable_path))
            }
            Err(error) => {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::Failed(error.clone()),
                );
                Err(format!(
                    "Could not install xml-lsp asset {asset_name}: {error}. Set XML_LSP_PATH to a local binary."
                ))
            }
        }
    }
}

impl zed::Extension for XmlExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Err(format!(
                "Unsupported XML language server: {language_server_id}"
            ));
        }

        Self::native_command(language_server_id, worktree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_x86_64_asset_and_executable_names() {
        assert_eq!(
            XmlExtension::release_asset(zed::Os::Windows, zed::Architecture::X8664),
            Ok(("xml-lsp-x86_64-pc-windows-msvc.exe", "xml-lsp.exe"))
        );
    }

    #[test]
    fn maps_linux_x86_64_asset_and_executable_names() {
        assert_eq!(
            XmlExtension::release_asset(zed::Os::Linux, zed::Architecture::X8664),
            Ok(("xml-lsp-x86_64-unknown-linux-gnu", "xml-lsp"))
        );
    }

    #[test]
    fn maps_macos_arm64_asset_and_executable_names() {
        assert_eq!(
            XmlExtension::release_asset(zed::Os::Mac, zed::Architecture::Aarch64),
            Ok(("xml-lsp-aarch64-apple-darwin", "xml-lsp"))
        );
    }

    #[test]
    fn rejects_unsupported_platforms() {
        assert!(XmlExtension::release_asset(zed::Os::Windows, zed::Architecture::Aarch64).is_err());
    }

    #[test]
    fn recognizes_windows_file_in_use_errors() {
        assert!(
            "Le processus ne peut pas accéder au fichier (os error 32)".contains("os error 32")
        );
        assert!(!"download failed with status 404".contains("os error 32"));
    }
}

zed::register_extension!(XmlExtension);
