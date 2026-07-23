#!/usr/bin/env python3
"""Lossless, standard-library XLSX extraction for Yunspire.

Source-coordinate facts and derived cleaned views remain separate. Formulas are
never recalculated, and links embedded in workbooks are never opened or fetched.
"""

from __future__ import annotations

import base64
import datetime as dt
import hashlib
import json
import math
import mimetypes
import posixpath
import re
import zipfile
from pathlib import Path
from urllib.parse import unquote, urlsplit
from xml.etree import ElementTree

from external_image_localizer import (
    ExternalImageLocalizer,
    localization_failure,
    localization_summary,
    public_asset,
)

CELL_RE = re.compile(r"^\$?([A-Za-z]{1,3})\$?(\d+)$")
RANGE_RE = re.compile(
    r"^\$?([A-Za-z]{1,3})\$?(\d+)(?::\$?([A-Za-z]{1,3})\$?(\d+))?$"
)
PLAIN_URL_RE = re.compile(
    r"(?i)(?:https?|ftp)://[^\s<>\"']+|mailto:[^\s<>\"']+|(?<![@\w])www\.[^\s<>\"']+"
)
FORMULA_REF_RE = re.compile(
    r"(?<![A-Za-z0-9_.])"
    r"(?P<prefix>"
    r"(?:'(?:[^']|'')*'|(?:\[[^\]]+\])?[^\s!'+\-*/^&=<>(),;:{}\[\]]+)!"
    r")?"
    r"(?P<reference>"
    r"(?P<cell_start>\$?[A-Za-z]{1,3}\$?\d+)"
    r"(?::(?P<cell_end>\$?[A-Za-z]{1,3}\$?\d+))?"
    r"|(?P<column_start>\$?[A-Za-z]{1,3}):(?P<column_end>\$?[A-Za-z]{1,3})"
    r"|(?P<row_start>\$?\d+):(?P<row_end>\$?\d+)"
    r")"
    r"(?![A-Za-z0-9_.])"
)
IMAGE_MIMES = {
    ".bmp": "image/bmp",
    ".emf": "image/emf",
    ".gif": "image/gif",
    ".jpeg": "image/jpeg",
    ".jpg": "image/jpeg",
    ".png": "image/png",
    ".svg": "image/svg+xml",
    ".tif": "image/tiff",
    ".tiff": "image/tiff",
    ".webp": "image/webp",
    ".wmf": "image/wmf",
}


class IntegrityReport:
    def __init__(self):
        self.errors = []
        self._seen = set()

    def fail(self, code, message, **evidence):
        record = {
            "code": str(code),
            "message": str(message),
            **{
                key: value
                for key, value in evidence.items()
                if value is not None and value != ""
            },
        }
        identity = tuple(
            (key, repr(value)) for key, value in sorted(record.items())
        )
        if identity not in self._seen:
            self._seen.add(identity)
            self.errors.append(record)

    def output(self, checks):
        return {
            "status": "incomplete" if self.errors else "complete",
            "errors": list(self.errors),
            "checks": dict(checks),
        }


def local_name(name):
    return name.rsplit("}", 1)[-1].split(":", 1)[-1]


def attribute(node, wanted, default=None):
    for key, value in node.attrib.items():
        if local_name(key) == wanted:
            return value
    return default


def first_descendant(node, wanted):
    return next((item for item in node.iter() if local_name(item.tag) == wanted), None)


def stable_id(prefix, *parts):
    source = "\x1f".join(str(part) for part in parts)
    return f"{prefix}-{hashlib.sha256(source.encode('utf-8')).hexdigest()}"


def stream_sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            chunk = source.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def column_number(letters):
    value = 0
    for character in letters.upper():
        value = value * 26 + ord(character) - ord("A") + 1
    return value


def column_letters(number):
    if number < 1:
        raise ValueError("Excel column numbers are one-based")
    output = []
    while number:
        number, remainder = divmod(number - 1, 26)
        output.append(chr(ord("A") + remainder))
    return "".join(reversed(output))


def cell_coordinates(reference):
    match = CELL_RE.match(reference or "")
    if not match:
        return None
    column = column_number(match.group(1))
    row = int(match.group(2))
    if not (1 <= column <= 16384 and 1 <= row <= 1048576):
        return None
    return row, column


def cell_reference(row, column):
    return f"{column_letters(column)}{row}"


def range_bounds(reference):
    match = RANGE_RE.match(reference or "")
    if not match:
        return None
    c1 = column_number(match.group(1))
    r1 = int(match.group(2))
    c2 = column_number(match.group(3) or match.group(1))
    r2 = int(match.group(4) or match.group(2))
    return min(r1, r2), min(c1, c2), max(r1, r2), max(c1, c2)


def relationship_part(source_part):
    return posixpath.join(
        posixpath.dirname(source_part),
        "_rels",
        f"{posixpath.basename(source_part)}.rels",
    )


def resolve_package_target(source_part, target):
    target = unquote((target or "").replace("\\", "/"))
    if target.startswith("/"):
        normalized = posixpath.normpath(target.lstrip("/"))
    else:
        normalized = posixpath.normpath(
            posixpath.join(posixpath.dirname(source_part), target)
        )
    if normalized == ".." or normalized.startswith("../") or normalized.startswith("/"):
        raise ValueError(f"OOXML relationship escapes the package: {target}")
    return normalized


def relationships(
    archive,
    names,
    source_part,
    warnings,
    integrity=None,
    required=False,
):
    part = relationship_part(source_part)
    if part not in names:
        if required and integrity is not None:
            integrity.fail(
                "excel_relationship_part_missing",
                f"声明的关系部件不存在：{part}",
                source_part=source_part,
                relationship_part=part,
            )
        return {}
    output = {}
    try:
        root = ElementTree.fromstring(archive.read(part))
    except (ElementTree.ParseError, KeyError) as exc:
        warnings.append(f"无法解析关系文件 {part}：{exc}")
        if integrity is not None:
            integrity.fail(
                "excel_relationship_part_unreadable",
                f"无法解析关系文件 {part}：{exc}",
                source_part=source_part,
                relationship_part=part,
            )
        return output
    for node in root.iter():
        if local_name(node.tag) != "Relationship":
            continue
        relation_id = node.attrib.get("Id", "")
        target = node.attrib.get("Target", "")
        if relation_id and relation_id in output and integrity is not None:
            integrity.fail(
                "excel_relationship_id_duplicate",
                f"关系部件 {part} 重复声明 {relation_id}",
                source_part=source_part,
                relationship_part=part,
                relationship_id=relation_id,
            )
        if (not relation_id or not target) and integrity is not None:
            integrity.fail(
                "excel_relationship_declaration_invalid",
                f"关系部件 {part} 包含缺少 Id 或 Target 的声明",
                source_part=source_part,
                relationship_part=part,
                relationship_id=relation_id or None,
                target=target or None,
            )
        mode = node.attrib.get("TargetMode", "") or "Internal"
        resolved = None
        if mode.lower() != "external":
            try:
                resolved = resolve_package_target(source_part, target)
            except ValueError as exc:
                warnings.append(str(exc))
                if integrity is not None:
                    integrity.fail(
                        "excel_relationship_target_invalid",
                        str(exc),
                        source_part=source_part,
                        relationship_part=part,
                        relationship_id=relation_id or None,
                        target=target or None,
                    )
        output[relation_id] = {
            "id": relation_id,
            "type": node.attrib.get("Type", ""),
            "target": target,
            "target_mode": mode,
            "resolved_target": resolved,
        }
    return output


def content_types(archive, names, integrity):
    defaults = {}
    overrides = {}
    if "[Content_Types].xml" not in names:
        integrity.fail(
            "excel_content_types_missing",
            "XLSX 缺少 [Content_Types].xml",
            source_part="[Content_Types].xml",
        )
        return defaults, overrides
    try:
        root = ElementTree.fromstring(archive.read("[Content_Types].xml"))
    except (ElementTree.ParseError, KeyError) as exc:
        integrity.fail(
            "excel_content_types_unreadable",
            f"无法解析 [Content_Types].xml：{exc}",
            source_part="[Content_Types].xml",
        )
        return defaults, overrides
    for node in root:
        if local_name(node.tag) == "Default":
            defaults[node.attrib.get("Extension", "").lower()] = node.attrib.get(
                "ContentType", ""
            )
        elif local_name(node.tag) == "Override":
            overrides[node.attrib.get("PartName", "").lstrip("/")] = node.attrib.get(
                "ContentType", ""
            )
    return defaults, overrides


def mime_type(part, types):
    defaults, overrides = types
    if part in overrides:
        return overrides[part]
    suffix = Path(part).suffix.lower()
    return (
        defaults.get(suffix.lstrip("."))
        or IMAGE_MIMES.get(suffix)
        or mimetypes.guess_type(part)[0]
        or "application/octet-stream"
    )


def read_shared_strings(archive, names, warnings, integrity):
    part = "xl/sharedStrings.xml"
    if part not in names:
        return []
    output = []
    try:
        with archive.open(part) as source:
            for _, node in ElementTree.iterparse(source, events=("end",)):
                if local_name(node.tag) != "si":
                    continue
                output.append(
                    "".join(
                        item.text or ""
                        for item in node.iter()
                        if local_name(item.tag) == "t"
                    )
                )
                node.clear()
    except (ElementTree.ParseError, KeyError) as exc:
        warnings.append(f"无法解析共享字符串：{exc}")
        integrity.fail(
            "excel_shared_strings_unreadable",
            f"无法解析共享字符串：{exc}",
            source_part=part,
        )
    return output


