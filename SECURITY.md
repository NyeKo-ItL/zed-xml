# Security policy

## Supported versions

Security fixes are released for the latest version of the extension and of `xml-lsp` (they always share the same version). Please update before reporting.

## Reporting a vulnerability

Please do not open a public issue for a vulnerability. Report it privately through GitHub: on the repository page, open **Security** > **Advisories** > **Report a vulnerability** ([private vulnerability reporting](https://github.com/NyeKo-ItL/zed-xml/security/advisories/new)).

Include, when possible:

- the version (`xml-lsp --version`, or the extension version shown in Zed) and the platform;
- a minimal document, schema, DTD or catalog that triggers the problem, and the settings used;
- what happens (crash, hang, memory growth, file read or network access that should not happen) and what you expected.

You should get an acknowledgement within a week. Once the problem is confirmed, a fix is prepared in a private fork of the advisory, released, and the advisory is published with credit to the reporter unless you prefer otherwise.

## Scope

In scope: anything a document, schema, DTD or catalog opened in the editor can make the extension or `xml-lsp` do beyond what the [security model](docs/configuration.md#security-model) allows, for example reading non-regular or network files, accessing the network, expanding entities without bound, or crashing or hanging the server (stack overflow, unbounded memory or time).

Out of scope: the behaviour of Zed itself, and settings that a user deliberately points at a file (a local schema path in `xml.fileAssociations` or `xml.catalogs` is read as configured).
