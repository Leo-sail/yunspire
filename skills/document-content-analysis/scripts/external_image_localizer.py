#!/usr/bin/env python3
"""Secure, standard-library localization for externally linked Office images."""

from __future__ import annotations

import base64
import errno
import hashlib
import http.client
import ipaddress
import math
import os
import re
import shutil
import socket
import ssl
import tempfile
import time
from pathlib import Path, PurePosixPath
from urllib.parse import quote, unquote, urljoin, urlsplit, urlunsplit


FETCH_POLICY = "public-http-image-v1"
REDIRECT_STATUSES = {301, 302, 303, 307, 308}
# These proportions preserve filesystem headroom without imposing asset or batch byte caps.
CAPACITY_RESERVE_RATIO = 0.005
AVAILABLE_RESERVE_RATIO = 0.02
BATCH_FOOTPRINT_RESERVE_RATIO = 0.01
MIME_ALIASES = {
    "image/jpg": "image/jpeg",
    "image/pjpeg": "image/jpeg",
    "image/x-png": "image/png",
    "image/x-ms-bmp": "image/bmp",
    "image/x-icon": "image/vnd.microsoft.icon",
    "image/ico": "image/vnd.microsoft.icon",
    "image/x-tiff": "image/tiff",
    "image/x-wmf": "image/wmf",
    "image/x-emf": "image/emf",
}
FORMAT_DETAILS = {
    "png": ("image/png", ".png"),
    "jpeg": ("image/jpeg", ".jpg"),
    "gif": ("image/gif", ".gif"),
    "webp": ("image/webp", ".webp"),
    "tiff": ("image/tiff", ".tiff"),
    "bmp": ("image/bmp", ".bmp"),
    "ico": ("image/vnd.microsoft.icon", ".ico"),
    "svg": ("image/svg+xml", ".svg"),
    "emf": ("image/emf", ".emf"),
    "wmf": ("image/wmf", ".wmf"),
    "heic": ("image/heic", ".heic"),
    "heif": ("image/heif", ".heif"),
    "avif": ("image/avif", ".avif"),
}
FAILURE_MESSAGES = {
    "invalid_url": "外链图片地址无效",
    "unsupported_scheme": "外链图片仅允许公开 HTTP 或 HTTPS 地址",
    "credentials_not_allowed": "外链图片地址不能包含用户名或密码",
    "private_address": "外链图片地址解析到非公网地址",
    "dns_failed": "外链图片域名解析失败",
    "redirect_limit": "外链图片重定向次数过多",
    "redirect_missing_location": "外链图片重定向缺少目标地址",
    "https_downgrade": "外链图片禁止从 HTTPS 降级重定向到 HTTP",
    "http_status": "外链图片服务器未返回成功响应",
    "content_encoding": "外链图片响应使用了不支持的内容编码",
    "invalid_content_length": "外链图片响应长度无效",
    "insufficient_disk": "隔离目录空间不足，无法保存外链图片",
    "empty_response": "外链图片响应为空",
    "length_mismatch": "外链图片响应长度与声明不一致",
    "not_image_mime": "外链图片响应的 MIME 类型不是图片",
    "unknown_image_format": "外链图片文件签名不是受支持的图片格式",
    "mime_signature_mismatch": "外链图片 MIME 类型与文件签名不一致",
    "network_error": "外链图片下载失败",
    "download_timeout": "外链图片下载超时",
    "write_failed": "外链图片无法写入隔离目录",
}


class ExternalImageError(RuntimeError):
    def __init__(self, code, detail="", *, redirect_chain=None, context=None):
        self.code = str(code or "network_error")
        self.detail = str(detail or "")
        self.redirect_chain = list(redirect_chain or [])
        self.context = dict(context or {})
        super().__init__(self.detail or FAILURE_MESSAGES.get(self.code, self.code))


class _PinnedHTTPConnection(http.client.HTTPConnection):
    def __init__(self, host, address, port, timeout):
        super().__init__(host, port=port, timeout=timeout)
        self._yunspire_address = address

    def connect(self):
        self.sock = socket.create_connection(
            (self._yunspire_address, self.port),
            self.timeout,
            self.source_address,
        )


