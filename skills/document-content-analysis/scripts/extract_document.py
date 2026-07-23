#!/usr/bin/env python3
"""Extract local documents into loss-preserving, untrusted analysis data."""

import argparse
import base64
import html
import hashlib
import json
import mimetypes
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import os
import zipfile
from pathlib import Path
from urllib.parse import urlsplit

from external_image_localizer import (
    ExternalImageLocalizer,
    localization_failure,
    localization_summary,
    public_asset,
)
from ooxml_excel import extract_xlsx
from ooxml_ppt import extract_pptx
from ooxml_word import extract_docx


MEDIA_SUFFIXES = {
    ".mp4", ".mov", ".m4v", ".webm", ".m3u8", ".mp3", ".m4a",
    ".aac", ".wav", ".aif", ".aiff", ".caf", ".flac", ".ogg", ".ts",
}
IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg", ".gif", ".webp"}
URL_RE = re.compile(
    r"(?i)(?:https?|ftp)://[^\s<>\"']+|mailto:[^\s<>\"']+|(?<![@\w])www\.[^\s<>\"']+"
)
URL_TRAILING = ".,;:!?)]}，。；：！？）】》"
MARKDOWN_ESCAPABLE_RE = re.compile(r"\\([!\"#$%&'()*+,\-./:;<=>?@\[\\\]^_`{|}~])")
PROGRESS_FILE = os.environ.get("YUNSPIRE_PROGRESS_FILE", "").strip()


def progress_checkpoint(label):
    if not PROGRESS_FILE:
        return
    try:
        with open(PROGRESS_FILE, "a", encoding="utf-8", newline="\n") as stream:
            stream.write(f"{time.time_ns()}\t{label}\n")
    except OSError:
        pass


def _progress_signature(paths):
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


def _monitor_progress_paths(paths, stop):
    previous = _progress_signature(paths)
    while not stop.wait(1):
        current = _progress_signature(paths)
        if current != previous:
            progress_checkpoint(
                f"native-output:files={current[0]}:bytes={current[1]}"
            )
            previous = current


