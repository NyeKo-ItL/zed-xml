#!/usr/bin/env python3
"""Relay between Zed and xml-lsp that logs every LSP message (method, id, size,
time) to $XML_LSP_TRACE (default /tmp/xml-lsp-trace.log).

Usage in Zed's settings.json:
  "lsp": { "xml-lsp": { "binary": { "path": "/path/to/scripts/trace-lsp.py" } } }
The server it starts is $XML_LSP_SERVER, else a local Cargo build, then a
binary cached by the extension.
"""
import glob
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

LOG = os.environ.get("XML_LSP_TRACE", "/tmp/xml-lsp-trace.log")
START = time.time()
LOCK = threading.Lock()


def log(line):
    with LOCK, open(LOG, "a") as out:
        out.write(f"{time.time() - START:8.3f} {line}\n")


def server_path():
    explicit = os.environ.get("XML_LSP_SERVER")
    if explicit:
        return explicit

    repository = Path(__file__).resolve().parents[1]
    candidates = [
        repository / "target" / "debug" / "xml-lsp",
        repository / "target" / "release" / "xml-lsp",
        repository / "target" / "local" / "release" / "xml-lsp",
    ]

    data_dir = os.environ.get("ZED_DATA_DIR")
    if data_dir:
        zed_data = Path(data_dir).expanduser()
    elif sys.platform == "darwin":
        zed_data = Path.home() / "Library" / "Application Support" / "Zed"
    elif sys.platform == "win32":
        zed_data = Path(os.environ.get("LOCALAPPDATA", "")) / "Zed"
    else:
        zed_data = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "zed"

    for extension_id in ("xml-tools", "xml"):
        candidates.extend(
            Path(path)
            for path in glob.glob(
                str(zed_data / "extensions" / "work" / extension_id / "xml-lsp-*")
            )
            if not path.endswith((".sha256", ".exe"))
        )
        candidates.extend(
            Path(path)
            for path in glob.glob(
                str(zed_data / "extensions" / "work" / extension_id / "xml-lsp-*.exe")
            )
            if not path.endswith(".sha256")
        )

    executable = [path for path in candidates if path.is_file() and os.access(path, os.X_OK)]
    if not executable:
        sys.exit(
            "trace-lsp: no xml-lsp binary found; build it with "
            "'cargo build -p xml-lsp' or set XML_LSP_SERVER"
        )
    return str(max(executable, key=lambda path: path.stat().st_mtime))


def describe(body):
    try:
        message = json.loads(body)
    except ValueError:
        return "invalid JSON"
    kind = "response" if "method" not in message else (
        "request" if "id" in message else "notification")
    text = "{} {} id={}".format(kind, message.get("method", ""), message.get("id"))
    if "error" in message:
        text += " ERROR {}".format(json.dumps(message["error"])[:300])
    result = message.get("result")
    if isinstance(result, list):
        text += " result=list[%d]" % len(result)
    return text


def pump(source, sink, label):
    while True:
        length = None
        header = b""
        while True:
            line = source.readline()
            if not line:
                log(f"{label} closed")
                try:
                    sink.close()
                except OSError:
                    pass
                return
            header += line
            if line in (b"\r\n", b"\n"):
                break
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":")[1])
        body = source.read(length) if length else b""
        log("%s %7d bytes %s" % (label, len(body), describe(body)))
        sink.write(header + body)
        sink.flush()


def main():
    command = [server_path()] + sys.argv[1:]
    log(f"starting {command}")
    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    threading.Thread(
        target=pump, args=(sys.stdin.buffer, child.stdin, "zed->srv"), daemon=True
    ).start()
    pump(child.stdout, sys.stdout.buffer, "srv->zed")
    log(f"server exited with {child.wait()}")


main()