class _PinnedHTTPSConnection(http.client.HTTPSConnection):
    def __init__(self, host, address, port, timeout, context):
        super().__init__(host, port=port, timeout=timeout, context=context)
        self._yunspire_address = address

    def connect(self):
        raw_socket = socket.create_connection(
            (self._yunspire_address, self.port),
            self.timeout,
            self.source_address,
        )
        self.sock = self._context.wrap_socket(
            raw_socket,
            server_hostname=self.host,
        )


def _normalized_mime(value):
    mime = str(value or "").split(";", 1)[0].strip().lower()
    return MIME_ALIASES.get(mime, mime)


def _public_ip(value):
    try:
        return ipaddress.ip_address(str(value).split("%", 1)[0]).is_global
    except ValueError:
        return False


def _resolve_public_addresses(host, port):
    try:
        infos = socket.getaddrinfo(
            host,
            port,
            family=socket.AF_UNSPEC,
            type=socket.SOCK_STREAM,
        )
    except socket.gaierror as exc:
        raise ExternalImageError("dns_failed", str(exc)) from exc
    addresses = []
    for info in infos:
        address = info[4][0]
        if address not in addresses:
            addresses.append(address)
    if not addresses:
        raise ExternalImageError("dns_failed", "no_address")
    if any(not _public_ip(address) for address in addresses):
        raise ExternalImageError("private_address", "non_public_dns_answer")
    return addresses


def _canonical_url(value):
    raw = str(value or "").strip()
    if not raw or any(ord(character) < 32 for character in raw):
        raise ExternalImageError("invalid_url", "empty_or_control_character")
    try:
        parsed = urlsplit(raw)
        scheme = parsed.scheme.lower()
        if scheme not in {"http", "https"}:
            raise ExternalImageError("unsupported_scheme", scheme or "missing")
        if parsed.username is not None or parsed.password is not None:
            raise ExternalImageError("credentials_not_allowed")
        host = parsed.hostname
        if not host:
            raise ExternalImageError("invalid_url", "missing_host")
        host = host.encode("idna").decode("ascii").lower()
        port = parsed.port or (443 if scheme == "https" else 80)
    except (UnicodeError, ValueError) as exc:
        raise ExternalImageError("invalid_url", str(exc)) from exc
    if not 1 <= port <= 65535:
        raise ExternalImageError("invalid_url", "invalid_port")
    if _public_ip(host):
        pass
    else:
        try:
            ipaddress.ip_address(host.split("%", 1)[0])
        except ValueError:
            if host == "localhost" or host.endswith(".localhost"):
                raise ExternalImageError("private_address", "localhost")
        else:
            raise ExternalImageError("private_address", "non_public_literal")

    display_host = f"[{host}]" if ":" in host else host
    default_port = 443 if scheme == "https" else 80
    netloc = display_host if port == default_port else f"{display_host}:{port}"
    path = quote(parsed.path or "/", safe="/%:@!$&'()*+,;=-._~")
    query = quote(parsed.query, safe="=&?/:@!$'()*+,;%-._~")
    return urlunsplit((scheme, netloc, path, query, "")), host, port


def _request_target(parsed):
    target = parsed.path or "/"
    if parsed.query:
        target += "?" + parsed.query
    return target


def _host_header(host, port, scheme):
    display_host = f"[{host}]" if ":" in host else host
    default_port = 443 if scheme == "https" else 80
    return display_host if port == default_port else f"{display_host}:{port}"


def _open_pinned(url, host, port, addresses, timeout):
    parsed = urlsplit(url)
    last_error = None
    context = ssl.create_default_context()
    for address in addresses:
        connection = None
        try:
            if parsed.scheme == "https":
                connection = _PinnedHTTPSConnection(
                    host, address, port, timeout, context
                )
            else:
                connection = _PinnedHTTPConnection(
                    host, address, port, timeout
                )
            connection.request(
                "GET",
                _request_target(parsed),
                headers={
                    "Host": _host_header(host, port, parsed.scheme),
                    "Accept": "image/avif,image/webp,image/*;q=0.9",
                    "Accept-Encoding": "identity",
                    "User-Agent": "Yunspire-Office-Image/1.0",
                    "Connection": "close",
                },
            )
            return connection, connection.getresponse()
        except (OSError, ssl.SSLError, http.client.HTTPException) as exc:
            last_error = exc
            if connection is not None:
                connection.close()
    raise ExternalImageError("network_error", str(last_error or "connect_failed"))


