#!/usr/bin/env python3
"""Yunspire first-party public video discovery, download, and local analysis."""
import argparse
import ipaddress
import json
import locale as system_locale
import math
import mimetypes
import os
import re
import shutil
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urljoin, urlparse
from urllib.request import HTTPRedirectHandler, Request, build_opener

import media_discovery

MAX_PAGE_BYTES = 12 * 1024 * 1024
USER_AGENT = "Yunspire/0.2 local public media collector"
AUTH_CONTEXT = {"allowed_hosts": set(), "headers": {}}
SENSITIVE_HEADERS = ("Cookie", "Authorization")
PROGRESS_FILE = os.environ.get("YUNSPIRE_PROGRESS_FILE", "").strip()


def progress_checkpoint(label):
    if not PROGRESS_FILE:
        return
    try:
        with open(PROGRESS_FILE, "a", encoding="utf-8", newline="\n") as stream:
            stream.write(f"{time.time_ns()}\t{label}\n")
    except OSError:
        pass


def normalize_bcp47_locale(value):
    value = str(value or "").strip()
    if not value or len(value) > 63:
        raise argparse.ArgumentTypeError("语音识别语言必须是有效的 BCP-47 标签")
    parts = value.split("-")
    if not 2 <= len(parts[0]) <= 8 or not parts[0].isalpha() or not parts[0].isascii():
        raise argparse.ArgumentTypeError("语音识别语言必须以 2 至 8 位字母语言代码开头")
    normalized = [parts[0].lower()]
    for part in parts[1:]:
        if not part or len(part) > 8 or not part.isalnum() or not part.isascii():
            raise argparse.ArgumentTypeError("语音识别语言包含无效的 BCP-47 子标签")
        if len(part) == 4 and part.isalpha():
            normalized.append(part.title())
        elif (len(part) == 2 and part.isalpha()) or (len(part) == 3 and part.isdigit()):
            normalized.append(part.upper())
        else:
            normalized.append(part.lower())
    return "-".join(normalized)


def default_speech_locale():
    configured = os.environ.get("YUNSPIRE_SPEECH_LOCALE", "").strip()
    if configured:
        return normalize_bcp47_locale(configured)
    if sys.platform == "win32":
        try:
            import ctypes

            buffer = ctypes.create_unicode_buffer(85)
            if ctypes.windll.kernel32.GetUserDefaultLocaleName(buffer, len(buffer)) > 1:
                return normalize_bcp47_locale(buffer.value)
        except (AttributeError, OSError, ValueError, argparse.ArgumentTypeError):
            pass
    try:
        detected = system_locale.getlocale()[0]
    except (ValueError, TypeError):
        detected = None
    if detected:
        try:
            return normalize_bcp47_locale(detected.replace("_", "-"))
        except argparse.ArgumentTypeError:
            pass
    return "zh-CN"


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


def validate_public_url(value):
    parsed = urlparse(value)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("URL 只允许 http 或 https 协议")
    if parsed.username or parsed.password:
        raise ValueError("URL 不能包含用户名或密码")
    host = parsed.hostname.rstrip(".").lower()
    if host == "localhost" or host.endswith((".localhost", ".local")):
        raise ValueError("禁止访问本机或局域网地址")
    try:
        addresses = socket.getaddrinfo(host, parsed.port or (443 if parsed.scheme == "https" else 80), type=socket.SOCK_STREAM)
    except socket.gaierror as exc:
        raise ValueError(f"无法解析公开地址：{exc}") from exc
    if not addresses:
        raise ValueError("URL 没有可验证的公开地址")
    for address in addresses:
        ip = ipaddress.ip_address(address[4][0].split("%", 1)[0])
        if not ip.is_global:
            raise ValueError("禁止访问私网、回环、链路本地或保留地址")
    return value


class PublicRedirectHandler(HTTPRedirectHandler):
    def redirect_request(self, request, file_pointer, code, message, headers, new_url):
        validate_public_url(new_url)
        redirected = super().redirect_request(request, file_pointer, code, message, headers, new_url)
        return apply_authorization(redirected) if redirected else None


