#!/usr/bin/env python3
"""Structure-preserving WordprocessingML extraction for Yunspire."""

from __future__ import annotations

import base64
import hashlib
import mimetypes
import posixpath
import re
import zipfile
from pathlib import Path
from xml.etree import ElementTree

from external_image_localizer import (
    ExternalImageLocalizer,
    localization_failure,
    localization_summary,
    public_asset,
)


FORMAT = "yunspire.office-document.v2"
URL_RE = re.compile(r"https?://[^\s<>\"']+", re.IGNORECASE)
URL_TRAILING = ".,;:!?，。！？；：、)]}）】》"
FIELD_URL_RE = re.compile(
    r"^\s*HYPERLINK\s+(?:\"([^\"]+)\"|(\S+))", re.IGNORECASE
)
FIELD_ANCHOR_RE = re.compile(
    r"\\l\s+(?:\"([^\"]+)\"|(\S+))", re.IGNORECASE
)


def _local(tag):
    return str(tag).rsplit("}", 1)[-1]


def _attr(node, name, default=""):
    if node is None:
        return default
    for key, value in node.attrib.items():
        if key == name or _local(key) == name:
            return value
    return default


def _first(node, name):
    return next((item for item in node.iter() if _local(item.tag) == name), None)


def _direct(node, name):
    return [item for item in node if _local(item.tag) == name]


def _relationship_part(part):
    directory, filename = posixpath.split(part)
    return posixpath.join(directory, "_rels", f"{filename}.rels")


def _relationship_kind(value):
    return str(value or "").rstrip("/").rsplit("/", 1)[-1]


def _internal_target(source_part, target):
    if target.startswith("/"):
        normalized = posixpath.normpath(target.lstrip("/"))
    else:
        normalized = posixpath.normpath(
            posixpath.join(posixpath.dirname(source_part), target)
        )
    if normalized.startswith("../") or normalized.startswith("/"):
        return ""
    return normalized


def _stream_sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        while True:
            chunk = source.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def _mime_type(name):
    custom = {
        ".emf": "image/emf",
        ".wmf": "image/wmf",
        ".svg": "image/svg+xml",
        ".tif": "image/tiff",
        ".tiff": "image/tiff",
    }
    suffix = Path(name).suffix.lower()
    return custom.get(suffix) or mimetypes.guess_type(name)[0] or (
        "application/octet-stream"
    )


def _safe_alt(value):
    return re.sub(r"[\[\]\r\n]", " ", str(value or "")).strip()


def _as_int(value, default=0):
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


class _IntegrityReport:
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