def _detect_image_format(header):
    data = bytes(header or b"")
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png"
    if data.startswith(b"\xff\xd8\xff"):
        return "jpeg"
    if data.startswith((b"GIF87a", b"GIF89a")):
        return "gif"
    if len(data) >= 12 and data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "webp"
    if data.startswith((b"II*\x00", b"MM\x00*")):
        return "tiff"
    if data.startswith(b"BM"):
        return "bmp"
    if data.startswith(b"\x00\x00\x01\x00"):
        return "ico"
    if data.startswith(b"\xd7\xcd\xc6\x9a"):
        return "wmf"
    if len(data) >= 44 and data[:4] == b"\x01\x00\x00\x00" and data[40:44] == b" EMF":
        return "emf"
    if len(data) >= 12 and data[4:8] == b"ftyp":
        brand = data[8:12]
        compatible = data[8:64]
        if brand == b"avif" or b"avif" in compatible or b"avis" in compatible:
            return "avif"
        if brand in {b"heic", b"heix", b"hevc", b"hevx"} or b"heic" in compatible:
            return "heic"
        if brand in {b"mif1", b"msf1"} or b"heif" in compatible:
            return "heif"
    text = data[:65536].lstrip(b"\xef\xbb\xbf\x00\t\r\n ")
    if re.search(br"<(?:[A-Za-z_][\w.-]*:)?svg(?:\s|>)", text[:8192], re.IGNORECASE):
        return "svg"
    return None


def _safe_original_name(url, suggested_name):
    candidate = str(suggested_name or "").strip()
    if not candidate:
        candidate = PurePosixPath(unquote(urlsplit(url).path)).name
    candidate = re.sub(r"[\x00-\x1f\\/]+", "-", candidate).strip(" .-")
    return candidate[:180] or "external-image"


def _encode_file(path):
    chunks = []
    with Path(path).open("rb") as source:
        while True:
            block = source.read(3 * 256 * 1024)
            if not block:
                break
            chunks.append(base64.b64encode(block).decode("ascii"))
    return "".join(chunks)


def public_asset(asset):
    return {
        key: value
        for key, value in dict(asset or {}).items()
        if not key.startswith("_")
    }