PUBLIC_OPENER = build_opener(PublicRedirectHandler())


def emit(payload):
    json.dump(payload, sys.stdout, ensure_ascii=False)


def base_result(url):
    return {
        "source_url": url,
        "title": "",
        "platform": media_discovery.platform_name(url),
        "source_kind": "url",
        "status": "pending",
        "transcript": "",
        "transcript_segments": [],
        "frames": [],
        "media_path": "",
        "metadata": {},
        "warnings": [],
        "errors": [],
        "auth_required": False,
    }


def read_limited(response, limit):
    data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError("响应超过安全大小上限")
    return data


def fetch(url, accept, referer="", timeout=30, extra_headers=None, retries=2):
    validate_public_url(url)
    headers = {"User-Agent": USER_AGENT, "Accept": accept, "Accept-Language": "zh-CN,zh;q=0.9,en;q=0.7"}
    if referer:
        headers["Referer"] = referer
    for name, value in (extra_headers or {}).items():
        if name.lower() in {"cookie", "authorization"}:
            continue
        if isinstance(value, str) and "\r" not in value and "\n" not in value:
            headers[name] = value
    for attempt in range(retries + 1):
        try:
            return PUBLIC_OPENER.open(apply_authorization(Request(url, headers=headers)), timeout=timeout)
        except HTTPError as exc:
            if exc.code not in {408, 425, 429, 500, 502, 503, 504} or attempt >= retries:
                raise
        except URLError:
            if attempt >= retries:
                raise
        time.sleep(min(2.0, 0.35 * (attempt + 1)))
    raise URLError("公开媒体请求失败")


def page_requires_authorization(text):
    value = text[:12000].lower()
    return any(signal in value for signal in (
        "captcha", "verify you are human", "access denied", "请完成验证", "安全验证",
        "请登录后继续", "扫码登录", "登录后查看", "内容不可见", "环境异常",
    ))


def fetch_page(url):
    with fetch(url, "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.2") as response:
        content_type = response.headers.get_content_type() or ""
        final_url = response.geturl()
        direct_media = content_type.startswith(("video/", "audio/")) or urlparse(final_url).path.lower().endswith((".mp4", ".mov", ".m4v", ".webm", ".m3u8", ".mp3", ".m4a"))
        if direct_media:
            return "", final_url, content_type
        raw = read_limited(response, MAX_PAGE_BYTES)
        encoding = response.headers.get_content_charset() or "utf-8"
    return raw.decode(encoding, errors="replace"), final_url, content_type


def download_file(url, target, referer):
    with fetch(url, "video/*,audio/*,application/octet-stream,*/*;q=0.5", referer, 60) as response:
        content_type = (response.headers.get_content_type() or "").lower()
        if content_type in {"text/html", "application/xhtml+xml"}:
            raise ValueError("媒体候选返回网页而不是媒体文件")
        with target.open("wb") as output:
            written = 0
            next_checkpoint = 8 * 1024 * 1024
            while True:
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                output.write(chunk)
                written += len(chunk)
                if written >= next_checkpoint:
                    progress_checkpoint(f"media-download:{written}")
                    next_checkpoint = written + 8 * 1024 * 1024
    progress_checkpoint(f"media-download-complete:{target.stat().st_size}")
    return target


def parse_attribute_list(value):
    output = {}
    for match in re.finditer(r'([A-Z0-9-]+)=("[^"]*"|[^,]*)', value):
        output[match.group(1)] = match.group(2).strip('"')
    return output


def read_text_url(url, referer):
    with fetch(url, "application/vnd.apple.mpegurl,text/vtt,text/plain,*/*;q=0.2", referer, 30) as response:
        return read_limited(response, 4 * 1024 * 1024).decode("utf-8", errors="replace"), response.geturl()