class _State:
    def __init__(
        self, archive, names, warnings, external_images, integrity
    ):
        self.archive = archive
        self.names = names
        self.warnings = warnings
        self.external_images = external_images
        self.integrity = integrity
        self.relationship_cache = {}
        self.assets = {}
        self.hyperlinks = []
        self.plain_urls = []
        self.paragraph_count = 0
        self.table_count = 0
        self.inline_count = 0
        self.reference_count = 0
        self.section_count = 0
        self.section_properties = []

    def next_paragraph_id(self):
        self.paragraph_count += 1
        return f"paragraph-{self.paragraph_count:06d}"

    def next_table_id(self):
        self.table_count += 1
        return f"table-{self.table_count:06d}"

    def next_inline_ordinal(self):
        self.inline_count += 1
        return self.inline_count

    def relationships(self, part):
        if part in self.relationship_cache:
            return self.relationship_cache[part]

        relation_part = _relationship_part(part)
        output = {}
        if relation_part not in self.names:
            self.relationship_cache[part] = output
            return output

        try:
            root = ElementTree.fromstring(self.archive.read(relation_part))
        except (KeyError, ElementTree.ParseError) as exc:
            self.warnings.append(f"无法解析关系部件 {relation_part}：{exc}")
            self.integrity.fail(
                "word_relationship_part_unreadable",
                f"无法解析关系部件 {relation_part}：{exc}",
                source_part=part,
                relationship_part=relation_part,
            )
            self.relationship_cache[part] = output
            return output

        for node in root.iter():
            if _local(node.tag) != "Relationship":
                continue
            relationship_id = node.attrib.get("Id", "")
            target = node.attrib.get("Target", "")
            if relationship_id and relationship_id in output:
                self.integrity.fail(
                    "word_relationship_id_duplicate",
                    f"关系部件 {relation_part} 重复声明 {relationship_id}",
                    source_part=part,
                    relationship_part=relation_part,
                    relationship_id=relationship_id,
                )
            if not relationship_id or not target:
                self.integrity.fail(
                    "word_relationship_declaration_invalid",
                    f"关系部件 {relation_part} 包含缺少 Id 或 Target 的声明",
                    source_part=part,
                    relationship_part=relation_part,
                    relationship_id=relationship_id or None,
                    target=target or None,
                )
            target_mode = node.attrib.get("TargetMode", "Internal")
            external = target_mode.lower() == "external"
            target_part = "" if external else _internal_target(part, target)
            if target and not external and not target_part:
                self.warnings.append(
                    f"关系 {relationship_id or '?'} 包含不安全目标：{target}"
                )
                self.integrity.fail(
                    "word_relationship_target_invalid",
                    f"关系 {relationship_id or '?'} 包含不可解析目标：{target}",
                    source_part=part,
                    relationship_part=relation_part,
                    relationship_id=relationship_id or None,
                    target=target,
                )
            output[relationship_id] = {
                "relationship_id": relationship_id,
                "relationship_type": node.attrib.get("Type", ""),
                "relationship_kind": _relationship_kind(
                    node.attrib.get("Type", "")
                ),
                "relationship_part": relation_part,
                "source_part": part,
                "target": target,
                "target_part": target_part or None,
                "target_mode": target_mode,
                "external": external,
            }

        self.relationship_cache[part] = output
        return output

    def image_node(self, part, relationship_id, linked, properties):
        relationship = self.relationships(part).get(relationship_id)
        common = {
            "type": "image",
            "relationship_id": relationship_id or None,
            "source_part": part,
            **properties,
        }
        if relationship is None:
            self.warnings.append(
                f"图片关系 {relationship_id or '?'} 在 "
                f"{_relationship_part(part)} 中不存在"
            )
            self.integrity.fail(
                "word_image_relationship_missing",
                f"图片关系 {relationship_id or '?'} 不存在",
                source_part=part,
                relationship_part=_relationship_part(part),
                relationship_id=relationship_id or None,
            )
            return {
                **common,
                "asset_id": None,
                "attachment_name": None,
                "unresolved": True,
            }

        if relationship.get("relationship_kind") != "image":
            self.integrity.fail(
                "word_image_relationship_type_invalid",
                f"图片关系 {relationship_id or '?'} 的类型不是 image",
                source_part=part,
                relationship_id=relationship_id or None,
                relationship_type=relationship.get("relationship_type"),
            )

        target = relationship.get("target") or ""
        target_part = relationship.get("target_part")
        if relationship["external"] or linked:
            localized = self.external_images.localize(
                target,
                suggested_name=(
                    properties.get("drawing_properties", {}).get("name")
                    or None
                ),
            )
            asset_id = localized["asset_id"]
            asset = self.assets.get(asset_id)
            if asset is None:
                asset = {
                    **public_asset(localized),
                    "target_part": None,
                    "source_parts": [],
                    "relationship_ids": [],
                    "external_sources": [],
                    "references": [],
                }
                if localized.get("_local_path"):
                    asset["_local_path"] = localized["_local_path"]
                self.assets[asset_id] = asset
            else:
                for key in (
                    "localized",
                    "localization",
                    "requested_url",
                    "resolved_url",
                    "detected_format",
                ):
                    if localized.get(key) is not None:
                        asset[key] = localized[key]
                if localized.get("_local_path"):
                    asset["_local_path"] = localized["_local_path"]
            if target and target not in asset.setdefault(
                "external_sources", []
            ):
                asset["external_sources"].append(target)
            attachment_name = asset.get("name")
            failure = localization_failure(localized)
            if failure:
                self.warnings.append(
                    "外链图片本地化失败："
                    f"{failure.get('message')} "
                    f"({failure.get('code')})；来源 {target}"
                )
                self.integrity.fail(
                    "word_required_external_image_unavailable",
                    f"外链图片未能本地化：{target}",
                    source_part=part,
                    relationship_id=relationship_id,
                    target=target,
                    reason_code=failure.get("code"),
                )
        elif target_part and target_part in self.names:
            try:
                data = self.archive.read(target_part)
            except (KeyError, OSError) as exc:
                self.warnings.append(
                    f"无法读取图片部件 {target_part}：{exc}"
                )
                self.integrity.fail(
                    "word_embedded_image_unreadable",
                    f"无法读取图片部件 {target_part}：{exc}",
                    source_part=part,
                    relationship_id=relationship_id,
                    target_part=target_part,
                )
                return {
                    **common,
                    "target_part": target_part,
                    "asset_id": None,
                    "attachment_name": None,
                    "unresolved": True,
                }

            digest = hashlib.sha256(data).hexdigest()
            asset_id = f"sha256:{digest}"
            suffix = Path(target_part).suffix.lower()
            attachment_name = f"asset-{digest}{suffix}"
            asset = self.assets.setdefault(
                asset_id,
                {
                    "asset_id": asset_id,
                    "sha256": digest,
                    "identifier_basis": "content",
                    "embedded": True,
                    "name": attachment_name,
                    "size": len(data),
                    "mime_type": _mime_type(target_part),
                    "target": target,
                    "target_part": target_part,
                    "source_parts": [],
                    "relationship_ids": [],
                    "references": [],
                    "_data": data,
                },
            )
            attachment_name = asset["name"]
        else:
            self.warnings.append(
                f"图片关系 {relationship_id} 指向不存在的部件："
                f"{target_part or target or '?'}"
            )
            self.integrity.fail(
                "word_embedded_image_part_missing",
                f"图片关系 {relationship_id} 指向不存在的部件",
                source_part=part,
                relationship_id=relationship_id,
                target_part=target_part,
                target=target or None,
            )
            return {
                **common,
                "target_part": target_part,
                "asset_id": None,
                "attachment_name": None,
                "unresolved": True,
            }

        if part not in asset["source_parts"]:
            asset["source_parts"].append(part)
        if relationship_id not in asset["relationship_ids"]:
            asset["relationship_ids"].append(relationship_id)

        return {
            **common,
            "relationship": dict(relationship),
            "target_part": target_part,
            "target": target if relationship["external"] else None,
            "asset_id": asset_id,
            "attachment_name": attachment_name,
            "localized": asset.get("localized"),
            "localization": dict(asset.get("localization") or {}),
            "unresolved": bool(localization_failure(asset)),
        }

    def register_image(self, node, location):
        asset_id = node.get("asset_id")
        if not asset_id or asset_id not in self.assets:
            return
        self.reference_count += 1
        reference = {
            "reference_id": f"image-reference-{self.reference_count:06d}",
            "asset_id": asset_id,
            "relationship_id": node.get("relationship_id"),
            "source_part": node.get("source_part"),
            "target_part": node.get("target_part"),
            "placement": node.get("placement"),
            "source_url": node.get("target"),
            **location,
        }
        node["reference"] = reference
        self.assets[asset_id]["references"].append(reference)

    def public_assets(self):
        return [
            {
                key: value
                for key, value in asset.items()
                if key not in {"_data", "_local_path"}
            }
            for asset in self.assets.values()
        ]

    def attachments(self):
        output = []
        for asset in self.assets.values():
            data = asset.get("_data")
            source_parts = list(asset.get("source_parts", []))
            if data is not None:
                payload = {
                    "asset_id": asset["asset_id"],
                    "sha256": asset["sha256"],
                    "name": asset["name"],
                    "size": asset["size"],
                    "mime_type": asset["mime_type"],
                    "data_base64": base64.b64encode(data).decode("ascii"),
                    "source_part": source_parts[0] if source_parts else None,
                    "source_parts": source_parts,
                    "target_part": asset.get("target_part"),
                    "references": asset["references"],
                }
            else:
                payload = self.external_images.attachment_payload(asset)
                if payload is None:
                    continue
                payload.update(
                    {
                        "source_part": (
                            source_parts[0]
                            if source_parts
                            else asset.get("requested_url")
                        ),
                        "source_parts": source_parts,
                        "source_url": asset.get("requested_url"),
                        "references": asset["references"],
                    }
                )
            output.append(payload)
        return output


def _drawing_properties(container):
    if _first(container, "inline") is not None:
        placement = "inline"
    elif _first(container, "anchor") is not None:
        placement = "floating"
    elif _first(container, "imagedata") is not None:
        placement = "vml"
    else:
        placement = "unknown"

    properties = {"placement": placement}
    drawing_properties = _first(container, "docPr")
    if drawing_properties is None:
        drawing_properties = _first(container, "cNvPr")
    if drawing_properties is not None:
        details = {
            "id": drawing_properties.attrib.get("id"),
            "name": drawing_properties.attrib.get("name"),
            "description": drawing_properties.attrib.get("descr"),
            "title": drawing_properties.attrib.get("title"),
        }
        properties["drawing_properties"] = details
        properties["alt_text"] = (
            details["description"]
            or details["title"]
            or details["name"]
            or ""
        )
    else:
        properties["alt_text"] = ""

    extent = _first(container, "extent")
    if extent is not None:
        properties["extent_emus"] = {
            "width": _as_int(extent.attrib.get("cx")),
            "height": _as_int(extent.attrib.get("cy")),
        }

    anchor = _first(container, "anchor")
    if anchor is not None:
        anchor_data = {
            "behind_document": _attr(anchor, "behindDoc") in {"1", "true"},
            "locked": _attr(anchor, "locked") in {"1", "true"},
            "layout_in_cell": _attr(anchor, "layoutInCell") not in {
                "0",
                "false",
            },
            "allow_overlap": _attr(anchor, "allowOverlap") not in {
                "0",
                "false",
            },
        }
        for key, element_name in (
            ("horizontal", "positionH"),
            ("vertical", "positionV"),
        ):
            position = _first(anchor, element_name)
            if position is None:
                continue
            offset = _first(position, "posOffset")
            alignment = _first(position, "align")
            anchor_data[key] = {
                "relative_from": _attr(position, "relativeFrom") or None,
                "offset_emus": (
                    _as_int(offset.text) if offset is not None else None
                ),
                "alignment": (
                    alignment.text if alignment is not None else None
                ),
            }
        wrap = next(
            (
                item
                for item in anchor.iter()
                if _local(item.tag).startswith("wrap")
            ),
            None,
        )
        if wrap is not None:
            anchor_data["wrap"] = _local(wrap.tag)
        properties["anchor"] = anchor_data
    return properties


