#!/usr/bin/env python3
"""Extract public HTML and localize its images without executing page content."""

import argparse
import base64
import hashlib
import html
import http.client
import ipaddress
import json
import os
import re
import shutil
import socket
import ssl
import sys
import tempfile
from html.parser import HTMLParser
from pathlib import Path
from urllib.error import HTTPError
from urllib.parse import quote, unquote_to_bytes, urljoin, urlparse, urlunparse
from urllib.request import Request


AUTH_CONTEXT = {"allowed_hosts": set(), "headers": {}}
SENSITIVE_HEADERS = ("Cookie", "Authorization")
PAGE_RESPONSE_BYTES = 8 * 1024 * 1024
IMAGE_RESPONSE_BYTES = 128 * 1024 * 1024
ALL_IMAGE_RESPONSE_BYTES = 1024 * 1024 * 1024
STREAM_CHUNK_BYTES = 256 * 1024
MINIMUM_FREE_DISK_BYTES = 32 * 1024 * 1024

SKIP_TAGS = {"script", "style", "noscript", "svg", "nav"}
SEMANTIC_FLOW_TAGS = {"article", "main"}
SEMANTIC_EDGE_TAGS = {"header", "footer"}
VOID_TAGS = {
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link",
    "meta", "param", "source", "track", "wbr",
}
SAFE_INERT_LINK_SCHEMES = {"http", "https", "ftp", "mailto", "tel"}
LINK_MARKER_RE = re.compile(
    r"<!--YUNSPIRE_LINK_(START|END):([a-z0-9-]+)-->", re.I
)
BLOCK_TAGS = {
    "address", "article", "aside", "blockquote", "dd", "details", "dialog",
    "div", "dl", "dt", "fieldset", "figcaption", "figure", "form", "h1",
    "h2", "h3", "h4", "h5", "h6", "hr", "li", "main", "menu", "ol",
    "p", "pre", "section", "summary", "table", "tbody", "td", "tfoot", "header", "footer",
    "th", "thead", "tr", "ul",
}

IMAGE_FORMATS = {
    "jpeg": ("jpg", "image/jpeg"),
    "png": ("png", "image/png"),
    "gif": ("gif", "image/gif"),
    "webp": ("webp", "image/webp"),
    "bmp": ("bmp", "image/bmp"),
    "tiff": ("tiff", "image/tiff"),
    "ico": ("ico", "image/x-icon"),
    "avif": ("avif", "image/avif"),
    "heic": ("heic", "image/heic"),
}
MIME_ALIASES = {
    "image/jpg": "image/jpeg",
    "image/pjpeg": "image/jpeg",
    "image/x-png": "image/png",
    "image/x-ms-bmp": "image/bmp",
    "image/x-icon": "image/x-icon",
    "image/vnd.microsoft.icon": "image/x-icon",
    "image/ico": "image/x-icon",
    "image/x-tiff": "image/tiff",
}


class ImageLocalizationError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def load_request_authorization(enabled):
    if not enabled:
        return
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, TypeError) as exc:
        raise ValueError("一次性授权数据无效") from exc
    allowed_hosts = payload.get("allowed_hosts") if isinstance(payload, dict) else []
    headers = payload.get("headers") if isinstance(payload, dict) else {}
    AUTH_CONTEXT["allowed_hosts"] = {
        str(host).strip().rstrip(".").lower()
        for host in allowed_hosts or []
        if str(host).strip()
    }
    AUTH_CONTEXT["headers"] = {
        name: str(headers[name])
        for name in SENSITIVE_HEADERS
        if isinstance(headers, dict)
        and isinstance(headers.get(name), str)
        and headers[name]
        and "\r" not in headers[name]
        and "\n" not in headers[name]
    }


def authorized_headers(url):
    host = (urlparse(url).hostname or "").rstrip(".").lower()
    return AUTH_CONTEXT["headers"] if host in AUTH_CONTEXT["allowed_hosts"] else {}


def apply_authorization(request):
    for name in SENSITIVE_HEADERS:
        request.remove_header(name)
    for name, value in authorized_headers(request.full_url).items():
        request.add_header(name, value)
    return request


def canonical_public_url(value):
    parsed = urlparse(value)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("URL 只允许 http 或 https 协议")
    if parsed.username or parsed.password:
        raise ValueError("URL 不能包含用户名或密码")
    try:
        host = parsed.hostname.rstrip(".").encode("idna").decode("ascii").lower()
        port = parsed.port or (443 if parsed.scheme == "https" else 80)
    except (UnicodeError, ValueError) as exc:
        raise ValueError("URL 域名或端口无效") from exc
    if host == "localhost" or host.endswith((".localhost", ".local")):
        raise ValueError("禁止访问本机或局域网地址")
    display_host = f"[{host}]" if ":" in host else host
    default_port = 443 if parsed.scheme == "https" else 80
    netloc = display_host if port == default_port else f"{display_host}:{port}"
    path = quote(parsed.path or "/", safe="/%:@!$&'()*+,;=-._~")
    query = quote(parsed.query, safe="=&?/:@!$'()*+,;%-._~")
    return urlunparse((parsed.scheme.lower(), netloc, path, "", query, "")), host, port


def resolve_public_addresses(host, port):
    try:
        addresses = socket.getaddrinfo(
            host,
            port,
            family=socket.AF_UNSPEC,
            type=socket.SOCK_STREAM,
        )
    except socket.gaierror as exc:
        raise ValueError(f"无法解析公开地址：{exc}") from exc
    if not addresses:
        raise ValueError("URL 没有可验证的公开地址")
    unique = []
    for address in addresses:
        value = address[4][0]
        ip = ipaddress.ip_address(value.split("%", 1)[0])
        if not ip.is_global:
            raise ValueError("禁止访问私网、回环、链路本地或保留地址")
        if value not in unique:
            unique.append(value)
    return unique


def validate_public_url(value):
    canonical, host, port = canonical_public_url(value)
    resolve_public_addresses(host, port)
    return canonical


class PinnedHTTPConnection(http.client.HTTPConnection):
    def __init__(self, host, address, port, timeout):
        super().__init__(host, port=port, timeout=timeout)
        self.pinned_address = address

    def connect(self):
        self.sock = socket.create_connection(
            (self.pinned_address, self.port), self.timeout, self.source_address
        )


class PinnedHTTPSConnection(http.client.HTTPSConnection):
    def __init__(self, host, address, port, timeout):
        super().__init__(host, port=port, timeout=timeout, context=ssl.create_default_context())
        self.pinned_address = address

    def connect(self):
        raw_socket = socket.create_connection(
            (self.pinned_address, self.port), self.timeout, self.source_address
        )
        self.sock = self._context.wrap_socket(raw_socket, server_hostname=self.host)