def temporal_styles(archive, names, warnings, integrity):
    if "xl/styles.xml" not in names:
        return {}
    try:
        root = ElementTree.fromstring(archive.read("xl/styles.xml"))
    except (ElementTree.ParseError, KeyError) as exc:
        warnings.append(f"无法解析 Excel 样式：{exc}")
        integrity.fail(
            "excel_styles_unreadable",
            f"无法解析 Excel 样式：{exc}",
            source_part="xl/styles.xml",
        )
        return {}
    custom = {
        node.attrib.get("numFmtId", ""): node.attrib.get("formatCode", "")
        for node in root.iter()
        if local_name(node.tag) == "numFmt"
    }
    built_in_dates = set(range(14, 18)) | set(range(27, 37)) | {50, 57}
    built_in_times = set(range(18, 22)) | {45, 46, 47}
    cell_formats = next(
        (node for node in root.iter() if local_name(node.tag) == "cellXfs"), None
    )
    output = {}
    if cell_formats is None:
        return output
    for index, node in enumerate(cell_formats):
        try:
            format_id = int(node.attrib.get("numFmtId", "0"))
        except ValueError:
            format_id = 0
        code = re.sub(
            r'"[^"]*"|\\.|\[[^\]]*\]',
            "",
            custom.get(str(format_id), ""),
        ).lower()
        has_date = format_id in built_in_dates or "y" in code or "d" in code
        has_time = format_id in built_in_times or "h" in code or "s" in code
        if format_id == 22 or (has_date and has_time):
            output[index] = "datetime"
        elif has_date:
            output[index] = "date"
        elif has_time:
            output[index] = "time"
    return output


def parse_number(value):
    try:
        number = float(value)
    except (TypeError, ValueError, OverflowError):
        return value
    if not math.isfinite(number):
        return value
    if number.is_integer() and abs(number) <= 9007199254740991:
        return int(number)
    return number


def excel_temporal(value, kind, date_1904):
    try:
        serial = float(value)
    except (TypeError, ValueError, OverflowError):
        return value
    whole_days = math.floor(serial)
    fraction = serial - whole_days
    microseconds = round(fraction * 86400 * 1000000)
    if microseconds >= 86400 * 1000000:
        whole_days += 1
        microseconds -= 86400 * 1000000
    if kind == "time":
        hours, remainder = divmod(microseconds, 3600 * 1000000)
        minutes, remainder = divmod(remainder, 60 * 1000000)
        seconds, micros = divmod(remainder, 1000000)
        rendered = f"{hours:02d}:{minutes:02d}:{seconds:02d}"
        return f"{rendered}.{micros:06d}".rstrip("0") if micros else rendered
    if date_1904:
        instant = dt.datetime(1904, 1, 1) + dt.timedelta(
            days=whole_days, microseconds=microseconds
        )
    elif whole_days == 60:
        time_part = (
            dt.datetime.min + dt.timedelta(microseconds=microseconds)
        ).time()
        if kind == "date":
            return "1900-02-29"
        rendered = time_part.isoformat(timespec="microseconds").rstrip("0").rstrip(".")
        return f"1900-02-29T{rendered}"
    else:
        adjusted = whole_days if whole_days < 60 else whole_days - 1
        instant = dt.datetime(1899, 12, 31) + dt.timedelta(
            days=adjusted, microseconds=microseconds
        )
    if kind == "date":
        return instant.date().isoformat()
    return instant.isoformat(timespec="microseconds").rstrip("0").rstrip(".")


def cell_value(
    cell,
    shared_strings,
    date_styles,
    date_1904,
    warnings,
    sheet_name,
    integrity,
):
    cell_type = cell.attrib.get("t", "")
    try:
        style_index = int(cell.attrib.get("s", "0") or 0)
    except ValueError:
        style_index = 0
    value_node = next(
        (node for node in cell if local_name(node.tag) == "v"), None
    )
    inline_node = next(
        (node for node in cell if local_name(node.tag) == "is"), None
    )
    raw = None if value_node is None else value_node.text
    if cell_type == "inlineStr" and inline_node is not None:
        value = "".join(
            node.text or ""
            for node in inline_node.iter()
            if local_name(node.tag) == "t"
        )
    elif raw is None:
        value = None
    elif cell_type == "s":
        try:
            value = shared_strings[int(raw)]
        except (ValueError, IndexError):
            value = raw
            warnings.append(
                f"工作表 {sheet_name} 的共享字符串索引无效：{raw}"
            )
            integrity.fail(
                "excel_shared_string_reference_unresolved",
                f"工作表 {sheet_name} 的共享字符串索引无效：{raw}",
                sheet_name=sheet_name,
                cell=cell.attrib.get("r") or None,
                shared_string_index=raw,
            )
    elif cell_type == "b":
        value = raw == "1"
    elif cell_type in {"e", "str", "d"}:
        value = raw
    elif style_index in date_styles:
        value = excel_temporal(raw, date_styles[style_index], date_1904)
    else:
        value = parse_number(raw)
    return value, raw, cell_type or "n", style_index


def formula_string_segments(expression):
    output = []
    start = 0
    index = 0
    in_string = False
    while index < len(expression):
        if expression[index] != '"':
            index += 1
            continue
        if not in_string:
            if start < index:
                output.append((False, expression[start:index]))
            start = index
            in_string = True
            index += 1
            continue
        if index + 1 < len(expression) and expression[index + 1] == '"':
            index += 2
            continue
        index += 1
        output.append((True, expression[start:index]))
        start = index
        in_string = False
    if start < len(expression):
        output.append((in_string, expression[start:]))
    return output


def translate_a1(reference, row_delta, column_delta):
    match = re.match(r"^(\$?)([A-Za-z]{1,3})(\$?)(\d+)$", reference)
    if not match:
        return reference
    absolute_column, letters, absolute_row, row_text = match.groups()
    column = column_number(letters)
    row = int(row_text)
    if column > 16384 or row > 1048576:
        return reference
    if not absolute_column:
        column += column_delta
    if not absolute_row:
        row += row_delta
    if not (1 <= column <= 16384 and 1 <= row <= 1048576):
        return "#REF!"
    return f"{absolute_column}{column_letters(column)}{absolute_row}{row}"


def translate_column_reference(reference, column_delta):
    match = re.match(r"^(\$?)([A-Za-z]{1,3})$", reference or "")
    if not match:
        return reference
    absolute, letters = match.groups()
    column = column_number(letters)
    if not absolute:
        column += column_delta
    if not 1 <= column <= 16384:
        return "#REF!"
    return f"{absolute}{column_letters(column)}"


def translate_row_reference(reference, row_delta):
    match = re.match(r"^(\$?)(\d+)$", reference or "")
    if not match:
        return reference
    absolute, row_text = match.groups()
    row = int(row_text)
    if not absolute:
        row += row_delta
    if not 1 <= row <= 1048576:
        return "#REF!"
    return f"{absolute}{row}"


def translate_formula(expression, base_reference, target_reference):
    base = cell_coordinates(base_reference)
    target = cell_coordinates(target_reference)
    if not expression or not base or not target:
        return expression
    row_delta = target[0] - base[0]
    column_delta = target[1] - base[1]

    def translate_segment(segment):
        def replace(match):
            if match.start() and segment[match.start() - 1] == "[":
                return match.group(0)
            if match.end() < len(segment) and segment[match.end()] == "]":
                return match.group(0)
            if (
                not match.group("prefix")
                and not any(
                    match.group(name)
                    for name in ("cell_end", "column_start", "row_start")
                )
                and segment[match.end() :].lstrip().startswith("(")
            ):
                return match.group(0)
            if match.group("cell_start"):
                start = translate_a1(
                    match.group("cell_start"), row_delta, column_delta
                )
                end = match.group("cell_end")
                if end:
                    end = translate_a1(end, row_delta, column_delta)
                reference = f"{start}{f':{end}' if end else ''}"
            elif match.group("column_start"):
                start = translate_column_reference(
                    match.group("column_start"), column_delta
                )
                end = translate_column_reference(
                    match.group("column_end"), column_delta
                )
                reference = f"{start}:{end}"
            else:
                start = translate_row_reference(
                    match.group("row_start"), row_delta
                )
                end = translate_row_reference(
                    match.group("row_end"), row_delta
                )
                reference = f"{start}:{end}"
            return f"{match.group('prefix') or ''}{reference}"

        return FORMULA_REF_RE.sub(replace, segment)

    return "".join(
        part if quoted else translate_segment(part)
        for quoted, part in formula_string_segments(expression)
    )


def formula_references(expression):
    output = []
    seen = set()
    for quoted, segment in formula_string_segments(expression or ""):
        if quoted:
            continue
        for match in FORMULA_REF_RE.finditer(segment):
            if (
                not match.group("prefix")
                and not any(
                    match.group(name)
                    for name in ("cell_end", "column_start", "row_start")
                )
                and segment[match.end() :].lstrip().startswith("(")
            ):
                continue
            reference = match.group("reference")
            prefix = (match.group("prefix") or "").rstrip("!")
            identity = prefix, reference
            if identity in seen:
                continue
            seen.add(identity)
            output.append(
                {
                    "sheet_or_book_prefix": prefix or None,
                    "reference": reference,
                    "external_workbook": "[" in prefix,
                }
            )
    return output


