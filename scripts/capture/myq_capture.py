"""mitmproxy addon: log the myQ app's API traffic with secrets removed.

Loaded by scripts/capture/capture.sh. Writes one JSON object per flow to the
file named by MYQ_CAPTURE_OUT (0600). No raw flow is written anywhere, so a
password or token typed into the app never reaches the disk.

Keeps what the API contract needs: method, host, path, parameter and header
names, JSON structure, status codes, and values that are constants of the app.
Replaces: anything whose key names a credential or identity, anything shaped
like a token, every cookie value, and every value in an HTML document.
"""

import json
import os
import re
import time
from urllib.parse import parse_qsl

SENSITIVE_KEY = re.compile(
    r"pass(word|wd|code)?$|pwd|token|secret|authorization|cookie|signature|assertion"
    r"|^code$|code_?verifier|code_?challenge$|^state$|nonce|session|csrf|otp|mfa_?code"
    r"|e-?mail|user_?name|^login|phone|first_?name|last_?name|full_?name|^name$"
    r"|address|street|city|zip|postal|latitude|longitude|^lat$|^lon$|^lng$|location"
    r"|serial|account_?id|user_?id|device_?id|installation_?id|fcm|push_?id|imei|^mac$",
    re.IGNORECASE,
)
JWT = re.compile(r"^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")
BEARER = re.compile(r"^(bearer|basic)\s", re.IGNORECASE)
OPAQUE = re.compile(r"^[A-Za-z0-9+/_=.-]{40,}$")
MAX_TEXT = 400


def redact_value(key, value):
    """Return value with credentials and identity removed, keeping its type."""
    if isinstance(value, dict):
        return {k: redact_value(k, v) for k, v in value.items()}
    if isinstance(value, list):
        return [redact_value(key, v) for v in value]
    if value is None or isinstance(value, bool):
        return value
    text = str(value)
    if key and SENSITIVE_KEY.search(str(key)):
        return f"<redacted:{type(value).__name__}:len={len(text)}>"
    if isinstance(value, (int, float)):
        return value
    if JWT.match(text) or BEARER.match(text):
        return f"<redacted:token:len={len(text)}>"
    if OPAQUE.match(text):
        return f"<redacted:opaque:len={len(text)}>"
    if len(text) > MAX_TEXT:
        return f"<truncated:len={len(text)}>{text[:80]}"
    return value


def redact_headers(headers):
    """Header names survive; cookie headers keep only the cookie names."""
    out = {}
    for name, value in headers:
        lower = name.lower()
        if lower == "cookie":
            out[name] = [c.split("=", 1)[0].strip() for c in value.split(";")]
        elif lower == "set-cookie":
            out.setdefault(name, []).append(value.split("=", 1)[0].strip())
        else:
            out[name] = redact_value(name, value)
    return out


_TAG = re.compile(r"<(form|/form|input|select|textarea|button)\b([^>]*)>", re.IGNORECASE)
_ATTR = re.compile(r"""([a-zA-Z_:-]+)\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)""")
_TITLE = re.compile(r"<title[^>]*>(.*?)</title>", re.IGNORECASE | re.DOTALL)


def _attrs(text):
    return {k.lower(): v.strip("\"'") for k, v in _ATTR.findall(text)}


def html_shape(text):
    """A page's title, form targets and input names; every value is dropped.

    A regular expression rather than html.parser: the mitmproxy binary bundles
    its own Python, and that module is not in it.
    """
    title = _TITLE.search(text)
    forms = []
    for tag, rest in _TAG.findall(text):
        tag = tag.lower()
        a = _attrs(rest)
        if tag == "form":
            forms.append({"method": a.get("method"), "action": a.get("action", "").split("?")[0], "inputs": []})
        elif tag != "/form" and forms:
            forms[-1]["inputs"].append({"tag": tag, "name": a.get("name"), "type": a.get("type"), "id": a.get("id")})
    return {"title": title.group(1).strip() if title else "", "forms": forms, "length": len(text)}


def body_shape(content_type, text):
    """Reduce a body to its redacted structure, by what it actually parses as."""
    if not text:
        return None
    ctype = (content_type or "").lower()
    stripped = text.lstrip()
    if "json" in ctype or stripped[:1] in ("{", "["):
        try:
            return {"json": redact_value(None, json.loads(text))}
        except ValueError:
            pass
    if "x-www-form-urlencoded" in ctype:
        pairs = parse_qsl(text, keep_blank_values=True)
        return {"form": [[k, redact_value(k, v)] for k, v in pairs]}
    if "html" in ctype or stripped[:1] == "<":
        return {"html": html_shape(text)}
    return {"text_length": len(text), "content_type": ctype}


def flow_record(method, host, path, query, req_headers, req_ctype, req_text, status, resp_headers, resp_ctype, resp_text):
    return {
        "ts": round(time.time(), 3),
        "method": method,
        "host": host,
        "path": path,
        "query": [[k, redact_value(k, v)] for k, v in query],
        "request_headers": redact_headers(req_headers),
        "request_body": body_shape(req_ctype, req_text),
        "status": status,
        "response_headers": redact_headers(resp_headers),
        "response_body": body_shape(resp_ctype, resp_text),
    }


class Capture:
    def __init__(self):
        path = os.environ["MYQ_CAPTURE_OUT"]
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        self.out = os.fdopen(fd, "a", buffering=1)

    def _write(self, record):
        self.out.write(json.dumps(record) + "\n")

    def response(self, flow):
        req, resp = flow.request, flow.response
        self._write(
            flow_record(
                req.method,
                req.pretty_host,
                req.path.split("?")[0],
                list(req.query.items(multi=True)),
                req.headers.items(multi=True),
                req.headers.get("content-type"),
                req.get_text(strict=False),
                resp.status_code,
                resp.headers.items(multi=True),
                resp.headers.get("content-type"),
                resp.get_text(strict=False),
            )
        )

    def error(self, flow):
        self._write({"ts": round(time.time(), 3), "event": "flow_error", "host": flow.request.pretty_host, "path": flow.request.path.split("?")[0], "error": str(flow.error)})

    def tls_failed_client(self, data):
        """The app refused the proxy's certificate: that host is pinned or uses its own trust."""
        sni = data.context.client.sni
        self._write({"ts": round(time.time(), 3), "event": "client_rejected_proxy_cert", "host": sni})


if "MYQ_CAPTURE_OUT" in os.environ:
    addons = [Capture()]