class PinnedResponse:
    def __init__(self, connection, response, final_url):
        self.connection = connection
        self.response = response
        self.headers = response.headers
        self.final_url = final_url

    def read(self, amount=None):
        return self.response.read(amount)

    def geturl(self):
        return self.final_url

    def close(self):
        try:
            self.response.close()
        finally:
            self.connection.close()

    def __enter__(self):
        return self

    def __exit__(self, _type, _value, _traceback):
        self.close()


def request_target(parsed):
    target = parsed.path or "/"
    if parsed.query:
        target += "?" + parsed.query
    return target


def host_header(host, port, scheme):
    display_host = f"[{host}]" if ":" in host else host
    default_port = 443 if scheme == "https" else 80
    return display_host if port == default_port else f"{display_host}:{port}"


def open_pinned_once(url, headers, timeout):
    canonical, host, port = canonical_public_url(url)
    addresses = resolve_public_addresses(host, port)
    parsed = urlparse(canonical)
    last_error = None
    for address in addresses:
        connection = None
        try:
            if parsed.scheme == "https":
                connection = PinnedHTTPSConnection(host, address, port, timeout)
            else:
                connection = PinnedHTTPConnection(host, address, port, timeout)
            connection.request(
                "GET",
                request_target(parsed),
                headers={
                    **headers,
                    "Host": host_header(host, port, parsed.scheme),
                    "Connection": "close",
                },
            )
            return canonical, connection, connection.getresponse()
        except (OSError, ssl.SSLError, http.client.HTTPException) as exc:
            last_error = exc
            if connection is not None:
                connection.close()
    raise ValueError(f"公开地址连接失败：{last_error or 'connect_failed'}")


def open_public(request, timeout, redirect_chain=None):
    redirects = redirect_chain if redirect_chain is not None else []
    current_url = request.full_url
    base_headers = {
        name: value
        for name, value in request.header_items()
        if name.lower() not in {"cookie", "authorization", "host", "connection"}
    }
    for redirect_index in range(6):
        hop_headers = dict(base_headers)
        hop_headers.update(authorized_headers(current_url))
        canonical, connection, response = open_pinned_once(
            current_url, hop_headers, timeout
        )
        if response.status not in {301, 302, 303, 307, 308}:
            if response.status >= 400:
                status = response.status
                reason = response.reason
                headers = response.headers
                response.close()
                connection.close()
                raise HTTPError(canonical, status, reason, headers, None)
            return PinnedResponse(connection, response, canonical)
        location = response.getheader("Location")
        status = response.status
        response.close()
        connection.close()
        if not location:
            raise ValueError("重定向响应缺少目标地址")
        target = validate_public_url(urljoin(canonical, location))
        if urlparse(canonical).scheme == "https" and urlparse(target).scheme != "https":
            raise ValueError("禁止 HTTPS 资源降级重定向到 HTTP")
        redirects.append({
            "status": status,
            "from_url": canonical,
            "to_url": target,
        })
        current_url = target
        if redirect_index >= 5:
            raise ValueError("重定向次数超过安全上限")
    raise ValueError("重定向次数超过安全上限")


def normalized_http_url(value):
    parsed = urlparse(value)
    if parsed.scheme not in {"http", "https"}:
        return value
    return urlunparse(parsed._replace(fragment=""))


def srcset_candidate(value):
    ranked = []
    for index, item in enumerate(str(value or "").split(",")):
        fields = item.strip().split()
        if not fields:
            continue
        source = fields[0]
        score = float(index)
        if len(fields) > 1:
            descriptor = fields[-1].lower()
            try:
                if descriptor.endswith("w"):
                    score = float(descriptor[:-1])
                elif descriptor.endswith("x"):
                    score = float(descriptor[:-1]) * 10000
            except ValueError:
                pass
        ranked.append((score, index, source))
    return max(ranked, default=(0, 0, ""))[2]


def select_image_source(values):
    candidates = []
    for key in ("data-original", "data-lazy-src", "data-src"):
        if values.get(key):
            candidates.append((values[key], key))
    for key in ("data-srcset", "srcset"):
        source = srcset_candidate(values.get(key))
        if source:
            candidates.append((source, key))
    if values.get("src"):
        candidates.append((values["src"], "src"))
    for source, attribute in candidates:
        if str(source).strip() and not str(source).strip().lower().startswith("data:image/"):
            return str(source).strip(), attribute
    return candidates[0] if candidates else ("", "")


def markdown_alt(value):
    return re.sub(r"\s+", " ", str(value or "")).strip().replace("[", "\\[").replace("]", "\\]")


def inert_link_policy(target):
    try:
        scheme = urlparse(target or "").scheme.lower()
    except ValueError:
        scheme = ""
    return {
        "content_role": "untrusted_data",
        "scheme": scheme or None,
        "auto_open": False,
        "auto_fetch": False,
        "capture_requires_explicit_user_request": scheme in {"http", "https", "ftp"},
    }


def normalize_inert_link_target(value, base_url):
    raw = html.unescape(str(value or "")).strip()
    if any(ord(character) < 32 or ord(character) == 127 for character in raw):
        raise ValueError("链接目标包含控制字符")
    absolute = urljoin(base_url, raw)
    try:
        parsed = urlparse(absolute)
    except ValueError as exc:
        raise ValueError("链接目标格式无效") from exc
    scheme = parsed.scheme.lower()
    if scheme not in SAFE_INERT_LINK_SCHEMES:
        raise ValueError(f"链接协议不受支持：{scheme or 'missing'}")
    if scheme in {"http", "https", "ftp"}:
        if not parsed.hostname or parsed.username or parsed.password:
            raise ValueError("链接目标缺少域名或包含凭据")
        try:
            host = parsed.hostname.rstrip(".").encode("idna").decode("ascii").lower()
            port = parsed.port
        except (UnicodeError, ValueError) as exc:
            raise ValueError("链接域名或端口无效") from exc
        default_port = {"http": 80, "https": 443, "ftp": 21}[scheme]
        display_host = f"[{host}]" if ":" in host else host
        netloc = display_host if port in {None, default_port} else f"{display_host}:{port}"
        path = quote(parsed.path or "/", safe="/%:@!$&'()*+,;=-._~")
        query = quote(parsed.query, safe="=&?/:@!$'()*+,;%-._~")
        fragment = quote(parsed.fragment, safe="=&?/:@!$'()*+,;%-._~")
        return urlunparse((scheme, netloc, path, "", query, fragment))
    if scheme == "mailto":
        if not parsed.path:
            raise ValueError("mailto 链接缺少收件地址")
        path = quote(parsed.path, safe="@:+,;=-._~%")
        query = quote(parsed.query, safe="=&?/:@!$'()*+,;%-._~")
        return urlunparse((scheme, "", path, "", query, ""))
    if not parsed.path:
        raise ValueError("tel 链接缺少号码")
    return urlunparse((scheme, "", quote(parsed.path, safe="+*#(),;=-._~%"), "", "", ""))