def _images(container, part, state):
    base_properties = _drawing_properties(container)
    all_links = _drawing_hyperlinks(container, part, state)
    candidates = []
    processed_nodes = set()
    declared_image_nodes = [
        node
        for node in container.iter()
        if _local(node.tag) in {"blip", "imagedata"}
    ]

    def image_relationship(node):
        name = _local(node.tag)
        if name == "blip":
            relationship_id = _attr(node, "embed")
            if relationship_id:
                return relationship_id, False
            relationship_id = _attr(node, "link")
            return relationship_id, bool(relationship_id)
        if name == "imagedata":
            return _attr(node, "id"), False
        return "", False

    def link_identity(link):
        provenance = link.get("provenance", {})
        return (
            link.get("interaction"),
            provenance.get("relationship_id"),
            provenance.get("anchor"),
            link.get("target"),
        )

    def copied_links(links):
        return [
            {**link, "provenance": dict(link.get("provenance", {}))}
            for link in links
        ]

    # A grouped drawing may contain several pictures, including repeated uses
    # of the same relationship. Keep one reference per picture scope instead
    # of collapsing them by relationship ID.
    for scope in container.iter():
        if _local(scope.tag) not in {"pic", "shape"}:
            continue
        scoped_nodes = []
        scoped_seen = set()
        for node in scope.iter():
            relationship_id, linked = image_relationship(node)
            identity = (relationship_id, linked)
            if not relationship_id or identity in scoped_seen:
                continue
            scoped_seen.add(identity)
            scoped_nodes.append((node, relationship_id, linked))
        if not scoped_nodes:
            continue
        scope_properties = _drawing_properties(scope)
        image_properties = dict(base_properties)
        for key in ("drawing_properties", "alt_text"):
            if scope_properties.get(key):
                image_properties[key] = scope_properties[key]
        scope_links = _drawing_hyperlinks(scope, part, state)
        image_properties["hyperlinks"] = copied_links(scope_links)
        for node, relationship_id, linked in scoped_nodes:
            processed_nodes.add(id(node))
            candidates.append(
                state.image_node(
                    part,
                    relationship_id,
                    linked,
                    {
                        **image_properties,
                        "hyperlinks": copied_links(scope_links),
                    },
                )
            )

    # Retain unusual drawing formats whose image relationship is not wrapped
    # in a standard picture or VML shape scope.
    for node in declared_image_nodes:
        if id(node) in processed_nodes:
            continue
        relationship_id, linked = image_relationship(node)
        if not relationship_id:
            state.integrity.fail(
                "word_image_relationship_id_missing",
                f"{part} 中的图片声明没有关系 ID",
                source_part=part,
                image_node=_local(node.tag),
            )
            continue
        candidates.append(
            state.image_node(
                part,
                relationship_id,
                linked,
                {**base_properties, "hyperlinks": []},
            )
        )

    scoped_link_identities = {
        link_identity(link)
        for image in candidates
        for link in image.get("hyperlinks", [])
    }
    drawing_links = [
        link
        for link in all_links
        if link_identity(link) not in scoped_link_identities
    ]
    if len(candidates) == 1:
        candidates[0]["hyperlinks"].extend(copied_links(drawing_links))
    elif drawing_links:
        candidates.append(
            {
                "type": "drawing_hyperlink",
                "text": "",
                "hyperlinks": copied_links(drawing_links),
                "relationship_ids": [
                    image.get("relationship_id")
                    for image in candidates
                    if image.get("relationship_id")
                ],
                "asset_ids": [
                    image.get("asset_id")
                    for image in candidates
                    if image.get("asset_id")
                ],
                "source_part": part,
            }
        )

    output = candidates
    if not output:
        state.warnings.append(
            f"{part} 中存在未绑定图片关系的绘图对象"
        )
        if declared_image_nodes:
            state.integrity.fail(
                "word_declared_image_unresolved",
                f"{part} 中的绘图对象未解析出任何图片",
                source_part=part,
                declared_image_node_count=len(declared_image_nodes),
            )
    return output


def _run_items(run, part, state):
    output = []
    for child in run:
        name = _local(child.tag)
        if name in {"rPr", "lastRenderedPageBreak", "delText"}:
            continue
        if name == "t":
            output.append({"type": "text", "text": child.text or ""})
        elif name == "tab":
            output.append({"type": "tab", "text": "\t"})
        elif name in {"br", "cr"}:
            output.append(
                {
                    "type": "break",
                    "kind": _attr(child, "type") or "line",
                    "text": "\n",
                }
            )
        elif name == "fldChar":
            output.append(
                {
                    "type": "field_control",
                    "action": _attr(child, "fldCharType"),
                    "source_part": part,
                    "text": "",
                }
            )
        elif name == "instrText":
            output.append(
                {
                    "type": "field_instruction",
                    "text": child.text or "",
                    "source_part": part,
                }
            )
        elif name in {"drawing", "pict", "object"}:
            output.extend(_images(child, part, state))
        elif name == "footnoteReference":
            note_id = _attr(child, "id")
            output.append(
                {
                    "type": "footnote_reference",
                    "note_id": note_id,
                    "text": f"[^footnote-{note_id}]",
                }
            )
        elif name == "endnoteReference":
            note_id = _attr(child, "id")
            output.append(
                {
                    "type": "endnote_reference",
                    "note_id": note_id,
                    "text": f"[^endnote-{note_id}]",
                }
            )
        elif name == "commentReference":
            output.append(
                {
                    "type": "comment_reference",
                    "comment_id": _attr(child, "id"),
                    "text": "",
                }
            )
        elif name == "noBreakHyphen":
            output.append({"type": "text", "text": "‑"})
        elif name == "softHyphen":
            output.append({"type": "text", "text": "\u00ad"})
        elif name == "sym":
            try:
                text = chr(int(_attr(child, "char"), 16))
            except (TypeError, ValueError):
                text = ""
            if text:
                output.append({"type": "text", "text": text})
        else:
            output.extend(_inline_items(child, part, state))
    return output


def _hyperlink_target(node, part, state):
    relationship_id = _attr(node, "id")
    anchor = _attr(node, "anchor") or _attr(node, "docLocation")
    relationship = (
        state.relationships(part).get(relationship_id)
        if relationship_id
        else None
    )
    if relationship_id and relationship is None:
        state.warnings.append(
            f"超链接关系 {relationship_id} 在 "
            f"{_relationship_part(part)} 中不存在"
        )
    if relationship:
        target = relationship.get("target") or (
            relationship.get("target_part") or ""
        )
        if anchor:
            target = f"{target}#{anchor}" if target else f"#{anchor}"
        return target, relationship["external"], {
            **relationship,
            "anchor": anchor or None,
            "provenance": "relationship",
        }
    if anchor:
        return f"#{anchor}", False, {
            "relationship_id": None,
            "relationship_part": _relationship_part(part),
            "source_part": part,
            "anchor": anchor,
            "target_mode": "Internal",
            "external": False,
            "provenance": "bookmark",
        }
    return None, False, {
        "relationship_id": relationship_id or None,
        "relationship_part": _relationship_part(part),
        "source_part": part,
        "anchor": None,
        "provenance": "unresolved",
    }