def function_arguments(expression, function_name):
    expression = (expression or "").strip().lstrip("=")
    match = re.match(
        rf"(?is)^(?:_xlfn\.)?{re.escape(function_name)}\s*\((.*)\)\s*$",
        expression,
    )
    if not match:
        return None
    source = match.group(1)
    output = []
    start = 0
    depth = 0
    index = 0
    in_string = False
    separator = None
    while index < len(source):
        character = source[index]
        if character == '"':
            if (
                in_string
                and index + 1 < len(source)
                and source[index + 1] == '"'
            ):
                index += 2
                continue
            in_string = not in_string
        elif not in_string:
            if character == "(":
                depth += 1
            elif character == ")":
                depth = max(0, depth - 1)
            elif depth == 0 and character in {",", ";"}:
                separator = separator or character
                if character == separator:
                    output.append(source[start:index].strip())
                    start = index + 1
        index += 1
    output.append(source[start:].strip())
    return output


def literal_formula_string(value):
    value = (value or "").strip()
    if len(value) >= 2 and value.startswith('"') and value.endswith('"'):
        return value[1:-1].replace('""', '"')
    return None


def plain_urls(value):
    if not isinstance(value, str):
        return []
    output = []
    for match in PLAIN_URL_RE.finditer(value):
        visible = match.group(0).rstrip(
            ".,;:!?)]}，。；：！？）】》"
        )
        target = (
            f"https://{visible}"
            if visible.lower().startswith("www.")
            else visible
        )
        output.append((visible, target))
    return output


def link_policy(target):
    try:
        scheme = urlsplit(target or "").scheme.lower()
    except ValueError:
        scheme = ""
    return {
        "scheme": scheme or None,
        "content_role": "untrusted_data",
        "auto_open": False,
        "auto_fetch": False,
    }


class AssetStore:
    def __init__(
        self,
        archive,
        names,
        types,
        warnings,
        external_images,
        integrity,
    ):
        self.archive = archive
        self.names = names
        self.types = types
        self.warnings = warnings
        self.external_images = external_images
        self.integrity = integrity
        self.by_part = {}
        self.by_id = {}

    def ensure(self, package_part):
        if not package_part:
            return None
        if package_part in self.by_part:
            return self.by_part[package_part]
        if package_part not in self.names:
            self.warnings.append(f"图片资源不存在：{package_part}")
            self.integrity.fail(
                "excel_embedded_image_part_missing",
                f"图片资源不存在：{package_part}",
                target_part=package_part,
            )
            return None
        try:
            data = self.archive.read(package_part)
        except KeyError as exc:
            self.warnings.append(f"无法读取图片资源 {package_part}：{exc}")
            self.integrity.fail(
                "excel_embedded_image_unreadable",
                f"无法读取图片资源 {package_part}：{exc}",
                target_part=package_part,
            )
            return None
        digest = hashlib.sha256(data).hexdigest()
        asset_id = f"sha256:{digest}"
        if asset_id not in self.by_id:
            self.by_id[asset_id] = {
                "asset_id": asset_id,
                "name": Path(package_part).name,
                "size": len(data),
                "mime_type": mime_type(package_part, self.types),
                "sha256": digest,
                "identifier_basis": "content",
                "embedded": True,
                "package_paths": [],
                "placement_ids": [],
                "data_base64": base64.b64encode(data).decode("ascii"),
            }
        else:
            self.by_id[asset_id].update(
                {
                    "embedded": True,
                    "data_base64": base64.b64encode(data).decode("ascii"),
                }
            )
        if package_part not in self.by_id[asset_id]["package_paths"]:
            self.by_id[asset_id]["package_paths"].append(package_part)
        self.by_part[package_part] = asset_id
        return asset_id

    def ensure_external(self, target, suggested_name=None):
        localized = self.external_images.localize(
            target, suggested_name=suggested_name
        )
        asset_id = localized["asset_id"]
        attachment = self.by_id.get(asset_id)
        if attachment is None:
            attachment = {
                **public_asset(localized),
                "package_paths": [],
                "placement_ids": [],
                "external_sources": [],
            }
            if localized.get("_local_path"):
                attachment["_local_path"] = localized["_local_path"]
            self.by_id[asset_id] = attachment
        else:
            for key in (
                "localized",
                "localization",
                "requested_url",
                "resolved_url",
                "detected_format",
            ):
                if localized.get(key) is not None:
                    attachment[key] = localized[key]
            if localized.get("_local_path"):
                attachment["_local_path"] = localized["_local_path"]
        if target and target not in attachment.setdefault(
            "external_sources", []
        ):
            attachment["external_sources"].append(target)
        failure = localization_failure(localized)
        if failure:
            self.warnings.append(
                "外链图片本地化失败："
                f"{failure.get('message')} "
                f"({failure.get('code')})；来源 {target}"
            )
            self.integrity.fail(
                "excel_required_external_image_unavailable",
                f"外链图片未能本地化：{target}",
                target=target,
                reason_code=failure.get("code"),
            )
        return asset_id

    def add_placement(self, asset_id, placement_id):
        if asset_id in self.by_id:
            placements = self.by_id[asset_id]["placement_ids"]
            if placement_id not in placements:
                placements.append(placement_id)

    def attachments(self):
        output = []
        for attachment in sorted(
            self.by_id.values(), key=lambda item: item["asset_id"]
        ):
            if attachment.get("data_base64") is not None:
                payload = {
                    key: value
                    for key, value in attachment.items()
                    if key != "_local_path"
                }
            else:
                payload = self.external_images.attachment_payload(attachment)
                if payload is None:
                    continue
            payload.update(
                {
                "package_paths": sorted(attachment["package_paths"]),
                "placement_ids": sorted(attachment["placement_ids"]),
                "source_part": (
                    sorted(attachment["package_paths"])[0]
                    if attachment["package_paths"]
                    else attachment.get("requested_url")
                ),
                "source_url": attachment.get("requested_url"),
                "references": [
                    {"placement_id": placement_id}
                    for placement_id in sorted(attachment["placement_ids"])
                ],
                }
            )
            output.append(payload)
        return output

    def metadata(self):
        return [
            {
                key: value
                for key, value in attachment.items()
                if key not in {"data_base64", "_local_path"}
            }
            for attachment in sorted(
                self.by_id.values(), key=lambda item: item["asset_id"]
            )
        ]


def workbook_metadata(
    archive, names, source_hash, warnings, integrity
):
    part = "xl/workbook.xml"
    if part not in names:
        raise ValueError("XLSX 缺少 xl/workbook.xml")
    root = ElementTree.fromstring(archive.read(part))
    rels = relationships(
        archive, names, part, warnings, integrity, required=True
    )
    workbook_properties = next(
        (
            dict(node.attrib)
            for node in root.iter()
            if local_name(node.tag) == "workbookPr"
        ),
        {},
    )
    calc_properties = next(
        (
            dict(node.attrib)
            for node in root.iter()
            if local_name(node.tag) == "calcPr"
        ),
        {},
    )
    date_1904 = str(workbook_properties.get("date1904", "")).lower() in {
        "1",
        "true",
    }
    defined_names = []
    for node in root.iter():
        if local_name(node.tag) != "definedName":
            continue
        defined_names.append(
            {
                "name": node.attrib.get("name", ""),
                "local_sheet_index": node.attrib.get("localSheetId"),
                "hidden": str(node.attrib.get("hidden", "")).lower()
                in {"1", "true"},
                "function": str(node.attrib.get("function", "")).lower()
                in {"1", "true"},
                "expression": node.text or "",
                "attributes": dict(node.attrib),
            }
        )

    worksheets = []
    non_worksheet_tabs = []
    for position, node in enumerate(
        (item for item in root.iter() if local_name(item.tag) == "sheet"),
        1,
    ):
        relation_id = attribute(node, "id", "")
        relation = rels.get(relation_id)
        target = relation.get("resolved_target") if relation else None
        relationship_type = relation.get("type", "") if relation else ""
        is_worksheet = bool(
            relation
            and (
                relationship_type.endswith("/worksheet")
                or str(target or "").startswith("xl/worksheets/")
            )
        )
        record = {
            "id": stable_id(
                "sheet",
                source_hash,
                position,
                node.attrib.get("sheetId", ""),
                target or "",
            ),
            "position": position,
            "name": node.attrib.get("name", f"工作表 {position}"),
            "state": node.attrib.get("state", "visible"),
            "sheet_id": node.attrib.get("sheetId", ""),
            "relationship_id": relation_id,
            "relationship_type": relationship_type,
            "source_part": target,
        }
        if not relation_id:
            integrity.fail(
                "excel_sheet_relationship_id_missing",
                f"工作簿中的标签 {record['name']} 缺少关系 ID",
                sheet_name=record["name"],
                sheet_position=position,
            )
        elif relation is None:
            integrity.fail(
                "excel_sheet_relationship_missing",
                f"工作簿中的标签 {record['name']} 引用了不存在的关系 {relation_id}",
                sheet_name=record["name"],
                sheet_position=position,
                relationship_id=relation_id,
            )
        elif not target:
            integrity.fail(
                "excel_sheet_target_missing",
                f"工作簿中的标签 {record['name']} 没有可解析的目标部件",
                sheet_name=record["name"],
                sheet_position=position,
                relationship_id=relation_id,
            )
        elif is_worksheet and not relationship_type.endswith("/worksheet"):
            integrity.fail(
                "excel_sheet_relationship_type_invalid",
                f"工作簿中的标签 {record['name']} 目标是工作表但关系类型不正确",
                sheet_name=record["name"],
                sheet_position=position,
                relationship_id=relation_id,
                relationship_type=relationship_type,
            )
        if is_worksheet:
            worksheets.append(record)
        else:
            non_worksheet_tabs.append(record)
    return {
        "date_1904": date_1904,
        "workbook_properties": workbook_properties,
        "calc_properties": {
            **calc_properties,
            "recalculation_performed": False,
            "cache_status": "source_cache_not_recalculated",
        },
        "defined_names": defined_names,
        "worksheets": worksheets,
        "non_worksheet_tabs": non_worksheet_tabs,
        "declared_tab_count": len(worksheets) + len(non_worksheet_tabs),
    }