def finalize_link_references(markdown, references, base_url):
    working = markdown
    errors = []
    by_id = {reference["link_id"]: reference for reference in references}
    for reference in references:
        link_id = reference["link_id"]
        token = f"yunspire-link://{link_id}"
        start_marker = f"<!--YUNSPIRE_LINK_START:{link_id}-->"
        end_marker = f"<!--YUNSPIRE_LINK_END:{link_id}-->"
        if working.count(start_marker) != 1 or working.count(end_marker) != 1 or working.count(token) != 1:
            reference["normalization"] = {
                "status": "failed",
                "code": "link_position_marker_mismatch",
                "message": "链接位置标记缺失或重复",
            }
            errors.append({
                "code": "link_position_marker_mismatch",
                "message": "链接位置标记缺失或重复，无法保证原文位置",
                "link_id": link_id,
                "provenance": reference.get("provenance", {}),
            })
            continue
        try:
            target = normalize_inert_link_target(reference.get("target_raw", ""), base_url)
        except ValueError as exc:
            reference["target"] = None
            reference["policy"] = inert_link_policy(reference.get("target_raw", ""))
            reference["normalization"] = {
                "status": "failed",
                "code": "link_target_not_preservable",
                "message": str(exc),
            }
            errors.append({
                "code": "link_target_not_preservable",
                "message": str(exc),
                "link_id": link_id,
                "target_raw": reference.get("target_raw", ""),
                "provenance": reference.get("provenance", {}),
            })
            start = working.index(start_marker)
            end = working.index(end_marker, start) + len(end_marker)
            segment = working[start + len(start_marker):end - len(end_marker)]
            suffix = f"](<{token}>)"
            display = segment[1:-len(suffix)] if segment.startswith("[") and segment.endswith(suffix) else segment
            working = working[:start] + display + working[end:]
            continue
        reference["target"] = target
        reference["policy"] = inert_link_policy(target)
        reference["normalization"] = {
            "status": "normalized_without_fetch",
            "base_url": base_url,
        }
        working = working.replace(token, target, 1)

    output = []
    cursor = 0
    output_length = 0
    offsets = {}
    for match in LINK_MARKER_RE.finditer(working):
        fragment = working[cursor:match.start()]
        output.append(fragment)
        output_length += len(fragment)
        marker_kind = match.group(1).lower()
        link_id = match.group(2).lower()
        if link_id not in by_id:
            errors.append({
                "code": "unknown_link_position_marker",
                "message": "正文包含无法映射的链接位置标记",
                "link_id": link_id,
            })
        else:
            offsets.setdefault(link_id, {})[marker_kind] = output_length
        cursor = match.end()
    output.append(working[cursor:])
    finalized = "".join(output)

    for reference in references:
        link_id = reference["link_id"]
        placement = offsets.get(link_id, {})
        start = placement.get("start")
        end = placement.get("end")
        if reference.get("normalization", {}).get("status") == "failed":
            continue
        if start is None or end is None or end < start:
            reference["normalization"] = {
                "status": "failed",
                "code": "link_markdown_position_missing",
                "message": "无法计算链接在忠实 Markdown 中的位置",
            }
            errors.append({
                "code": "link_markdown_position_missing",
                "message": "无法计算链接在忠实 Markdown 中的位置",
                "link_id": link_id,
                "provenance": reference.get("provenance", {}),
            })
            continue
        previous_newline = finalized.rfind("\n", 0, start)
        reference["markdown_offset_start"] = start
        reference["markdown_offset_end"] = end
        reference["markdown_syntax"] = finalized[start:end]
        reference["provenance"]["markdown_offset_start"] = start
        reference["provenance"]["markdown_offset_end"] = end
        reference["provenance"]["markdown_line"] = finalized.count("\n", 0, start) + 1
        reference["provenance"]["markdown_column"] = start - previous_newline
    return finalized, references, errors