def _drawing_hyperlinks(container, part, state):
    output = []
    seen = set()
    for node in container.iter():
        kind = _local(node.tag)
        if kind not in {"hlinkClick", "hlinkHover", "hlinkMouseOver"}:
            continue
        interaction = "click" if kind == "hlinkClick" else "hover"
        target, external, provenance = _hyperlink_target(node, part, state)
        identity = (
            interaction,
            provenance.get("relationship_id"),
            provenance.get("anchor"),
            target,
        )
        if identity in seen:
            continue
        seen.add(identity)
        output.append(
            {
                "interaction": interaction,
                "target": target,
                "external": external,
                "tooltip": _attr(node, "tooltip") or None,
                "provenance": provenance,
            }
        )
    return output


def _field_hyperlink(instruction, children, part):
    url_match = FIELD_URL_RE.search(instruction or "")
    anchor_match = FIELD_ANCHOR_RE.search(instruction or "")
    target = (
        (url_match.group(1) or url_match.group(2))
        if url_match
        else ""
    )
    anchor = (
        (anchor_match.group(1) or anchor_match.group(2))
        if anchor_match
        else ""
    )
    if anchor:
        target = f"{target}#{anchor}" if target else f"#{anchor}"
    if not target:
        return None
    external = bool(
        re.match(r"^[a-z][a-z0-9+.-]*:", target, re.IGNORECASE)
    )
    return {
        "type": "hyperlink",
        "target": target,
        "external": external,
        "provenance": {
            "relationship_id": None,
            "relationship_part": _relationship_part(part),
            "source_part": part,
            "anchor": anchor or None,
            "target_mode": "External" if external else "Internal",
            "external": external,
            "provenance": "field_instruction",
            "field_instruction": instruction,
        },
        "children": children,
    }


def _inline_items(node, part, state):
    output = []
    for child in node:
        name = _local(child.tag)
        if name in {"pPr", "rPr", "tblPr", "tcPr", "trPr"}:
            continue
        if name == "r":
            output.extend(_run_items(child, part, state))
        elif name == "hyperlink":
            target, external, provenance = _hyperlink_target(
                child, part, state
            )
            output.append(
                {
                    "type": "hyperlink",
                    "target": target,
                    "external": external,
                    "provenance": provenance,
                    "children": _inline_items(child, part, state),
                }
            )
        elif name == "fldSimple":
            children = _inline_items(child, part, state)
            hyperlink = _field_hyperlink(
                _attr(child, "instr"), children, part
            )
            output.extend([hyperlink] if hyperlink else children)
        elif name in {"del", "moveFrom"}:
            continue
        elif name in {
            "ins",
            "moveTo",
            "smartTag",
            "customXml",
            "sdt",
            "sdtContent",
        }:
            output.extend(_inline_items(child, part, state))
        elif name == "AlternateContent":
            choice = next(
                (
                    item
                    for item in child
                    if _local(item.tag) == "Choice"
                ),
                None,
            )
            fallback = next(
                (
                    item
                    for item in child
                    if _local(item.tag) == "Fallback"
                ),
                None,
            )
            selected = choice if choice is not None else fallback
            if selected is not None:
                output.extend(_inline_items(selected, part, state))
        elif name in {"drawing", "pict", "object"}:
            output.extend(_images(child, part, state))
        elif name == "bookmarkStart":
            output.append(
                {
                    "type": "bookmark_start",
                    "bookmark_id": _attr(child, "id"),
                    "name": _attr(child, "name"),
                    "text": "",
                }
            )
        elif name == "bookmarkEnd":
            output.append(
                {
                    "type": "bookmark_end",
                    "bookmark_id": _attr(child, "id"),
                    "text": "",
                }
            )
        elif name in {"commentRangeStart", "commentRangeEnd"}:
            output.append(
                {
                    "type": (
                        "comment_range_start"
                        if name.endswith("Start")
                        else "comment_range_end"
                    ),
                    "comment_id": _attr(child, "id"),
                    "text": "",
                }
            )
        else:
            output.extend(_inline_items(child, part, state))
    return output


def _fold_complex_fields(items, part, state):
    output = []
    stack = []

    def emit(item):
        if item is None:
            return
        if stack:
            if stack[-1]["phase"] == "result":
                stack[-1]["children"].append(item)
            return
        output.append(item)

    for item in items:
        if item.get("type") == "hyperlink":
            item["children"] = _fold_complex_fields(
                item.get("children", []), part, state
            )
        item_type = item.get("type")
        if item_type == "field_control":
            action = item.get("action")
            if action == "begin":
                stack.append(
                    {"instruction": [], "children": [], "phase": "instruction"}
                )
            elif action == "separate":
                if stack:
                    stack[-1]["phase"] = "result"
                else:
                    state.warnings.append(f"{part} 中存在没有 begin 的字段 separate")
            elif action == "end":
                if not stack:
                    state.warnings.append(f"{part} 中存在没有 begin 的字段 end")
                    continue
                frame = stack.pop()
                hyperlink = _field_hyperlink(
                    "".join(frame["instruction"]), frame["children"], part
                )
                folded = [hyperlink] if hyperlink else frame["children"]
                for child in folded:
                    emit(child)
            continue
        if item_type == "field_instruction":
            if stack:
                stack[-1]["instruction"].append(item.get("text", ""))
            continue
        emit(item)

    while stack:
        frame = stack.pop()
        state.warnings.append(f"{part} 中存在未闭合的复杂字段，已保留可见结果")
        folded = list(frame["children"])
        if stack:
            stack[-1]["children"].extend(folded)
        else:
            output.extend(folded)
    return output


def _split_urls(text):
    output = []
    cursor = 0
    for match in URL_RE.finditer(text):
        if match.start() > cursor:
            output.append(
                {"type": "text", "text": text[cursor:match.start()]}
            )
        candidate = match.group(0)
        target = candidate.rstrip(URL_TRAILING)
        trailing = candidate[len(target):]
        if target:
            output.append(
                {"type": "url", "text": target, "target": target}
            )
        if trailing:
            output.append({"type": "text", "text": trailing})
        cursor = match.end()
    if cursor < len(text):
        output.append({"type": "text", "text": text[cursor:]})
    return output


def _normalize_inlines(items, split_urls=True):
    output = []
    for item in items:
        if item.get("type") == "text" and split_urls:
            output.extend(_split_urls(item.get("text", "")))
        elif item.get("type") == "hyperlink":
            item["children"] = _normalize_inlines(
                item.get("children", []), False
            )
            output.append(item)
        else:
            output.append(item)
    return output


def _visible_text(items):
    output = []
    for item in items:
        if item.get("type") == "hyperlink":
            output.append(_visible_text(item.get("children", [])))
        else:
            output.append(item.get("text", ""))
    return "".join(output)


