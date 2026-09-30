mod sha256;

use std::fs;

use zed_extension_api::{
    self as zed,
    serde_json::Value,
    settings::{CommandSettings, LspSettings},
};

const LANGUAGE_SERVER_ID: &str = "xml-lsp";
const EXPECTED_LSP_VERSION: &str = env!("CARGO_PKG_VERSION");
const XML_LSP_PATH_ENV: &str = "XML_LSP_PATH";
const XML_LSP_DOWNLOAD_URL_ENV: &str = "XML_LSP_DOWNLOAD_URL";
const XML_LSP_DOWNLOAD_SHA256_ENV: &str = "XML_LSP_DOWNLOAD_SHA256";
const RELEASE_REPOSITORY: &str = "NyeKo-ItL/zed-xml";

/// A platform with a prebuilt `xml-lsp` release asset.
struct Platform {
    os: zed::Os,
    architecture: zed::Architecture,
    asset: &'static str,
    label: &'static str,
}

/// Platforms the extension downloads a binary for. Linux uses the static
/// musl builds, which run on any distribution regardless of its glibc
/// version. Keep in sync with the `native` matrix of `.github/workflows/ci.yml`.
const SUPPORTED_PLATFORMS: &[Platform] = &[
    Platform {
        os: zed::Os::Linux,
        architecture: zed::Architecture::X8664,
        asset: "xml-lsp-x86_64-unknown-linux-musl",
        label: "Linux x86_64",
    },
    Platform {
        os: zed::Os::Linux,
        architecture: zed::Architecture::Aarch64,
        asset: "xml-lsp-aarch64-unknown-linux-musl",
        label: "Linux aarch64",
    },
    Platform {
        os: zed::Os::Mac,
        architecture: zed::Architecture::X8664,
        asset: "xml-lsp-x86_64-apple-darwin",
        label: "macOS x86_64",
    },
    Platform {
        os: zed::Os::Mac,
        architecture: zed::Architecture::Aarch64,
        asset: "xml-lsp-aarch64-apple-darwin",
        label: "macOS aarch64",
    },
    Platform {
        os: zed::Os::Windows,
        architecture: zed::Architecture::X8664,
        asset: "xml-lsp-x86_64-pc-windows-msvc.exe",
        label: "Windows x86_64",
    },
    Platform {
        os: zed::Os::Windows,
        architecture: zed::Architecture::Aarch64,
        asset: "xml-lsp-aarch64-pc-windows-msvc.exe",
        label: "Windows aarch64",
    },
];

fn os_name(os: zed::Os) -> &'static str {
    match os {
        zed::Os::Mac => "macOS",
        zed::Os::Linux => "Linux",
        zed::Os::Windows => "Windows",
    }
}