def formula_record(
    cell, formula_node, sheet_id, value, raw_value, cell_type
):
    expression = formula_node.text
    expression = expression.strip() if expression is not None else None
    formula_type = formula_node.attrib.get("t", "normal") or "normal"
    cache_status = (
        "missing_not_recalculated"
        if raw_value is None
        else (
            "cached_error_not_recalculated"
            if cell_type == "e"
            else "cached_not_recalculated"
        )
    )
    return {
        "id": stable_id("formula", sheet_id, cell["ref"]),
        "cell": cell["ref"],
        "expression": expression,
        "type": formula_type,
        "shared_id": formula_node.attrib.get("si"),
        "range": formula_node.attrib.get("ref"),
        "attributes": dict(formula_node.attrib),
        "base": None,
        "expanded_shared_expression": expression,
        "cached_value": value if raw_value is not None else None,
        "cached_raw_value": raw_value,
        "cache_status": cache_status,
        "recalculation_status": "not_recalculated",
        "references": [],
    }


def read_worksheet(
    archive,
    names,
    descriptor,
    shared_strings,
    date_styles,
    date_1904,
    warnings,
    integrity,
):
    part = descriptor["source_part"]
    cells = []
    row_metadata = []
    formulas = []
    raw_hyperlinks = []
    drawing_ids = []
    legacy_drawing_ids = []
    merged_ranges = []
    dimension = None
    if not part or part not in names:
        raise ValueError(f"工作表文件不存在：{part or '未提供'}")
    next_row = 1
    with archive.open(part) as source:
        for _, node in ElementTree.iterparse(source, events=("end",)):
            kind = local_name(node.tag)
            if kind == "dimension":
                dimension = node.attrib.get("ref")
                node.clear()
            elif kind == "mergeCell":
                if node.attrib.get("ref"):
                    merged_ranges.append(node.attrib["ref"])
                node.clear()
            elif kind == "hyperlink":
                raw_hyperlinks.append(dict(node.attrib))
                node.clear()
            elif kind == "drawing":
                relation_id = attribute(node, "id")
                if relation_id:
                    drawing_ids.append(relation_id)
                else:
                    integrity.fail(
                        "excel_drawing_relationship_id_missing",
                        f"工作表 {descriptor['name']} 的 Drawing 声明缺少关系 ID",
                        sheet_id=descriptor["id"],
                        sheet_name=descriptor["name"],
                        source_part=part,
                    )
                node.clear()
            elif kind == "legacyDrawing":
                relation_id = attribute(node, "id")
                if relation_id:
                    legacy_drawing_ids.append(relation_id)
                else:
                    integrity.fail(
                        "excel_legacy_drawing_relationship_id_missing",
                        f"工作表 {descriptor['name']} 的旧式 Drawing 声明缺少关系 ID",
                        sheet_id=descriptor["id"],
                        sheet_name=descriptor["name"],
                        source_part=part,
                    )
                node.clear()
            elif kind == "row":
                try:
                    row_number = int(node.attrib.get("r", next_row))
                except ValueError:
                    row_number = next_row
                next_row = row_number + 1
                row_entry = {
                    "row": row_number,
                    "hidden": str(node.attrib.get("hidden", "")).lower()
                    in {"1", "true"},
                    "height": node.attrib.get("ht"),
                    "outline_level": node.attrib.get("outlineLevel"),
                    "collapsed": str(
                        node.attrib.get("collapsed", "")
                    ).lower()
                    in {"1", "true"},
                    "cell_refs": [],
                }
                previous_column = 0
                for cell_node in (
                    item
                    for item in node
                    if local_name(item.tag) == "c"
                ):
                    reference = cell_node.attrib.get("r")
                    coordinates = cell_coordinates(reference or "")
                    if coordinates:
                        cell_row, column = coordinates
                    else:
                        cell_row = row_number
                        column = previous_column + 1
                        reference = cell_reference(cell_row, column)
                    previous_column = column
                    value, raw, cell_type, style_index = cell_value(
                        cell_node,
                        shared_strings,
                        date_styles,
                        date_1904,
                        warnings,
                        descriptor["name"],
                        integrity,
                    )
                    cell = {
                        "ref": reference,
                        "row": cell_row,
                        "column": column,
                        "column_letter": column_letters(column),
                        "value": value,
                        "raw_value": raw,
                        "data_type": cell_type,
                        "style_index": style_index,
                        "formula": None,
                        "hyperlink_ids": [],
                    }
                    formula_node = next(
                        (
                            item
                            for item in cell_node
                            if local_name(item.tag) == "f"
                        ),
                        None,
                    )
                    if formula_node is not None:
                        formula = formula_record(
                            cell,
                            formula_node,
                            descriptor["id"],
                            value,
                            raw,
                            cell_type,
                        )
                        cell["formula"] = formula
                        formulas.append(formula)
                    cells.append(cell)
                    row_entry["cell_refs"].append(reference)
                row_metadata.append(row_entry)
                node.clear()

    shared_masters = {}
    for formula in formulas:
        shared_id = formula.get("shared_id")
        if (
            formula["type"] == "shared"
            and shared_id is not None
            and formula.get("expression")
        ):
            shared_masters[shared_id] = {
                "cell": formula["cell"],
                "expression": formula["expression"],
                "range": formula.get("range"),
            }

    cells_by_ref = {cell["ref"]: cell for cell in cells}
    for formula in formulas:
        if formula["type"] == "shared" and formula.get("shared_id") is not None:
            master = shared_masters.get(formula["shared_id"])
            formula["base"] = master
            if master:
                formula["expanded_shared_expression"] = (
                    formula["expression"]
                    or translate_formula(
                        master["expression"],
                        master["cell"],
                        formula["cell"],
                    )
                )
            else:
                formula["expanded_shared_expression"] = formula["expression"]
                warnings.append(
                    f"工作表 {descriptor['name']} 单元格 "
                    f"{formula['cell']} 的共享公式缺少基准公式"
                )
                integrity.fail(
                    "excel_shared_formula_base_missing",
                    f"工作表 {descriptor['name']} 单元格 {formula['cell']} 的共享公式缺少基准公式",
                    sheet_id=descriptor["id"],
                    sheet_name=descriptor["name"],
                    cell=formula["cell"],
                    shared_id=formula.get("shared_id"),
                )
        effective = (
            formula.get("expanded_shared_expression")
            or formula.get("expression")
            or ""
        )
        formula["references"] = formula_references(effective)
        cell = cells_by_ref.get(formula["cell"])
        if cell and formula["cached_raw_value"] is None:
            cell["value"] = f"={effective}" if effective else "=<unresolved>"

    return {
        **descriptor,
        "parse_status": "parsed",
        "source_dimension": dimension,
        "merged_ranges": merged_ranges,
        "row_metadata": row_metadata,
        "cells": cells,
        "formulas": formulas,
        "raw_hyperlinks": raw_hyperlinks,
        "drawing_relationship_ids": drawing_ids,
        "legacy_drawing_relationship_ids": legacy_drawing_ids,
    }


def add_hyperlink(output, seen, sheet, link):
    identity = json.dumps(
        {
            key: link.get(key)
            for key in (
                "kind",
                "ref",
                "target",
                "target_expression",
                "location",
                "relationship_id",
            )
        },
        ensure_ascii=False,
        sort_keys=True,
    )
    if identity in seen:
        return
    seen.add(identity)
    link["id"] = stable_id("hyperlink", sheet["id"], identity)
    target = link.get("target") or link.get("location") or ""
    link["policy"] = link_policy(target)
    output.append(link)