def select_hls_variant(text, playlist_url):
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    variants = []
    for index, line in enumerate(lines[:-1]):
        if line.startswith("#EXT-X-STREAM-INF:"):
            attrs = parse_attribute_list(line.split(":", 1)[1])
            bandwidth = int(attrs.get("AVERAGE-BANDWIDTH", attrs.get("BANDWIDTH", "0")) or 0)
            resolution = attrs.get("RESOLUTION", "0x0").lower().split("x", 1)
            pixels = 0
            if len(resolution) == 2 and all(item.isdigit() for item in resolution):
                pixels = int(resolution[0]) * int(resolution[1])
            variants.append((pixels, bandwidth, urljoin(playlist_url, lines[index + 1])))
    return max(variants, default=(0, 0, playlist_url))[2]


def parse_byte_range(value, previous_end=0):
    raw = value.strip().strip('"')
    length_text, separator, offset_text = raw.partition("@")
    length = int(length_text)
    offset = int(offset_text) if separator else previous_end
    if length <= 0 or offset < 0:
        raise ValueError("HLS 字节范围无效")
    return offset, length


def hls_objects(text, playlist_url):
    if not text.lstrip().startswith("#EXTM3U"):
        raise ValueError("HLS 清单缺少 EXTM3U 标记")
    if re.search(r"^#EXT-X-KEY:", text, re.I | re.M):
        raise ValueError("检测到加密 HLS，云枢不会规避加密或访问控制")
    if "#EXT-X-ENDLIST" not in text.upper():
        raise ValueError("检测到仍在更新的直播 HLS，云枢只处理已结束的公开媒体")
    objects = []
    pending_range = None
    previous_end = 0
    for raw in text.splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.upper().startswith("#EXT-X-MAP:"):
            attrs = parse_attribute_list(line.split(":", 1)[1])
            uri = attrs.get("URI", "")
            byte_range = parse_byte_range(attrs["BYTERANGE"], 0) if attrs.get("BYTERANGE") else None
            if uri:
                objects.append((urljoin(playlist_url, uri), byte_range))
            continue
        if line.upper().startswith("#EXT-X-BYTERANGE:"):
            pending_range = parse_byte_range(line.split(":", 1)[1], previous_end)
            continue
        if line.startswith("#"):
            continue
        objects.append((urljoin(playlist_url, line), pending_range))
        if pending_range:
            previous_end = pending_range[0] + pending_range[1]
            pending_range = None
    return objects


def write_hls_object(url, referer, byte_range, output):
    headers = {}
    expected_length = None
    if byte_range:
        start, length = byte_range
        headers["Range"] = f"bytes={start}-{start + length - 1}"
        expected_length = length
    with fetch(
        url,
        "video/mp2t,video/mp4,video/*,application/octet-stream,*/*;q=0.2",
        referer,
        30,
        extra_headers=headers,
    ) as response:
        if byte_range and getattr(response, "status", response.getcode()) != 206:
            raise ValueError("HLS 服务器没有遵守字节范围请求")
        written = 0
        while True:
            read_size = 1024 * 1024
            if expected_length is not None:
                remaining = expected_length - written
                if remaining <= 0:
                    break
                read_size = min(read_size, remaining)
            chunk = response.read(read_size)
            if not chunk:
                break
            output.write(chunk)
            written += len(chunk)
        if expected_length is not None and response.read(1):
            raise ValueError("HLS 字节范围响应超过请求长度")
    if expected_length is not None and written != expected_length:
        raise ValueError("HLS 字节范围响应长度不一致")
    return written


def download_hls(url, target, referer):
    text, playlist_url = read_text_url(url, referer)
    variant = select_hls_variant(text, playlist_url)
    if variant != playlist_url:
        text, playlist_url = read_text_url(variant, referer)
    objects = hls_objects(text, playlist_url)
    if not objects:
        raise ValueError("HLS 清单没有可下载分片")
    with target.open("wb") as output:
        for index, (object_url, byte_range) in enumerate(objects, 1):
            write_hls_object(object_url, referer, byte_range, output)
            progress_checkpoint(f"hls-segment:{index}")
    return target