def _position_inlines(items, state, location, cursor):
    images = []
    for inline_index, item in enumerate(items):
        item["inline_index"] = inline_index
        item["inline_ordinal"] = state.next_inline_ordinal()
        start = cursor[0]
        item_type = item.get("type")

        if item_type == "hyperlink":
            item["text_offset_start"] = start
            images.extend(
                _position_inlines(
                    item.get("children", []),
                    state,
                    location,
                    cursor,
                )
            )
            item["text_offset_end"] = cursor[0]
            item["text"] = _visible_text(item.get("children", []))
            item["link_index"] = len(state.hyperlinks)
            state.hyperlinks.append(
                {
                    "target": item.get("target"),
                    "display_text": item["text"],
                    "external": bool(item.get("external")),
                    "provenance": item.get("provenance", {}),
                    **location,
                    "text_offset_start": item["text_offset_start"],
                    "text_offset_end": item["text_offset_end"],
                    "inline_ordinal": item["inline_ordinal"],
                }
            )
            continue

        if item_type == "image":
            item["text_offset"] = cursor[0]
            image_location = {
                **location,
                "text_offset": cursor[0],
                "inline_ordinal": item["inline_ordinal"],
            }
            state.register_image(
                item,
                image_location,
            )
            link_indices = []
            for drawing_link in item.get("hyperlinks", []):
                link_index = len(state.hyperlinks)
                drawing_link["link_index"] = link_index
                link_indices.append(link_index)
                reference = item.get("reference") or {}
                state.hyperlinks.append(
                    {
                        "target": drawing_link.get("target"),
                        "display_text": item.get("alt_text") or "图片链接",
                        "external": bool(drawing_link.get("external")),
                        "source_kind": "image_hyperlink",
                        "interaction": drawing_link.get("interaction"),
                        "tooltip": drawing_link.get("tooltip"),
                        "asset_id": item.get("asset_id"),
                        "image_reference_id": reference.get("reference_id"),
                        "provenance": drawing_link.get("provenance", {}),
                        **image_location,
                        "text_offset_start": cursor[0],
                        "text_offset_end": cursor[0],
                    }
                )
            if link_indices:
                item["link_indices"] = link_indices
                if item.get("reference") is not None:
                    item["reference"]["link_indices"] = list(link_indices)
            images.append(item)
            continue

        if item_type == "drawing_hyperlink":
            link_indices = []
            for drawing_link in item.get("hyperlinks", []):
                link_index = len(state.hyperlinks)
                link_indices.append(link_index)
                state.hyperlinks.append(
                    {
                        "target": drawing_link.get("target"),
                        "display_text": "组合绘图链接",
                        "external": bool(drawing_link.get("external")),
                        "source_kind": "drawing_hyperlink",
                        "interaction": drawing_link.get("interaction"),
                        "tooltip": drawing_link.get("tooltip"),
                        "related_asset_ids": list(item.get("asset_ids", [])),
                        "related_relationship_ids": list(
                            item.get("relationship_ids", [])
                        ),
                        "provenance": drawing_link.get("provenance", {}),
                        **location,
                        "text_offset_start": cursor[0],
                        "text_offset_end": cursor[0],
                        "inline_ordinal": item["inline_ordinal"],
                    }
                )
            item["link_indices"] = link_indices
            continue

        text = item.get("text", "")
        cursor[0] += len(text)
        item["text_offset_start"] = start
        item["text_offset_end"] = cursor[0]
        if item_type == "url":
            item["plain_url_index"] = len(state.plain_urls)
            state.plain_urls.append(
                {
                    "target": item.get("target"),
                    "display_text": text,
                    "external": True,
                    "provenance": {
                        "source_part": location["source_part"],
                        "provenance": "plain_text",
                    },
                    **location,
                    "text_offset_start": start,
                    "text_offset_end": cursor[0],
                    "inline_ordinal": item["inline_ordinal"],
                }
            )
    return images


def _paragraph_properties(paragraph):
    properties = next(
        (item for item in paragraph if _local(item.tag) == "pPr"),
        None,
    )
    if properties is None:
        return {
            "style": None,
            "numbering": None,
            "section_break": False,
        }

    style_node = _first(properties, "pStyle")
    numbering = _first(properties, "numPr")
    numbering_data = None
    if numbering is not None:
        level = _first(numbering, "ilvl")
        number_id = _first(numbering, "numId")
        numbering_data = {
            "level": _as_int(_attr(level, "val")),
            "number_id": (
                _attr(number_id, "val") if number_id is not None else None
            ),
        }
    return {
        "style": (
            _attr(style_node, "val") if style_node is not None else None
        ),
        "numbering": numbering_data,
        "section_break": _first(properties, "sectPr") is not None,
    }


def _parse_paragraph(paragraph, part, state, path, location_base):
    paragraph_id = state.next_paragraph_id()
    inlines = _normalize_inlines(
        _fold_complex_fields(
            _inline_items(paragraph, part, state), part, state
        )
    )
    location = {
        "source_part": part,
        "block_path": path,
        "body_block_index": location_base.get("body_block_index"),
        "paragraph_id": paragraph_id,
        "table_path": list(location_base.get("table_path", [])),
        "table_cell": location_base.get("table_cell"),
        "story_type": location_base.get("story_type", "body"),
        "story_id": location_base.get("story_id"),
    }
    cursor = [0]
    images = _position_inlines(
        inlines, state, location, cursor
    )
    text = _visible_text(inlines)
    for image in images:
        offset = image.get("text_offset", 0)
        before = text[max(0, offset - 300):offset]
        after = text[offset:offset + 300]
        image["before_text"] = before
        image["after_text"] = after
        if image.get("reference") is not None:
            image["reference"]["before_text"] = before
            image["reference"]["after_text"] = after

    properties = _paragraph_properties(paragraph)
    paragraph_record = {
        "type": "paragraph",
        "id": paragraph_id,
        "native_id": _attr(paragraph, "paraId") or None,
        "source_part": part,
        "path": path,
        "properties": properties,
        "text": text,
        "inlines": inlines,
    }
    paragraph_properties = next(
        (item for item in paragraph if _local(item.tag) == "pPr"), None
    )
    section = (
        next(
            (item for item in paragraph_properties if _local(item.tag) == "sectPr"),
            None,
        )
        if paragraph_properties is not None
        else None
    )
    if section is not None:
        state.section_properties.append(
            _parse_section_properties(
                section,
                part,
                state,
                f"{path}/properties/section",
                {
                    "owner_type": "paragraph",
                    "paragraph_id": paragraph_id,
                    "block_path": path,
                },
            )
        )
    return paragraph_record


def _cell_properties(cell):
    properties = next(
        (item for item in cell if _local(item.tag) == "tcPr"),
        None,
    )
    if properties is None:
        return {"grid_span": 1, "vertical_merge": None}
    grid_span = _first(properties, "gridSpan")
    vertical_merge = _first(properties, "vMerge")
    span = max(1, _as_int(_attr(grid_span, "val"), 1))
    merge = None
    if vertical_merge is not None:
        merge = _attr(vertical_merge, "val") or "continue"
    return {"grid_span": span, "vertical_merge": merge}


def _ordered_block_elements(container):
    for child in container:
        name = _local(child.tag)
        if name in {"p", "tbl"}:
            yield child
        elif name in {"del", "moveFrom"}:
            continue
        elif name in {"sdt", "customXml", "ins", "moveTo"}:
            content = next(
                (
                    item
                    for item in child
                    if _local(item.tag) == "sdtContent"
                ),
                child,
            )
            yield from _ordered_block_elements(content)


def _parse_blocks(container, part, state, base_path, location_base):
    blocks = []
    for child in container:
        if _local(child.tag) == "altChunk":
            relationship_id = _attr(child, "id")
            state.integrity.fail(
                "word_alt_chunk_unparsed",
                f"{part} 包含无法忠实解析的 altChunk",
                source_part=part,
                relationship_id=relationship_id or None,
                block_path=base_path,
            )
    for local_index, child in enumerate(
        _ordered_block_elements(container)
    ):
        path = f"{base_path}/{local_index}"
        location = dict(location_base)
        if (
            location.get("story_type", "body") == "body"
            and location.get("body_block_index") is None
        ):
            location["body_block_index"] = local_index
        if _local(child.tag) == "p":
            blocks.append(
                _parse_paragraph(
                    child, part, state, path, location
                )
            )
        else:
            blocks.append(
                _parse_table(
                    child, part, state, path, location
                )
            )
    return blocks


