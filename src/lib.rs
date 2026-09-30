use zed_extension_api::{self as zed, serde_json::Value, settings::LspSettings};

const LANGUAGE_SERVER_ID: &str = "xml-lsp";
const EXPECTED_LSP_VERSION: &str = env!("CARGO_PKG_VERSION");
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

        Self::downloaded_command(language_server_id, worktree)
    }

    fn command(path: String) -> zed::Command {
        zed::Command {
            command: path,
            args: vec!["--stdio".to_owned()],
            env: Vec::new(),
        }
    }

    fn version_output_matches(stdout: &[u8]) -> bool {
        String::from_utf8_lossy(stdout).trim()
            == format!("{LANGUAGE_SERVER_ID} {EXPECTED_LSP_VERSION}")
    }

    fn installed_version(executable: &str) -> zed::Result<String> {
        let mut command = zed::process::Command::new(executable.to_owned()).arg("--version");
        let output = command
            .output()
            .map_err(|error| format!("could not run --version: {error}"))?;
        if output.status != Some(0) {
            return Err(format!("--version exited with status {:?}", output.status));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn check_installed_version(executable: &str) -> zed::Result<()> {
        let version = Self::installed_version(executable)?;
        let expected = format!("{LANGUAGE_SERVER_ID} {EXPECTED_LSP_VERSION}");
        if Self::version_output_matches(version.as_bytes()) {
            Ok(())
        } else {
            Err(format!("reports {version:?}, expected {expected:?}"))
        }
    }

    fn installed_version_matches(executable: &str) -> bool {
        Self::check_installed_version(executable).is_ok()
    }

    /// `workspace/configuration` configuration: the `xml` section of the Zed
    /// `lsp.xml-lsp.settings` settings, written with or without the `xml` key
    /// (`{"xml": {"format": …}}` or `{"format": …}`).
    fn workspace_configuration(settings: Option<Value>) -> Option<Value> {
        let settings = settings.filter(|settings| !settings.is_null())?;
        if settings.get("xml").is_some() {
            return Some(settings);
        }
        Some(zed::serde_json::json!({ "xml": settings }))
    }

    /// `initializationOptions`: those of `lsp.xml-lsp.initialization_options`
    /// when present, otherwise the settings (LemMinX convention
    /// `{"settings": {"xml": …}}`) so that the server applies them from
    /// initialization onwards.
    fn initialization_options(
        initialization_options: Option<Value>,
        settings: Option<Value>,
    ) -> Option<Value> {
        if let Some(options) = initialization_options.filter(|options| !options.is_null()) {
            return Some(options);
        }
        Self::workspace_configuration(settings)
            .map(|configuration| zed::serde_json::json!({ "settings": configuration }))
    }

    fn lsp_settings(worktree: &zed::Worktree) -> LspSettings {
        LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree).unwrap_or_default()
    }

    fn release_asset(os: zed::Os, architecture: zed::Architecture) -> zed::Result<&'static str> {
        match (os, architecture) {
            (zed::Os::Windows, zed::Architecture::X8664) => {
                Ok("xml-lsp-x86_64-pc-windows-msvc.exe")
            }
            (zed::Os::Linux, zed::Architecture::X8664) => Ok("xml-lsp-x86_64-unknown-linux-gnu"),
            (zed::Os::Mac, zed::Architecture::Aarch64) => Ok("xml-lsp-aarch64-apple-darwin"),
            _ => Err(
                "Unsupported platform for xml-lsp. Set XML_LSP_PATH to a native binary.".to_owned(),
            ),
        }
    }

    fn executable_name(os: zed::Os) -> String {
        let extension = if matches!(os, zed::Os::Windows) {
            ".exe"
        } else {
            ""
        };
        format!("{LANGUAGE_SERVER_ID}-{EXPECTED_LSP_VERSION}{extension}")
    }

    fn downloaded_command(
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let (os, architecture) = zed::current_platform();
        let asset_name = Self::release_asset(os, architecture)?;
        let executable = Self::executable_name(os);
        let override_url = Self::environment(worktree, XML_LSP_DOWNLOAD_URL_ENV);

        if Self::installed_version_matches(&executable) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::None,
            );
            return Ok(Self::command(executable));
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::Downloading,
        );

        let result: zed::Result<String> = (|| {
            let (download_url, executable_path) = if let Some(url) = override_url {
                // The extension host does not create parent directories for downloads.
                (url, executable.clone())
            } else {
                (
                    format!(
                        "https://github.com/{RELEASE_REPOSITORY}/releases/download/v{EXPECTED_LSP_VERSION}/{asset_name}"
                    ),
                    executable.clone(),
                )
            };

            if let Err(error) = zed::download_file(
                &download_url,
                &executable_path,
                zed::DownloadedFileType::Uncompressed,
            ) {
                // A second Zed window may try to refresh the same binary while the
                // first LSP process has it open. Reuse it only when it is already the
                // expected version; never hide an error for an outdated binary.
                if !(error.contains("os error 32")
                    && Self::installed_version_matches(&executable_path))
                {
                    return Err(error);
                }
            }
            if !matches!(os, zed::Os::Windows) {
                zed::make_file_executable(&executable_path)?;
            }
            if let Err(error) = Self::check_installed_version(&executable_path) {
                return Err(format!("Downloaded xml-lsp {error}"));
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

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<Value>> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Ok(None);
        }
        let settings = Self::lsp_settings(worktree);
        Ok(Self::initialization_options(
            settings.initialization_options,
            settings.settings,
        ))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<Value>> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Ok(None);
        }
        Ok(Self::workspace_configuration(
            Self::lsp_settings(worktree).settings,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_x86_64_asset_and_executable_names() {
        assert_eq!(
            XmlExtension::release_asset(zed::Os::Windows, zed::Architecture::X8664),
            Ok("xml-lsp-x86_64-pc-windows-msvc.exe")
        );
        assert_eq!(
            XmlExtension::executable_name(zed::Os::Windows),
            format!("xml-lsp-{EXPECTED_LSP_VERSION}.exe")
        );
    }

    #[test]
    fn maps_linux_x86_64_asset_and_executable_names() {
        assert_eq!(
            XmlExtension::release_asset(zed::Os::Linux, zed::Architecture::X8664),
            Ok("xml-lsp-x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            XmlExtension::executable_name(zed::Os::Linux),
            format!("xml-lsp-{EXPECTED_LSP_VERSION}")
        );
    }

    #[test]
    fn maps_macos_arm64_asset_and_executable_names() {
        assert_eq!(
            XmlExtension::release_asset(zed::Os::Mac, zed::Architecture::Aarch64),
            Ok("xml-lsp-aarch64-apple-darwin")
        );
        assert_eq!(
            XmlExtension::executable_name(zed::Os::Mac),
            format!("xml-lsp-{EXPECTED_LSP_VERSION}")
        );
    }

    #[test]
    fn rejects_unsupported_platforms() {
        assert!(XmlExtension::release_asset(zed::Os::Windows, zed::Architecture::Aarch64).is_err());
    }

    #[test]
    fn accepts_the_expected_version_output() {
        assert!(XmlExtension::version_output_matches(
            format!("{LANGUAGE_SERVER_ID} {EXPECTED_LSP_VERSION}\n").as_bytes()
        ));
    }

    #[test]
    fn rejects_another_version() {
        assert!(!XmlExtension::version_output_matches(b"xml-lsp 0.0.0\n"));
    }

    #[test]
    fn rejects_malformed_version_output() {
        assert!(!XmlExtension::version_output_matches(b"xml-lsp\n"));
        assert!(!XmlExtension::version_output_matches(b""));
    }

    #[test]
    fn wraps_lsp_settings_in_the_xml_section() {
        use zed::serde_json::json;
        assert_eq!(XmlExtension::workspace_configuration(None), None);
        assert_eq!(
            XmlExtension::workspace_configuration(Some(Value::Null)),
            None
        );
        assert_eq!(
            XmlExtension::workspace_configuration(Some(json!({"format": {"enabled": false}}))),
            Some(json!({"xml": {"format": {"enabled": false}}}))
        );
        assert_eq!(
            XmlExtension::workspace_configuration(Some(json!({"xml": {"catalogs": ["c.xml"]}}))),
            Some(json!({"xml": {"catalogs": ["c.xml"]}}))
        );
    }

    #[test]
    fn prefers_explicit_initialization_options() {
        use zed::serde_json::json;
        assert_eq!(XmlExtension::initialization_options(None, None), None);
        assert_eq!(
            XmlExtension::initialization_options(
                Some(json!({"xml": {"validation": {"enabled": false}}})),
                Some(json!({"format": {"enabled": false}})),
            ),
            Some(json!({"xml": {"validation": {"enabled": false}}}))
        );
        assert_eq!(
            XmlExtension::initialization_options(
                Some(Value::Null),
                Some(json!({"format": {"enabled": false}}))
            ),
            Some(json!({"settings": {"xml": {"format": {"enabled": false}}}}))
        );
    }

    #[test]
    fn recognizes_windows_file_in_use_errors() {
        assert!("The process cannot access the file (os error 32)".contains("os error 32"));
        assert!(!"download failed with status 404".contains("os error 32"));
    }
}

zed::register_extension!(XmlExtension);