class PageParser(HTMLParser):
    def __init__(self, source_url):
        super().__init__(convert_charrefs=True)
        self.source_url = source_url
        self.title = []
        self.parts = []
        self.image_references = []
        self.link_references = []
        self.structure_errors = []
        self.semantic_regions = []
        self.meta = {}
        self.base_href = ""
        self._stack = []
        self._skip_frames = []
        self._anchor_stack = []
        self._semantic_edge_stack = []
        self._in_title = False
        self._pre_depth = 0
        self._finalized = False

    def _append_break(self, count=2):
        self.parts.append("\n" * count)

    def _append_text(self, data):
        if self._pre_depth:
            value = html.unescape(data)
            self.parts.append(value)
            for frame in reversed(self._anchor_stack):
                if frame.get("tracked"):
                    frame["reference"]["display_fragments"].append(value)
                    break
            return
        decoded = html.unescape(data)
        value = re.sub(r"\s+", " ", decoded)
        if not value.strip():
            if self.parts and not self.parts[-1].endswith((" ", "\n")):
                self.parts.append(" ")
            return
        self.parts.append(value)
        for frame in reversed(self._anchor_stack):
            if frame.get("tracked"):
                frame["reference"]["display_fragments"].append(value)
                break

    def _semantic_region(self, stack=None):
        tags = list(stack if stack is not None else self._stack)
        if "article" in tags:
            if "header" in tags:
                return "article_header"
            if "footer" in tags:
                return "article_footer"
            return "article_body"
        if "main" in tags:
            if "header" in tags:
                return "main_header"
            if "footer" in tags:
                return "main_footer"
            return "main_body"
        return "page_body"

    def _dom_path(self, leaf=None):
        tags = list(self._stack)
        if leaf and (not tags or tags[-1] != leaf):
            tags.append(leaf)
        return "/".join(tags)

    def _record_structure_error(self, code, message, **details):
        line, column = self.getpos()
        self.structure_errors.append({
            "code": code,
            "message": message,
            "html_line": line,
            "html_column": column + 1,
            "dom_path": self._dom_path(),
            **details,
        })

    def _start_link(self, values):
        occurrence = len(self.link_references) + 1
        target_raw = values.get("href", "")
        identity = hashlib.sha256(
            f"{self.source_url}\0{occurrence}\0{target_raw}".encode("utf-8")
        ).hexdigest()[:24]
        link_id = f"web-link-{identity}"
        line, column = self.getpos()
        reference = {
            "link_id": link_id,
            "source": "html_anchor",
            "source_kind": "html_anchor",
            "occurrence_index": occurrence,
            "display_text": "",
            "display_fragments": [],
            "target_raw": target_raw,
            "title_text": re.sub(r"\s+", " ", values.get("title", "")).strip(),
            "rel": [item for item in re.split(r"\s+", values.get("rel", "").strip()) if item],
            "download": values.get("download") if "download" in values else None,
            "image_reference_ids": [],
            "provenance": {
                "source_kind": "html_anchor",
                "html_line_start": line,
                "html_column_start": column + 1,
                "dom_path": self._dom_path(),
                "semantic_region": self._semantic_region(),
            },
        }
        self.link_references.append(reference)
        self.parts.append(f"<!--YUNSPIRE_LINK_START:{link_id}-->[")
        self._anchor_stack.append({"tracked": True, "reference": reference})

    def _end_link(self):
        if not self._anchor_stack:
            self._record_structure_error(
                "unmatched_anchor_end", "发现没有对应开始标签的链接结束标签"
            )
            return
        frame = self._anchor_stack.pop()
        if not frame.get("tracked"):
            return
        reference = frame["reference"]
        link_id = reference["link_id"]
        line, column = self.getpos()
        reference["display_text"] = re.sub(
            r"\s+", " ", "".join(reference.pop("display_fragments", []))
        ).strip()
        reference["provenance"]["html_line_end"] = line
        reference["provenance"]["html_column_end"] = column + 1
        self.parts.append(
            f"](<yunspire-link://{link_id}>)<!--YUNSPIRE_LINK_END:{link_id}-->"
        )

    def _append_image(self, values, source, source_attribute):
        occurrence = len(self.image_references) + 1
        identity = hashlib.sha256(
            f"{self.source_url}\0{occurrence}\0{source}".encode("utf-8")
        ).hexdigest()[:24]
        reference_id = f"web-image-reference-{identity}"
        alt = markdown_alt(values.get("alt") or values.get("title") or f"网页图片 {occurrence}")
        placeholder = f"attachment://{reference_id}"
        self.parts.append(f"![{alt}]({placeholder})")
        active_link = next(
            (frame["reference"] for frame in reversed(self._anchor_stack) if frame.get("tracked")),
            None,
        )
        line, column = self.getpos()
        reference = {
            "reference_id": reference_id,
            "source": "html_content_flow",
            "source_kind": "html_image",
            "source_value": source,
            "source_attribute": source_attribute,
            "occurrence_index": occurrence,
            "flow_index": occurrence,
            "alt_text": re.sub(r"\s+", " ", values.get("alt", "")).strip(),
            "title_text": re.sub(r"\s+", " ", values.get("title", "")).strip(),
            "link_ids": [active_link["link_id"]] if active_link else [],
            "provenance": {
                "source_kind": "html_image",
                "html_line": line,
                "html_column": column + 1,
                "dom_path": self._dom_path("img"),
                "semantic_region": self._semantic_region(),
            },
            "placement": {
                "kind": "html_content_flow",
                "sequence": occurrence,
                "required": True,
            },
        }
        self.image_references.append(reference)
        if active_link:
            active_link["image_reference_ids"].append(reference_id)
            active_link["display_fragments"].append(
                reference["alt_text"] or reference["title_text"]
            )

    def handle_starttag(self, tag, attrs):
        values = {str(key).lower(): value or "" for key, value in attrs}
        lower = tag.lower()
        ancestors = list(self._stack)
        if lower not in VOID_TAGS:
            self._stack.append(lower)
        if lower == "title":
            self._in_title = True
        if lower == "meta" and values.get("content"):
            key = values.get("property") or values.get("name")
            if key:
                self.meta[key.lower()] = values["content"].strip()
        if lower == "base" and values.get("href") and not self.base_href:
            self.base_href = values["href"].strip()
        semantic_edge_outside_flow = (
            lower in SEMANTIC_EDGE_TAGS
            and not any(item in SEMANTIC_FLOW_TAGS for item in ancestors)
        )
        if lower in SKIP_TAGS or semantic_edge_outside_flow:
            self._skip_frames.append({"tag": lower, "depth": len(self._stack)})
            return
        if self._skip_frames:
            return
        if lower in SEMANTIC_EDGE_TAGS:
            line, column = self.getpos()
            self._semantic_edge_stack.append({
                "tag": lower,
                "html_line_start": line,
                "html_column_start": column + 1,
                "semantic_region": self._semantic_region(),
                "image_count_start": len(self.image_references),
                "link_count_start": len(self.link_references),
            })
        if lower in BLOCK_TAGS:
            self._append_break(2)
        if lower == "br":
            self._append_break(1)
        elif lower == "li":
            self.parts.append("- ")
        elif lower in {"h1", "h2", "h3", "h4", "h5", "h6"}:
            self.parts.append(f"{'#' * int(lower[1])} ")
        elif lower == "pre":
            self.parts.append("```text\n")
            self._pre_depth += 1
        elif lower == "img":
            source, source_attribute = select_image_source(values)
            if source:
                self._append_image(values, source, source_attribute)
            else:
                self._record_structure_error(
                    "html_image_source_missing",
                    "图片标签没有可保真的 src、srcset 或延迟加载来源",
                    dom_path=self._dom_path("img"),
                )
        elif lower == "a":
            has_href = any(str(key).lower() == "href" for key, _value in attrs)
            if has_href:
                if any(frame.get("tracked") for frame in self._anchor_stack):
                    self._record_structure_error(
                        "nested_anchor_not_preservable",
                        "HTML 包含嵌套链接，无法无损转换为 Markdown",
                    )
                    self._anchor_stack.append({"tracked": False})
                else:
                    self._start_link(values)
            else:
                self._anchor_stack.append({"tracked": False})

    def handle_endtag(self, tag):
        lower = tag.lower()
        if lower == "title":
            self._in_title = False
        was_skipped = bool(self._skip_frames)
        if self._skip_frames:
            matching = next(
                (
                    index for index in range(len(self._skip_frames) - 1, -1, -1)
                    if self._skip_frames[index]["tag"] == lower
                ),
                None,
            )
            if matching is not None:
                del self._skip_frames[matching:]
        if not was_skipped and lower == "a":
            self._end_link()
        if not was_skipped and lower == "pre" and self._pre_depth:
            self._pre_depth -= 1
            self.parts.append("\n```")
        if not was_skipped and lower in BLOCK_TAGS:
            self._append_break(2)
        if not was_skipped and lower in SEMANTIC_EDGE_TAGS:
            matching = next(
                (
                    index for index in range(len(self._semantic_edge_stack) - 1, -1, -1)
                    if self._semantic_edge_stack[index]["tag"] == lower
                ),
                None,
            )
            if matching is None:
                self._record_structure_error(
                    "semantic_region_end_unmatched",
                    f"语义区域 {lower} 缺少可匹配的开始标签",
                )
            else:
                region = self._semantic_edge_stack.pop(matching)
                line, column = self.getpos()
                region.update({
                    "html_line_end": line,
                    "html_column_end": column + 1,
                    "image_reference_ids": [
                        item["reference_id"]
                        for item in self.image_references[region["image_count_start"]:]
                    ],
                    "link_ids": [
                        item["link_id"]
                        for item in self.link_references[region["link_count_start"]:]
                    ],
                })
                region.pop("image_count_start", None)
                region.pop("link_count_start", None)
                self.semantic_regions.append(region)
        if self._stack:
            if lower in self._stack:
                reverse_index = self._stack[::-1].index(lower)
                del self._stack[len(self._stack) - reverse_index - 1:]
            else:
                self._stack.pop()

    def handle_data(self, data):
        if self._skip_frames:
            return
        value = re.sub(r"\s+", " ", html.unescape(data)).strip()
        if self._in_title:
            if value:
                self.title.append(value)
            return
        if "head" in self._stack:
            return
        self._append_text(data)

    def finalize_structure(self):
        if self._finalized:
            return
        self._finalized = True
        while self._anchor_stack:
            frame = self._anchor_stack[-1]
            if frame.get("tracked"):
                self._record_structure_error(
                    "anchor_end_missing",
                    "链接缺少结束标签，无法确认显示内容与目标的边界",
                    link_id=frame["reference"]["link_id"],
                )
                self._end_link()
            else:
                self._anchor_stack.pop()
        if self._pre_depth:
            self._record_structure_error(
                "preformatted_block_end_missing",
                "预格式化文本缺少结束标签，无法确认正文边界",
            )
            self.parts.append("\n```")
            self._pre_depth = 0
        for region in self._semantic_edge_stack:
            self._record_structure_error(
                "semantic_region_end_missing",
                f"语义区域 {region['tag']} 缺少结束标签",
            )
        self._semantic_edge_stack = []
        for frame in self._skip_frames:
            self._record_structure_error(
                "excluded_region_end_missing",
                f"被排除区域 {frame['tag']} 缺少结束标签，无法确认正文恢复位置",
            )
        self._skip_frames = []

    def content_markdown(self):
        self.finalize_structure()
        value = "".join(self.parts)
        value = re.sub(r"[ \t]+\n", "\n", value)
        value = re.sub(r"\n[ \t]+", "\n", value)
        value = re.sub(r"\n{3,}", "\n\n", value)
        return value.strip()


