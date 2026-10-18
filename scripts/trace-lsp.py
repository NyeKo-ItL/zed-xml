#!/usr/bin/env python3
"""Relay between Zed and xml-lsp that logs every LSP message (method, id, size,
time) to $XML_LSP_TRACE (default /tmp/xml-lsp-trace.log).

Usage in Zed's settings.json:
  "lsp": { "xml-lsp": { "binary": { "path": "/path/to/scripts/trace-lsp.py" } } }
The server it starts is $XML_LSP_SERVER, else the cached binary of the
extension (extensions/work/xml-tools/xml-lsp-<version>).
"""
import glob, json, os, subprocess, sys, threading, time

LOG = os.environ.get("XML_LSP_TRACE", "/tmp/xml-lsp-trace.log")
START = time.time()
LOCK = threading.Lock()


def log(line):
    with LOCK, open(LOG, "a") as out:
        out.write("%8.3f %s\n" % (time.time() - START, line))


def server_path():
    if os.environ.get("XML_LSP_SERVER"):
        return os.environ["XML_LSP_SERVER"]
    pattern = os.path.expanduser(
        "~/Library/Application Support/Zed/extensions/work/xml-tools/xml-lsp-*"
    )
    found = sorted(
        path for path in glob.glob(pattern) if not path.endswith(".sha256")
    )
    if not found:
        sys.exit("trace-lsp: no xml-lsp binary found; set XML_LSP_SERVER")
    return found[-1]


def describe(body):
    try:
        message = json.loads(body)
    except ValueError:
        return "invalid JSON"
    kind = "response" if "method" not in message else (
        "request" if "id" in message else "notification")
    text = "%s %s id=%s" % (kind, message.get("method", ""), message.get("id"))
    if "error" in message:
        text += " ERROR %s" % json.dumps(message["error"])[:300]
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
                log("%s closed" % label)
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
    log("starting %s" % command)
    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    threading.Thread(
        target=pump, args=(sys.stdin.buffer, child.stdin, "zed->srv"), daemon=True
    ).start()
    pump(child.stdout, sys.stdout.buffer, "srv->zed")
    log("server exited with %s" % child.wait())


main()