def parse_subtitle(text):
    lines = []
    for raw in text.splitlines():
        value = raw.strip()
        if not value or value == "WEBVTT" or "-->" in value or re.fullmatch(r"\d+", value):
            continue
        value = re.sub(r"<[^>]+>", "", value)
        if value and (not lines or lines[-1] != value):
            lines.append(value)
    return "\n".join(lines)


def progress_signature(paths):
    file_count = 0
    byte_count = 0
    newest_change = 0
    for raw_path in paths:
        path = Path(raw_path)
        candidates = [path] if path.is_file() else (
            (candidate for candidate in path.rglob("*") if candidate.is_file())
            if path.is_dir()
            else ()
        )
        for candidate in candidates:
            try:
                stat = candidate.stat()
            except OSError:
                continue
            file_count += 1
            byte_count += stat.st_size
            newest_change = max(newest_change, stat.st_mtime_ns)
    return file_count, byte_count, newest_change


def monitor_progress_paths(paths, stop):
    previous = progress_signature(paths)
    while not stop.wait(1):
        current = progress_signature(paths)
        if current != previous:
            progress_checkpoint(
                f"native-output:files={current[0]}:bytes={current[1]}"
            )
            previous = current


def run(command, timeout=300, progress_paths=()):
    stop = threading.Event()
    monitor = None
    if progress_paths:
        monitor = threading.Thread(
            target=monitor_progress_paths,
            args=(tuple(progress_paths), stop),
            daemon=True,
        )
        monitor.start()
    options = {
        "capture_output": True,
        "text": True,
        "encoding": "utf-8",
        "errors": "strict",
        "timeout": timeout,
    }
    if sys.platform == "win32":
        options["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    try:
        return subprocess.run(command, **options)
    except (OSError, UnicodeError, subprocess.TimeoutExpired) as exc:
        return subprocess.CompletedProcess(command, 1, "", str(exc))
    finally:
        stop.set()
        if monitor is not None:
            monitor.join(timeout=2)


def windows_native_adapter(environment_name, packaged_name, build_name):
    configured = os.environ.get(environment_name, "").strip()
    candidates = []
    if configured:
        candidates.append(Path(configured).expanduser())
    candidates.extend(
        (
            Path(__file__).with_name("bin") / packaged_name,
            Path(__file__).resolve().parents[3]
            / "src-tauri"
            / "target"
            / "yunspire-native"
            / build_name,
        )
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    return None


def compile_media_adapter(output_dir):
    if sys.platform == "win32":
        adapter = windows_native_adapter(
            "YUNSPIRE_WINDOWS_MEDIA_ADAPTER",
            "yunspire-media.exe",
            "yunspire-media.exe",
        )
        if adapter:
            return adapter, ""
        return None, "Windows 媒体适配器未随安装包部署；请使用完整 Yunspire 安装包重新安装"
    if sys.platform != "darwin":
        return None, "当前平台不支持云枢本地视频帧/音轨适配器"
    compiler = shutil.which("clang")
    source = Path(__file__).with_name("yunspire_media.m")
    target = output_dir / "yunspire-media"
    if not compiler:
        return None, "本机缺少 Apple clang 编译器"
    result = run([compiler, "-fobjc-arc", str(source), "-framework", "AVFoundation", "-framework", "CoreMedia", "-framework", "CoreGraphics", "-framework", "Foundation", "-framework", "ImageIO", "-o", str(target)], 120)
    return (target, "") if result.returncode == 0 else (None, result.stderr[-800:])


def validate_windows_media_payload(payload, output_dir):
    if not isinstance(payload, dict) or payload.get("schema") != "yunspire.windows-media.v1":
        raise ValueError("windows_media_adapter_schema_mismatch")
    root = Path(output_dir).resolve()
    frames = payload.get("frames")
    timestamps = payload.get("frame_timestamps_ms")
    differences = payload.get("frame_difference_scores")
    warnings = payload.get("warnings")
    errors = payload.get("errors")
    if not all(isinstance(value, list) for value in (frames, timestamps, differences, warnings, errors)):
        raise ValueError("windows_media_adapter_contract_invalid")
    if len(frames) != len(timestamps) or len(frames) != len(differences):
        raise ValueError("windows_media_frame_manifest_mismatch")
    previous_timestamp = -1
    for timestamp in timestamps:
        if not isinstance(timestamp, int) or timestamp < previous_timestamp:
            raise ValueError("windows_media_frame_timestamps_invalid")
        previous_timestamp = timestamp
    if any(
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or not math.isfinite(value)
        or value < 0
        for value in differences
    ):
        raise ValueError("windows_media_frame_differences_invalid")
    normalized_frames = []
    for frame in frames:
        if not isinstance(frame, str):
            raise ValueError("windows_media_frame_path_invalid")
        path = Path(frame).resolve(strict=True)
        path.relative_to(root)
        with path.open("rb") as stream:
            if stream.read(2) != b"\xff\xd8":
                raise ValueError("windows_media_frame_format_invalid")
        normalized_frames.append(str(path))
    audio_path = payload.get("audio_path")
    if not isinstance(audio_path, str):
        raise ValueError("windows_media_audio_path_invalid")
    if audio_path:
        audio = Path(audio_path).resolve(strict=True)
        audio.relative_to(root)
        with audio.open("rb") as stream:
            header = stream.read(12)
        if len(header) != 12 or header[:4] != b"RIFF" or header[8:] != b"WAVE":
            raise ValueError("windows_media_audio_format_invalid")
        payload["audio_path"] = str(audio)
    duration = payload.get("duration_seconds")
    candidates = payload.get("frame_candidate_count")
    if (
        not isinstance(duration, (int, float))
        or isinstance(duration, bool)
        or not math.isfinite(duration)
        or duration < 0
    ):
        raise ValueError("windows_media_duration_invalid")
    if not isinstance(candidates, int) or isinstance(candidates, bool) or candidates < len(frames):
        raise ValueError("windows_media_candidate_count_invalid")
    if payload.get("frame_selection_method") != "yunspire-windows-mediafoundation-v1":
        raise ValueError("windows_media_selection_method_invalid")
    if not frames and not audio_path and not errors:
        raise ValueError("windows_media_empty_success")
    if any(not isinstance(value, str) for value in warnings + errors):
        raise ValueError("windows_media_diagnostics_invalid")
    payload["frames"] = normalized_frames
    return payload


def validate_windows_speech_payload(payload):
    if not isinstance(payload, dict) or payload.get("schema") != "yunspire.windows-speech.v1":
        raise ValueError("windows_speech_adapter_schema_mismatch")
    transcript = payload.get("transcript")
    segments = payload.get("segments")
    warnings = payload.get("warnings")
    errors = payload.get("errors")
    if not isinstance(transcript, str) or not all(isinstance(value, list) for value in (segments, warnings, errors)):
        raise ValueError("windows_speech_adapter_contract_invalid")
    if any(not isinstance(value, str) for value in warnings + errors):
        raise ValueError("windows_speech_diagnostics_invalid")
    on_device = payload.get("on_device")
    if not isinstance(on_device, bool):
        raise ValueError("windows_speech_on_device_state_invalid")
    previous_start = 0
    for segment in segments:
        if not isinstance(segment, dict) or not isinstance(segment.get("text"), str):
            raise ValueError("windows_speech_segment_invalid")
        start = segment.get("start_ms")
        end = segment.get("end_ms")
        if not isinstance(start, int) or not isinstance(end, int) or start < previous_start or end < start:
            raise ValueError("windows_speech_timestamps_invalid")
        previous_start = start
    if transcript and not segments:
        raise ValueError("windows_speech_segments_missing")
    if not transcript and not errors:
        raise ValueError("windows_speech_empty_success")
    if transcript and on_device is not True:
        raise ValueError("windows_speech_not_on_device")
    return payload


def local_media_analysis(media, output_dir, locale):
    adapter, detail = compile_media_adapter(output_dir)
    if not adapter:
        warning = detail or "媒体适配器没有返回编译信息"
        error = "windows_media_adapter_missing" if sys.platform == "win32" else "media_adapter_build_failed"
        return {"duration_seconds": 0, "audio_path": "", "frames": [], "warnings": [warning], "errors": [error]}
    progress_checkpoint("media-analysis-started")
    result = run([str(adapter), str(media), str(output_dir)], None, (output_dir,))
    progress_checkpoint("media-analysis-finished")
    try:
        if result.returncode == 0:
            payload = json.loads(result.stdout)
            return validate_windows_media_payload(payload, output_dir) if sys.platform == "win32" else payload
        warning = (result.stderr or "").strip() or "媒体适配器进程未返回结果"
        return {"duration_seconds": 0, "audio_path": "", "frames": [], "warnings": [warning], "errors": ["media_adapter_failed"]}
    except (json.JSONDecodeError, OSError, ValueError) as exc:
        detail = str(exc) or "媒体适配器返回无效 JSON"
        return {"duration_seconds": 0, "audio_path": "", "frames": [], "warnings": [detail], "errors": ["media_adapter_invalid"]}


def local_transcription(audio_path, output_dir, locale):
    if not audio_path:
        return {"transcript": "", "segments": [], "warnings": ["媒体没有可转写音轨"], "errors": []}
    if sys.platform == "win32":
        program = windows_native_adapter(
            "YUNSPIRE_WINDOWS_SPEECH_ADAPTER",
            "yunspire-speech.exe",
            "yunspire-speech.exe",
        )
        if program is None:
            return {
                "transcript": "",
                "segments": [],
                "warnings": ["Windows 本地语音适配器未随安装包部署；请使用完整 Yunspire 安装包重新安装"],
                "errors": ["windows_speech_adapter_missing"],
            }
        progress_checkpoint(f"speech-analysis-started:{locale}")
        result = run([str(program), audio_path, locale], None)
        progress_checkpoint(f"speech-analysis-finished:{locale}")
        try:
            if result.returncode == 0:
                return validate_windows_speech_payload(json.loads(result.stdout))
            warning = (result.stderr or "").strip() or "Windows 本地语音识别进程未返回结果"
            return {"transcript": "", "segments": [], "warnings": [warning], "errors": ["windows_speech_adapter_failed"]}
        except (json.JSONDecodeError, ValueError) as exc:
            detail = str(exc) or "Windows 本地语音适配器返回无效 JSON"
            return {"transcript": "", "segments": [], "warnings": [detail], "errors": ["windows_speech_adapter_invalid"]}
    if sys.platform != "darwin":
        return {
            "transcript": "",
            "segments": [],
            "warnings": ["当前平台不支持云枢本地语音转写"],
            "errors": ["platform_speech_transcription_unavailable"],
        }
    program = Path(__file__).with_name("yunspire_transcribe.py")
    result = run([sys.executable, str(program), audio_path, "--locale", locale, "--work-dir", str(output_dir)], 720)
    try:
        if result.returncode == 0:
            return json.loads(result.stdout)
        warning = (result.stderr or "").strip() or "本机语音识别进程未返回结果；请检查 macOS 语音识别权限"
        return {"transcript": "", "segments": [], "warnings": [warning], "errors": ["local_transcription_failed"]}
    except json.JSONDecodeError:
        return {"transcript": "", "segments": [], "warnings": ["本地转写器返回无效 JSON"], "errors": ["local_transcription_invalid"]}


def apply_local_analysis(result, media, output_dir, locale):
    analysis = local_media_analysis(media, output_dir, locale)
    result["frames"] = analysis.get("frames", [])
    result["metadata"]["duration_seconds"] = analysis.get("duration_seconds", 0)
    if analysis.get("frame_timestamps_ms"):
        result["metadata"]["frame_timestamps_ms"] = analysis["frame_timestamps_ms"]
    if analysis.get("frame_difference_scores"):
        result["metadata"]["frame_difference_scores"] = analysis["frame_difference_scores"]
    result["metadata"]["frame_candidate_count"] = analysis.get("frame_candidate_count", 0)
    result["metadata"]["frame_selection_method"] = analysis.get("frame_selection_method", "")
    result["warnings"].extend(analysis.get("warnings", []))
    result["errors"].extend(analysis.get("errors", []))
    speech = local_transcription(analysis.get("audio_path", ""), output_dir, locale)
    result["transcript"] = speech.get("transcript", "")
    result["transcript_segments"] = speech.get("segments", [])
    result["metadata"]["speech_locale"] = speech.get("locale", locale)
    result["metadata"]["speech_on_device"] = speech.get("on_device", False)
    result["warnings"].extend(speech.get("warnings", []))
    result["errors"].extend(speech.get("errors", []))


def completed_analysis_status(result):
    has_analysis = bool(result.get("transcript") or result.get("frames"))
    if not has_analysis or result.get("errors"):
        return "partial"
    return "completed"


def process_local_file(source, output_dir_value, locale):
    result = base_result("")
    result["source_kind"] = "local_file"
    result["platform"] = "本地文件"
    result["title"] = source.stem[:300]
    if source.suffix.lower() not in media_discovery.MEDIA_SUFFIXES:
        result["status"] = "failed"
        result["errors"].append("unsupported_local_media")
        result["warnings"].append(f"不支持的本地媒体格式：{source.suffix or '无扩展名'}")
        return result
    size = source.stat().st_size
    if size <= 0:
        result["status"] = "failed"
        result["errors"].append("local_media_size_invalid")
        result["warnings"].append("本地媒体为空")
        return result
    result["metadata"] = {
        "extractor_version": 2,
        "source_kind": "local_file",
        "size_bytes": size,
        "file_extension": source.suffix.lower(),
    }
    if not output_dir_value:
        result["status"] = "discovered"
        result["warnings"].append("需要输出目录才能复制原始媒体并执行本地处理")
        return result
    output_dir = Path(output_dir_value).expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    target = output_dir / f"original{source.suffix.lower()}"
    if source.resolve() != target:
        shutil.copyfile(source, target)
    else:
        target = source
    result["media_path"] = str(target)
    apply_local_analysis(result, target, output_dir, locale)
    result["status"] = completed_analysis_status(result)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("source")
    parser.add_argument("--output-dir", default="")
    parser.add_argument(
        "--locale",
        type=normalize_bcp47_locale,
        default=default_speech_locale(),
    )
    parser.add_argument("--request-headers-stdin", action="store_true")
    args = parser.parse_args()
    local_source = Path(args.source).expanduser()
    if local_source.is_file():
        emit(process_local_file(local_source.resolve(), args.output_dir, args.locale))
        return
    result = base_result(args.source)
    try:
        load_request_authorization(args.request_headers_stdin)
    except ValueError as exc:
        result["status"] = "failed"
        result["warnings"].append(str(exc))
        result["errors"].append("authorization_invalid")
        emit(result)
        return
    parsed = urlparse(args.source)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        result["status"] = "failed"
        result["errors"].append("invalid_video_url")
        emit(result)
        return
    try:
        page, final_url, content_type = fetch_page(args.source)
        progress_checkpoint("source-page-fetched")
    except HTTPError as exc:
        if exc.code in {401, 403}:
            result["auth_required"] = True
            result["status"] = "waiting_authorization"
        else:
            result["status"] = "failed"
        result["warnings"].append(f"链接读取失败：HTTP {exc.code}")
        result["errors"].append("page_unavailable")
        emit(result)
        return
    except (URLError, OSError, ValueError) as exc:
        result["status"] = "failed"
        result["warnings"].append(f"公开链接读取失败：{exc}")
        result["errors"].append("public_page_unavailable")
        emit(result)
        return
    if page_requires_authorization(page):
        result["auth_required"] = True
        result["status"] = "waiting_authorization"
        result["warnings"].append("页面要求登录或人工验证，请完成平台官方流程后创建一次性授权")
        result["errors"].append("authorization_required")
        emit(result)
        return
    title, candidates, metadata = media_discovery.discover_media(page, final_url, content_type)
    result["title"] = title
    result["metadata"] = {"final_url": final_url, **metadata}
    if not candidates:
        result["status"] = "failed"
        result["warnings"].append("公开页面没有暴露可下载媒体；如内容仅对登录用户开放，请先使用平台导出或在浏览器中下载后作为本地文件导入")
        result["errors"].append("public_media_not_exposed")
        emit(result)
        return
    if not args.output_dir:
        result["status"] = "discovered"
        result["warnings"].append("已发现公开媒体地址；需要输出目录才能保存并执行本地处理")
        emit(result)
        return
    output_dir = Path(args.output_dir).expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    subtitles = [url for url, kind in candidates if kind == "subtitle"]
    media_candidates = [(url, kind) for url, kind in candidates if kind in {"media", "hls"}]
    media = None
    selected_candidate = None
    for index, (url, kind) in enumerate(media_candidates[:8], 1):
        extension = ".ts" if kind == "hls" else Path(urlparse(url).path).suffix.lower()
        if extension not in media_discovery.MEDIA_SUFFIXES:
            guessed = mimetypes.guess_extension(metadata.get("og:video:type", "")) or ".mp4"
            extension = guessed if guessed in {".mp4", ".mov", ".webm"} else ".mp4"
        target = output_dir / f"original-{index}{extension}"
        try:
            media = download_hls(url, target, final_url) if kind == "hls" else download_file(url, target, final_url)
            if media.stat().st_size > 0:
                selected_candidate = {"index": index, "kind": kind, "host": urlparse(url).hostname or ""}
                break
        except (HTTPError, URLError, OSError, ValueError) as exc:
            result["warnings"].append(f"媒体候选 {index} 下载失败：{exc}")
            media = None
    if media is None:
        result["status"] = "failed"
        result["errors"].append("public_media_download_failed")
        emit(result)
        return
    result["media_path"] = str(media)
    result["metadata"]["selected_candidate"] = selected_candidate or {}

    for subtitle_url in subtitles[:4]:
        try:
            subtitle_text, _ = read_text_url(subtitle_url, final_url)
            transcript = parse_subtitle(subtitle_text)
            if transcript:
                result["transcript"] = transcript
                break
        except (HTTPError, URLError, OSError, ValueError):
            continue
    analysis = local_media_analysis(media, output_dir, args.locale)
    result["frames"] = analysis.get("frames", [])
    result["metadata"]["duration_seconds"] = analysis.get("duration_seconds", 0)
    if analysis.get("frame_timestamps_ms"):
        result["metadata"]["frame_timestamps_ms"] = analysis["frame_timestamps_ms"]
    if analysis.get("frame_difference_scores"):
        result["metadata"]["frame_difference_scores"] = analysis["frame_difference_scores"]
    result["metadata"]["frame_candidate_count"] = analysis.get("frame_candidate_count", 0)
    result["metadata"]["frame_selection_method"] = analysis.get("frame_selection_method", "")
    result["warnings"].extend(analysis.get("warnings", []))
    result["errors"].extend(analysis.get("errors", []))
    if not result["transcript"]:
        speech = local_transcription(analysis.get("audio_path", ""), output_dir, args.locale)
        result["transcript"] = speech.get("transcript", "")
        result["transcript_segments"] = speech.get("segments", [])
        result["metadata"]["speech_locale"] = speech.get("locale", args.locale)
        result["metadata"]["speech_on_device"] = speech.get("on_device", False)
        result["warnings"].extend(speech.get("warnings", []))
        result["errors"].extend(speech.get("errors", []))
    result["status"] = completed_analysis_status(result)
    emit(result)


if __name__ == "__main__":
    main()