def blocked_page(title, body, host):
    combined = f"{title} {body[:5000]}".lower()
    signals = (
        "captcha", "verify you are human", "access denied", "enable javascript",
        "请完成验证", "安全验证", "访问过于频繁", "请登录后继续", "扫码登录",
        "环境异常", "请求异常", "页面不存在", "内容不可见",
    )
    platform_hosts = (
        "xiaohongshu.com", "xhslink.com", "douyin.com", "tiktok.com",
        "weixin.qq.com", "x.com", "twitter.com",
    )
    plain_body = re.sub(r"!\[[^]]*\]\([^)]*\)|[#*`>_-]", " ", body)
    return any(signal in combined for signal in signals) or (
        any(value in host.lower() for value in platform_hosts)
        and len(plain_body.strip()) < 160
    )


def structured_article(raw_text):
    candidates = re.findall(
        r'<script[^>]+type=["\']application/ld\+json["\'][^>]*>(.*?)</script>',
        raw_text,
        re.I | re.S,
    )
    text_parts = []
    images = []
    metadata = {}

    def walk(value):
        if isinstance(value, list):
            for item in value:
                walk(item)
            return
        if not isinstance(value, dict):
            return
        article_type = str(value.get("@type") or "").lower()
        if any(kind in article_type for kind in ("article", "posting", "news")):
            for key in ("headline", "description", "articleBody"):
                content = value.get(key)
                if isinstance(content, str) and content.strip() and content.strip() not in text_parts:
                    text_parts.append(content.strip())
            for key in ("datePublished", "dateModified"):
                if value.get(key):
                    metadata[key] = value[key]
            image = value.get("image")
            values = image if isinstance(image, list) else [image]
            for item in values:
                url = item.get("url") if isinstance(item, dict) else item
                if isinstance(url, str) and url not in images:
                    images.append(url)
        for child in value.values():
            if isinstance(child, (dict, list)):
                walk(child)

    for candidate in candidates:
        try:
            walk(json.loads(html.unescape(candidate.strip())))
        except (json.JSONDecodeError, TypeError):
            continue
    return "\n\n".join(text_parts), images, metadata


def detect_image_format(header):
    if header.startswith(b"\xff\xd8\xff"):
        return "jpeg"
    if header.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png"
    if header.startswith((b"GIF87a", b"GIF89a")):
        return "gif"
    if len(header) >= 12 and header[:4] == b"RIFF" and header[8:12] == b"WEBP":
        return "webp"
    if header.startswith(b"BM"):
        return "bmp"
    if header.startswith((b"II*\x00", b"MM\x00*")):
        return "tiff"
    if header.startswith(b"\x00\x00\x01\x00"):
        return "ico"
    if len(header) >= 12 and header[4:8] == b"ftyp":
        brand = header[8:12]
        compatible = header[8:64]
        if brand in {b"avif", b"avis"} or b"avif" in compatible or b"avis" in compatible:
            return "avif"
        if brand in {b"heic", b"heix", b"hevc", b"hevx", b"mif1", b"msf1"}:
            return "heic"
    stripped = header.lstrip(b"\xef\xbb\xbf\x00\t\r\n ")[:256].lower()
    if stripped.startswith(b"<svg") or (stripped.startswith(b"<?xml") and b"<svg" in stripped):
        raise ImageLocalizationError("unsafe_svg", "SVG 图片可能包含主动内容，已阻止直接嵌入")
    raise ImageLocalizationError("invalid_image_signature", "响应内容不是受支持的真实图片")


def parse_content_length(headers):
    try:
        value = int(headers.get("Content-Length", "0"))
        return value if value >= 0 else 0
    except (TypeError, ValueError):
        return 0


def disk_space_available(directory, required):
    free = shutil.disk_usage(directory).free
    if free < required + MINIMUM_FREE_DISK_BYTES:
        raise ImageLocalizationError("insufficient_disk_space", "保存网页图片所需磁盘空间不足")


def finalize_image_file(temporary_path, output_root, digest, image_format):
    extension, mime_type = IMAGE_FORMATS[image_format]
    name = f"web-image-{digest[:24]}.{extension}"
    destination = output_root / name
    if destination.exists():
        if destination.is_symlink() or not destination.is_file():
            raise ImageLocalizationError("unsafe_destination", "网页图片目标路径不是普通文件")
        temporary_path.unlink(missing_ok=True)
    else:
        os.replace(str(temporary_path), str(destination))
    return name, destination, mime_type