fn architecture_name(architecture: zed::Architecture) -> &'static str {
    match architecture {
        zed::Architecture::Aarch64 => "aarch64",
        zed::Architecture::X86 => "x86",
        zed::Architecture::X8664 => "x86_64",
    }
}

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
        let binary = Self::lsp_settings(worktree).binary;
        let configured_path = binary
            .as_ref()
            .and_then(|binary| binary.path.clone())
            .filter(|path| !path.trim().is_empty());
        let mut command = if let Some(path) = configured_path {
            Self::command(path)
        } else if let Some(path) = Self::environment(worktree, XML_LSP_PATH_ENV) {
            Self::command(path)
        } else {
            Self::downloaded_command(language_server_id, worktree)?
        };
        Self::apply_binary_settings(&mut command, binary);
        Ok(command)
    }

    /// Applies `lsp.xml-lsp.binary.arguments` (replacing the default
    /// `--stdio`) and `lsp.xml-lsp.binary.env`, whichever binary is started.
    fn apply_binary_settings(command: &mut zed::Command, binary: Option<CommandSettings>) {
        let Some(binary) = binary else {
            return;
        };
        if let Some(arguments) = binary.arguments {
            command.args = arguments;
        }
        if let Some(env) = binary.env {
            let mut env = env.into_iter().collect::<Vec<_>>();
            env.sort();
            command.env.extend(env);
        }
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
        SUPPORTED_PLATFORMS
            .iter()
            .find(|platform| platform.os == os && platform.architecture == architecture)
            .map(|platform| platform.asset)
            .ok_or_else(|| Self::unsupported_platform_message(os, architecture))
    }

    fn unsupported_platform_message(os: zed::Os, architecture: zed::Architecture) -> String {
        let supported = SUPPORTED_PLATFORMS
            .iter()
            .map(|platform| platform.label)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "xml-lsp has no prebuilt binary for {} {}. Prebuilt binaries exist for: {supported}. \
             Build it from source (cargo build -p xml-lsp --release in a clone of \
             https://github.com/{RELEASE_REPOSITORY}) and set {XML_LSP_PATH_ENV} to the binary.",
            os_name(os),
            architecture_name(architecture),
        )
    }

    fn executable_name(os: zed::Os) -> String {
        let extension = if matches!(os, zed::Os::Windows) {
            ".exe"
        } else {
            ""
        };
        format!("{LANGUAGE_SERVER_ID}-{EXPECTED_LSP_VERSION}{extension}")
    }

    /// File next to the cached binary recording the checksum it was verified
    /// against.
    fn checksum_file_name(executable: &str) -> String {
        format!("{executable}.sha256")
    }

    fn release_url(asset_name: &str) -> String {
        format!(
            "https://github.com/{RELEASE_REPOSITORY}/releases/download/v{EXPECTED_LSP_VERSION}/{asset_name}"
        )
    }

    /// Expected SHA-256 of `asset_name` from the content of a `.sha256` file
    /// (`<hex>`, `<hex>  <name>` or `<hex> *<name>`) or of a `SHA256SUMS` file
    /// (one such line per asset).
    fn parse_checksum(content: &str, asset_name: &str) -> zed::Result<String> {
        let mut unnamed = None;
        for line in content.lines() {
            let mut fields = line.split_whitespace();
            let Some(hash) = fields.next() else {
                continue;
            };
            let name = fields.next().map(|name| name.trim_start_matches('*'));
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            match name {
                Some(name) if name == asset_name => return Ok(hash.to_ascii_lowercase()),
                None if unnamed.is_none() => unnamed = Some(hash.to_ascii_lowercase()),
                _ => {}
            }
        }
        unnamed.ok_or_else(|| format!("no SHA-256 checksum for {asset_name} in the checksum file"))
    }

    fn verify_checksum(data: &[u8], expected: &str) -> zed::Result<()> {
        let actual = sha256::hex_digest(data);
        if actual.eq_ignore_ascii_case(expected) {
            Ok(())
        } else {
            Err(format!(
                "SHA-256 mismatch: expected {expected}, downloaded file has {actual}"
            ))
        }
    }

    fn verify_file(path: &str, expected: &str) -> zed::Result<()> {
        let data = fs::read(path).map_err(|error| format!("could not read {path}: {error}"))?;
        Self::verify_checksum(&data, expected)
    }

    /// The cached binary is reused only when it reports the expected version
    /// and still matches the checksum recorded when it was downloaded.
    fn cached_binary_is_valid(executable: &str, asset_name: &str) -> bool {
        let Ok(recorded) = fs::read_to_string(Self::checksum_file_name(executable)) else {
            return false;
        };
        Self::parse_checksum(&recorded, asset_name)
            .and_then(|expected| Self::verify_file(executable, &expected))
            .is_ok()
            && Self::installed_version_matches(executable)
    }

    /// Expected checksum of the binary: `XML_LSP_DOWNLOAD_SHA256` when set,
    /// otherwise the `.sha256` file published next to the binary.
    fn expected_checksum(
        worktree: &zed::Worktree,
        download_url: &str,
        asset_name: &str,
        checksum_path: &str,
    ) -> zed::Result<String> {
        if let Some(checksum) = Self::environment(worktree, XML_LSP_DOWNLOAD_SHA256_ENV) {
            return Self::parse_checksum(&checksum, asset_name)
                .map_err(|_| format!("{XML_LSP_DOWNLOAD_SHA256_ENV} is not a SHA-256 checksum"));
        }
        let checksum_url = format!("{download_url}.sha256");
        zed::download_file(
            &checksum_url,
            checksum_path,
            zed::DownloadedFileType::Uncompressed,
        )
        .map_err(|error| format!("could not download the checksum {checksum_url}: {error}"))?;
        let content = fs::read_to_string(checksum_path)
            .map_err(|error| format!("could not read {checksum_path}: {error}"))?;
        Self::parse_checksum(&content, asset_name)
    }

    fn downloaded_command(
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let (os, architecture) = zed::current_platform();
        let asset_name = Self::release_asset(os, architecture)?;
        let executable = Self::executable_name(os);
        let checksum_path = Self::checksum_file_name(&executable);
        let override_url = Self::environment(worktree, XML_LSP_DOWNLOAD_URL_ENV);

        if Self::cached_binary_is_valid(&executable, asset_name) {
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
            // The extension host does not create parent directories for
            // downloads: everything lands in the extension work directory.
            let download_url = override_url.unwrap_or_else(|| Self::release_url(asset_name));
            // Remove a checksum recorded for a previous binary first, so that an
            // interrupted download is never considered verified.
            let _ = fs::remove_file(&checksum_path);
            let expected =
                Self::expected_checksum(worktree, &download_url, asset_name, &checksum_path)?;

            if let Err(error) = zed::download_file(
                &download_url,
                &executable,
                zed::DownloadedFileType::Uncompressed,
            ) {
                // A second Zed window may try to refresh the same binary while the
                // first LSP process has it open. Reuse it only when it is already the
                // expected binary; never hide an error for an outdated one.
                if !(error.contains("os error 32")
                    && Self::verify_file(&executable, &expected).is_ok()
                    && Self::installed_version_matches(&executable))
                {
                    return Err(error);
                }
            }
            if let Err(error) = Self::verify_file(&executable, &expected) {
                let _ = fs::remove_file(&executable);
                return Err(format!("Downloaded xml-lsp rejected: {error}"));
            }
            fs::write(&checksum_path, format!("{expected}  {asset_name}\n"))
                .map_err(|error| format!("could not write {checksum_path}: {error}"))?;
            if !matches!(os, zed::Os::Windows) {
                zed::make_file_executable(&executable)?;
            }
            if let Err(error) = Self::check_installed_version(&executable) {
                return Err(format!("Downloaded xml-lsp {error}"));
            }
            Ok(executable.clone())
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
                    "Could not install xml-lsp asset {asset_name}: {error}. Set {XML_LSP_PATH_ENV} to a local binary."
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
    fn maps_every_supported_platform_to_its_release_asset() {
        let cases = [
            (
                zed::Os::Linux,
                zed::Architecture::X8664,
                "xml-lsp-x86_64-unknown-linux-musl",
            ),
            (
                zed::Os::Linux,
                zed::Architecture::Aarch64,
                "xml-lsp-aarch64-unknown-linux-musl",
            ),
            (
                zed::Os::Mac,
                zed::Architecture::X8664,
                "xml-lsp-x86_64-apple-darwin",
            ),
            (
                zed::Os::Mac,
                zed::Architecture::Aarch64,
                "xml-lsp-aarch64-apple-darwin",
            ),
            (
                zed::Os::Windows,
                zed::Architecture::X8664,
                "xml-lsp-x86_64-pc-windows-msvc.exe",
            ),
            (
                zed::Os::Windows,
                zed::Architecture::Aarch64,
                "xml-lsp-aarch64-pc-windows-msvc.exe",
            ),
        ];
        for (os, architecture, asset) in cases {
            assert_eq!(XmlExtension::release_asset(os, architecture), Ok(asset));
        }
        assert_eq!(cases.len(), SUPPORTED_PLATFORMS.len());
    }

    #[test]
    fn the_release_workflow_builds_every_supported_platform() {
        let workflow = include_str!("../.github/workflows/ci.yml");
        for platform in SUPPORTED_PLATFORMS {
            let target = platform
                .asset
                .trim_start_matches("xml-lsp-")
                .trim_end_matches(".exe");
            assert!(
                workflow.contains(&format!("target: {target}\n")),
                "the native release matrix does not build {target}"
            );
        }
    }

    #[test]
    fn names_the_cached_executable_after_the_version() {
        assert_eq!(
            XmlExtension::executable_name(zed::Os::Windows),
            format!("xml-lsp-{EXPECTED_LSP_VERSION}.exe")
        );
        assert_eq!(
            XmlExtension::executable_name(zed::Os::Linux),
            format!("xml-lsp-{EXPECTED_LSP_VERSION}")
        );
        assert_eq!(
            XmlExtension::executable_name(zed::Os::Mac),
            format!("xml-lsp-{EXPECTED_LSP_VERSION}")
        );
        assert_eq!(
            XmlExtension::checksum_file_name("xml-lsp-1.0.0.exe"),
            "xml-lsp-1.0.0.exe.sha256"
        );
    }

    #[test]
    fn downloads_from_the_release_of_the_extension_version() {
        assert_eq!(
            XmlExtension::release_url("xml-lsp-x86_64-apple-darwin"),
            format!(
                "https://github.com/NyeKo-ItL/zed-xml/releases/download/v{EXPECTED_LSP_VERSION}/xml-lsp-x86_64-apple-darwin"
            )
        );
    }

    #[test]
    fn rejects_unsupported_platforms_listing_the_supported_ones() {
        for os in [zed::Os::Linux, zed::Os::Mac, zed::Os::Windows] {
            let error = XmlExtension::release_asset(os, zed::Architecture::X86).unwrap_err();
            assert!(error.contains(&format!("{} x86.", os_name(os))), "{error}");
            for platform in SUPPORTED_PLATFORMS {
                assert!(error.contains(platform.label), "{error}");
            }
            assert!(error.contains("XML_LSP_PATH"), "{error}");
        }
    }

    const HASH: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn parses_checksum_files() {
        let asset = "xml-lsp-x86_64-apple-darwin";
        // sha256sum output, binary mode marker, bare hash, uppercase.
        for content in [
            format!("{HASH}  {asset}\n"),
            format!("{HASH} *{asset}\r\n"),
            format!("{HASH}\n"),
            format!("{}  {asset}", HASH.to_uppercase()),
        ] {
            assert_eq!(
                XmlExtension::parse_checksum(&content, asset),
                Ok(HASH.to_owned())
            );
        }
        // SHA256SUMS: the line of the asset, not the first one.
        let other = "0".repeat(64);
        let sums = format!("{other}  xml-lsp-aarch64-apple-darwin\n{HASH}  {asset}\n");
        assert_eq!(
            XmlExtension::parse_checksum(&sums, asset),
            Ok(HASH.to_owned())
        );
    }

    #[test]
    fn rejects_missing_or_malformed_checksums() {
        let asset = "xml-lsp-x86_64-apple-darwin";
        for content in [
            String::new(),
            "not a checksum\n".to_owned(),
            format!("{}  {asset}\n", &HASH[..63]),
            format!("{}g  {asset}\n", &HASH[..63]),
            format!("{HASH}  xml-lsp-aarch64-apple-darwin\n"),
            "<html>Not Found</html>".to_owned(),
        ] {
            assert!(
                XmlExtension::parse_checksum(&content, asset).is_err(),
                "{content:?}"
            );
        }
    }

    #[test]
    fn verifies_downloaded_bytes_against_the_checksum() {
        assert_eq!(XmlExtension::verify_checksum(b"abc", HASH), Ok(()));
        assert_eq!(
            XmlExtension::verify_checksum(b"abc", &HASH.to_uppercase()),
            Ok(())
        );
        let error = XmlExtension::verify_checksum(b"abd", HASH).unwrap_err();
        assert!(error.contains("SHA-256 mismatch"), "{error}");
        assert!(error.contains(HASH), "{error}");
    }

    #[test]
    fn verifies_files_on_disk() {
        let directory =
            std::env::temp_dir().join(format!("zed-xml-extension-checksum-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("xml-lsp");
        fs::write(&file, b"abc").unwrap();
        let path = file.to_string_lossy();
        assert_eq!(XmlExtension::verify_file(&path, HASH), Ok(()));
        fs::write(&file, b"abc\n").unwrap();
        assert!(XmlExtension::verify_file(&path, HASH).is_err());
        let missing = directory.join("missing").to_string_lossy().into_owned();
        assert!(XmlExtension::verify_file(&missing, HASH).is_err());
        let _ = fs::remove_dir_all(&directory);
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

    /// The `[[capabilities]]` entries of `extension.toml` of the given kind,
    /// as `key = value` lines.
    fn manifest_capabilities(kind: &str) -> Vec<Vec<(&'static str, &'static str)>> {
        let manifest = include_str!("../extension.toml");
        manifest
            .split("[[capabilities]]")
            .skip(1)
            .map(|block| {
                block
                    .lines()
                    .take_while(|line| !line.trim_start().starts_with('['))
                    .filter_map(|line| line.split_once('='))
                    .map(|(key, value)| (key.trim(), value.trim()))
                    .collect::<Vec<_>>()
            })
            .filter(|entries| entries.contains(&("kind", &*format!("\"{kind}\""))))
            .collect()
    }

    /// The strings of a TOML array of strings such as `["a", "b"]`.
    fn toml_strings(value: &str) -> Vec<&str> {
        value.split('"').skip(1).step_by(2).collect()
    }

    /// Mirrors Zed's `DownloadFileCapability::allows`.
    fn download_allowed(host: &str, path: &[&str], url: &str) -> bool {
        let Some(rest) = url.strip_prefix("https://") else {
            return false;
        };
        let (url_host, url_path) = rest.split_once('/').unwrap_or((rest, ""));
        if host != "*" && host != url_host {
            return false;
        }
        let segments = url_path.split('/').collect::<Vec<_>>();
        for (index, segment) in path.iter().enumerate() {
            if *segment == "**" {
                return true;
            }
            if index >= segments.len() || (*segment != "*" && *segment != segments[index]) {
                return false;
            }
        }
        path.len() >= segments.len()
    }

    #[test]
    fn the_manifest_declares_the_version_of_the_extension() {
        let manifest = include_str!("../extension.toml");
        assert!(manifest.contains(&format!("\nversion = \"{EXPECTED_LSP_VERSION}\"\n")));
        assert!(manifest.contains("\nschema_version = 1\n"));
        assert!(manifest.contains(&format!(
            "\nrepository = \"https://github.com/{RELEASE_REPOSITORY}\"\n"
        )));
    }

    #[test]
    fn the_manifest_allows_running_the_cached_binaries() {
        let capabilities = manifest_capabilities("process:exec");
        for os in [zed::Os::Linux, zed::Os::Windows] {
            let executable = format!("\"{}\"", XmlExtension::executable_name(os));
            assert!(
                capabilities.iter().any(|entries| {
                    entries.contains(&("command", executable.as_str()))
                        && entries.contains(&("args", "[\"--version\"]"))
                }),
                "no process:exec capability for {executable} --version"
            );
        }
    }

    #[test]
    fn the_manifest_allows_downloading_the_release_assets() {
        let capabilities = manifest_capabilities("download_file");
        assert_eq!(capabilities.len(), 1, "{capabilities:?}");
        let entries = &capabilities[0];
        let value = |key: &str| {
            entries
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| *value)
                .unwrap_or_default()
        };
        let host = value("host").trim_matches('"');
        let path = toml_strings(value("path"));
        for platform in SUPPORTED_PLATFORMS {
            let url = XmlExtension::release_url(platform.asset);
            assert!(download_allowed(host, &path, &url), "{url} is not allowed");
            let checksum = format!("{url}.sha256");
            assert!(download_allowed(host, &path, &checksum), "{checksum}");
        }
        // Scoped to this repository and version.
        assert!(!download_allowed(
            host,
            &path,
            "https://github.com/NyeKo-ItL/zed-xml/releases/download/v0.0.0/xml-lsp"
        ));
        assert!(!download_allowed(
            host,
            &path,
            "https://example.com/NyeKo-ItL/zed-xml/releases/download/v0.0.0/xml-lsp"
        ));
    }

    #[test]
    fn applies_the_binary_settings() {
        let mut command = XmlExtension::command("xml-lsp".to_owned());
        XmlExtension::apply_binary_settings(&mut command, None);
        assert_eq!(command.args, ["--stdio"]);

        XmlExtension::apply_binary_settings(
            &mut command,
            Some(CommandSettings {
                path: None,
                arguments: None,
                env: Some(
                    [("B", "2"), ("A", "1")]
                        .into_iter()
                        .map(|(key, value)| (key.to_owned(), value.to_owned()))
                        .collect(),
                ),
            }),
        );
        assert_eq!(command.args, ["--stdio"]);
        assert_eq!(
            command.env,
            [
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "2".to_owned())
            ]
        );

        XmlExtension::apply_binary_settings(
            &mut command,
            Some(CommandSettings {
                path: Some("/opt/xml-lsp".to_owned()),
                arguments: Some(vec!["--stdio".to_owned(), "--verbose".to_owned()]),
                env: None,
            }),
        );
        assert_eq!(command.args, ["--stdio", "--verbose"]);
    }

    #[test]
    fn recognizes_windows_file_in_use_errors() {
        assert!("The process cannot access the file (os error 32)".contains("os error 32"));
        assert!(!"download failed with status 404".contains("os error 32"));
    }
}

zed::register_extension!(XmlExtension);
