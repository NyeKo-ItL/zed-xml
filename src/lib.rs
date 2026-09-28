use zed_extension_api as zed;

const LANGUAGE_SERVER_ID: &str = "xml-lsp";
const XML_LSP_PATH_ENV: &str = "XML_LSP_PATH";

struct XmlExtension;

impl XmlExtension {
    fn environment(worktree: &zed::Worktree, name: &str) -> Option<String> {
        worktree
            .shell_env()
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn native_command(worktree: &zed::Worktree) -> zed::Result<zed::Command> {
        if let Some(path) = Self::environment(worktree, XML_LSP_PATH_ENV) {
            return Ok(zed::Command {
                command: path,
                args: vec!["--stdio".to_owned()],
                env: Vec::new(),
            });
        }

        if let Some(binary) = worktree.which(LANGUAGE_SERVER_ID) {
            return Ok(zed::Command {
                command: binary,
                args: vec!["--stdio".to_owned()],
                env: Vec::new(),
            });
        }

        if let Some(cargo) = worktree.which("cargo") {
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

        Err(format!(
            "{LANGUAGE_SERVER_ID} was not found. Set {XML_LSP_PATH_ENV} or install it on PATH."
        ))
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

        Self::native_command(worktree)
    }
}

zed::register_extension!(XmlExtension);