def download_external_image(source_url, output_root, remaining_bytes):
    try:
        validate_public_url(source_url)
    except ValueError as exc:
        raise ImageLocalizationError("non_public_image_url", str(exc)) from exc
    if remaining_bytes <= 0:
        raise ImageLocalizationError("aggregate_byte_budget_exceeded", "网页图片累计响应超过 1 GB 安全边界")
    redirect_chain = []
    request = Request(source_url, headers={
        "User-Agent": "Yunspire/0.1 local knowledge capture",
        "Accept": "image/avif,image/webp,image/png,image/jpeg,image/gif,image/*;q=0.8",
        "Accept-Encoding": "identity",
    })
    temporary_path = None
    try:
        with open_public(request, timeout=20, redirect_chain=redirect_chain) as response:
            final_url = normalized_http_url(response.geturl())
            validate_public_url(final_url)
            content_encoding = str(response.headers.get("Content-Encoding") or "identity").strip().lower()
            if content_encoding not in {"", "identity"}:
                raise ImageLocalizationError(
                    "unsupported_content_encoding",
                    f"图片响应使用了不支持的 Content-Encoding：{content_encoding}",
                )
            content_length = parse_content_length(response.headers)
            byte_boundary = min(IMAGE_RESPONSE_BYTES, remaining_bytes)
            if content_length > byte_boundary:
                raise ImageLocalizationError(
                    "image_byte_boundary_exceeded",
                    f"图片响应声明大小 {content_length} 字节，超过当前 {byte_boundary} 字节安全边界",
                )
            disk_space_available(output_root, content_length or STREAM_CHUNK_BYTES)
            declared_mime = str(response.headers.get_content_type() or "").lower()
            temporary = tempfile.NamedTemporaryFile(
                mode="wb", prefix=".web-image-", suffix=".part", dir=str(output_root), delete=False
            )
            temporary_path = Path(temporary.name)
            hasher = hashlib.sha256()
            header = bytearray()
            written = 0
            next_disk_check = 8 * 1024 * 1024
            try:
                while True:
                    block = response.read(STREAM_CHUNK_BYTES)
                    if not block:
                        break
                    written += len(block)
                    if written > byte_boundary:
                        raise ImageLocalizationError(
                            "image_byte_boundary_exceeded",
                            f"图片响应超过当前 {byte_boundary} 字节安全边界",
                        )
                    if len(header) < 8192:
                        header.extend(block[:8192 - len(header)])
                    hasher.update(block)
                    temporary.write(block)
                    if written >= next_disk_check:
                        disk_space_available(output_root, STREAM_CHUNK_BYTES)
                        next_disk_check += 8 * 1024 * 1024
                temporary.flush()
                os.fsync(temporary.fileno())
            finally:
                temporary.close()
            if written == 0:
                raise ImageLocalizationError("empty_image", "图片响应为空")
            if content_length and written != content_length:
                raise ImageLocalizationError(
                    "image_length_mismatch",
                    f"图片实际大小 {written} 与响应声明 {content_length} 不一致",
                )
            image_format = detect_image_format(bytes(header))
            _, actual_mime = IMAGE_FORMATS[image_format]
            if declared_mime and not (
                declared_mime.startswith("image/")
                or declared_mime in {"application/octet-stream", "binary/octet-stream"}
            ):
                raise ImageLocalizationError(
                    "invalid_image_mime",
                    f"图片响应 Content-Type 无效：{declared_mime}",
                )
            normalized_declared_mime = MIME_ALIASES.get(declared_mime, declared_mime)
            if normalized_declared_mime.startswith("image/") and normalized_declared_mime != actual_mime:
                raise ImageLocalizationError(
                    "image_mime_signature_mismatch",
                    f"图片 Content-Type {declared_mime} 与真实格式 {actual_mime} 不一致",
                )
            digest = hasher.hexdigest()
            name, destination, mime_type = finalize_image_file(
                temporary_path, output_root, digest, image_format
            )
            temporary_path = None
            return {
                "asset_id": f"sha256:{digest}",
                "sha256": digest,
                "name": name,
                "mime_type": mime_type,
                "declared_mime_type": declared_mime or None,
                "size": written,
                "source_url": source_url,
                "final_url": final_url,
                "redirect_chain": redirect_chain,
                "local_attachment_path": str(destination),
                "localization": {
                    "status": "localized",
                    "fetch_policy": "public-http-image-v1",
                    "source_url": source_url,
                    "final_url": final_url,
                    "redirect_chain": redirect_chain,
                    "dns_pinned": True,
                    "mime_verified": True,
                    "magic_bytes_verified": True,
                    "byte_length": written,
                },
            }
    except ImageLocalizationError:
        raise
    except HTTPError as exc:
        raise ImageLocalizationError("image_http_error", f"图片读取失败：HTTP {exc.code}") from exc
    except Exception as exc:
        raise ImageLocalizationError("image_fetch_failed", str(exc)) from exc
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def store_inline_image(data_uri, output_root, remaining_bytes):
    match = re.match(r"^data:([^;,]+)?((?:;[^,]*)?),(.*)$", data_uri, re.I | re.S)
    if not match or not str(match.group(1) or "").lower().startswith("image/"):
        raise ImageLocalizationError("invalid_inline_image", "内嵌图片 data URI 无效")
    metadata = match.group(2) or ""
    payload = match.group(3)
    try:
        if ";base64" in metadata.lower():
            data = base64.b64decode(payload, validate=True)
        else:
            data = unquote_to_bytes(payload)
    except (ValueError, TypeError) as exc:
        raise ImageLocalizationError("invalid_inline_image", "内嵌图片编码无效") from exc
    boundary = min(IMAGE_RESPONSE_BYTES, remaining_bytes)
    if len(data) > boundary:
        raise ImageLocalizationError("image_byte_boundary_exceeded", "内嵌图片超过当前字节安全边界")
    image_format = detect_image_format(data[:8192])
    disk_space_available(output_root, len(data))
    temporary = tempfile.NamedTemporaryFile(
        mode="wb", prefix=".web-image-", suffix=".part", dir=str(output_root), delete=False
    )
    temporary_path = Path(temporary.name)
    try:
        temporary.write(data)
        temporary.flush()
        os.fsync(temporary.fileno())
        temporary.close()
        digest = hashlib.sha256(data).hexdigest()
        name, destination, mime_type = finalize_image_file(
            temporary_path, output_root, digest, image_format
        )
        temporary_path = None
        return {
            "asset_id": f"sha256:{digest}",
            "sha256": digest,
            "name": name,
            "mime_type": mime_type,
            "declared_mime_type": str(match.group(1) or "").lower() or None,
            "size": len(data),
            "source_url": f"inline-data:sha256:{digest}",
            "final_url": None,
            "redirect_chain": [],
            "local_attachment_path": str(destination),
            "localization": {
                "status": "localized",
                "fetch_policy": "inline-image-data-v1",
                "magic_bytes_verified": True,
                "byte_length": len(data),
            },
        }
    finally:
        try:
            temporary.close()
        except Exception:
            pass
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def resolve_reference(reference, base_url):
    source_value = str(reference.get("source_value") or "").strip()
    if source_value.lower().startswith("data:image/"):
        reference["resolved_url"] = source_value
        reference["source_kind"] = "inline_image"
        return reference
    absolute = normalized_http_url(urljoin(base_url, source_value))
    reference["resolved_url"] = absolute
    reference["source_kind"] = "external_image"
    return reference