def _parse_table(table, part, state, path, location_base):
    table_id = state.next_table_id()
    table_path = list(location_base.get("table_path", []))
    table_path.append(table_id)

    grid = next(
        (item for item in table if _local(item.tag) == "tblGrid"),
        None,
    )
    grid_columns = []
    if grid is not None:
        for column in _direct(grid, "gridCol"):
            grid_columns.append(
                {"width_twips": _as_int(_attr(column, "w"))}
            )

    rows = []
    row_nodes = [
        item for item in table if _local(item.tag) == "tr"
    ]
    for row_index, row in enumerate(row_nodes):
        cells = []
        column_index = 0
        for cell_index, cell in enumerate(_direct(row, "tc")):
            properties = _cell_properties(cell)
            cell_id = (
                f"{table_id}-row-{row_index + 1}-"
                f"cell-{cell_index + 1}"
            )
            table_cell = {
                "cell_id": cell_id,
                "row_index": row_index,
                "cell_index": cell_index,
                "column_index": column_index,
                "grid_span": properties["grid_span"],
                "vertical_merge": properties["vertical_merge"],
            }
            cell_path = (
                f"{path}/rows/{row_index}/cells/"
                f"{cell_index}/blocks"
            )
            blocks = _parse_blocks(
                cell,
                part,
                state,
                cell_path,
                {
                    **location_base,
                    "table_path": table_path,
                    "table_cell": table_cell,
                },
            )
            cells.append(
                {
                    **table_cell,
                    "path": (
                        f"{path}/rows/{row_index}/cells/{cell_index}"
                    ),
                    "blocks": blocks,
                    "text": "\n".join(
                        block.get("text", "")
                        for block in blocks
                        if block.get("text")
                    ),
                }
            )
            column_index += properties["grid_span"]
        rows.append({"row_index": row_index, "cells": cells})

    return {
        "type": "table",
        "id": table_id,
        "source_part": part,
        "path": path,
        "grid_columns": grid_columns,
        "rows": rows,
    }


def _render_inlines(items):
    output = []
    ignored = {
        "bookmark_start",
        "bookmark_end",
        "comment_range_start",
        "comment_range_end",
        "comment_reference",
    }
    for item in items:
        item_type = item.get("type")
        if item_type == "hyperlink":
            label = _render_inlines(item.get("children", []))
            label = label or item.get("target") or "链接"
            target = item.get("target")
            output.append(f"[{label}]({target})" if target else label)
        elif item_type == "url":
            output.append(
                f"<{item.get('target', item.get('text', ''))}>"
            )
        elif item_type == "image":
            alt = _safe_alt(item.get("alt_text") or "图片")
            reference_id = (item.get("reference") or {}).get("reference_id")
            if item.get("attachment_name") and reference_id:
                output.append(
                    f"![{alt}](attachment://"
                    f"{reference_id})"
                )
            elif item.get("attachment_name"):
                output.append("<!-- unresolved-image-reference -->")
            elif item.get("target"):
                failure = item.get("localization") or {}
                output.append(
                    "[外链图片本地化失败："
                    f"{alt}；{failure.get('message') or '无法读取'}]"
                )
            else:
                output.append("<!-- unresolved-image -->")
        elif item_type == "tab":
            output.append("    ")
        elif item_type == "break":
            if item.get("kind") == "page":
                output.append(
                    "\n\n<!-- page-break -->\n\n"
                )
            else:
                output.append("  \n")
        elif item_type in ignored:
            continue
        else:
            output.append(item.get("text", ""))
    return "".join(output)


def _render_paragraph(block):
    content = _render_inlines(block.get("inlines", []))
    properties = block.get("properties", {})
    style = str(properties.get("style") or "")
    heading = re.search(
        r"(?:heading|标题)\s*([1-6])",
        style,
        re.IGNORECASE,
    )
    if heading:
        return f"{'#' * int(heading.group(1))} {content}"
    numbering = properties.get("numbering")
    if numbering:
        indent = "  " * int(numbering.get("level") or 0)
        return f"{indent}- {content}"
    return content


def _render_table(block):
    lines = ["<table>", "  <tbody>"]
    for row in block.get("rows", []):
        lines.append("    <tr>")
        for cell in row.get("cells", []):
            attributes = []
            if cell.get("grid_span", 1) > 1:
                attributes.append(
                    f' colspan="{cell["grid_span"]}"'
                )
            if cell.get("vertical_merge"):
                attributes.append(
                    f' data-vertical-merge="'
                    f'{cell["vertical_merge"]}"'
                )
            lines.append(
                f"      <td{''.join(attributes)}>"
            )
            rendered = _render_blocks(cell.get("blocks", []))
            lines.extend(
                f"        {line}"
                for line in rendered.splitlines()
            )
            lines.append("      </td>")
        lines.append("    </tr>")
    lines.extend(["  </tbody>", "</table>"])
    return "\n".join(lines)


def _render_blocks(blocks):
    rendered = []
    for block in blocks:
        if block.get("type") == "paragraph":
            rendered.append(_render_paragraph(block))
        elif block.get("type") == "table":
            rendered.append(_render_table(block))
    return "\n\n".join(rendered)


def _parse_section_properties(section, part, state, path, owner):
    state.section_count += 1
    references = []
    relationships = state.relationships(part)
    for node in section.iter():
        kind = _local(node.tag)
        if kind not in {"headerReference", "footerReference"}:
            continue
        relationship_id = _attr(node, "id")
        relationship = relationships.get(relationship_id)
        references.append(
            {
                "kind": "header" if kind == "headerReference" else "footer",
                "variant": _attr(node, "type") or "default",
                "relationship_id": relationship_id or None,
                "target_part": (
                    relationship.get("target_part") if relationship else None
                ),
                "relationship": relationship,
            }
        )
        if relationship_id and relationship is None:
            state.warnings.append(
                f"分节属性引用了不存在的关系 {relationship_id}"
            )
            state.integrity.fail(
                "word_story_relationship_missing",
                f"分节属性引用了不存在的关系 {relationship_id}",
                source_part=part,
                relationship_id=relationship_id,
                story_type=(
                    "header" if kind == "headerReference" else "footer"
                ),
            )
        elif not relationship_id:
            state.integrity.fail(
                "word_story_relationship_id_missing",
                "分节属性中的页眉或页脚声明缺少关系 ID",
                source_part=part,
                story_type=(
                    "header" if kind == "headerReference" else "footer"
                ),
            )
        elif not relationship.get("target_part"):
            state.integrity.fail(
                "word_story_target_missing",
                f"分节属性关系 {relationship_id} 没有可解析的 story 部件",
                source_part=part,
                relationship_id=relationship_id,
                story_type=(
                    "header" if kind == "headerReference" else "footer"
                ),
            )
        elif relationship.get("relationship_kind") != (
            "header" if kind == "headerReference" else "footer"
        ):
            expected = "header" if kind == "headerReference" else "footer"
            state.integrity.fail(
                "word_story_relationship_type_invalid",
                f"分节属性关系 {relationship_id} 的类型不是 {expected}",
                source_part=part,
                relationship_id=relationship_id,
                relationship_type=relationship.get("relationship_type"),
                expected_story_type=expected,
            )
    page_size = _first(section, "pgSz")
    page_margin = _first(section, "pgMar")
    return {
        "section_id": f"section-{state.section_count:06d}",
        "source_part": part,
        "path": path,
        "owner": owner,
        "references": references,
        "page_size_twips": (
            {
                "width": _as_int(_attr(page_size, "w")),
                "height": _as_int(_attr(page_size, "h")),
                "orientation": _attr(page_size, "orient") or "portrait",
            }
            if page_size is not None
            else None
        ),
        "page_margins_twips": (
            {
                key: _as_int(_attr(page_margin, key))
                for key in ("top", "right", "bottom", "left", "header", "footer", "gutter")
            }
            if page_margin is not None
            else None
        ),
    }