def attach_hyperlinks(sheet, sheet_relationships, warnings):
    links = []
    seen = set()
    for raw in sheet.pop("raw_hyperlinks", []):
        relation_id = next(
            (
                value
                for key, value in raw.items()
                if local_name(key) == "id"
            ),
            None,
        )
        relation = sheet_relationships.get(relation_id or "")
        location = raw.get("location")
        target = relation.get("target") if relation else None
        kind = (
            "internal"
            if location or str(target or "").startswith("#")
            else "external"
        )
        add_hyperlink(
            links,
            seen,
            sheet,
            {
                "kind": kind,
                "source": "worksheet_hyperlink",
                "ref": raw.get("ref"),
                "target": target,
                "target_expression": None,
                "location": location,
                "display": raw.get("display"),
                "tooltip": raw.get("tooltip"),
                "relationship_id": relation_id,
            },
        )
        if relation_id and relation is None:
            warnings.append(
                f"工作表 {sheet['name']} 的超链接关系不存在：{relation_id}"
            )

    for cell in sheet["cells"]:
        formula = cell.get("formula")
        effective = (
            formula.get("expanded_shared_expression")
            or formula.get("expression")
            or ""
            if formula
            else ""
        )
        arguments = function_arguments(effective, "HYPERLINK")
        if arguments:
            target = literal_formula_string(arguments[0])
            display = (
                literal_formula_string(arguments[1])
                if len(arguments) > 1
                else None
            )
            add_hyperlink(
                links,
                seen,
                sheet,
                {
                    "kind": (
                        "internal"
                        if str(target or "").startswith("#")
                        else "formula"
                    ),
                    "source": "HYPERLINK_formula",
                    "ref": cell["ref"],
                    "target": target,
                    "target_expression": arguments[0],
                    "location": (
                        str(target)[1:]
                        if str(target or "").startswith("#")
                        else None
                    ),
                    "display": display,
                    "tooltip": None,
                    "relationship_id": None,
                },
            )
        if not formula:
            for visible, target in plain_urls(cell.get("value")):
                add_hyperlink(
                    links,
                    seen,
                    sheet,
                    {
                        "kind": "plain_text",
                        "source": "cell_text",
                        "ref": cell["ref"],
                        "target": target,
                        "target_expression": None,
                        "location": None,
                        "display": visible,
                        "tooltip": None,
                        "relationship_id": None,
                    },
                )

    for link in links:
        bounds = range_bounds(link.get("ref") or "")
        if not bounds:
            continue
        r1, c1, r2, c2 = bounds
        for cell in sheet["cells"]:
            if r1 <= cell["row"] <= r2 and c1 <= cell["column"] <= c2:
                cell["hyperlink_ids"].append(link["id"])
    sheet["hyperlinks"] = links


def normalize_header(value, column, used):
    header = re.sub(r"\s+", " ", str(value or "")).strip()
    header = header or f"column_{column}"
    base = header
    suffix = 2
    while header in used:
        header = f"{base}_{suffix}"
        suffix += 1
    used.add(header)
    return header


def infer_type(values):
    kinds = set()
    for value in values:
        if value is None:
            continue
        if isinstance(value, bool):
            kinds.add("boolean")
        elif isinstance(value, int):
            kinds.add("integer")
        elif isinstance(value, float):
            kinds.add("number")
        elif isinstance(value, str) and re.fullmatch(
            r"\d{4}-\d{2}-\d{2}", value
        ):
            kinds.add("date")
        elif isinstance(value, str) and re.fullmatch(
            r"\d{4}-\d{2}-\d{2}T.*", value
        ):
            kinds.add("datetime")
        elif isinstance(value, str) and re.fullmatch(
            r"\d{2}:\d{2}:\d{2}(?:\.\d+)?", value
        ):
            kinds.add("time")
        else:
            kinds.add("string")
    if not kinds:
        return "null"
    if kinds <= {"integer", "number"}:
        return "number" if "number" in kinds else "integer"
    return next(iter(kinds)) if len(kinds) == 1 else "mixed"


def build_cleaned_view(sheet):
    rows = {}
    for cell in sheet["cells"]:
        rows.setdefault(cell["row"], {})[cell["column"]] = cell
    meaningful_rows = [
        row
        for row in sorted(rows)
        if any(
            cell.get("value") not in (None, "")
            or cell.get("formula")
            or cell.get("hyperlink_ids")
            for cell in rows[row].values()
        )
    ]
    active_columns = sorted(
        {
            column
            for row in meaningful_rows
            for column, cell in rows[row].items()
            if cell.get("value") not in (None, "")
            or cell.get("formula")
            or cell.get("hyperlink_ids")
        }
    )
    header_row = meaningful_rows[0] if meaningful_rows else None
    used = set()
    columns = []
    header_by_column = {}
    for column in active_columns:
        header_cell = rows.get(header_row, {}).get(column) if header_row else None
        header = normalize_header(
            header_cell.get("value") if header_cell else None,
            column,
            used,
        )
        header_by_column[column] = header
        columns.append(
            {
                "source_column": column,
                "column_letter": column_letters(column),
                "header": header,
                "header_cell": (
                    header_cell["ref"] if header_cell else None
                ),
            }
        )

    cleaned_rows = []
    seen = {}
    duplicates = 0
    for row_number in meaningful_rows:
        if row_number == header_row:
            continue
        values = {}
        cell_refs = {}
        formula_ids = {}
        hyperlink_ids = {}
        identity_values = []
        for column in active_columns:
            header = header_by_column[column]
            cell = rows[row_number].get(column)
            value = cell.get("value") if cell else None
            values[header] = value
            cell_refs[header] = (
                cell["ref"] if cell else cell_reference(row_number, column)
            )
            formula = cell.get("formula") if cell else None
            formula_ids[header] = formula.get("id") if formula else None
            hyperlink_ids[header] = (
                list(cell.get("hyperlink_ids", [])) if cell else []
            )
            identity_values.append(
                {
                    "value": value,
                    "formula": (
                        formula.get("expanded_shared_expression")
                        or formula.get("expression")
                        if formula
                        else None
                    ),
                }
            )
        identity = json.dumps(
            identity_values,
            ensure_ascii=False,
            sort_keys=True,
            default=str,
        )
        duplicate_of = seen.get(identity)
        if duplicate_of is None:
            seen[identity] = row_number
        else:
            duplicates += 1
        cleaned_rows.append(
            {
                "source_row": row_number,
                "values": values,
                "cell_refs": cell_refs,
                "formula_ids": formula_ids,
                "hyperlink_ids": hyperlink_ids,
                "is_duplicate": duplicate_of is not None,
                "duplicate_of_source_row": duplicate_of,
            }
        )

    for column in columns:
        values = [
            row["values"].get(column["header"]) for row in cleaned_rows
        ]
        column["type"] = infer_type(values)
        column["non_null_count"] = sum(
            value not in (None, "") for value in values
        )
        column["null_count"] = sum(
            value in (None, "") for value in values
        )

    return {
        "header_strategy": "first_nonempty_row_derived_view",
        "header_row": header_row,
        "columns": columns,
        "rows": cleaned_rows,
        "duplicate_rows_marked": duplicates,
        "duplicate_rows_removed": 0,
        "raw_cells_preserved": True,
    }


def marker(anchor, wanted):
    node = next(
        (item for item in anchor if local_name(item.tag) == wanted), None
    )
    if node is None:
        return None
    values = {}
    for child in node:
        key = local_name(child.tag)
        try:
            values[key] = int(child.text or "0")
        except ValueError:
            values[key] = child.text
    row_zero = values.get("row")
    column_zero = values.get("col")
    result = {
        "row_zero_based": row_zero,
        "column_zero_based": column_zero,
        "row_offset_emu": values.get("rowOff"),
        "column_offset_emu": values.get("colOff"),
        "cell": None,
    }
    if isinstance(row_zero, int) and isinstance(column_zero, int):
        result["row"] = row_zero + 1
        result["column"] = column_zero + 1
        result["column_letter"] = column_letters(column_zero + 1)
        result["cell"] = cell_reference(row_zero + 1, column_zero + 1)
    return result


def anchor_geometry(anchor):
    kind = local_name(anchor.tag)
    start = marker(anchor, "from")
    end = marker(anchor, "to")
    position_node = next(
        (item for item in anchor if local_name(item.tag) == "pos"), None
    )
    extent_node = next(
        (item for item in anchor if local_name(item.tag) == "ext"), None
    )
    covered_range = None
    if start and end and start.get("cell") and end.get("cell"):
        covered_range = f"{start['cell']}:{end['cell']}"
    return {
        "type": kind,
        "edit_as": anchor.attrib.get("editAs"),
        "from": start,
        "to": end,
        "covered_range": covered_range,
        "absolute_position_emu": (
            {
                "x": position_node.attrib.get("x"),
                "y": position_node.attrib.get("y"),
            }
            if position_node is not None
            else None
        ),
        "extent_emu": (
            {
                "cx": extent_node.attrib.get("cx"),
                "cy": extent_node.attrib.get("cy"),
            }
            if extent_node is not None
            else None
        ),
    }


def concise_cell(cell, header_by_column):
    formula = cell.get("formula")
    return {
        "ref": cell["ref"],
        "value": cell.get("value"),
        "header": header_by_column.get(cell["column"]),
        "formula_id": formula.get("id") if formula else None,
        "hyperlink_ids": list(cell.get("hyperlink_ids", [])),
    }


def anchor_context(anchor, sheet):
    start = anchor.get("from")
    end = anchor.get("to")
    if not start or not start.get("row") or not start.get("column"):
        return {
            "relation": "absolute_no_cell_anchor",
            "anchor_cell": None,
            "header_context": [],
            "row_context": [],
            "covered_cells": [],
        }
    header_by_column = {
        column["source_column"]: column["header"]
        for column in sheet["cleaned_view"]["columns"]
    }
    start_row = start["row"]
    start_column = start["column"]
    row_context = [
        concise_cell(cell, header_by_column)
        for cell in sheet["cells"]
        if cell["row"] == start_row
    ]
    if end and end.get("row") and end.get("column"):
        r1, r2 = sorted((start_row, end["row"]))
        c1, c2 = sorted((start_column, end["column"]))
        covered = [
            concise_cell(cell, header_by_column)
            for cell in sheet["cells"]
            if r1 <= cell["row"] <= r2
            and c1 <= cell["column"] <= c2
        ]
    else:
        r1 = r2 = start_row
        c1 = c2 = start_column
        covered = [
            concise_cell(cell, header_by_column)
            for cell in sheet["cells"]
            if cell["row"] == start_row
            and cell["column"] == start_column
        ]
    headers = [
        {
            "source_column": column,
            "column_letter": column_letters(column),
            "header": header_by_column.get(column),
        }
        for column in sorted(header_by_column)
        if c1 <= column <= c2
    ]
    return {
        "relation": "deterministic_ooxml_anchor",
        "anchor_cell": start["cell"],
        "anchor_row": start_row,
        "anchor_column": start_column,
        "header_context": headers,
        "row_context": row_context,
        "covered_cells": covered,
        "semantic_relation": {
            "status": "not_inferred_by_parser",
            "confidence": None,
            "evidence_cells": [],
        },
    }


