use zed_extension_api as zed;

const DOWNLOADED_JAR: &str = "lemminx.jar";
const DEFAULT_LEMMINX_URL: &str = "https://repo.eclipse.org/content/repositories/lemminx-releases/org/eclipse/lemminx/org.eclipse.lemminx/0.31.2/org.eclipse.lemminx-0.31.2-uber.jar";

struct XmlExtension;

impl XmlExtension {
    fn environment(worktree: &zed::Worktree, name: &str) -> Option<String> {
        worktree
            .shell_env()
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn java_command(&self, worktree: &zed::Worktree, jar: String) -> zed::Result<zed::Command> {
        let java = worktree.which("java").ok_or_else(|| {
            "LemMinX requires Java 17+; `java` was not found on PATH.".to_string()
        })?;

        Ok(zed::Command {
            command: java,
            args: vec!["-jar".to_string(), jar],
            env: Vec::new(),
        })
    }

    fn download_jar(
        &self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<String> {
        let url = Self::environment(worktree, "LEMMINX_DOWNLOAD_URL")
            .unwrap_or_else(|| DEFAULT_LEMMINX_URL.to_string());

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::Downloading,
        );
        let result =
            zed::download_file(&url, DOWNLOADED_JAR, zed::DownloadedFileType::Uncompressed);

        match result {
            Ok(()) => Ok(DOWNLOADED_JAR.to_string()),
            Err(error) => {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::Failed(error.clone()),
                );
                Err(format!("Could not download LemMinX from {url}: {error}"))
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
        if language_server_id.as_ref() != "lemminx" {
            return Err(format!(
                "Unsupported XML language server: {language_server_id}"
            ));
        }

        if let Some(lemminx) = worktree.which("lemminx") {
            return Ok(zed::Command {
                command: lemminx,
                args: Vec::new(),
                env: Vec::new(),
            });
        }

        if let Some(jar) = Self::environment(worktree, "LEMMINX_JAR") {
            return self.java_command(worktree, jar);
        }

        let jar = self.download_jar(language_server_id, worktree)?;
        self.java_command(worktree, jar)
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> zed::Result<Option<zed::serde_json::Value>> {
        if language_server_id.as_ref() != "lemminx" {
            return Ok(None);
        }

        Ok(Some(zed::serde_json::json!({
            "xml": {
                "format": {
                    "enabled": true,
                    "splitAttributes": true,
                    "joinContentLines": false,
                    "joinCommentLines": false
                },
                "validation": {
                    "enabled": true
                },
                "completion": {
                    "autoCloseTags": true
                }
            }
        })))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> zed::Result<Option<zed::serde_json::Value>> {
        if language_server_id.as_ref() != "lemminx" {
            return Ok(None);
        }

        Ok(Some(zed::serde_json::json!({
            "xml": {
                "format": {
                    "enabled": true,
                    "splitAttributes": true
                },
                "validation": {
                    "enabled": true
                },
                "catalogs": []
            }
        })))
    }
}

zed::register_extension!(XmlExtension);