def supplemental_reference(source_url, source_kind, sequence):
    identity = hashlib.sha256(
        f"metadata\0{source_kind}\0{source_url}".encode("utf-8")
    ).hexdigest()[:24]
    reference_id = f"web-image-reference-{identity}"
    return {
        "reference_id": reference_id,
        "source": "page_metadata",
        "source_kind": source_kind,
        "source_value": source_url,
        "source_attribute": source_kind,
        "occurrence_index": sequence,
        "flow_index": sequence,
        "alt_text": "来源附图",
        "title_text": "",
        "placement": {
            "kind": "metadata_appendix",
            "sequence": sequence,
            "required": True,
        },
    }


def reference_context(markdown, token, start_cursor):
    offset = markdown.find(token, start_cursor)
    if offset < 0:
        return start_cursor, {}
    return offset + len(token), {
        "markdown_offset_start": offset,
        "markdown_offset_end": offset + len(token),
        "context_before": markdown[max(0, offset - 180):offset].strip(),
        "context_after": markdown[offset + len(token):offset + len(token) + 180].strip(),
    }


def localize_references(markdown, references, output_root):
    unique_results = {}
    localized_assets = {}
    failures = []
    bytes_downloaded = 0
    for reference in references:
        resolved = str(reference.get("resolved_url") or "")
        result = unique_results.get(resolved)
        if result is None:
            remaining = ALL_IMAGE_RESPONSE_BYTES - bytes_downloaded
            try:
                if resolved.lower().startswith("data:image/"):
                    localized = store_inline_image(resolved, output_root, remaining)
                else:
                    localized = download_external_image(resolved, output_root, remaining)
                bytes_downloaded += int(localized.get("size") or 0)
                result = {"status": "localized", "asset": localized}
            except ImageLocalizationError as exc:
                result = {
                    "status": "failed",
                    "failure": {
                        "source_url": resolved,
                        "code": exc.code,
                        "message": str(exc),
                    },
                }
            unique_results[resolved] = result
        if result["status"] == "failed":
            failure = dict(result["failure"])
            failure["reference_id"] = reference["reference_id"]
            failure["placement"] = reference["placement"]
            failures.append(failure)
            reference["localization"] = {
                "status": "failed",
                "code": failure["code"],
                "message": failure["message"],
            }
            continue
        localized = result["asset"]
        asset_id = localized["asset_id"]
        reference["asset_id"] = asset_id
        reference["attachment_name"] = localized["name"]
        reference["localization"] = {
            "status": "localized",
            "source_url": localized["source_url"],
            "final_url": localized.get("final_url"),
        }
        asset = localized_assets.get(asset_id)
        if asset is None:
            asset = dict(localized)
            asset["reference_id"] = reference["reference_id"]
            asset["references"] = []
            asset["source_urls"] = []
            asset["localizations"] = []
            asset["placement_required"] = True
            localized_assets[asset_id] = asset
        if localized["source_url"] not in asset["source_urls"]:
            asset["source_urls"].append(localized["source_url"])
            asset["localizations"].append(dict(localized["localization"]))
        asset["references"].append(dict(reference))

    cursor = 0
    for reference in references:
        final_token = f"attachment://{reference['reference_id']}"
        cursor, context = reference_context(markdown, final_token, cursor)
        reference["placement"].update(context)
        if reference.get("asset_id"):
            asset = localized_assets.get(reference["asset_id"])
            if asset:
                for stored_reference in asset["references"]:
                    if stored_reference["reference_id"] == reference["reference_id"]:
                        stored_reference["placement"].update(context)
                        break
    return markdown, list(localized_assets.values()), failures, unique_results, bytes_downloaded


def authorization_output(parsed, source_url, status):
    return {
        "title": parsed.netloc,
        "source_url": source_url,
        "content_markdown": "",
        "embedded_links": [],
        "structure_errors": [],
        "images": [],
        "localized_image_urls": [],
        "failed_image_urls": [],
        "image_references": [],
        "attachments": [],
        "external_image_localization": {
            "external_asset_count": 0,
            "localized_asset_count": 0,
            "failed_asset_count": 0,
            "reference_count": 0,
            "localized_reference_count": 0,
            "failed_reference_count": 0,
            "all_external_images_localized": True,
        },
        "metadata": {
            "host": parsed.netloc,
            "http_status": status,
            "ordinary_links_opened_or_fetched": False,
        },
        "warnings": [f"页面返回 HTTP {status}，需要完成平台官方授权"],
        "errors": ["authorization_required"],
        "auth_required": True,
    }