def _read_xml_part(part, state):
    if not part or part not in state.names:
        state.warnings.append(f"Word 附属部件不存在：{part or '?'}")
        state.integrity.fail(
            "word_story_part_missing",
            f"Word 附属 story 部件不存在：{part or '?'}",
            source_part=part or None,
        )
        return None
    try:
        return ElementTree.fromstring(state.archive.read(part))
    except (KeyError, ElementTree.ParseError) as exc:
        state.warnings.append(f"无法解析 Word 附属部件 {part}：{exc}")
        state.integrity.fail(
            "word_story_part_unreadable",
            f"无法解析 Word 附属 story 部件 {part}：{exc}",
            source_part=part,
        )
        return None


def _story_location(story_type, story_id):
    return {
        "story_type": story_type,
        "story_id": story_id,
        "body_block_index": None,
        "table_path": [],
        "table_cell": None,
    }


def _parse_related_parts(document_part, state):
    document_relationships = state.relationships(document_part)
    sections_by_target = {}
    for section in state.section_properties:
        for reference in section.get("references", []):
            target = reference.get("target_part")
            if target:
                sections_by_target.setdefault(target, []).append(section["section_id"])

    related = {
        "headers": [],
        "footers": [],
        "footnotes": [],
        "endnotes": [],
        "comments": [],
    }
    story_groups = {}
    for relationship in document_relationships.values():
        kind = relationship.get("relationship_kind")
        target = relationship.get("target_part")
        if kind in {
            "header",
            "footer",
            "footnotes",
            "endnotes",
            "comments",
        } and not target:
            state.integrity.fail(
                "word_story_target_missing",
                f"声明的 {kind} 关系没有可解析目标",
                source_part=document_part,
                relationship_id=relationship.get("relationship_id"),
                story_type=kind,
            )
        if kind in {"header", "footer"} and target:
            key = (kind, target)
            record = story_groups.setdefault(
                key,
                {
                    "story_type": kind,
                    "story_id": f"{kind}-{len(story_groups) + 1:06d}",
                    "source_part": target,
                    "relationship_ids": [],
                    "referenced_by_sections": sorted(set(sections_by_target.get(target, []))),
                    "blocks": [],
                },
            )
            record["relationship_ids"].append(relationship["relationship_id"])

    for (kind, target), record in story_groups.items():
        root = _read_xml_part(target, state)
        if root is not None:
            expected_root = "hdr" if kind == "header" else "ftr"
            if _local(root.tag) != expected_root:
                state.integrity.fail(
                    "word_story_root_invalid",
                    f"{target} 的根部件不是 {expected_root}",
                    source_part=target,
                    story_type=kind,
                    root_name=_local(root.tag),
                )
            record["blocks"] = _parse_blocks(
                root,
                target,
                state,
                f"related_parts/{kind}s/{record['story_id']}/blocks",
                _story_location(kind, record["story_id"]),
            )
        related[f"{kind}s"].append(record)

    for relationship in document_relationships.values():
        kind = relationship.get("relationship_kind")
        if kind not in {"footnotes", "endnotes", "comments"}:
            continue
        part = relationship.get("target_part")
        root = _read_xml_part(part, state)
        if root is None:
            continue
        expected_root = (
            kind if kind in {"footnotes", "endnotes"} else "comments"
        )
        if _local(root.tag) != expected_root:
            state.integrity.fail(
                "word_story_root_invalid",
                f"{part} 的根部件不是 {expected_root}",
                source_part=part,
                story_type=kind,
                root_name=_local(root.tag),
            )
        if kind in {"footnotes", "endnotes"}:
            singular = "footnote" if kind == "footnotes" else "endnote"
            for node in root:
                if _local(node.tag) != singular:
                    continue
                note_id = _attr(node, "id")
                note_type = _attr(node, "type") or "normal"
                related[kind].append(
                    {
                        "note_id": note_id,
                        "note_type": note_type,
                        "special": note_type != "normal",
                        "source_part": part,
                        "blocks": _parse_blocks(
                            node,
                            part,
                            state,
                            f"related_parts/{kind}/{note_id}/blocks",
                            _story_location(singular, note_id),
                        ),
                    }
                )
        else:
            for node in root:
                if _local(node.tag) != "comment":
                    continue
                comment_id = _attr(node, "id")
                related["comments"].append(
                    {
                        "comment_id": comment_id,
                        "author": _attr(node, "author") or None,
                        "initials": _attr(node, "initials") or None,
                        "created_at": _attr(node, "date") or None,
                        "source_part": part,
                        "blocks": _parse_blocks(
                            node,
                            part,
                            state,
                            f"related_parts/comments/{comment_id}/blocks",
                            _story_location("comment", comment_id),
                        ),
                    }
                )
    return related


def _collect_story_references(value, output=None):
    output = output or {
        "footnote_reference": set(),
        "endnote_reference": set(),
        "comment_reference": set(),
        "comment_range_start": set(),
        "comment_range_end": set(),
    }
    if isinstance(value, dict):
        item_type = value.get("type")
        if item_type in output:
            identifier = value.get("note_id") or value.get("comment_id")
            if identifier not in {None, ""}:
                output[item_type].add(str(identifier))
        for item in value.values():
            _collect_story_references(item, output)
    elif isinstance(value, list):
        for item in value:
            _collect_story_references(item, output)
    return output


def _validate_story_references(blocks, related, state):
    references = _collect_story_references(blocks)
    definitions = {
        "footnote_reference": {
            str(item.get("note_id"))
            for item in related.get("footnotes", [])
            if not item.get("special")
        },
        "endnote_reference": {
            str(item.get("note_id"))
            for item in related.get("endnotes", [])
            if not item.get("special")
        },
        "comment_reference": {
            str(item.get("comment_id"))
            for item in related.get("comments", [])
        },
    }
    definitions["comment_range_start"] = definitions["comment_reference"]
    definitions["comment_range_end"] = definitions["comment_reference"]
    for reference_type, identifiers in references.items():
        missing = sorted(identifiers - definitions[reference_type])
        for identifier in missing:
            state.integrity.fail(
                "word_story_reference_unresolved",
                f"{reference_type} {identifier} 没有对应的 story 定义",
                story_type=reference_type,
                story_id=identifier,
            )


def _definition(label, rendered):
    lines = rendered.splitlines() or [""]
    return f"{label}: {lines[0]}" + "".join(f"\n    {line}" for line in lines[1:])