def picture_properties(picture):
    properties = first_descendant(picture, "cNvPr")
    source_rect = first_descendant(picture, "srcRect")
    transform = first_descendant(picture, "xfrm")
    return {
        "office_id": (
            properties.attrib.get("id") if properties is not None else None
        ),
        "name": (
            properties.attrib.get("name") if properties is not None else None
        ),
        "description": (
            properties.attrib.get("descr")
            if properties is not None
            else None
        ),
        "title": (
            properties.attrib.get("title")
            if properties is not None
            else None
        ),
        "crop": dict(source_rect.attrib) if source_rect is not None else {},
        "transform": dict(transform.attrib) if transform is not None else {},
    }


def drawing_hyperlink(picture, drawing_rels):
    click = first_descendant(picture, "hlinkClick")
    if click is None:
        return None
    relation_id = attribute(click, "id")
    relation = drawing_rels.get(relation_id or "")
    target = relation.get("target") if relation else None
    return {
        "kind": "drawing",
        "relationship_id": relation_id,
        "target": target,
        "tooltip": click.attrib.get("tooltip"),
        "policy": link_policy(target),
    }


def parse_drawings(
    archive, names, sheet, sheet_rels, assets, warnings, integrity
):
    placements = []
    for drawing_relation_id in sheet["drawing_relationship_ids"]:
        drawing_relation = sheet_rels.get(drawing_relation_id)
        drawing_part = (
            drawing_relation.get("resolved_target")
            if drawing_relation
            else None
        )
        if not drawing_part or drawing_part not in names:
            warnings.append(
                f"工作表 {sheet['name']} 的 Drawing 不存在："
                f"{drawing_relation_id}"
            )
            integrity.fail(
                "excel_drawing_part_missing",
                f"工作表 {sheet['name']} 的 Drawing 不存在：{drawing_relation_id}",
                sheet_id=sheet["id"],
                sheet_name=sheet["name"],
                relationship_id=drawing_relation_id,
                target_part=drawing_part,
            )
            continue
        if not str(drawing_relation.get("type") or "").endswith(
            "/drawing"
        ):
            integrity.fail(
                "excel_drawing_relationship_type_invalid",
                f"工作表 {sheet['name']} 的关系 {drawing_relation_id} 不是 drawing",
                sheet_id=sheet["id"],
                sheet_name=sheet["name"],
                relationship_id=drawing_relation_id,
                relationship_type=drawing_relation.get("type"),
            )
        drawing_rels = relationships(
            archive,
            names,
            drawing_part,
            warnings,
            integrity,
            required=False,
        )
        try:
            root = ElementTree.fromstring(archive.read(drawing_part))
        except (ElementTree.ParseError, KeyError) as exc:
            warnings.append(f"无法解析 Drawing {drawing_part}：{exc}")
            integrity.fail(
                "excel_drawing_part_unreadable",
                f"无法解析 Drawing {drawing_part}：{exc}",
                sheet_id=sheet["id"],
                sheet_name=sheet["name"],
                source_part=drawing_part,
            )
            continue
        drawing_order = 0
        for anchor in root:
            anchor_type = local_name(anchor.tag)
            anchor_pictures = [
                item
                for item in anchor.iter()
                if local_name(item.tag) == "pic"
            ]
            if anchor_type not in {
                "oneCellAnchor",
                "twoCellAnchor",
                "absoluteAnchor",
            }:
                if anchor_pictures:
                    integrity.fail(
                        "excel_image_anchor_unsupported",
                        f"Drawing {drawing_part} 包含无法定位的图片锚点 {anchor_type}",
                        sheet_id=sheet["id"],
                        sheet_name=sheet["name"],
                        source_part=drawing_part,
                        anchor_type=anchor_type,
                        picture_count=len(anchor_pictures),
                    )
                continue
            pictures = anchor_pictures
            for picture in pictures:
                drawing_order += 1
                blip = first_descendant(picture, "blip")
                relation_id = (
                    attribute(blip, "embed")
                    or attribute(blip, "link")
                    if blip is not None
                    else None
                )
                relation = drawing_rels.get(relation_id or "")
                package_part = (
                    relation.get("resolved_target") if relation else None
                )
                external_target = (
                    relation.get("target")
                    if relation
                    and relation.get("target_mode", "").lower()
                    == "external"
                    else None
                )
                properties = picture_properties(picture)
                if not relation_id:
                    integrity.fail(
                        "excel_image_relationship_id_missing",
                        f"Drawing {drawing_part} 中的图片缺少关系 ID",
                        sheet_id=sheet["id"],
                        sheet_name=sheet["name"],
                        source_part=drawing_part,
                        drawing_order=drawing_order,
                    )
                elif relation is None:
                    integrity.fail(
                        "excel_image_relationship_missing",
                        f"Drawing {drawing_part} 中的图片关系 {relation_id} 不存在",
                        sheet_id=sheet["id"],
                        sheet_name=sheet["name"],
                        source_part=drawing_part,
                        relationship_id=relation_id,
                        drawing_order=drawing_order,
                    )
                elif not str(relation.get("type") or "").endswith(
                    "/image"
                ):
                    integrity.fail(
                        "excel_image_relationship_type_invalid",
                        f"Drawing {drawing_part} 中的关系 {relation_id} 不是 image",
                        sheet_id=sheet["id"],
                        sheet_name=sheet["name"],
                        source_part=drawing_part,
                        relationship_id=relation_id,
                        relationship_type=relation.get("type"),
                        drawing_order=drawing_order,
                    )
                if external_target:
                    asset_id = assets.ensure_external(
                        external_target,
                        suggested_name=(
                            properties.get("name")
                            or properties.get("description")
                        ),
                    )
                else:
                    asset_id = (
                        assets.ensure(package_part)
                        if package_part and package_part in names
                        else None
                    )
                if not asset_id:
                    integrity.fail(
                        "excel_required_image_unresolved",
                        f"Drawing {drawing_part} 中的图片未解析出附件",
                        sheet_id=sheet["id"],
                        sheet_name=sheet["name"],
                        source_part=drawing_part,
                        relationship_id=relation_id,
                        drawing_order=drawing_order,
                        target_part=package_part,
                        external_target=external_target,
                    )
                geometry = anchor_geometry(anchor)
                anchor_valid = True
                if anchor_type in {"oneCellAnchor", "twoCellAnchor"}:
                    anchor_valid = bool(
                        geometry.get("from")
                        and geometry["from"].get("cell")
                    )
                    if anchor_type == "twoCellAnchor":
                        anchor_valid = anchor_valid and bool(
                            geometry.get("to")
                            and geometry["to"].get("cell")
                        )
                    if anchor_type == "oneCellAnchor":
                        anchor_valid = anchor_valid and bool(
                            geometry.get("extent_emu")
                        )
                elif anchor_type == "absoluteAnchor":
                    anchor_valid = bool(
                        geometry.get("absolute_position_emu")
                        and geometry.get("extent_emu")
                    )
                if not anchor_valid:
                    integrity.fail(
                        "excel_image_anchor_incomplete",
                        f"Drawing {drawing_part} 中的图片锚点缺少完整位置数据",
                        sheet_id=sheet["id"],
                        sheet_name=sheet["name"],
                        source_part=drawing_part,
                        relationship_id=relation_id,
                        drawing_order=drawing_order,
                        anchor_type=anchor_type,
                    )
                placement_id = stable_id(
                    "placement",
                    sheet["id"],
                    drawing_part,
                    drawing_order,
                    asset_id or external_target or relation_id,
                )
                placement = {
                    "id": placement_id,
                    "source": "worksheet_drawing",
                    "sheet_id": sheet["id"],
                    "sheet_name": sheet["name"],
                    "drawing_part": drawing_part,
                    "drawing_order": drawing_order,
                    "asset_id": asset_id,
                    "external_target": external_target,
                    "relationship_id": relation_id,
                    "properties": properties,
                    "anchor": geometry,
                    "anchor_context": anchor_context(geometry, sheet),
                    "click_hyperlink": drawing_hyperlink(
                        picture, drawing_rels
                    ),
                    "external_image_localization": dict(
                        assets.by_id.get(asset_id, {}).get(
                            "localization"
                        )
                        or {}
                    ),
                }
                placements.append(placement)
                if asset_id:
                    assets.add_placement(asset_id, placement_id)
    return placements


