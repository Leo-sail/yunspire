#!/usr/bin/env python3
"""云枢媒体采集 v2 的第一方候选发现器。"""
from dataclasses import dataclass
from html.parser import HTMLParser
import html
import json
import re
from urllib.parse import unquote, urljoin, urlparse, urlunparse


MEDIA_SUFFIXES = (
    ".mp4", ".mov", ".m4v", ".webm", ".mp3", ".m4a", ".aac", ".wav", ".aif",
    ".aiff", ".caf", ".flac", ".ogg", ".ts",
)
SUBTITLE_SUFFIXES = (".vtt", ".srt", ".ass", ".ttml")
SCRIPT_IDS = (
    "__NEXT_DATA__", "SIGI_STATE", "RENDER_DATA", "__INITIAL_STATE__",
    "__PLAYINFO__", "__APOLLO_STATE__", "__UNIVERSAL_DATA_FOR_REHYDRATION__",
)


@dataclass(frozen=True)
class MediaCandidate:
    url: str
    kind: str
    source: str
    score: int
    width: int = 0
    height: int = 0
    bitrate: int = 0


class MediaMarkupParser(HTMLParser):
    """读取公开 HTML 标记，不执行脚本。"""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.title_parts = []
        self.meta = {}
        self.media = []
        self.subtitles = []
        self.in_title = False

    @staticmethod
    def _attributes(attrs):
        return {str(key).lower(): value for key, value in attrs if key}

    def handle_starttag(self, tag, attrs):
        values = self._attributes(attrs)
        lower = tag.lower()
        if lower == "title":
            self.in_title = True
            return
        if lower == "meta":
            key = values.get("property") or values.get("name") or values.get("itemprop")
            content = values.get("content")
            if key and content:
                self.meta[key.lower()] = content.strip()
            return
        if lower in {"video", "audio", "source"}:
            for key in ("src", "data-src", "data-url", "data-video", "data-href", "data-source"):
                if values.get(key):
                    self.media.append(values[key])
            return
        if lower == "track":
            for key in ("src", "data-src", "data-url"):
                if values.get(key):
                    self.subtitles.append(values[key])
                    break

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)

    def handle_endtag(self, tag):
        if tag.lower() == "title":
            self.in_title = False

    def handle_data(self, data):
        if self.in_title:
            value = re.sub(r"\s+", " ", data).strip()
            if value:
                self.title_parts.append(value)


def platform_name(value):
    host = (urlparse(value).hostname or "").rstrip(".").lower()
    profiles = (
        (("xiaohongshu.com", "xhslink.com"), "小红书"),
        (("douyin.com", "iesdouyin.com"), "抖音"),
        (("weixin.qq.com", "channels.weixin.qq.com", "weishi.qq.com"), "微信"),
        (("tiktok.com",), "TikTok"),
        (("youtube.com", "youtu.be", "youtube-nocookie.com"), "YouTube"),
        (("x.com", "twitter.com"), "X"),
        (("bilibili.com", "b23.tv", "bilivideo.com", "bilivideo.cn"), "哔哩哔哩"),
    )
    for domains, label in profiles:
        if any(host == domain or host.endswith("." + domain) for domain in domains):
            return label
    return host


def _decode_url(value):
    if not isinstance(value, str):
        return ""
    normalized = html.unescape(value.strip().strip("\"'"))
    normalized = normalized.replace("\\u002F", "/").replace("\\/", "/")
    normalized = normalized.replace("\\u003A", ":").replace("\\u0026", "&")
    normalized = unquote(normalized)
    if normalized.lower().startswith(("data:", "blob:", "javascript:")):
        return ""
    return normalized


def _positive_integer(value):
    try:
        number = int(float(value))
        return number if number > 0 else 0
    except (TypeError, ValueError):
        return 0


def _dimensions(value):
    if not isinstance(value, dict):
        return 0, 0, 0
    width = _positive_integer(value.get("width") or value.get("w"))
    height = _positive_integer(value.get("height") or value.get("h"))
    bitrate = _positive_integer(
        value.get("bitrate") or value.get("bandwidth") or value.get("average_bandwidth")
    )
    return width, height, bitrate


def _suffix_kind(value):
    lowered = value.lower() if isinstance(value, str) else ""
    path = urlparse(lowered).path
    if ".m3u8" in lowered:
        return "hls"
    if path.endswith(SUBTITLE_SUFFIXES):
        return "subtitle"
    if path.endswith(MEDIA_SUFFIXES):
        return "media"
    return ""