def _render_related_parts(related):
    sections = []
    for key, title in (("headers", "页眉"), ("footers", "页脚")):
        for index, story in enumerate(related.get(key, []), 1):
            content = _render_blocks(story.get("blocks", []))
            if content:
                sections.append(
                    f"## {title} {index}\n\n"
                    f"<!-- source_part={story['source_part']} -->\n\n{content}"
                )
    definitions = []
    for key, prefix in (("footnotes", "footnote"), ("endnotes", "endnote")):
        for note in related.get(key, []):
            if note.get("special"):
                continue
            definitions.append(
                _definition(
                    f"[^{prefix}-{note['note_id']}]",
                    _render_blocks(note.get("blocks", [])),
                )
            )
    if definitions:
        sections.append("## 脚注与尾注\n\n" + "\n\n".join(definitions))
    comments = []
    for comment in related.get("comments", []):
        content = _render_blocks(comment.get("blocks", []))
        author_suffix = f" · {comment['author']}" if comment.get("author") else ""
        comments.append(
            f"### 批注 {comment['comment_id']}{author_suffix}\n\n{content}"
        )
    if comments:
        sections.append("## 批注\n\n" + "\n\n".join(comments))
    return "\n\n".join(sections)


def _main_document_part(archive, names, warnings, integrity):
    package_relationships = "_rels/.rels"
    if package_relationships not in names:
        integrity.fail(
            "word_package_relationships_missing",
            "DOCX 缺少包级关系部件 _rels/.rels",
            relationship_part=package_relationships,
        )
    else:
        try:
            root = ElementTree.fromstring(
                archive.read(package_relationships)
            )
            found_office_document = False
            for node in root.iter():
                if _local(node.tag) != "Relationship":
                    continue
                if _relationship_kind(
                    node.attrib.get("Type", "")
                ) != "officeDocument":
                    continue
                found_office_document = True
                target = posixpath.normpath(
                    node.attrib.get("Target", "").lstrip("/")
                )
                if (
                    target in names
                    and not target.startswith("../")
                ):
                    return target
                integrity.fail(
                    "word_main_document_target_missing",
                    "包级 officeDocument 关系指向不存在的主文档部件",
                    relationship_part=package_relationships,
                    target=target or None,
                )
            if not found_office_document:
                integrity.fail(
                    "word_main_document_relationship_missing",
                    "包级关系部件没有 officeDocument 声明",
                    relationship_part=package_relationships,
                )
        except ElementTree.ParseError as exc:
            warnings.append(
                f"无法解析 {package_relationships}：{exc}"
            )
            integrity.fail(
                "word_package_relationships_unreadable",
                f"无法解析 {package_relationships}：{exc}",
                relationship_part=package_relationships,
            )
    if "word/document.xml" in names:
        return "word/document.xml"
    return ""


def _validate_archive_paths(archive):
    for info in archive.infolist():
        normalized = posixpath.normpath(info.filename)
        if (
            normalized.startswith("../")
            or normalized.startswith("/")
        ):
            raise ValueError("OOXML 包含不安全的文件路径")


def extract_docx(path, external_asset_directory=None):
    """Return (markdown, structured, attachments, warnings).

    No document, text, media, table or attachment size limit is
    imposed here. Downstream models may batch requests, but callers
    must not discard source text or original attachment bytes.
    """

    source_path = Path(path).expanduser().resolve()
    warnings = []
    integrity = _IntegrityReport()
    if not source_path.is_file():
        raise FileNotFoundError(
            f"Word 文件不存在：{source_path}"
        )
    external_images = ExternalImageLocalizer(external_asset_directory)

    try:
        with zipfile.ZipFile(source_path) as archive:
            _validate_archive_paths(archive)
            names = set(archive.namelist())
            document_part = _main_document_part(
                archive, names, warnings, integrity
            )
            if not document_part:
                raise ValueError(
                    "DOCX 缺少 Word 主文档部件"
                )
            try:
                root = ElementTree.fromstring(
                    archive.read(document_part)
                )
            except ElementTree.ParseError as exc:
                raise ValueError(
                    f"无法解析 Word 主文档：{exc}"
                ) from exc

            body = _first(root, "body")
            if body is None:
                raise ValueError(
                    "Word 主文档缺少正文 body"
                )

            state = _State(
                archive, names, warnings, external_images, integrity
            )
            blocks = _parse_blocks(
                body,
                document_part,
                state,
                "body/blocks",
                {
                    "story_type": "body",
                    "story_id": None,
                    "body_block_index": None,
                    "table_path": [],
                    "table_cell": None,
                },
            )
            for index, block in enumerate(blocks):
                block["body_block_index"] = index

            for node in body:
                if _local(node.tag) != "sectPr":
                    continue
                state.section_properties.append(
                    _parse_section_properties(
                        node,
                        document_part,
                        state,
                        f"body/section-properties/{state.section_count}",
                        {"owner_type": "body", "block_path": "body"},
                    )
                )

            markdown = _render_blocks(blocks)
            related_parts = _parse_related_parts(document_part, state)
            _validate_story_references(blocks, related_parts, state)
            related_markdown = _render_related_parts(related_parts)
            if related_markdown:
                markdown = f"{markdown}\n\n---\n\n{related_markdown}"
            attachments = state.attachments()
            structured = {
                "format": FORMAT,
                "document_type": "wordprocessingml",
                "source": {
                    "file_name": source_path.name,
                    "byte_length": source_path.stat().st_size,
                    "sha256": _stream_sha256(source_path),
                    "main_document_part": document_part,
                },
                "blocks": blocks,
                "section_properties": state.section_properties,
                "related_parts": related_parts,
                "assets": state.public_assets(),
                "hyperlinks": state.hyperlinks,
                "plain_urls": state.plain_urls,
                "integrity": integrity.output(
                    {
                        "main_document_part": document_part,
                        "body_parsed": True,
                        "declared_section_story_count": sum(
                            len(section.get("references", []))
                            for section in state.section_properties
                        ),
                        "parsed_header_count": len(
                            related_parts["headers"]
                        ),
                        "parsed_footer_count": len(
                            related_parts["footers"]
                        ),
                        "parsed_footnote_count": len(
                            related_parts["footnotes"]
                        ),
                        "parsed_endnote_count": len(
                            related_parts["endnotes"]
                        ),
                        "parsed_comment_count": len(
                            related_parts["comments"]
                        ),
                        "resolved_image_reference_count": (
                            state.reference_count
                        ),
                    }
                ),
                "statistics": {
                    "body_block_count": len(blocks),
                    "paragraph_count": state.paragraph_count,
                    "table_count": state.table_count,
                    "asset_count": len(state.assets),
                    "image_reference_count": (
                        state.reference_count
                    ),
                    "hyperlink_count": len(
                        state.hyperlinks
                    ),
                    "plain_url_count": len(
                        state.plain_urls
                    ),
                    "header_count": len(related_parts["headers"]),
                    "footer_count": len(related_parts["footers"]),
                    "footnote_count": len(
                        [item for item in related_parts["footnotes"] if not item["special"]]
                    ),
                    "endnote_count": len(
                        [item for item in related_parts["endnotes"] if not item["special"]]
                    ),
                    "comment_count": len(related_parts["comments"]),
                },
                "external_image_localization": localization_summary(
                    [
                        asset
                        for asset in state.public_assets()
                        if asset.get("localization")
                    ]
                ),
                "security": {
                    "content_role": "untrusted_data",
                    "instruction_authority": False,
                    "tool_authority": False,
                    "external_targets_are_data": True,
                },
            }
            return (
                markdown,
                structured,
                attachments,
                warnings,
            )
    except zipfile.BadZipFile as exc:
        raise ValueError(
            f"无效的 DOCX/OOXML 文件：{exc}"
        ) from exc
    finally:
        external_images.close()


__all__ = ["extract_docx"]