def main():
    argument_parser = argparse.ArgumentParser()
    argument_parser.add_argument("url")
    argument_parser.add_argument("--request-headers-stdin", action="store_true")
    argument_parser.add_argument("--attachment-output-dir")
    args = argument_parser.parse_args()
    try:
        load_request_authorization(args.request_headers_stdin)
        validate_public_url(args.url)
    except ValueError as exc:
        raise SystemExit(str(exc)) from exc

    parsed = urlparse(args.url)
    request = Request(args.url, headers={
        "User-Agent": "Yunspire/0.1 local knowledge capture",
        "Accept": "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1",
        "Accept-Encoding": "identity",
    })
    warnings = []
    page_redirect_chain = []
    try:
        with open_public(request, timeout=20, redirect_chain=page_redirect_chain) as response:
            final_page_url = normalized_http_url(response.geturl())
            validate_public_url(final_page_url)
            content_encoding = str(response.headers.get("Content-Encoding") or "identity").strip().lower()
            if content_encoding not in {"", "identity"}:
                raise ValueError(f"网页响应使用了不支持的 Content-Encoding：{content_encoding}")
            declared_length = parse_content_length(response.headers)
            if declared_length > PAGE_RESPONSE_BYTES:
                raise ValueError("网页响应超过 8 MB 安全上限")
            raw = response.read(PAGE_RESPONSE_BYTES + 1)
            if len(raw) > PAGE_RESPONSE_BYTES:
                raise ValueError("网页响应超过 8 MB 安全上限")
            encoding = response.headers.get_content_charset() or "utf-8"
    except HTTPError as exc:
        if exc.code in {401, 403}:
            json.dump(authorization_output(parsed, args.url, exc.code), sys.stdout, ensure_ascii=False)
            return
        raise SystemExit(f"网页读取失败：HTTP {exc.code}") from exc
    except Exception as exc:
        raise SystemExit(f"网页读取失败：{exc}") from exc

    decoded = raw.decode(encoding, errors="replace")
    page = PageParser(final_page_url)
    page.feed(decoded)
    page.close()
    page.finalize_structure()
    title = " ".join(page.title).strip() or page.meta.get("og:title") or parsed.netloc
    body = page.content_markdown()
    structured_body, structured_images, structured_metadata = structured_article(decoded)
    body_source = "html_content_flow"
    if not body.strip() and structured_body.strip():
        body = structured_body.strip()
        body_source = "json_ld_article_body"

    auth_required = blocked_page(title, body, parsed.netloc)
    if auth_required:
        body = ""
        page.image_references = []
        page.link_references = []
        page.structure_errors = []
        warnings.append("页面返回登录、验证、风控或空壳内容，未将其误判为原文正文")
    if not body and not warnings:
        fallback = page.meta.get("og:description") or page.meta.get("description")
        if fallback:
            body = fallback
            body_source = "page_description"
            warnings.append("页面没有可提取的正文，仅返回元数据描述")
        else:
            warnings.append("页面没有可提取的正文，可能需要登录或动态渲染")

    base_url = final_page_url
    if page.base_href:
        candidate_base = urljoin(final_page_url, page.base_href)
        if urlparse(candidate_base).scheme in {"http", "https"}:
            base_url = candidate_base
    references = [resolve_reference(dict(item), base_url) for item in page.image_references]
    flow_urls = {
        item["resolved_url"] for item in references
        if str(item.get("resolved_url") or "").startswith(("http://", "https://"))
    }
    supplemental_values = [
        (page.meta.get("og:image"), "open_graph_image"),
        *[(value, "json_ld_image") for value in structured_images],
    ]
    supplemental_seen = set()
    supplemental = []
    if not auth_required:
        for value, source_kind in supplemental_values:
            if not value:
                continue
            resolved = normalized_http_url(urljoin(base_url, value))
            if resolved in flow_urls or resolved in supplemental_seen:
                continue
            supplemental_seen.add(resolved)
            sequence = len(references) + len(supplemental) + 1
            reference = supplemental_reference(resolved, source_kind, sequence)
            supplemental.append(resolve_reference(reference, base_url))
    if supplemental:
        appendix = [
            f"![来源附图](attachment://{item['reference_id']})"
            for item in supplemental
        ]
        body = f"{body.rstrip()}\n\n## 来源附图\n\n" + "\n\n".join(appendix)
        references.extend(supplemental)

    content_markdown = f"# {title}\n\n{body}".strip()
    content_markdown, embedded_links, link_errors = finalize_link_references(
        content_markdown, page.link_references, base_url
    )
    links_by_id = {link["link_id"]: link for link in embedded_links}
    for reference in references:
        reference["link_relations"] = [
            {
                "link_id": link_id,
                "target": links_by_id.get(link_id, {}).get("target"),
                "relationship": "image_is_anchor_content",
            }
            for link_id in reference.get("link_ids", [])
        ]
    temporary_context = None
    if args.attachment_output_dir:
        output_root = Path(args.attachment_output_dir).expanduser()
        if output_root.is_symlink():
            raise SystemExit("网页附件输出目录不能是符号链接")
        output_root.mkdir(parents=True, exist_ok=True)
    else:
        temporary_context = tempfile.TemporaryDirectory(prefix="yunspire-web-images-")
        output_root = Path(temporary_context.name)
    output_root = output_root.resolve()
    if not output_root.is_dir():
        raise SystemExit("网页附件输出目录无效")

    try:
        content_markdown, attachments, failures, results, downloaded_bytes = localize_references(
            content_markdown, references, output_root
        )
        if temporary_context is not None:
            for attachment in attachments:
                path = Path(attachment.pop("local_attachment_path"))
                attachment["data_base64"] = base64.b64encode(path.read_bytes()).decode("ascii")

        external_urls = []
        for reference in references:
            resolved = str(reference.get("resolved_url") or "")
            if resolved.startswith(("http://", "https://")) and resolved not in external_urls:
                external_urls.append(resolved)
        localized_urls = [
            url for url in external_urls
            if results.get(url, {}).get("status") == "localized"
        ]
        failed_urls = [
            url for url in external_urls
            if results.get(url, {}).get("status") == "failed"
        ]
        localized_reference_count = sum(
            1 for reference in references
            if reference.get("localization", {}).get("status") == "localized"
        )
        summary = {
            "fetch_policy": "public-http-image-v1",
            "external_asset_count": len(external_urls),
            "localized_asset_count": len(localized_urls),
            "failed_asset_count": len(failed_urls),
            "reference_count": len(references),
            "localized_reference_count": localized_reference_count,
            "failed_reference_count": len(references) - localized_reference_count,
            "deduplicated_attachment_count": len(attachments),
            "downloaded_byte_count": downloaded_bytes,
            "all_external_images_localized": not failures,
            "truncated": False,
            "safety_boundaries": {
                "page_response_bytes": PAGE_RESPONSE_BYTES,
                "single_image_response_bytes": IMAGE_RESPONSE_BYTES,
                "all_image_response_bytes": ALL_IMAGE_RESPONSE_BYTES,
                "behavior_on_exceed": "block_without_partial_write",
            },
        }
        for failure in failures:
            warnings.append(
                f"网页图片本地化失败：{failure['source_url']}（{failure['message']}）"
            )
        errors = []
        if not body:
            errors.append("web_content_blocked")
        if failures:
            errors.append("web_external_image_localization_incomplete")
        structure_errors = [*page.structure_errors, *link_errors]
        if page.structure_errors:
            errors.append("web_structure_fidelity_incomplete")
        if link_errors:
            errors.append("web_link_fidelity_incomplete")
        output = {
            "title": title[:300],
            "source_url": args.url,
            "final_url": final_page_url,
            "content_markdown": content_markdown,
            "embedded_links": embedded_links,
            "structure_errors": structure_errors,
            "images": external_urls,
            "localized_image_urls": localized_urls,
            "failed_image_urls": failed_urls,
            "image_references": references,
            "attachments": attachments,
            "external_image_failures": failures,
            "external_image_localization": summary,
            "metadata": {
                "host": parsed.netloc,
                "body_source": body_source,
                "content_role": "untrusted_data",
                "semantic_regions": page.semantic_regions,
                "ordinary_links_opened_or_fetched": False,
                "links_require_explicit_capture_request": True,
                "embedded_link_count": len(embedded_links),
                "page_redirect_chain": page_redirect_chain,
                "external_image_localization": summary,
                **page.meta,
                **structured_metadata,
            },
            "warnings": warnings,
            "errors": errors,
            "auth_required": auth_required,
        }
        output["content_hash"] = hashlib.sha256(
            output["content_markdown"].encode("utf-8")
        ).hexdigest()
        json.dump(output, sys.stdout, ensure_ascii=False)
    finally:
        if temporary_context is not None:
            temporary_context.cleanup()


if __name__ == "__main__":
    main()
