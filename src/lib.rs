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

    fn downloaded_command(
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let (os, architecture) = zed::current_platform();
        let (target, executable) = match (os, architecture) {
            (zed::Os::Windows, zed::Architecture::X8664) => {
                ("x86_64-pc-windows-msvc", "xml-lsp.exe")
            }
            (zed::Os::Linux, zed::Architecture::X8664) => ("x86_64-unknown-linux-gnu", "xml-lsp"),

            (zed::Os::Mac, zed::Architecture::Aarch64) => ("aarch64-apple-darwin", "xml-lsp"),
            _ => {
                return Err(
                    "Unsupported platform for xml-lsp. Set XML_LSP_PATH to a native binary."
                        .to_owned(),
                );
            }
        };
        let url = Self::environment(worktree, XML_LSP_DOWNLOAD_URL_ENV).unwrap_or_else(|| {
            format!(
                "https://github.com/{RELEASE_REPOSITORY}/releases/latest/download/xml-lsp-{target}"
            )
        });

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::Downloading,
        );
        if let Err(error) =
            zed::download_file(&url, executable, zed::DownloadedFileType::Uncompressed)
        {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Failed(error.clone()),
            );
            return Err(format!(
                "Could not install xml-lsp from {url}: {error}. Set XML_LSP_PATH to a local binary."
            ));
        }
        if !matches!(os, zed::Os::Windows) {
            zed::make_file_executable(executable)?;
        }
        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::None,
        );
        Ok(Self::command(executable.to_owned()))
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

zed::register_extension!(XmlExtension);