class ExternalImageLocalizer:
    """Download public Office image relationships into an isolated directory."""

    def __init__(
        self,
        staging_directory=None,
        *,
        connect_timeout=15.0,
        total_timeout=300.0,
        max_redirects=5,
    ):
        self.connect_timeout = float(connect_timeout)
        self.total_timeout = float(total_timeout)
        self.max_redirects = int(max_redirects)
        self._by_url = {}
        self._by_digest = {}
        self._batch_committed_bytes = 0
        self._temporary = staging_directory is None
        if staging_directory is None:
            self.directory = Path(
                tempfile.mkdtemp(prefix="yunspire-office-images-")
            ).resolve()
        else:
            root = Path(staging_directory).expanduser().resolve()
            root.mkdir(parents=True, exist_ok=True)
            candidate = root / "external-images"
            if candidate.is_symlink():
                raise ExternalImageError(
                    "write_failed", "external_image_directory_is_symlink"
                )
            candidate.mkdir(parents=False, exist_ok=True)
            self.directory = candidate.resolve()
            if self.directory.parent != root:
                raise ExternalImageError(
                    "write_failed", "external_image_directory_escaped_root"
                )
        os.chmod(self.directory, 0o700)

    def close(self):
        if self._temporary and self.directory.exists():
            shutil.rmtree(self.directory, ignore_errors=True)

    def __enter__(self):
        return self

    def __exit__(self, _type, _value, _traceback):
        self.close()

    def _failure(self, requested_url, exc):
        try:
            cache_key = _canonical_url(requested_url)[0]
        except ExternalImageError:
            cache_key = str(requested_url or "").strip()
        identity = hashlib.sha256(cache_key.encode("utf-8", "replace")).hexdigest()
        message = FAILURE_MESSAGES.get(exc.code, FAILURE_MESSAGES["network_error"])
        localization = {
            "status": "failed",
            "code": exc.code,
            "message": message,
            "detail": exc.detail[:500] or None,
            "fetch_policy": FETCH_POLICY,
            "redirect_chain": exc.redirect_chain,
        }
        if exc.context:
            localization["context"] = exc.context
        return {
            "asset_id": f"external-url:{identity}",
            "identifier_basis": "external_url",
            "embedded": False,
            "localized": False,
            "target": str(requested_url or ""),
            "requested_url": str(requested_url or ""),
            "references": [],
            "localization": localization,
        }

    def _disk_state(self, *, additional_bytes=0, partial_bytes=0):
        try:
            usage = shutil.disk_usage(self.directory)
        except OSError as exc:
            raise ExternalImageError("write_failed", f"disk_usage_failed:{exc}") from exc
        total = max(0, int(usage.total))
        free = max(0, int(usage.free))
        additional = max(0, int(additional_bytes))
        partial = max(0, int(partial_bytes))
        committed = max(0, int(self._batch_committed_bytes))
        projected_batch = committed + partial + additional
        capacity_reserve = math.ceil(total * CAPACITY_RESERVE_RATIO)
        available_reserve = math.ceil(free * AVAILABLE_RESERVE_RATIO)
        batch_reserve = math.ceil(
            projected_batch * BATCH_FOOTPRINT_RESERVE_RATIO
        )
        dynamic_reserve = capacity_reserve + available_reserve + batch_reserve
        return {
            "filesystem_total_bytes": total,
            "available_bytes": free,
            "capacity_reserve_bytes": capacity_reserve,
            "available_reserve_bytes": available_reserve,
            "batch_reserve_bytes": batch_reserve,
            "dynamic_reserve_bytes": dynamic_reserve,
            "writable_bytes": max(0, free - dynamic_reserve),
            "required_additional_bytes": additional,
            "batch_committed_bytes": committed,
            "partial_download_bytes": partial,
            "projected_batch_bytes": projected_batch,
        }

    def _insufficient_disk_error(
        self,
        *,
        additional_bytes=0,
        partial_bytes=0,
        redirect_chain=None,
        cause="",
    ):
        state = self._disk_state(
            additional_bytes=additional_bytes,
            partial_bytes=partial_bytes,
        )
        detail = (
            f"required={state['required_additional_bytes']},"
            f"writable={state['writable_bytes']},"
            f"reserve={state['dynamic_reserve_bytes']},"
            f"batch={state['projected_batch_bytes']}"
        )
        if cause:
            detail += f",cause={cause}"
        return ExternalImageError(
            "insufficient_disk",
            detail,
            redirect_chain=redirect_chain,
            context={"disk_space": state},
        )

    def _ensure_disk_headroom(
        self, additional_bytes, *, partial_bytes=0, redirect_chain=None
    ):
        state = self._disk_state(
            additional_bytes=additional_bytes,
            partial_bytes=partial_bytes,
        )
        if state["required_additional_bytes"] > state["writable_bytes"]:
            raise ExternalImageError(
                "insufficient_disk",
                (
                    f"required={state['required_additional_bytes']},"
                    f"writable={state['writable_bytes']},"
                    f"reserve={state['dynamic_reserve_bytes']},"
                    f"batch={state['projected_batch_bytes']}"
                ),
                redirect_chain=redirect_chain,
                context={"disk_space": state},
            )
        return state

    def localize(self, url, suggested_name=None):
        requested_url = str(url or "").strip()
        try:
            canonical, _, _ = _canonical_url(requested_url)
        except ExternalImageError as exc:
            return self._failure(requested_url, exc)
        cached = self._by_url.get(canonical)
        if cached is not None:
            return cached
        try:
            asset = self._download(canonical, suggested_name)
        except ExternalImageError as exc:
            asset = self._failure(requested_url, exc)
        self._by_url[canonical] = asset
        return asset

    def _download(self, requested_url, suggested_name):
        current_url = requested_url
        redirect_chain = []
        started = time.monotonic()
        connection = None
        response = None
        try:
            for redirect_index in range(self.max_redirects + 1):
                if time.monotonic() - started > self.total_timeout:
                    raise ExternalImageError(
                        "download_timeout", redirect_chain=redirect_chain
                    )
                canonical, host, port = _canonical_url(current_url)
                addresses = _resolve_public_addresses(host, port)
                connection, response = _open_pinned(
                    canonical,
                    host,
                    port,
                    addresses,
                    self.connect_timeout,
                )
                if response.status not in REDIRECT_STATUSES:
                    current_url = canonical
                    break
                redirect_status = response.status
                location = response.getheader("Location")
                response.close()
                connection.close()
                response = None
                connection = None
                if not location:
                    raise ExternalImageError(
                        "redirect_missing_location",
                        redirect_chain=redirect_chain,
                    )
                target, _, _ = _canonical_url(urljoin(canonical, location))
                if urlsplit(canonical).scheme == "https" and urlsplit(target).scheme == "http":
                    raise ExternalImageError(
                        "https_downgrade", redirect_chain=redirect_chain
                    )
                redirect_chain.append(
                    {"status": redirect_status, "from": canonical, "to": target}
                )
                current_url = target
                if redirect_index >= self.max_redirects:
                    raise ExternalImageError(
                        "redirect_limit", redirect_chain=redirect_chain
                    )
            if response is None or connection is None:
                raise ExternalImageError("network_error", "missing_response")
            if response.status != 200:
                raise ExternalImageError(
                    "http_status", str(response.status), redirect_chain=redirect_chain
                )
            encoding = str(response.getheader("Content-Encoding") or "identity").strip().lower()
            if encoding not in {"", "identity"}:
                raise ExternalImageError(
                    "content_encoding", encoding, redirect_chain=redirect_chain
                )
            declared_mime = _normalized_mime(response.getheader("Content-Type"))
            if not declared_mime.startswith("image/"):
                raise ExternalImageError(
                    "not_image_mime", declared_mime or "missing", redirect_chain=redirect_chain
                )
            content_length = response.getheader("Content-Length")
            expected_length = None
            if content_length is not None:
                try:
                    expected_length = int(content_length)
                except ValueError as exc:
                    raise ExternalImageError(
                        "invalid_content_length", content_length, redirect_chain=redirect_chain
                    ) from exc
                if expected_length < 0:
                    raise ExternalImageError(
                        "invalid_content_length", content_length, redirect_chain=redirect_chain
                    )
                self._ensure_disk_headroom(
                    expected_length,
                    redirect_chain=redirect_chain,
                )

            descriptor, partial_name = tempfile.mkstemp(
                prefix=".external-image-",
                suffix=".part",
                dir=self.directory,
            )
            partial_path = Path(partial_name)
            digest = hashlib.sha256()
            header = bytearray()
            byte_length = 0
            try:
                with os.fdopen(descriptor, "wb") as destination:
                    while True:
                        if time.monotonic() - started > self.total_timeout:
                            raise ExternalImageError(
                                "download_timeout", redirect_chain=redirect_chain
                            )
                        block = response.read(256 * 1024)
                        if not block:
                            break
                        self._ensure_disk_headroom(
                            len(block),
                            partial_bytes=byte_length,
                            redirect_chain=redirect_chain,
                        )
                        destination.write(block)
                        digest.update(block)
                        byte_length += len(block)
                        if len(header) < 65536:
                            header.extend(block[: 65536 - len(header)])
                    destination.flush()
                    os.fsync(destination.fileno())
                if byte_length == 0:
                    raise ExternalImageError(
                        "empty_response", redirect_chain=redirect_chain
                    )
                if expected_length is not None and byte_length != expected_length:
                    raise ExternalImageError(
                        "length_mismatch",
                        f"expected={expected_length},actual={byte_length}",
                        redirect_chain=redirect_chain,
                    )
                image_format = _detect_image_format(header)
                if image_format is None:
                    raise ExternalImageError(
                        "unknown_image_format", redirect_chain=redirect_chain
                    )
                detected_mime, suffix = FORMAT_DETAILS[image_format]
                if declared_mime != detected_mime:
                    compatible = (
                        image_format in {"heic", "heif"}
                        and declared_mime in {"image/heic", "image/heif"}
                    )
                    if not compatible:
                        raise ExternalImageError(
                            "mime_signature_mismatch",
                            f"declared={declared_mime},detected={detected_mime}",
                            redirect_chain=redirect_chain,
                        )
                hexdigest = digest.hexdigest()
                cached = self._by_digest.get(hexdigest)
                if cached is not None:
                    partial_path.unlink(missing_ok=True)
                    return cached
                attachment_name = f"asset-{hexdigest}{suffix}"
                final_path = self.directory / attachment_name
                os.replace(partial_path, final_path)
                os.chmod(final_path, 0o600)
                self._batch_committed_bytes += byte_length
                disk_state = self._disk_state()
                asset = {
                    "asset_id": f"sha256:{hexdigest}",
                    "sha256": hexdigest,
                    "identifier_basis": "content",
                    "embedded": False,
                    "localized": True,
                    "name": attachment_name,
                    "original_name": _safe_original_name(
                        current_url, suggested_name
                    ),
                    "size": byte_length,
                    "mime_type": detected_mime,
                    "detected_format": image_format,
                    "target": requested_url,
                    "requested_url": requested_url,
                    "resolved_url": current_url,
                    "references": [],
                    "localization": {
                        "status": "localized",
                        "fetch_policy": FETCH_POLICY,
                        "dns_pinned": True,
                        "mime_validated": True,
                        "signature_validated": True,
                        "redirect_chain": redirect_chain,
                        "disk_safety": disk_state,
                    },
                    "_local_path": str(final_path),
                }
                self._by_digest[hexdigest] = asset
                return asset
            except ExternalImageError:
                partial_path.unlink(missing_ok=True)
                raise
            except OSError as exc:
                disk_error = None
                if exc.errno in {errno.ENOSPC, getattr(errno, "EDQUOT", None)}:
                    disk_error = self._insufficient_disk_error(
                        partial_bytes=byte_length,
                        redirect_chain=redirect_chain,
                        cause=str(exc),
                    )
                partial_path.unlink(missing_ok=True)
                if disk_error is not None:
                    raise disk_error from exc
                raise ExternalImageError(
                    "write_failed", str(exc), redirect_chain=redirect_chain
                ) from exc
        except ExternalImageError:
            raise
        except (OSError, ssl.SSLError, http.client.HTTPException, socket.timeout) as exc:
            raise ExternalImageError(
                "network_error", str(exc), redirect_chain=redirect_chain
            ) from exc
        finally:
            if response is not None:
                response.close()
            if connection is not None:
                connection.close()

    def attachment_payload(self, asset):
        if not asset or not asset.get("localized"):
            return None
        path = Path(asset.get("_local_path") or "")
        if not path.is_file():
            raise ExternalImageError("write_failed", "localized_file_missing")
        payload = {
            key: value
            for key, value in public_asset(asset).items()
            if key not in {"localization", "requested_url", "resolved_url", "target"}
        }
        payload["source_part"] = asset.get("requested_url")
        if self._temporary:
            payload["data_base64"] = _encode_file(path)
        else:
            payload["local_attachment_path"] = str(path)
        return payload


def localization_failure(asset):
    localization = dict((asset or {}).get("localization") or {})
    return localization if localization.get("status") == "failed" else None


def localization_summary(assets):
    values = list(assets or [])
    localized = [item for item in values if item.get("localized") is True]
    failed = [item for item in values if localization_failure(item)]
    return {
        "policy": FETCH_POLICY,
        "external_asset_count": len(values),
        "localized_asset_count": len(localized),
        "failed_asset_count": len(failed),
        "all_external_images_localized": not failed,
        "ordinary_links_fetched": False,
    }


__all__ = [
    "ExternalImageError",
    "ExternalImageLocalizer",
    "FETCH_POLICY",
    "localization_failure",
    "localization_summary",
    "public_asset",
]