def cell_image_catalog(
    archive, names, assets, warnings, integrity
):
    entries = []
    lookup = {}
    parts = sorted(
        name
        for name in names
        if posixpath.basename(name).lower() == "cellimages.xml"
    )
    for part in parts:
        try:
            root = ElementTree.fromstring(archive.read(part))
        except (ElementTree.ParseError, KeyError) as exc:
            warnings.append(f"无法解析单元格图片目录 {part}：{exc}")
            integrity.fail(
                "excel_cell_image_catalog_unreadable",
                f"无法解析单元格图片目录 {part}：{exc}",
                source_part=part,
            )
            continue
        has_relationship_images = any(
            local_name(item.tag) == "blip"
            and bool(attribute(item, "embed") or attribute(item, "link"))
            for item in root.iter()
        )
        rels = relationships(
            archive,
            names,
            part,
            warnings,
            integrity,
            required=has_relationship_images,
        )
        for index, node in enumerate(
            (
                item
                for item in root.iter()
                if local_name(item.tag) == "cellImage"
            ),
            1,
        ):
            picture = first_descendant(node, "pic")
            if picture is None:
                integrity.fail(
                    "excel_cell_image_picture_missing",
                    f"单元格图片目录 {part} 的第 {index} 项缺少 pic",
                    source_part=part,
                    catalog_index=index,
                )
                continue
            properties = picture_properties(picture)
            blip = first_descendant(picture, "blip")
            relation_id = (
                attribute(blip, "embed")
                or attribute(blip, "link")
                if blip is not None
                else None
            )
            relation = rels.get(relation_id or "")
            package_part = (
                relation.get("resolved_target") if relation else None
            )
            external_target = (
                relation.get("target")
                if relation
                and relation.get("target_mode", "").lower() == "external"
                else None
            )
            asset_id = (
                assets.ensure_external(
                    external_target,
                    suggested_name=(
                        properties.get("name")
                        or properties.get("description")
                    ),
                )
                if external_target
                else assets.ensure(package_part)
                if package_part
                else None
            )
            if not relation_id:
                integrity.fail(
                    "excel_cell_image_relationship_id_missing",
                    f"单元格图片目录 {part} 的第 {index} 项缺少关系 ID",
                    source_part=part,
                    catalog_index=index,
                )
            elif relation is None:
                integrity.fail(
                    "excel_cell_image_relationship_missing",
                    f"单元格图片目录 {part} 的关系 {relation_id} 不存在",
                    source_part=part,
                    catalog_index=index,
                    relationship_id=relation_id,
                )
            elif not str(relation.get("type") or "").endswith(
                "/image"
            ):
                integrity.fail(
                    "excel_cell_image_relationship_type_invalid",
                    f"单元格图片目录 {part} 的关系 {relation_id} 不是 image",
                    source_part=part,
                    catalog_index=index,
                    relationship_id=relation_id,
                    relationship_type=relation.get("type"),
                )
            if not asset_id:
                integrity.fail(
                    "excel_cell_image_unresolved",
                    f"单元格图片目录 {part} 的第 {index} 项未解析出附件",
                    source_part=part,
                    catalog_index=index,
                    relationship_id=relation_id,
                    target_part=package_part,
                    external_target=external_target,
                )
            entry = {
                "catalog_id": stable_id(
                    "cell-image",
                    part,
                    index,
                    asset_id or relation_id,
                ),
                "source_part": part,
                "asset_id": asset_id,
                "relationship_id": relation_id,
                "external_target": external_target,
                "external_image_localization": dict(
                    assets.by_id.get(asset_id, {}).get("localization") or {}
                ),
                "properties": properties,
            }
            entries.append(entry)
            for key in (
                properties.get("name"),
                properties.get("description"),
                properties.get("title"),
                properties.get("office_id"),
            ):
                if key:
                    lookup[str(key)] = entry
    return entries, lookup


def formula_image_placements(
    sheet, catalog_lookup, assets, warnings, integrity
):
    output = []
    for cell in sheet["cells"]:
        formula = cell.get("formula")
        if not formula:
            continue
        expression = (
            formula.get("expanded_shared_expression")
            or formula.get("expression")
            or ""
        )
        source = None
        asset_id = None
        external_target = None
        catalog_key = None
        arguments = function_arguments(expression, "DISPIMG")
        if arguments:
            source = "DISPIMG_formula"
            catalog_key = literal_formula_string(arguments[0])
            entry = catalog_lookup.get(str(catalog_key))
            if entry:
                asset_id = entry.get("asset_id")
                external_target = entry.get("external_target")
            else:
                warnings.append(
                    f"工作表 {sheet['name']} 单元格 {cell['ref']} "
                    f"无法匹配单元格图片 {catalog_key or arguments[0]}"
                )
                integrity.fail(
                    "excel_formula_image_catalog_entry_missing",
                    f"工作表 {sheet['name']} 单元格 {cell['ref']} 无法匹配单元格图片",
                    sheet_id=sheet["id"],
                    sheet_name=sheet["name"],
                    cell=cell["ref"],
                    catalog_key=catalog_key or arguments[0],
                )
        else:
            arguments = function_arguments(expression, "IMAGE")
            if arguments:
                source = "IMAGE_formula"
                external_target = literal_formula_string(arguments[0])
                if external_target:
                    asset_id = assets.ensure_external(external_target)
                else:
                    integrity.fail(
                        "excel_formula_image_target_unresolved",
                        f"工作表 {sheet['name']} 单元格 {cell['ref']} 的 IMAGE 公式不是可静态解析的网址",
                        sheet_id=sheet["id"],
                        sheet_name=sheet["name"],
                        cell=cell["ref"],
                        target_expression=arguments[0],
                    )
        if not source:
            continue
        if not asset_id:
            integrity.fail(
                "excel_formula_image_unresolved",
                f"工作表 {sheet['name']} 单元格 {cell['ref']} 的公式图片未解析出附件",
                sheet_id=sheet["id"],
                sheet_name=sheet["name"],
                cell=cell["ref"],
                image_source=source,
                catalog_key=catalog_key,
                external_target=external_target,
            )
        geometry = {
            "type": "in_cell",
            "edit_as": None,
            "from": {
                "row_zero_based": cell["row"] - 1,
                "column_zero_based": cell["column"] - 1,
                "row_offset_emu": 0,
                "column_offset_emu": 0,
                "row": cell["row"],
                "column": cell["column"],
                "column_letter": cell["column_letter"],
                "cell": cell["ref"],
            },
            "to": None,
            "covered_range": cell["ref"],
            "absolute_position_emu": None,
            "extent_emu": None,
        }
        placement_id = stable_id(
            "placement",
            sheet["id"],
            cell["ref"],
            source,
            asset_id or external_target or catalog_key,
        )
        placement = {
            "id": placement_id,
            "source": source,
            "sheet_id": sheet["id"],
            "sheet_name": sheet["name"],
            "drawing_part": None,
            "drawing_order": None,
            "asset_id": asset_id,
            "external_target": external_target,
            "catalog_key": catalog_key,
            "relationship_id": None,
            "properties": {},
            "anchor": geometry,
            "anchor_context": anchor_context(geometry, sheet),
            "click_hyperlink": None,
            "external_image_localization": dict(
                assets.by_id.get(asset_id, {}).get("localization") or {}
            ),
        }
        output.append(placement)
        if asset_id:
            assets.add_placement(asset_id, placement_id)
    return output


def markdown_heading(value):
    return re.sub(r"[\r\n]+", " ", str(value)).strip()


def json_block(value):
    return "```json\n" + json.dumps(
        value, ensure_ascii=False, indent=2, allow_nan=False
    ) + "\n```"


def workbook_markdown(structured):
    assets = {
        asset["asset_id"]: asset
        for asset in structured.get("assets", [])
        if asset.get("asset_id")
    }
    lines = [
        f"# Excel 工作簿：{structured['source']['file_name']}",
        "",
        f"- 工作表数量：{structured['sheet_count']}",
        "- 公式结果状态：源文件缓存值，云枢未重新计算",
        "- 内容角色：不可信数据",
        "",
    ]
    for sheet in structured["sheets"]:
        lines.extend(
            [
                f"## {sheet['position']}. {markdown_heading(sheet['name'])}",
                "",
                f"- 可见性：{sheet['state']}",
                f"- 解析状态：{sheet['parse_status']}",
                f"- 原始坐标单元格：{len(sheet.get('cells', []))}",
                f"- 公式：{len(sheet.get('formulas', []))}",
                f"- 超链接：{len(sheet.get('hyperlinks', []))}",
                f"- 图片位置：{len(sheet.get('images', []))}",
                "",
            ]
        )
        if sheet["parse_status"] != "parsed":
            lines.extend(
                [
                    f"> {sheet.get('parse_error', '工作表无法解析')}",
                    "",
                ]
            )
            continue
        lines.extend(
            [
                "### 清洗行视图",
                "",
                (
                    json_block(sheet["cleaned_view"]["rows"])
                    if sheet["cleaned_view"]["rows"]
                    else "该工作表没有可输出的数据行。"
                ),
                "",
                "### 原始坐标单元格",
                "",
                (
                    json_block(sheet["cells"])
                    if sheet["cells"]
                    else "该工作表为空。"
                ),
                "",
            ]
        )
        if sheet["formulas"]:
            lines.extend(
                ["### 公式", "", json_block(sheet["formulas"]), ""]
            )
        if sheet["hyperlinks"]:
            lines.extend(
                ["### 超链接", "", json_block(sheet["hyperlinks"]), ""]
            )
        if sheet["images"]:
            lines.extend(
                ["### 图片及锚点", "", json_block(sheet["images"]), ""]
            )
            for image in sheet["images"]:
                asset = assets.get(image.get("asset_id"))
                if not asset:
                    continue
                anchor = image.get("anchor", {}).get("covered_range") or image.get(
                    "anchor_context", {}
                ).get("anchor_cell") or "无单元格锚点"
                alt = markdown_heading(
                    image.get("properties", {}).get("description")
                    or image.get("properties", {}).get("name")
                    or asset.get("name")
                    or "工作表图片"
                )
                lines.extend([f"#### 图片位置：{sheet['name']}!{anchor}", ""])
                if asset.get("name") and asset.get("localized") is not False:
                    lines.extend(
                        [f"![{alt}](attachment://{image['id']})", ""]
                    )
                else:
                    failure = asset.get("localization") or {}
                    lines.extend(
                        [
                            "[外链图片本地化失败："
                            f"{alt}；{failure.get('message') or '无法读取'}]",
                            "",
                        ]
                    )
    if structured["non_worksheet_tabs"]:
        lines.extend(
            [
                "## 非工作表标签",
                "",
                json_block(structured["non_worksheet_tabs"]),
                "",
            ]
        )
    return "\n".join(lines)