def _run_utf8_subprocess(command, timeout, progress_paths=()):
    stop = threading.Event()
    monitor = None
    if progress_paths:
        monitor = threading.Thread(
            target=_monitor_progress_paths,
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


def stream_sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            chunk = source.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def link_policy(target):
    try:
        scheme = urlsplit(target or "").scheme.lower()
    except ValueError:
        scheme = ""
    return {
        "content_role": "untrusted_data",
        "scheme": scheme or None,
        "auto_open": False,
        "auto_fetch": False,
        "capture_requires_explicit_user_request": scheme in {"http", "https"},
    }


def plain_text_links(text, source_part):
    links = []
    for match in URL_RE.finditer(text or ""):
        visible = match.group(0).rstrip(URL_TRAILING)
        if not visible:
            continue
        target = f"https://{visible}" if visible.lower().startswith("www.") else visible
        offset_start = match.start()
        offset_end = offset_start + len(visible)
        line = text.count("\n", 0, offset_start) + 1
        previous_newline = text.rfind("\n", 0, offset_start)
        column = offset_start - previous_newline
        link_identity = "\x1f".join(
            (str(source_part), str(offset_start), target)
        )
        links.append(
            {
                "link_id": "link-"
                + hashlib.sha256(link_identity.encode()).hexdigest(),
                "source": "plain_text",
                "source_part": source_part,
                "display_text": visible,
                "target": target,
                "text_offset_start": offset_start,
                "text_offset_end": offset_end,
                "provenance": {
                    "source_kind": "text_span",
                    "source_part": source_part,
                    "text_offset_start": offset_start,
                    "text_offset_end": offset_end,
                    "line": line,
                    "column": column,
                },
                "policy": link_policy(target),
            }
        )
    return links


def text_structure(path, text, document_type="text"):
    links = plain_text_links(text, path.name)
    links_by_line = {}
    line_start = 0
    for line_number, line in enumerate(text.splitlines(keepends=True), 1):
        line_end = line_start + len(line)
        links_by_line[line_number] = [
            link["link_id"]
            for link in links
            if line_start <= link["text_offset_start"] < line_end
        ]
        line_start = line_end
    blocks = [
        {
            "block_id": f"line-{line_number:06d}",
            "line": line_number,
            "text": line.rstrip("\r\n"),
            "link_ids": links_by_line.get(line_number, []),
        }
        for line_number, line in enumerate(text.splitlines(keepends=True), 1)
    ]
    if text and not blocks:
        blocks.append({"block_id": "line-000001", "line": 1, "text": text, "link_ids": []})
    return {
        "format": "yunspire.text-document.v1",
        "document_type": document_type,
        "source": {
            "file_name": path.name,
            "byte_length": path.stat().st_size,
            "sha256": stream_sha256(path),
        },
        "blocks": blocks,
        "links": links,
        "extraction": {"truncated": False, "links_opened_or_fetched": False},
        "security": {
            "content_role": "untrusted_data",
            "instruction_authority": False,
            "tool_authority": False,
        },
    }


def _markdown_unescape(value):
    return html.unescape(MARKDOWN_ESCAPABLE_RE.sub(r"\1", str(value or "")))


def _markdown_label_key(value):
    return " ".join(_markdown_unescape(value).split()).casefold()


def _markdown_is_escaped(text, offset):
    backslashes = 0
    cursor = offset - 1
    while cursor >= 0 and text[cursor] == "\\":
        backslashes += 1
        cursor -= 1
    return backslashes % 2 == 1


def _merge_ranges(ranges):
    output = []
    for start, end in sorted(ranges):
        if start >= end:
            continue
        if output and start <= output[-1][1]:
            output[-1] = (output[-1][0], max(output[-1][1], end))
        else:
            output.append((start, end))
    return output


def _range_containing(offset, ranges):
    # Ranges are sparse in normal Markdown, so a binary search keeps scanning
    # proportional to source length even for large documents.
    low = 0
    high = len(ranges)
    while low < high:
        middle = (low + high) // 2
        if ranges[middle][0] <= offset:
            low = middle + 1
        else:
            high = middle
    index = low - 1
    if index >= 0 and offset < ranges[index][1]:
        return ranges[index]
    return None


def _markdown_ignored_ranges(text):
    ranges = []
    fence = None
    fence_start = None
    offset = 0
    for line in text.splitlines(keepends=True):
        body = line.rstrip("\r\n")
        opening = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", body)
        if fence is None and opening:
            marker = opening.group(1)
            fence = (marker[0], len(marker))
            fence_start = offset
        elif fence is not None:
            closing = re.match(
                r"^ {0,3}" + re.escape(fence[0]) + "{" + str(fence[1]) + r",}[ \t]*$",
                body,
            )
            if closing:
                ranges.append((fence_start, offset + len(line)))
                fence = None
                fence_start = None
        offset += len(line)
    if fence is not None:
        ranges.append((fence_start, len(text)))

    comment_start = 0
    while True:
        comment_start = text.find("<!--", comment_start)
        if comment_start < 0:
            break
        comment_end = text.find("-->", comment_start + 4)
        comment_end = len(text) if comment_end < 0 else comment_end + 3
        ranges.append((comment_start, comment_end))
        comment_start = comment_end

    blocked = _merge_ranges(ranges)
    cursor = 0
    while cursor < len(text):
        containing = _range_containing(cursor, blocked)
        if containing:
            cursor = containing[1]
            continue
        if text[cursor] != "`" or _markdown_is_escaped(text, cursor):
            cursor += 1
            continue
        run_end = cursor + 1
        while run_end < len(text) and text[run_end] == "`":
            run_end += 1
        marker = text[cursor:run_end]
        search = run_end
        closing = -1
        while True:
            candidate = text.find(marker, search)
            if candidate < 0:
                break
            before_tick = candidate > 0 and text[candidate - 1] == "`"
            after = candidate + len(marker)
            after_tick = after < len(text) and text[after] == "`"
            if not before_tick and not after_tick:
                closing = candidate
                break
            search = candidate + 1
        if closing < 0:
            cursor = run_end
            continue
        ranges.append((cursor, closing + len(marker)))
        cursor = closing + len(marker)
    return _merge_ranges(ranges)


def _markdown_closing_bracket(text, opening, ignored_ranges):
    depth = 1
    cursor = opening + 1
    while cursor < len(text):
        containing = _range_containing(cursor, ignored_ranges)
        if containing:
            cursor = containing[1]
            continue
        character = text[cursor]
        if character == "\\":
            cursor += 2
            continue
        if character == "[":
            depth += 1
        elif character == "]":
            depth -= 1
            if depth == 0:
                return cursor
        cursor += 1
    return None


def _markdown_reference_definitions(text, ignored_ranges):
    definitions = {}
    offset = 0
    for line in text.splitlines(keepends=True):
        body = line.rstrip("\r\n")
        if _range_containing(offset, ignored_ranges):
            offset += len(line)
            continue
        opening = re.match(r"^ {0,3}\[", body)
        if not opening:
            offset += len(line)
            continue
        bracket = body.find("[", opening.start())
        closing = _markdown_closing_bracket(body, bracket, [])
        if closing is None or closing + 1 >= len(body) or body[closing + 1] != ":":
            offset += len(line)
            continue
        label = body[bracket + 1 : closing]
        label_key = _markdown_label_key(label)
        if not label_key or label_key in definitions:
            offset += len(line)
            continue
        cursor = closing + 2
        while cursor < len(body) and body[cursor] in " \t":
            cursor += 1
        if cursor >= len(body):
            offset += len(line)
            continue
        if body[cursor] == "<":
            destination_start = cursor + 1
            destination_end = destination_start
            while destination_end < len(body):
                if body[destination_end] == "\\":
                    destination_end += 2
                    continue
                if body[destination_end] == ">":
                    break
                destination_end += 1
            if destination_end >= len(body) or body[destination_end] != ">":
                offset += len(line)
                continue
            remainder = body[destination_end + 1 :].strip()
        else:
            destination_start = cursor
            while cursor < len(body) and not body[cursor].isspace():
                if body[cursor] == "\\" and cursor + 1 < len(body):
                    cursor += 2
                else:
                    cursor += 1
            destination_end = cursor
            remainder = body[destination_end:].strip()
        title = ""
        if remainder:
            pairs = {'"': '"', "'": "'", "(": ")"}
            closer = pairs.get(remainder[0])
            if not closer or len(remainder) < 2 or remainder[-1] != closer:
                offset += len(line)
                continue
            title = _markdown_unescape(remainder[1:-1])
        raw_target = body[destination_start:destination_end]
        definitions[label_key] = {
            "label": label,
            "target": _markdown_unescape(raw_target),
            "title": title,
            "definition_offset_start": offset + bracket,
            "definition_offset_end": offset + len(body),
            "destination_offset_start": offset + destination_start,
            "destination_offset_end": offset + destination_end,
        }
        offset += len(line)
    return definitions


def _markdown_inline_destination(text, opening):
    cursor = opening + 1
    while cursor < len(text) and text[cursor] in " \t\r\n":
        cursor += 1
    if cursor >= len(text):
        return None
    if text[cursor] == "<":
        destination_start = cursor + 1
        cursor = destination_start
        while cursor < len(text):
            if text[cursor] in "\r\n":
                return None
            if text[cursor] == "\\":
                cursor += 2
                continue
            if text[cursor] == ">":
                break
            cursor += 1
        if cursor >= len(text):
            return None
        destination_end = cursor
        cursor += 1
    else:
        destination_start = cursor
        depth = 0
        while cursor < len(text):
            character = text[cursor]
            if character == "\\":
                cursor += 2
                continue
            if character == "(":
                depth += 1
            elif character == ")":
                if depth == 0:
                    destination_end = cursor
                    return {
                        "destination_start": destination_start,
                        "destination_end": destination_end,
                        "end": cursor + 1,
                        "title": "",
                    }
                depth -= 1
            elif character.isspace() and depth == 0:
                break
            cursor += 1
        destination_end = cursor
    while cursor < len(text) and text[cursor] in " \t\r\n":
        cursor += 1
    title = ""
    if cursor < len(text) and text[cursor] != ")":
        pairs = {'"': '"', "'": "'", "(": ")"}
        closer = pairs.get(text[cursor])
        if not closer:
            return None
        title_start = cursor + 1
        cursor = title_start
        while cursor < len(text):
            if text[cursor] == "\\":
                cursor += 2
                continue
            if text[cursor] == closer:
                break
            cursor += 1
        if cursor >= len(text):
            return None
        title = _markdown_unescape(text[title_start:cursor])
        cursor += 1
        while cursor < len(text) and text[cursor] in " \t\r\n":
            cursor += 1
    if cursor >= len(text) or text[cursor] != ")":
        return None
    return {
        "destination_start": destination_start,
        "destination_end": destination_end,
        "end": cursor + 1,
        "title": title,
    }


def _is_external_markdown_image(target):
    try:
        return urlsplit(str(target or "").strip()).scheme.lower() in {"http", "https"}
    except ValueError:
        return False


def _markdown_image_occurrences(path, text):
    ignored_ranges = _markdown_ignored_ranges(text)
    definitions = _markdown_reference_definitions(text, ignored_ranges)
    source_identity = hashlib.sha256(
        (path.name + "\0" + text).encode("utf-8")
    ).hexdigest()
    occurrences = []
    cursor = 0
    while cursor < len(text) - 1:
        containing = _range_containing(cursor, ignored_ranges)
        if containing:
            cursor = containing[1]
            continue
        if text[cursor : cursor + 2] != "![" or _markdown_is_escaped(text, cursor):
            cursor += 1
            continue
        label_end = _markdown_closing_bracket(text, cursor + 1, ignored_ranges)
        if label_end is None:
            cursor += 2
            continue
        raw_alt = text[cursor + 2 : label_end]
        after = label_end + 1
        syntax = None
        target = None
        title = ""
        occurrence_end = after
        definition = None
        destination_offset_start = None
        destination_offset_end = None
        if after < len(text) and text[after] == "(":
            inline = _markdown_inline_destination(text, after)
            if inline:
                syntax = "inline"
                destination_offset_start = inline["destination_start"]
                destination_offset_end = inline["destination_end"]
                target = _markdown_unescape(
                    text[destination_offset_start:destination_offset_end]
                )
                title = inline["title"]
                occurrence_end = inline["end"]
        elif after < len(text) and text[after] == "[":
            reference_end = _markdown_closing_bracket(text, after, ignored_ranges)
            if reference_end is not None:
                raw_reference = text[after + 1 : reference_end] or raw_alt
                definition = definitions.get(_markdown_label_key(raw_reference))
                if definition:
                    syntax = "reference"
                    target = definition["target"]
                    title = definition["title"]
                    occurrence_end = reference_end + 1
        else:
            definition = definitions.get(_markdown_label_key(raw_alt))
            if definition:
                syntax = "shortcut_reference"
                target = definition["target"]
                title = definition["title"]
                occurrence_end = after
        if not syntax or not _is_external_markdown_image(target):
            cursor = max(after, cursor + 2)
            continue
        line = text.count("\n", 0, cursor) + 1
        previous_newline = text.rfind("\n", 0, cursor)
        column = cursor - previous_newline
        identity = "\x1f".join(
            (source_identity, str(cursor), str(occurrence_end), str(target))
        )
        occurrences.append(
            {
                "reference_id": "markdown-image-reference-"
                + hashlib.sha256(identity.encode("utf-8")).hexdigest()[:24],
                "source": "markdown_image",
                "source_kind": "markdown_image",
                "source_part": path.name,
                "source_url": target,
                "syntax": syntax,
                "reference_label": definition.get("label") if definition else None,
                "alt_text": _markdown_unescape(raw_alt),
                "raw_alt_text": raw_alt,
                "title_text": title,
                "markdown_offset_start": cursor,
                "markdown_offset_end": occurrence_end,
                "destination_offset_start": destination_offset_start,
                "destination_offset_end": destination_offset_end,
                "line": line,
                "column": column,
                "definition": dict(definition) if definition else None,
                "placement": {
                    "kind": "markdown_inline_flow",
                    "required": True,
                    "line": line,
                    "column": column,
                    "markdown_offset_start": cursor,
                    "markdown_offset_end": occurrence_end,
                    "context_before": text[max(0, cursor - 180) : cursor].strip(),
                    "context_after": text[occurrence_end : occurrence_end + 180].strip(),
                },
            }
        )
        cursor = occurrence_end
    return occurrences


def _markdown_failure_marker(reference, failure):
    alt = " ".join(str(reference.get("alt_text") or "图片").split())
    alt = alt.replace("[", "（").replace("]", "）")
    message = failure.get("message") or "无法读取"
    return f"[外链图片本地化失败：{alt}；{message}]"


def _render_localized_markdown(source, references):
    output = []
    cursor = 0
    rendered_length = 0
    for reference in sorted(references, key=lambda item: item["markdown_offset_start"]):
        start = reference["markdown_offset_start"]
        end = reference["markdown_offset_end"]
        unchanged = source[cursor:start]
        output.append(unchanged)
        rendered_length += len(unchanged)
        rendered_start = rendered_length
        if reference.get("localized"):
            attachment_uri = f"attachment://{reference['reference_id']}"
            if reference["syntax"] == "inline":
                original = source[start:end]
                absolute_destination_start = reference["destination_offset_start"]
                absolute_destination_end = reference["destination_offset_end"]
                relative_start = absolute_destination_start - start
                relative_end = absolute_destination_end - start
                replacement = (
                    original[:relative_start]
                    + attachment_uri
                    + original[relative_end:]
                )
            else:
                replacement = f"![{reference['raw_alt_text']}]({attachment_uri})"
        else:
            replacement = _markdown_failure_marker(
                reference, reference.get("localization") or {}
            )
        output.append(replacement)
        rendered_end = rendered_start + len(replacement)
        rendered_length = rendered_end
        reference["placement"].update(
            {
                "rendered_markdown_offset_start": rendered_start,
                "rendered_markdown_offset_end": rendered_end,
            }
        )
        cursor = end
    output.append(source[cursor:])
    rendered = "".join(output)
    for reference in references:
        start = reference["placement"]["rendered_markdown_offset_start"]
        end = reference["placement"]["rendered_markdown_offset_end"]
        reference["placement"]["rendered_context_before"] = rendered[
            max(0, start - 180) : start
        ].strip()
        reference["placement"]["rendered_context_after"] = rendered[
            end : end + 180
        ].strip()
    return rendered


def extract_markdown(path, source, external_asset_directory=None):
    references = _markdown_image_occurrences(path, source)
    assets = {}
    warnings = []
    attachments = []
    external_images = ExternalImageLocalizer(external_asset_directory)
    try:
        for reference in references:
            localized = external_images.localize(
                reference["source_url"],
                suggested_name=reference.get("alt_text") or None,
            )
            asset_id = localized["asset_id"]
            reference["asset_id"] = asset_id
            reference["attachment_name"] = localized.get("name")
            reference["localized"] = localized.get("localized") is True
            reference["localization"] = dict(localized.get("localization") or {})
            asset = assets.get(asset_id)
            if asset is None:
                asset = {
                    **public_asset(localized),
                    "attachment_name": localized.get("name"),
                    "source_parts": [path.name],
                    "external_sources": [],
                    "references": [],
                }
                if localized.get("_local_path"):
                    asset["_local_path"] = localized["_local_path"]
                assets[asset_id] = asset
            if reference["source_url"] not in asset["external_sources"]:
                asset["external_sources"].append(reference["source_url"])
            asset["references"].append(reference)
            failure = localization_failure(localized)
            if failure:
                warnings.append(
                    "外链图片本地化失败："
                    f"{failure.get('message')} ({failure.get('code')})；"
                    f"来源 {reference['source_url']}"
                )

        rendered = _render_localized_markdown(source, references)
        structured = text_structure(path, rendered, "markdown")
        image_links = []
        for reference in references:
            policy = link_policy(reference["source_url"])
            policy.update(
                {
                    "external_image": True,
                    "localized": reference["localized"],
                    "capture_requires_explicit_user_request": False,
                    "capture_candidate": False,
                }
            )
            image_links.append(
                {
                    "link_id": reference["reference_id"],
                    "source": "markdown_image",
                    "source_part": path.name,
                    "display_text": reference.get("alt_text") or "外链图片",
                    "target": reference["source_url"],
                    "provenance": reference,
                    "localized": reference["localized"],
                    "external_image_localization": reference["localization"],
                    "policy": policy,
                }
            )
        structured["links"].extend(image_links)
        links_by_line = {}
        for reference in references:
            rendered_start = reference["placement"]["rendered_markdown_offset_start"]
            rendered_line = rendered.count("\n", 0, rendered_start) + 1
            reference["placement"]["rendered_line"] = rendered_line
            links_by_line.setdefault(rendered_line, []).append(reference["reference_id"])
        for block in structured["blocks"]:
            block.setdefault("link_ids", []).extend(links_by_line.get(block["line"], []))
        structured["assets"] = [
            public_asset(asset) for asset in assets.values()
        ]
        structured["image_references"] = references
        structured["source"]["rendered_sha256"] = hashlib.sha256(
            rendered.encode("utf-8")
        ).hexdigest()
        structured["extraction"].update(
            {
                "links_opened_or_fetched": any(
                    reference["localized"] for reference in references
                ),
                "ordinary_links_opened_or_fetched": False,
                "external_image_localization": localization_summary(assets.values()),
            }
        )
        for asset in assets.values():
            if asset.get("localized") is not True:
                continue
            payload = external_images.attachment_payload(asset)
            if payload is None:
                continue
            payload.update(
                {
                    "source_part": path.name,
                    "source_parts": [path.name],
                    "source_url": asset["external_sources"][0],
                    "source_urls": list(asset["external_sources"]),
                    "references": asset["references"],
                }
            )
            attachments.append(payload)
        return rendered, structured, attachments, warnings
    finally:
        external_images.close()


def normalize_office_links(structured):
    if not isinstance(structured, dict):
        return []
    candidates = []
    document_type = structured.get("document_type")
    if structured.get("format") == "yunspire.cleaned-workbook.v2":
        for sheet in structured.get("sheets", []):
            for link in sheet.get("hyperlinks", []):
                candidates.append({**link, "sheet_id": sheet.get("id"), "sheet_name": sheet.get("name")})
            for image in sheet.get("images", []):
                target = str(image.get("external_target") or image.get("target") or "").strip()
                if not target:
                    continue
                properties = image.get("properties") if isinstance(image.get("properties"), dict) else {}
                candidates.append(
                    {
                        "id": image.get("id"),
                        "target": target,
                        "display_text": properties.get("description") or properties.get("name") or "外部图片",
                        "source": "external_asset",
                        "source_kind": "external_asset",
                        "asset_id": image.get("asset_id"),
                        "relationship_id": image.get("relationship_id"),
                        "sheet_id": sheet.get("id"),
                        "sheet_name": sheet.get("name"),
                        "image_id": image.get("id"),
                        "anchor": image.get("anchor"),
                        "anchor_context": image.get("anchor_context"),
                    }
                )
    elif document_type == "presentation":
        candidates.extend(structured.get("links", []))
    else:
        candidates.extend(structured.get("hyperlinks", []))
        candidates.extend(structured.get("plain_urls", []))

    for asset in structured.get("assets", []):
        target = str(asset.get("target") or "").strip()
        if asset.get("embedded") is False and target:
            candidates.append(
                {
                    **asset,
                    "source": "external_asset",
                    "source_kind": "external_asset",
                    "display_text": asset.get("name") or "外部资源",
                }
            )

    output = []
    seen = set()
    external_assets = {}
    for asset in structured.get("assets", []):
        if not asset.get("localization"):
            continue
        for target in (
            asset.get("target"),
            asset.get("requested_url"),
            asset.get("resolved_url"),
            *asset.get("external_sources", []),
        ):
            if target:
                external_assets[str(target)] = asset
    for index, candidate in enumerate(candidates, 1):
        target = str(candidate.get("target") or candidate.get("location") or "").strip()
        if not target:
            continue
        identity = json.dumps(candidate, ensure_ascii=False, sort_keys=True, default=str)
        if identity in seen:
            continue
        seen.add(identity)
        external_asset = external_assets.get(target, {})
        localization = (
            candidate.get("external_image_localization")
            or candidate.get("localization")
            or external_asset.get("localization")
            or {}
        )
        localized = localization.get("status") == "localized"
        policy = link_policy(target)
        if localization:
            policy.update(
                {
                    "external_image": True,
                    "localized": localized,
                    "capture_requires_explicit_user_request": False,
                    "capture_candidate": False,
                }
            )
        output.append(
            {
                "link_id": candidate.get("link_id") or candidate.get("id") or f"office-link-{index:06d}",
                "target": target,
                "display_text": candidate.get("display_text") or candidate.get("display") or "",
                "source": candidate.get("source") or candidate.get("source_kind") or "office_relationship",
                "provenance": candidate,
                "localized": localized if localization else None,
                "external_image_localization": localization or None,
                "policy": policy,
            }
        )
    return output


def external_image_contract(structured):
    if not isinstance(structured, dict):
        return [], [], []
    assets = {
        asset.get("asset_id"): asset
        for asset in structured.get("assets", [])
        if asset.get("asset_id")
    }
    candidates = []

    def add(url, asset_id, source_part, reference=None, anchor=None, localization=None):
        target = str(url or "").strip()
        if not target:
            return
        asset = assets.get(asset_id, {})
        status = dict(
            localization
            or asset.get("localization")
            or {}
        )
        localized = status.get("status") == "localized"
        reference = dict(reference or {})
        identity = json.dumps(
            {
                "url": target,
                "asset_id": asset_id,
                "source_part": source_part,
                "reference": reference,
                "anchor": anchor,
            },
            ensure_ascii=False,
            sort_keys=True,
            default=str,
        )
        candidates.append(
            {
                "candidate_id": "external-image-"
                + hashlib.sha256(identity.encode("utf-8")).hexdigest()[:24],
                "asset_id": asset_id,
                "url": target,
                "source_url": target,
                "source_part": source_part,
                "reference": reference,
                "anchor": anchor,
                "localized": localized,
                "attachment_name": asset.get("name")
                or asset.get("attachment_name"),
                "reason": None if localized else status.get("message") or "外链图片未本地化",
                "reason_code": None if localized else status.get("code") or "not_localized",
                "localization": status,
            }
        )

    if structured.get("format") == "yunspire.cleaned-workbook.v2":
        for sheet in structured.get("sheets", []):
            for image in sheet.get("images", []):
                target = image.get("external_target")
                if not target:
                    continue
                add(
                    target,
                    image.get("asset_id"),
                    image.get("drawing_part") or sheet.get("source_part"),
                    {
                        "placement_id": image.get("id"),
                        "sheet_id": sheet.get("id"),
                        "sheet_name": sheet.get("name"),
                        "anchor_context": image.get("anchor_context"),
                    },
                    image.get("anchor"),
                    image.get("external_image_localization"),
                )
    elif structured.get("document_type") == "presentation":
        for slide in structured.get("slides", []):
            for element in slide.get("elements", []):
                target = element.get("external_source")
                if not target:
                    continue
                add(
                    target,
                    element.get("asset_id"),
                    element.get("source_part") or slide.get("part"),
                    {
                        "slide_id": slide.get("slide_id"),
                        "element_id": element.get("element_id"),
                        "z_order": element.get("z_order"),
                        "reading_order": element.get("reading_order"),
                        "source_layer": element.get("source_layer"),
                    },
                    element.get("bbox_emu"),
                    element.get("external_image_localization"),
                )
    else:
        for asset in structured.get("assets", []):
            if not asset.get("localization"):
                continue
            references = asset.get("references") or [{}]
            for reference in references:
                source_parts = asset.get("source_parts") or []
                add(
                    reference.get("source_url")
                    or asset.get("requested_url")
                    or asset.get("target"),
                    asset.get("asset_id"),
                    reference.get("source_part")
                    or (source_parts[0] if source_parts else None),
                    reference,
                    reference.get("anchor"),
                    asset.get("localization"),
                )

    unique = []
    seen = set()
    for candidate in candidates:
        if candidate["candidate_id"] in seen:
            continue
        seen.add(candidate["candidate_id"])
        unique.append(candidate)
    localized = [item for item in unique if item["localized"]]
    failures = [item for item in unique if not item["localized"]]
    return unique, localized, failures


def office_integrity_errors(structured, source_path=None):
    """Promote parser integrity failures to the ingestion-blocking contract."""

    if not isinstance(structured, dict):
        return []
    if structured.get("format") not in {
        "yunspire.office-document.v2",
        "yunspire.cleaned-workbook.v2",
    }:
        return []
    integrity = structured.get("integrity")
    if not isinstance(integrity, dict):
        return []
    status = str(integrity.get("status") or "").strip().lower()
    if status in {"", "complete"}:
        return []

    failures = []
    prefix = (
        f"{Path(source_path).name}: " if source_path is not None else ""
    )
    for item in integrity.get("errors") or []:
        if isinstance(item, dict):
            code = str(item.get("code") or "unknown").strip()
            message = str(item.get("message") or code).strip()
        else:
            code = "unknown"
            message = str(item).strip() or code
        failures.append(
            f"{prefix}office_structure_incomplete:{code}: {message}"
        )
    return failures or [
        f"{prefix}office_structure_incomplete:unknown: Office 解析器报告结构不完整"
    ]


def image_attachment(path):
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    return {
        "asset_id": f"sha256:{digest}",
        "name": path.name,
        "size": len(data),
        "mime_type": mimetypes.guess_type(path.name)[0] or "application/octet-stream",
        "sha256": digest,
        "source_part": path.name,
        "references": [{"source": "standalone_file", "source_part": path.name}],
        "data_base64": base64.b64encode(data).decode("ascii"),
    }


def structured_attachment(path, structured):
    data = json.dumps(structured, ensure_ascii=False, indent=2, allow_nan=False).encode("utf-8")
    suffix = "cleaned" if path.suffix.lower() == ".xlsx" else "office"
    return {
        "asset_id": f"structured-{hashlib.sha256(data).hexdigest()}",
        "name": f"{path.stem}.{suffix}.json",
        "size": len(data),
        "mime_type": "application/json",
        "source_part": path.name,
        "references": [],
        "data_base64": base64.b64encode(data).decode("ascii"),
    }


def safe_attachment_file_name(value):
    name = Path(str(value or "attachment.bin")).name
    cleaned = re.sub(r"[^0-9A-Za-z._-]+", "-", name).strip(".-")
    return cleaned[:180] or "attachment.bin"


def write_base64_attachment(encoded, target):
    # Base64 strings can be very large. Decode aligned chunks instead of
    # creating a second full binary copy in memory.
    remainder = ""
    with target.open("wb") as destination:
        for offset in range(0, len(encoded), 4 * 1024 * 1024):
            block = remainder + encoded[offset : offset + 4 * 1024 * 1024]
            complete = len(block) - (len(block) % 4)
            if complete:
                destination.write(base64.b64decode(block[:complete], validate=True))
            remainder = block[complete:]
        if remainder:
            destination.write(base64.b64decode(remainder, validate=True))


def materialize_attachments(attachments, output_directory):
    if output_directory is None:
        return attachments
    output_directory.mkdir(parents=True, exist_ok=True)
    materialized = []
    for index, attachment in enumerate(attachments, 1):
        value = dict(attachment)
        encoded = value.pop("data_base64", None)
        if encoded:
            identity = str(value.get("sha256") or value.get("asset_id") or index)
            identity = hashlib.sha256(identity.encode("utf-8")).hexdigest()[:20]
            target = output_directory / f"{identity}-{safe_attachment_file_name(value.get('name'))}"
            write_base64_attachment(encoded, target)
            value["local_attachment_path"] = str(target.resolve())
            value["size"] = target.stat().st_size
            value["sha256"] = stream_sha256(target)
        materialized.append(value)
    return materialized


def deduplicate_capture_attachments(attachments):
    """Keep one byte payload while retaining every document position."""
    merged = []
    by_content = {}
    list_fields = (
        "references",
        "source_parts",
        "source_urls",
        "external_sources",
        "placement_ids",
        "package_paths",
        "relationship_ids",
        "original_names",
    )

    def append_unique(values, additions):
        seen = {
            json.dumps(value, ensure_ascii=False, sort_keys=True, default=str)
            for value in values
        }
        for value in additions:
            identity = json.dumps(
                value, ensure_ascii=False, sort_keys=True, default=str
            )
            if identity not in seen:
                seen.add(identity)
                values.append(value)

    for attachment in attachments:
        current = dict(attachment)
        identity = capture_attachment_identity(current)
        if not identity:
            merged.append(current)
            continue

        existing = by_content.get(identity)
        if existing is None:
            for field in list_fields:
                if isinstance(current.get(field), list):
                    current[field] = list(current[field])
            source_part = current.get("source_part")
            if source_part:
                current.setdefault("source_parts", [])
                append_unique(current["source_parts"], [source_part])
            by_content[identity] = current
            merged.append(current)
            continue

        for field in list_fields:
            additions = current.get(field)
            if not isinstance(additions, list):
                continue
            existing.setdefault(field, [])
            append_unique(existing[field], additions)
        source_part = current.get("source_part")
        if source_part:
            existing.setdefault("source_parts", [])
            append_unique(existing["source_parts"], [source_part])

        for key, value in current.items():
            if key not in existing or existing[key] in (None, "", [], {}):
                existing[key] = value

    return merged


def capture_attachment_identity(attachment):
    digest = str(attachment.get("sha256") or "").strip().lower()
    asset_id = str(attachment.get("asset_id") or "").strip()
    return f"sha256:{digest}" if digest else asset_id


def capture_attachment_metadata(attachment):
    return {
        key: value
        for key, value in attachment.items()
        if key not in {"data_base64", "local_attachment_path", "_local_path"}
    }


POSITION_ID_FIELDS = (
    "reference_id",
    "image_reference_id",
    "placement_id",
    "element_id",
)
POSITION_ID_LIST_FIELDS = ("reference_ids", "placement_ids")


def capture_source_relative_path(path, raw_paths):
    source = Path(path).expanduser().resolve()
    for raw_path in raw_paths:
        root = Path(raw_path).expanduser().resolve()
        if root.is_file() and root == source:
            return root.name
        if root.is_dir():
            try:
                return source.relative_to(root).as_posix()
            except ValueError:
                continue
    return source.name


def capture_position_namespace(item, raw_paths):
    structured = item.get("structured_data")
    source = structured.get("source", {}) if isinstance(structured, dict) else {}
    digest = str(source.get("sha256") or "").strip().lower()
    path = Path(item["path"])
    if not digest and path.is_file():
        digest = stream_sha256(path)
    relative_path = capture_source_relative_path(path, raw_paths)
    path_identity = hashlib.sha256(
        str(path.expanduser().resolve()).encode("utf-8")
    ).hexdigest()
    identity = f"{relative_path}\0{digest}\0{path_identity}"
    return "capture-file-" + hashlib.sha256(identity.encode("utf-8")).hexdigest()[:16]


def attachment_position_ids(attachment):
    identifiers = []

    def add(value):
        value = str(value or "").strip()
        if value and value not in identifiers:
            identifiers.append(value)

    for key in POSITION_ID_FIELDS:
        add(attachment.get(key))
    for key in POSITION_ID_LIST_FIELDS:
        for value in attachment.get(key, []):
            add(value)
    for reference in attachment.get("references", []):
        if not isinstance(reference, dict):
            continue
        for key in POSITION_ID_FIELDS:
            add(reference.get(key))
    return identifiers


def namespaced_position_id(namespace, identifier):
    candidate = f"{namespace}-{identifier}"
    has_control = any(
        ord(character) < 32 or 127 <= ord(character) <= 159
        for character in candidate
    )
    if len(candidate) <= 180 and not has_control:
        return candidate
    digest = hashlib.sha256(identifier.encode("utf-8")).hexdigest()[:32]
    return f"{namespace}-position-{digest}"


def position_id_field(key):
    key = str(key or "")
    return (
        key in POSITION_ID_FIELDS
        or key in POSITION_ID_LIST_FIELDS
        or key == "id"
        or key == "reading_order"
        or key.endswith("_id")
        or key.endswith("_ids")
    )


def rewrite_position_ids(value, mapping, parent_key=""):
    if isinstance(value, dict):
        return {
            key: rewrite_position_ids(item, mapping, key)
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [rewrite_position_ids(item, mapping, parent_key) for item in value]
    if isinstance(value, str) and position_id_field(parent_key):
        return mapping.get(value, value)
    return value


def rewrite_attachment_placeholders(text, mapping):
    rewritten = str(text or "")
    for identifier in sorted(mapping, key=len, reverse=True):
        token = re.compile(
            rf"attachment://{re.escape(identifier)}(?=$|[\s)\]}}>'\"])")
        rewritten = token.sub(
            "attachment://" + mapping[identifier],
            rewritten,
        )
    return rewritten


def namespace_capture_positions(item, raw_paths):
    identifiers = []
    for attachment in item.get("attachments", []):
        for identifier in attachment_position_ids(attachment):
            if identifier not in identifiers:
                identifiers.append(identifier)
    unpositioned_images = [
        attachment
        for attachment in item.get("attachments", [])
        if str(attachment.get("mime_type") or "").lower().startswith("image/")
        and not attachment_position_ids(attachment)
    ]
    if not identifiers and not unpositioned_images:
        return item

    namespace = capture_position_namespace(item, raw_paths)
    mapping = {
        identifier: namespaced_position_id(namespace, identifier)
        for identifier in identifiers
    }
    for attachment in unpositioned_images:
        source_token = str(
            attachment.get("name") or attachment.get("asset_id") or ""
        ).strip()
        if not source_token:
            continue
        identity = "\0".join(
            (
                source_token,
                str(attachment.get("asset_id") or ""),
                str(attachment.get("sha256") or ""),
            )
        )
        synthetic_id = namespaced_position_id(
            namespace,
            "standalone-"
            + hashlib.sha256(identity.encode("utf-8")).hexdigest()[:20],
        )
        references = attachment.setdefault("references", [])
        if references and isinstance(references[0], dict):
            references[0] = {**references[0], "reference_id": synthetic_id}
        else:
            references.append(
                {
                    "reference_id": synthetic_id,
                    "source": "standalone_file",
                    "source_part": attachment.get("source_part"),
                }
            )
        mapping[source_token] = synthetic_id
    text = rewrite_attachment_placeholders(item.get("text"), mapping)
    rewritten = rewrite_position_ids(item, mapping)
    rewritten["text"] = text
    rewritten["position_id_namespace"] = namespace
    return rewritten


def _windows_pdf_adapter_path():
    configured = os.environ.get("YUNSPIRE_WINDOWS_PDF_ADAPTER", "").strip()
    candidates = []
    if configured:
        candidates.append(Path(configured).expanduser())
    candidates.extend(
        (
            Path(__file__).with_name("yunspire_pdf_windows.exe"),
            Path(__file__).resolve().parents[3]
            / "src-tauri"
            / "target"
            / "yunspire-native"
            / "yunspire_pdf_windows.exe",
        )
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    return None


def _pdf_reference_id(source_sha256, page_number):
    identity = f"{source_sha256}\0page\0{page_number}"
    return "pdf-page-reference-" + hashlib.sha256(identity.encode()).hexdigest()[:24]


def _windows_pdf_result(path, payload, output_directory, keep_local_paths):
    warnings = [str(value) for value in payload.get("warnings", [])]
    errors = [str(value) for value in payload.get("errors", [])]
    page_count = payload.get("page_count")
    if not isinstance(page_count, int) or page_count <= 0:
        errors.append("pdf_page_count_invalid")
        page_count = max(0, page_count if isinstance(page_count, int) else 0)
    raw_pages = payload.get("pages")
    if not isinstance(raw_pages, list):
        raw_pages = []
        errors.append("pdf_page_manifest_invalid")

    source_sha256 = stream_sha256(path)
    source_root = output_directory.resolve()
    attachments = []
    pages = []
    markdown = []
    seen_page_numbers = set()
    for raw_page in raw_pages:
        if not isinstance(raw_page, dict):
            errors.append("pdf_page_manifest_entry_invalid")
            continue
        page_number = raw_page.get("page_number")
        if not isinstance(page_number, int) or not 1 <= page_number <= page_count:
            errors.append(f"pdf_page_number_invalid:{page_number}")
            continue
        if page_number in seen_page_numbers:
            errors.append(f"pdf_page_number_duplicate:{page_number}")
            continue
        seen_page_numbers.add(page_number)
        rendered_path = Path(str(raw_page.get("path") or ""))
        try:
            rendered_path = rendered_path.resolve(strict=True)
            rendered_path.relative_to(source_root)
        except (OSError, ValueError):
            errors.append(f"pdf_page_path_invalid:{page_number}")
            continue
        try:
            size = rendered_path.stat().st_size
            with rendered_path.open("rb") as source:
                magic = source.read(3)
        except OSError as exc:
            errors.append(f"pdf_page_read_failed:{page_number}:{exc}")
            continue
        if size <= 0 or magic[:2] != b"\xff\xd8":
            errors.append(f"pdf_page_image_invalid:{page_number}")
            continue
        declared_size = raw_page.get("byte_length")
        if not isinstance(declared_size, int) or declared_size != size:
            errors.append(f"pdf_page_size_mismatch:{page_number}")
            continue

        digest = stream_sha256(rendered_path)
        reference_id = _pdf_reference_id(source_sha256, page_number)
        name = f"{path.stem}-pdf-page-{page_number:05d}.jpg"
        reference = {
            "reference_id": reference_id,
            "source": "pdf_page_render",
            "source_part": path.name,
            "page_number": page_number,
            "width_points": raw_page.get("width_points"),
            "height_points": raw_page.get("height_points"),
            "render_width": raw_page.get("render_width"),
            "render_height": raw_page.get("render_height"),
        }
        attachment = {
            "asset_id": f"sha256:{digest}",
            "name": name,
            "size": size,
            "mime_type": "image/jpeg",
            "sha256": digest,
            "source_part": path.name,
            "page_number": page_number,
            "references": [reference],
            "renderer": "Windows.Data.Pdf",
            "model_analysis_input": True,
        }
        if keep_local_paths:
            attachment["local_attachment_path"] = str(rendered_path)
        else:
            attachment["data_base64"] = base64.b64encode(
                rendered_path.read_bytes()
            ).decode("ascii")
        attachments.append(attachment)
        page_record = {
            "page_id": f"pdf-page-{page_number:06d}",
            "page_number": page_number,
            "width_points": raw_page.get("width_points"),
            "height_points": raw_page.get("height_points"),
            "render_width": raw_page.get("render_width"),
            "render_height": raw_page.get("render_height"),
            "reference_id": reference_id,
            "asset_id": attachment["asset_id"],
            "resolution_reduced_for_model_input": bool(
                raw_page.get("resolution_reduced")
            ),
        }
        pages.append(page_record)
        markdown.extend(
            (
                f"### 第 {page_number} 页",
                "",
                f"![{path.name} 第 {page_number} 页](attachment://{reference_id})",
                "",
            )
        )

    expected_pages = set(range(1, page_count + 1))
    missing_pages = sorted(expected_pages - seen_page_numbers)
    if missing_pages:
        errors.append(
            "pdf_pages_missing:" + ",".join(str(value) for value in missing_pages)
        )
    if len(pages) != page_count:
        errors.append("pdf_render_incomplete")
    errors = list(dict.fromkeys(errors))
    warnings = list(dict.fromkeys(warnings))
    integrity_status = "complete" if not errors and len(pages) == page_count else "incomplete"
    structured = {
        "format": "yunspire.pdf-document.v1",
        "document_type": "pdf",
        "source": {
            "file_name": path.name,
            "byte_length": path.stat().st_size,
            "sha256": source_sha256,
        },
        "renderer": payload.get("renderer") or "Windows.Data.Pdf",
        "page_count": page_count,
        "pages": pages,
        "links": [],
        "integrity": {
            "status": integrity_status,
            "errors": errors,
            "checks": {
                "declared_page_count": page_count,
                "rendered_page_count": len(pages),
                "all_pages_rendered": len(pages) == page_count,
                "page_images_byte_verified": len(pages) == page_count,
            },
        },
        "extraction": {
            "truncated": False,
            "parse_limits_applied": [],
            "text_layer_extracted": False,
            "visual_pages_rendered": len(pages),
            "links_opened_or_fetched": False,
        },
        "security": {
            "content_role": "untrusted_data",
            "instruction_authority": False,
            "tool_authority": False,
        },
    }
    return "\n".join(markdown).strip(), structured, attachments, warnings, errors


def _extract_windows_pdf(path, attachment_output_directory):
    adapter = _windows_pdf_adapter_path()
    if adapter is None:
        return "", None, [], [], [
            "windows_pdf_adapter_missing: 云枢 Windows PDF 原生适配器不存在"
        ]
    temporary = None
    if attachment_output_directory is None:
        temporary = tempfile.TemporaryDirectory()
        output_directory = Path(temporary.name).resolve()
    else:
        output_directory = Path(attachment_output_directory).resolve()
        output_directory.mkdir(parents=True, exist_ok=True)
    pdf_output = output_directory / (
        "pdf-" + hashlib.sha256(str(path.resolve()).encode()).hexdigest()[:20]
    )
    pdf_output.mkdir(parents=True, exist_ok=True)
    try:
        progress_checkpoint("windows-pdf-render-started")
        result = _run_utf8_subprocess(
            [str(adapter), str(path), str(pdf_output)],
            timeout=None,
            progress_paths=(pdf_output,),
        )
        progress_checkpoint("windows-pdf-render-finished")
        if result.returncode != 0:
            return "", None, [], [], [
                f"windows_pdf_adapter_failed:exit={result.returncode}:{result.stderr[-500:]}"
            ]
        try:
            payload = json.loads(result.stdout)
        except json.JSONDecodeError:
            return "", None, [], [], ["windows_pdf_adapter_invalid_json"]
        if payload.get("schema") != "yunspire.windows-pdf.v1":
            return "", None, [], [], ["windows_pdf_adapter_schema_mismatch"]
        return _windows_pdf_result(
            path,
            payload,
            pdf_output,
            attachment_output_directory is not None,
        )
    except subprocess.TimeoutExpired:
        return "", None, [], [], ["windows_pdf_adapter_timeout"]
    finally:
        if temporary is not None:
            temporary.cleanup()


def extract_pdf(path, attachment_output_directory=None):
    if sys.platform == "win32":
        return _extract_windows_pdf(path, attachment_output_directory)
    if sys.platform != "darwin":
        return "", None, [], [], [
            "platform_pdf_adapter_unavailable: 当前平台没有云枢 PDF 原生适配器"
        ]
    compiler = shutil.which("clang")
    if not compiler:
        return "", None, [], [], [
            "macos_pdf_adapter_compiler_missing: 本机缺少 Apple clang 编译器"
        ]
    with tempfile.TemporaryDirectory() as temporary:
        source = Path(__file__).with_name("yunspire_pdf.m")
        adapter = Path(temporary) / "yunspire-pdf"
        build = _run_utf8_subprocess(
            [compiler, "-fobjc-arc", str(source), "-framework", "PDFKit", "-framework", "AppKit", "-framework", "Foundation", "-o", str(adapter)],
            timeout=120,
        )
        if build.returncode != 0:
            return "", None, [], [], [
                f"macos_pdf_adapter_build_failed:{build.stderr[-500:]}"
            ]
        result = _run_utf8_subprocess([str(adapter), str(path)], timeout=180)
        if result.returncode != 0:
            return "", None, [], [], [
                f"macos_pdf_adapter_failed:{result.stderr[-500:]}"
            ]
        try:
            payload = json.loads(result.stdout)
        except json.JSONDecodeError:
            return "", None, [], [], ["macos_pdf_adapter_invalid_json"]
        text = payload.get("text", "")
        return (
            text,
            text_structure(path, text, "pdf"),
            payload.get("attachments", []),
            payload.get("warnings", []),
            payload.get("errors", []),
        )


def empty_result(path, warning=None, error=None):
    return {
        "path": str(path),
        "type": path.suffix.lower(),
        "text": "",
        "structured_data": None,
        "links": [],
        "warnings": [warning] if warning else [],
        "errors": [error] if error else [],
        "attachments": [],
        "external_image_candidates": [],
        "external_image_localized": [],
        "external_image_failures": [],
    }


def extract_one(path, external_asset_directory=None):
    suffix = path.suffix.lower()
    if suffix in MEDIA_SUFFIXES:
        return {**empty_result(path), "delegated_to": "video-content-analysis"}

    warnings = []
    attachments = []
    structured = None
    links = []
    errors = []
    if suffix in {".md", ".markdown"}:
        text = path.read_text(encoding="utf-8", errors="replace")
        text, structured, markdown_attachments, markdown_warnings = extract_markdown(
            path, text, external_asset_directory
        )
        attachments.extend(markdown_attachments)
        warnings.extend(markdown_warnings)
        links = structured["links"]
    elif suffix == ".txt":
        text = path.read_text(encoding="utf-8", errors="replace")
        structured = text_structure(path, text, "text")
        links = structured["links"]
    elif suffix == ".xlsx":
        text, structured, office_attachments, office_warnings = extract_xlsx(
            path, external_asset_directory
        )
        attachments.extend(office_attachments)
        warnings.extend(office_warnings)
        links = normalize_office_links(structured)
        attachments.append(structured_attachment(path, structured))
    elif suffix == ".docx":
        text, structured, office_attachments, office_warnings = extract_docx(
            path, external_asset_directory
        )
        attachments.extend(office_attachments)
        warnings.extend(office_warnings)
        links = normalize_office_links(structured)
        attachments.append(structured_attachment(path, structured))
    elif suffix == ".pptx":
        text, structured, office_attachments, office_warnings = extract_pptx(
            path, external_asset_directory
        )
        attachments.extend(office_attachments)
        warnings.extend(office_warnings)
        links = normalize_office_links(structured)
        attachments.append(structured_attachment(path, structured))
    elif suffix == ".pdf":
        text, structured, pdf_attachments, pdf_warnings, pdf_errors = extract_pdf(
            path, external_asset_directory
        )
        attachments.extend(pdf_attachments)
        warnings.extend(pdf_warnings)
        errors.extend(pdf_errors)
        links = structured.get("links", []) if isinstance(structured, dict) else []
    elif suffix in IMAGE_SUFFIXES:
        attachment = image_attachment(path)
        attachments.append(attachment)
        text = f"![{path.name}](attachment://{path.name})"
        warnings.append("图片已读取为视觉分析附件")
    else:
        return empty_result(path, warning=f"暂不支持的文件类型：{suffix or '无扩展名'}")

    external_candidates, external_localized, external_failures = (
        external_image_contract(structured)
    )
    errors.extend(office_integrity_errors(structured, path))
    if external_failures:
        errors.append("external_image_localization_incomplete")
    integrity = (
        structured.get("integrity")
        if isinstance(structured, dict)
        and isinstance(structured.get("integrity"), dict)
        else None
    )
    return {
        "path": str(path),
        "type": suffix,
        "text": text,
        "structured_data": structured,
        "links": links,
        "warnings": warnings,
        "errors": errors,
        "integrity_status": (
            integrity.get("status") if integrity else None
        ),
        "attachments": attachments,
        "external_image_candidates": external_candidates,
        "external_image_localized": external_localized,
        "external_image_failures": external_failures,
    }


def safe_extract_one(path, external_asset_directory=None):
    try:
        return extract_one(path, external_asset_directory)
    except (OSError, ValueError, RuntimeError, zipfile.BadZipFile) as exc:
        return empty_result(path, error=f"{path.name} 解析失败：{exc}")


def discovered_files(raw_paths):
    files = []
    for source_index, raw in enumerate(raw_paths, 1):
        progress_checkpoint(f"source-discovery-started:{source_index}")
        unresolved = Path(raw).expanduser()
        if unresolved.is_symlink():
            continue
        path = unresolved.resolve()
        if path.is_dir():
            for directory, directory_names, file_names in os.walk(path, followlinks=False):
                directory_path = Path(directory)
                directory_names[:] = [
                    name
                    for name in directory_names
                    if not name.startswith(".")
                    and not (directory_path / name).is_symlink()
                ]
                for name in file_names:
                    child = directory_path / name
                    if not name.startswith(".") and child.is_file() and not child.is_symlink():
                        files.append(child.resolve())
                        if len(files) % 100 == 0:
                            progress_checkpoint(f"source-discovery-files:{len(files)}")
        elif path.is_file() and not path.is_symlink():
            files.append(path)
        progress_checkpoint(f"source-discovery-finished:{source_index}:{len(files)}")
    unique = []
    seen = set()
    for path in files:
        if path not in seen:
            seen.add(path)
            unique.append(path)
    return unique


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("paths", nargs="+")
    parser.add_argument("--attachment-output-dir")
    args = parser.parse_args()
    attachment_output_directory = (
        Path(args.attachment_output_dir).expanduser().resolve()
        if args.attachment_output_dir
        else None
    )
    results = []
    for index, path in enumerate(discovered_files(args.paths), 1):
        progress_checkpoint(f"document-started:{index}:{path.name}")
        results.append(
            namespace_capture_positions(
                safe_extract_one(path, attachment_output_directory),
                args.paths,
            )
        )
        progress_checkpoint(f"document-finished:{index}:{path.name}")
    content = "\n\n".join(
        f"## {Path(item['path']).name}\n\n{item['text']}"
        for item in results
        if item["text"]
    )
    attachments = deduplicate_capture_attachments(
        attachment
        for item in results
        for attachment in item["attachments"]
    )
    attachments = materialize_attachments(
        attachments, attachment_output_directory
    )
    for item in results:
        item["attachments"] = [
            capture_attachment_metadata(attachment)
            for attachment in item["attachments"]
        ]
    delegated_media_count = sum(item.get("delegated_to") == "video-content-analysis" for item in results)
    embedded_links = [
        {**link, "file_path": item["path"]}
        for item in results
        for link in item.get("links", [])
    ]
    errors = list(
        dict.fromkeys(
            error
            for item in results
            for error in item.get("errors", [])
        )
    )
    external_image_candidates = [
        {**candidate, "file_path": item["path"]}
        for item in results
        for candidate in item.get("external_image_candidates", [])
    ]
    external_image_localized = [
        candidate
        for candidate in external_image_candidates
        if candidate.get("localized") is True
    ]
    external_image_failures = [
        candidate
        for candidate in external_image_candidates
        if candidate.get("localized") is not True
    ]
    if external_image_failures and "external_image_localization_incomplete" not in errors:
        errors.append("external_image_localization_incomplete")
    has_analyzable_content = bool(content.strip() or attachments)
    if not has_analyzable_content and not delegated_media_count and not errors:
        errors.append("document_content_unavailable")
    output = {
        "files": results,
        "structured_data": [
            {"path": item["path"], "data": item["structured_data"]}
            for item in results
            if item.get("structured_data") is not None
        ],
        "embedded_links": embedded_links,
        "attachments": attachments,
        "content_markdown": content,
        "external_image_candidates": external_image_candidates,
        "external_image_localized": external_image_localized,
        "external_image_failures": external_image_failures,
        "metadata": {
            "content_role": "untrusted_data",
            "office_pipeline": "position_preserving_ooxml_v2",
            "excel_pipeline": "coordinates_then_clean_json_v2",
            "links_opened_or_fetched": bool(external_image_localized),
            "ordinary_links_opened_or_fetched": False,
            "links_require_explicit_capture_request": True,
            "external_image_candidates": external_image_candidates,
            "external_image_localized": external_image_localized,
            "external_image_failures": external_image_failures,
            "external_image_candidate_count": len(external_image_candidates),
            "external_image_localized_count": len(external_image_localized),
            "external_image_failure_count": len(external_image_failures),
            "parse_limits_applied": [],
            "truncated": False,
            "delegated_media_count": delegated_media_count,
        },
        "warnings": [warning for item in results for warning in item["warnings"]],
        "errors": errors,
    }
    output["content_hash"] = hashlib.sha256(content.encode()).hexdigest()
    json.dump(output, sys.stdout, ensure_ascii=False, allow_nan=False)


if __name__ == "__main__":
    main()