def _score(kind, source, width=0, height=0, bitrate=0):
    source_weight = {
        "direct": 100,
        "json": 84,
        "markup": 74,
        "meta": 68,
        "text": 42,
    }.get(source, 36)
    kind_weight = {"hls": 8, "media": 0, "subtitle": -35}.get(kind, 0)
    resolution_weight = min(24, (width * height) // 200_000) if width and height else 0
    bitrate_weight = min(12, bitrate // 500_000) if bitrate else 0
    return source_weight + kind_weight + resolution_weight + bitrate_weight


def _add_candidate(candidates, value, kind, source, base_url, details=None):
    normalized = _decode_url(value)
    if not normalized:
        return
    if not normalized.lower().startswith(("http://", "https://", "//", "/", "./", "../")) and not _suffix_kind(normalized):
        return
    absolute = urljoin(base_url, normalized)
    parsed = urlparse(absolute)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        return
    detected_kind = _suffix_kind(absolute) or kind
    if detected_kind not in {"media", "hls", "subtitle"}:
        return
    width, height, bitrate = _dimensions(details)
    candidate = MediaCandidate(
        url=absolute,
        kind=detected_kind,
        source=source,
        score=_score(detected_kind, source, width, height, bitrate),
        width=width,
        height=height,
        bitrate=bitrate,
    )
    key = (candidate.url, candidate.kind)
    previous = candidates.get(key)
    if previous is None or candidate.score > previous.score:
        candidates[key] = candidate


def _script_json_objects(text):
    id_pattern = "|".join(re.escape(value) for value in SCRIPT_IDS)
    patterns = (
        r'<script[^>]+type=["\']application/ld\+json["\'][^>]*>(.*?)</script>',
        rf'<script[^>]+id=["\'](?:{id_pattern})["\'][^>]*>(.*?)</script>',
    )
    for pattern in patterns:
        for raw in re.findall(pattern, text, re.I | re.S):
            decoded = html.unescape(raw.strip())
            try:
                yield json.loads(decoded)
            except (TypeError, json.JSONDecodeError):
                try:
                    yield json.loads(unquote(decoded))
                except (TypeError, json.JSONDecodeError):
                    continue

    assignment = re.compile(
        r'(?:window\.)?(?:__INITIAL_STATE__|__playinfo__|__PLAYINFO__)\s*=\s*',
        re.I,
    )
    decoder = json.JSONDecoder()
    for match in assignment.finditer(text):
        try:
            value, _ = decoder.raw_decode(text[match.end():].lstrip())
            yield value
        except json.JSONDecodeError:
            continue


def _walk(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from _walk(child)
    elif isinstance(value, list):
        for child in value:
            yield from _walk(child)


def _media_key(value):
    lowered = str(value).lower().replace("-", "_")
    return any(marker in lowered for marker in (
        "contenturl", "content_url", "playaddr", "play_addr", "playurl", "play_url",
        "downloadaddr", "download_addr", "video_url", "videourl", "masterurl",
        "master_url", "streamurl", "stream_url", "hlsurl", "hls_url", "video_src",
        "video_source", "media_url", "mediaurl", "url_default", "urldefault",
        "baseurl", "base_url", "backupurl", "backup_url",
    ))


def _subtitle_key(value):
    lowered = str(value).lower()
    return any(marker in lowered for marker in ("caption", "subtitle", "webvtt", "srt", "track"))


def _context_is_media(value):
    lowered = str(value).lower()
    return any(marker in lowered for marker in ("video", "audio", "media", "stream", "play", "download", "dash", "durl"))


def _add_json_value(candidates, item, key, base_url, details=None, media_context=False):
    if isinstance(item, str):
        if _subtitle_key(key):
            _add_candidate(candidates, item, "subtitle", "json", base_url, details)
        elif _media_key(key) or (media_context and (
            item.lower().startswith(("http://", "https://", "//", "/")) or bool(_suffix_kind(item))
        )):
            _add_candidate(candidates, item, _suffix_kind(item) or "media", "json", base_url, details)
        return
    if isinstance(item, list):
        for child in item:
            _add_json_value(
                candidates,
                child,
                key,
                base_url,
                child if isinstance(child, dict) else details,
                media_context or _context_is_media(key),
            )
        return
    if isinstance(item, dict):
        next_context = media_context or _context_is_media(key)
        for child_key, child in item.items():
            _add_json_value(candidates, child, child_key, base_url, item, next_context)


def _diagnostic_url(value):
    parsed = urlparse(value)
    return urlunparse((parsed.scheme, parsed.netloc, parsed.path, "", "", ""))


def discover_media(text, final_url, content_type):
    """返回标题、候选列表和不含查询凭据的候选诊断。"""
    page = text or ""
    candidates = {}
    parser = MediaMarkupParser()
    try:
        parser.feed(page)
    except Exception:
        parser = MediaMarkupParser()

    metadata = {
        key.lower(): html.unescape(value)
        for key, value in parser.meta.items()
    }
    title = metadata.get("og:title") or metadata.get("twitter:title") or " ".join(parser.title_parts).strip()
    direct = (content_type or "").lower().startswith(("video/", "audio/")) or bool(_suffix_kind(final_url))
    if direct:
        _add_candidate(candidates, final_url, _suffix_kind(final_url) or "media", "direct", final_url)

    for key in (
        "og:video:url", "og:video:secure_url", "og:video", "og:audio",
        "twitter:player:stream",
    ):
        _add_candidate(candidates, metadata.get(key), "media", "meta", final_url)
    for value in parser.media:
        _add_candidate(candidates, value, _suffix_kind(value) or "media", "markup", final_url)
    for value in parser.subtitles:
        _add_candidate(candidates, value, "subtitle", "markup", final_url)

    for value in _script_json_objects(page):
        for node in _walk(value):
            if not title:
                for key in ("name", "headline", "title", "desc", "description"):
                    item = node.get(key)
                    if isinstance(item, str) and item.strip():
                        title = item.strip()
                        break
        _add_json_value(candidates, value, "root", final_url)

    for match in re.findall(r'https?:\\?/\\?/[^"\'\s<>]+', page):
        decoded = _decode_url(match)
        kind = _suffix_kind(decoded)
        if kind:
            _add_candidate(candidates, decoded, kind, "text", final_url)

    ordered = sorted(candidates.values(), key=lambda item: (-item.score, item.url))
    metadata["extractor_version"] = 2
    metadata["media_candidate_count"] = sum(item.kind in {"media", "hls"} for item in ordered)
    metadata["subtitle_candidate_count"] = sum(item.kind == "subtitle" for item in ordered)
    metadata["candidate_diagnostics"] = [
        {
            "kind": item.kind,
            "source": item.source,
            "score": item.score,
            "width": item.width,
            "height": item.height,
            "bitrate": item.bitrate,
            "url": _diagnostic_url(item.url),
            "query_redacted": bool(urlparse(item.url).query),
        }
        for item in ordered[:24]
    ]
    return title[:300], [(item.url, item.kind) for item in ordered], metadata