def validate_package_paths(archive):
    for info in archive.infolist():
        normalized = posixpath.normpath(info.filename.replace("\\", "/"))
        if (
            normalized == ".."
            or normalized.startswith("../")
            or normalized.startswith("/")
        ):
            raise ValueError("OOXML 包含不安全的文件路径")


def _extract_xlsx(path, external_images):
    """Return markdown, structured workbook, image attachments, and warnings."""

    path = Path(path).expanduser().resolve()
    warnings = []
    integrity = IntegrityReport()
    source_hash = stream_sha256(path)
    source_size = path.stat().st_size

    with zipfile.ZipFile(path) as archive:
        validate_package_paths(archive)
        names = set(archive.namelist())
        types = content_types(archive, names, integrity)
        shared_strings = read_shared_strings(
            archive, names, warnings, integrity
        )
        date_styles = temporal_styles(
            archive, names, warnings, integrity
        )
        workbook = workbook_metadata(
            archive, names, source_hash, warnings, integrity
        )
        assets = AssetStore(
            archive,
            names,
            types,
            warnings,
            external_images,
            integrity,
        )

        for media_part in sorted(
            name for name in names if name.startswith("xl/media/")
        ):
            assets.ensure(media_part)

        catalog_entries, catalog_lookup = cell_image_catalog(
            archive, names, assets, warnings, integrity
        )

        sheets = []
        for descriptor in workbook["worksheets"]:
            sheet_error_start = len(integrity.errors)
            try:
                sheet = read_worksheet(
                    archive,
                    names,
                    descriptor,
                    shared_strings,
                    date_styles,
                    workbook["date_1904"],
                    warnings,
                    integrity,
                )
                raw_relationship_ids = [
                    value
                    for item in sheet.get("raw_hyperlinks", [])
                    for key, value in item.items()
                    if local_name(key) == "id" and value
                ]
                sheet_rels = relationships(
                    archive,
                    names,
                    descriptor["source_part"],
                    warnings,
                    integrity,
                    required=bool(
                        sheet["drawing_relationship_ids"]
                        or sheet["legacy_drawing_relationship_ids"]
                        or raw_relationship_ids
                    ),
                )
                attach_hyperlinks(sheet, sheet_rels, warnings)
                sheet["cleaned_view"] = build_cleaned_view(sheet)
                images = parse_drawings(
                    archive,
                    names,
                    sheet,
                    sheet_rels,
                    assets,
                    warnings,
                    integrity,
                )
                images.extend(
                    formula_image_placements(
                        sheet,
                        catalog_lookup,
                        assets,
                        warnings,
                        integrity,
                    )
                )
                sheet["images"] = images
                if sheet["legacy_drawing_relationship_ids"]:
                    warnings.append(
                        f"工作表 {sheet['name']} 包含旧式 VML Drawing；"
                        "其关系已保留，但不能伪装成已解析的现代图片锚点"
                    )
                    integrity.fail(
                        "excel_legacy_drawing_unparsed",
                        f"工作表 {sheet['name']} 的旧式 VML Drawing 未完成忠实解析",
                        sheet_id=sheet["id"],
                        sheet_name=sheet["name"],
                        relationship_ids=list(
                            sheet["legacy_drawing_relationship_ids"]
                        ),
                    )
                sheet["statistics"] = {
                    "source_row_records": len(sheet["row_metadata"]),
                    "raw_cell_count": len(sheet["cells"]),
                    "cleaned_row_count": len(
                        sheet["cleaned_view"]["rows"]
                    ),
                    "duplicate_rows_marked": sheet["cleaned_view"][
                        "duplicate_rows_marked"
                    ],
                    "duplicate_rows_removed": 0,
                    "formula_count": len(sheet["formulas"]),
                    "hyperlink_count": len(sheet["hyperlinks"]),
                    "image_placement_count": len(sheet["images"]),
                    "truncated": False,
                }
                sheet_errors = integrity.errors[sheet_error_start:]
                if sheet_errors:
                    sheet["parse_status"] = "incomplete"
                    sheet["integrity_errors"] = list(sheet_errors)
            except (
                ElementTree.ParseError,
                KeyError,
                OSError,
                ValueError,
            ) as exc:
                warning = (
                    f"工作表 {descriptor['name']} 解析失败：{exc}"
                )
                warnings.append(warning)
                integrity.fail(
                    "excel_worksheet_parse_failed",
                    warning,
                    sheet_id=descriptor["id"],
                    sheet_name=descriptor["name"],
                    source_part=descriptor.get("source_part"),
                )
                sheet = {
                    **descriptor,
                    "parse_status": "failed",
                    "parse_error": str(exc),
                    "source_dimension": None,
                    "merged_ranges": [],
                    "row_metadata": [],
                    "cells": [],
                    "formulas": [],
                    "hyperlinks": [],
                    "drawing_relationship_ids": [],
                    "legacy_drawing_relationship_ids": [],
                    "cleaned_view": {
                        "header_strategy": None,
                        "header_row": None,
                        "columns": [],
                        "rows": [],
                        "duplicate_rows_marked": 0,
                        "duplicate_rows_removed": 0,
                        "raw_cells_preserved": True,
                    },
                    "images": [],
                    "statistics": {
                        "source_row_records": 0,
                        "raw_cell_count": 0,
                        "cleaned_row_count": 0,
                        "duplicate_rows_marked": 0,
                        "duplicate_rows_removed": 0,
                        "formula_count": 0,
                        "hyperlink_count": 0,
                        "image_placement_count": 0,
                        "truncated": False,
                    },
                }
            sheets.append(sheet)

        if not sheets:
            integrity.fail(
                "excel_no_declared_worksheets",
                "XLSX 不包含可解析的工作表声明",
            )
        if all(sheet.get("parse_status") == "failed" for sheet in sheets):
            integrity.fail(
                "excel_all_worksheets_failed",
                "XLSX 的全部工作表均解析失败",
                declared_worksheet_count=len(
                    workbook["worksheets"]
                ),
            )

        placed_assets = {
            image["asset_id"]
            for sheet in sheets
            for image in sheet["images"]
            if image.get("asset_id")
        }
        attachments = assets.attachments()
        structured = {
            "format": "yunspire.cleaned-workbook.v2",
            "source": {
                "file_name": path.name,
                "byte_length": source_size,
                "sha256": source_hash,
            },
            "sheet_count": len(sheets),
            "sheet_order": [sheet["id"] for sheet in sheets],
            "sheets": sheets,
            "non_worksheet_tabs": workbook["non_worksheet_tabs"],
            "defined_names": workbook["defined_names"],
            "workbook_properties": workbook["workbook_properties"],
            "calc_properties": workbook["calc_properties"],
            "date_1904": workbook["date_1904"],
            "cell_image_catalog": catalog_entries,
            "assets": assets.metadata(),
            "integrity": integrity.output(
                {
                    "declared_tab_count": workbook[
                        "declared_tab_count"
                    ],
                    "declared_worksheet_count": len(
                        workbook["worksheets"]
                    ),
                    "returned_worksheet_count": len(sheets),
                    "parsed_worksheet_count": sum(
                        sheet.get("parse_status") == "parsed"
                        for sheet in sheets
                    ),
                    "incomplete_worksheet_count": sum(
                        sheet.get("parse_status") == "incomplete"
                        for sheet in sheets
                    ),
                    "failed_worksheet_count": sum(
                        sheet.get("parse_status") == "failed"
                        for sheet in sheets
                    ),
                    "resolved_image_placement_count": sum(
                        bool(image.get("asset_id"))
                        for sheet in sheets
                        for image in sheet.get("images", [])
                    ),
                }
            ),
            "unplaced_asset_ids": sorted(
                attachment["asset_id"]
                for attachment in attachments
                if attachment["asset_id"] not in placed_assets
            ),
            "rich_data_parts": sorted(
                name
                for name in names
                if name.startswith("xl/richData/")
                or name == "xl/metadata.xml"
            ),
            "extraction": {
                "real_workbook_sheet_order": True,
                "hidden_sheets_included": True,
                "raw_coordinate_cells_preserved": True,
                "duplicate_rows_removed": False,
                "formulas_recalculated": False,
                "links_opened_or_fetched": False,
                "external_image_localization": localization_summary(
                    [
                        asset
                        for asset in assets.metadata()
                        if asset.get("localization")
                    ]
                ),
                "parse_limits_applied": [],
                "truncated": False,
            },
            "security": {
                "content_role": "untrusted_data",
                "instruction_authority": False,
                "tool_authority": False,
            },
        }

    return workbook_markdown(structured), structured, attachments, warnings


def extract_xlsx(path, external_asset_directory=None):
    """Return markdown, structured workbook, image attachments, and warnings."""

    external_images = ExternalImageLocalizer(external_asset_directory)
    try:
        return _extract_xlsx(path, external_images)
    finally:
        external_images.close()
